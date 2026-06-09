//! PNG chunk framing (read + write).
//!
//! Every chunk has this layout (RFC 2083 §3.2):
//!
//! ```text
//!   4 bytes  length  (big-endian, *only* the data portion)
//!   4 bytes  type    (ASCII, case-sensitive)
//!   N bytes  data    (where N = length)
//!   4 bytes  CRC32   (over type + data, PNG flavour)
//! ```
//!
//! The 8-byte magic `\x89PNG\r\n\x1a\n` precedes the first chunk.

use crate::error::{PngError as Error, Result};
use crate::filter::crc32;

/// PNG file magic.
pub const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Maximum chunk length we'll accept from untrusted input. The spec allows
/// up to 2^31-1 bytes, but it's unreasonable in practice.
pub const MAX_CHUNK_LEN: u32 = 0x7FFF_FFFF;

/// A parsed chunk borrowed from a larger buffer.
#[derive(Debug, Clone, Copy)]
pub struct ChunkRef<'a> {
    pub chunk_type: [u8; 4],
    pub data: &'a [u8],
}

impl<'a> ChunkRef<'a> {
    pub fn type_str(&self) -> &str {
        std::str::from_utf8(&self.chunk_type).unwrap_or("????")
    }

    pub fn is_type(&self, t: &[u8; 4]) -> bool {
        &self.chunk_type == t
    }

    /// Return the typed [`ChunkType`] for this chunk's 4-byte name so callers
    /// can interrogate the W3C PNG 3rd Edition §5.4 property bits without
    /// re-deriving them at every call site.
    pub fn type_code(&self) -> ChunkType {
        ChunkType(self.chunk_type)
    }
}

/// Wrapper over a PNG chunk's 4-byte ASCII name exposing the W3C PNG 3rd
/// Edition §5.4 ("Chunk naming conventions") property bits.
///
/// Bit 5 (value `0x20`) of each name byte is the property bit for that
/// position — uppercase = `0`, lowercase = `1` — and the four positions
/// have distinct semantics (§5.4 Table 6):
///
/// | Byte | Bit 5 = 0 (uppercase) | Bit 5 = 1 (lowercase) |
/// |------|-----------------------|------------------------|
/// | 1    | critical              | ancillary              |
/// | 2    | public                | private                |
/// | 3    | reserved-bit clear    | reserved-bit set       |
/// | 4    | unsafe to copy        | safe to copy           |
///
/// The §5.4 worked example (the hypothetical chunk type `cHNk`) decodes as
/// ancillary, public, reserved-bit-clear, safe-to-copy.
///
/// Property bits are "an inherent part of the chunk type, and hence are
/// fixed for any chunk type" (§5.4) — `CHNK` and `cHNk` are unrelated
/// chunk types, not the same chunk with different properties. Callers
/// that want chunk-identity comparison should use [`ChunkRef::is_type`]
/// (or equality on the wrapped `[u8; 4]`); the property accessors below
/// only matter for chunks whose 4-byte name the decoder does not
/// recognise (§13.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkType(pub [u8; 4]);

impl ChunkType {
    /// W3C PNG 3rd Edition §5.4 property-bit mask (bit 5, value 32 / `0x20`).
    /// Identical to the case-bit of an ASCII letter — uppercase has bit 5
    /// clear, lowercase has bit 5 set.
    pub const PROPERTY_BIT: u8 = 0x20;

    /// Wrap a 4-byte chunk name.
    pub const fn new(name: [u8; 4]) -> Self {
        Self(name)
    }

    /// Borrow the raw 4-byte name.
    pub const fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }

    /// Render the chunk name as a UTF-8 `&str`. PNG chunk names are pure
    /// ASCII letters per §5.4, so a non-letter byte (e.g. from a corrupted
    /// or private third-party chunk that violates the §13.1 "type names
    /// shall consist of letters" rule) falls back to `"????"` — the same
    /// fallback [`ChunkRef::type_str`] uses.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("????")
    }

    /// §5.4 Table 6 row 1: ancillary bit (byte 1, lowercase = 1).
    /// `true` for chunks an unaware decoder may safely skip past
    /// (e.g. `tIME`, `tEXt`, `pHYs`), `false` for chunks the decoder
    /// must understand to display the image (`IHDR`, `PLTE`, `IDAT`,
    /// `IEND`). When the bit is `0` on an unrecognised chunk type a
    /// decoder "shall indicate to the user that the image contains
    /// information it cannot safely interpret" (§5.4).
    pub const fn is_ancillary(&self) -> bool {
        self.0[0] & Self::PROPERTY_BIT != 0
    }

    /// Inverse of [`Self::is_ancillary`] — `true` when the chunk is one
    /// the decoder must understand to display the image.
    pub const fn is_critical(&self) -> bool {
        !self.is_ancillary()
    }

    /// §5.4 Table 6 row 2: private bit (byte 2, lowercase = 1).
    /// `true` when the chunk type is a private extension (lowercase
    /// second letter, e.g. `prVt`). Public chunk types are "reserved for
    /// definition by the W3C" (§5.4) — every chunk in §11.2 / §11.3 of
    /// the spec uses an uppercase second letter.
    pub const fn is_private(&self) -> bool {
        self.0[1] & Self::PROPERTY_BIT != 0
    }

    /// Inverse of [`Self::is_private`].
    pub const fn is_public(&self) -> bool {
        !self.is_private()
    }

    /// §5.4 Table 6 row 3: reserved bit (byte 3, lowercase = 1). "If
    /// the reserved bit is 1, the datastream does not conform to this
    /// version of PNG." (§5.4) — surfaced here so callers can reject
    /// such streams without redoing the bit-math. Every chunk defined
    /// by W3C PNG 3rd Edition uses an uppercase third letter
    /// (§5.4 closing sentence: "all chunk names shall have uppercase
    /// third letters").
    pub const fn is_reserved_bit_set(&self) -> bool {
        self.0[2] & Self::PROPERTY_BIT != 0
    }

    /// §5.4 Table 6 row 4: safe-to-copy bit (byte 4, lowercase = 1).
    /// Relevant only to PNG editors (§14.2): on an unrecognised
    /// ancillary chunk the editor may carry forward chunks where the
    /// fourth-letter property bit is `1` even after image-data edits,
    /// but must drop those where it is `0`. All five §11.3.2 colour-
    /// space chunks (`cHRM`, `gAMA`, `iCCP`, `sBIT`, `sRGB`) are
    /// unsafe-to-copy because their meaning depends on image-data
    /// fidelity, and `tIME` is unsafe-to-copy because the last-
    /// modification time becomes incorrect the moment image data is
    /// edited (uppercase fourth letter `E`); the text chunks
    /// (`tEXt` / `zTXt` / `iTXt`) and `pHYs` survive image-data edits
    /// unchanged and so carry the safe-to-copy bit set.
    pub const fn is_safe_to_copy(&self) -> bool {
        self.0[3] & Self::PROPERTY_BIT != 0
    }

    /// Inverse of [`Self::is_safe_to_copy`].
    pub const fn is_unsafe_to_copy(&self) -> bool {
        !self.is_safe_to_copy()
    }

    /// Check the §5.4 / §13.1 well-formedness constraint that every
    /// byte of the chunk name "shall consist of letters" — that is, an
    /// ASCII A..Z or a..z. A chunk whose name contains a digit or a
    /// punctuation byte is non-conforming even if its property bits
    /// happen to be all-zero.
    pub const fn is_well_formed_name(&self) -> bool {
        let mut i = 0;
        while i < 4 {
            let b = self.0[i];
            let upper = b >= b'A' && b <= b'Z';
            let lower = b >= b'a' && b <= b'z';
            if !(upper || lower) {
                return false;
            }
            i += 1;
        }
        true
    }
}

impl From<[u8; 4]> for ChunkType {
    fn from(b: [u8; 4]) -> Self {
        Self(b)
    }
}

impl From<ChunkType> for [u8; 4] {
    fn from(c: ChunkType) -> [u8; 4] {
        c.0
    }
}

/// The IHDR colour-type byte (W3C PNG 3rd Edition §11.2.1 "Color type
/// is a single-byte integer") with the W3C PNG3 §6.1 / Table 9 named
/// values surfaced so callers do not have to memorise the numeric
/// encoding every time they branch on it.
///
/// §6.1 defines each colour type as "the sum of the following values:
/// 1 (palette used), 2 (truecolor used) and 4 (alpha used)" — so the
/// five permitted values land on a sparse subset of `0..=7`:
///
/// | Value | PNG image type        | Components               |
/// |-------|-----------------------|--------------------------|
/// | 0     | Greyscale             | gray                     |
/// | 2     | Truecolor             | R, G, B                  |
/// | 3     | Indexed-color         | palette index            |
/// | 4     | Greyscale with alpha  | gray, alpha              |
/// | 6     | Truecolor with alpha  | R, G, B, alpha           |
///
/// Values 1, 5, and 7 are explicitly absent from §6.1 / Table 9 — `1`
/// would mean "palette used without truecolor", which is meaningless
/// (an indexed image is colour type 3, not 1), and `5` / `7` would
/// imply "palette + alpha" combinations the spec does not define
/// (indexed transparency lives in the `tRNS` chunk, not the colour
/// type byte). [`ColourType::from_byte`] therefore rejects every
/// value outside `{0, 2, 3, 4, 6}` so callers cannot accidentally
/// invent a non-conforming combination.
///
/// W3C PNG3 §11.2.1 Table 12 ("Allowed combinations of color type and
/// bit depth") restricts which bit depths pair with which colour
/// type; the [`ColourType::allows_bit_depth`] predicate decodes that
/// table without reproducing it at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ColourType {
    /// Colour type 0 — each pixel is a greyscale sample.
    Greyscale = 0,
    /// Colour type 2 — each pixel is an R,G,B triple.
    Truecolor = 2,
    /// Colour type 3 — each pixel is a palette index; a `PLTE` chunk
    /// shall appear (Table 12).
    IndexedColor = 3,
    /// Colour type 4 — each pixel is a greyscale sample followed by
    /// an alpha sample.
    GreyscaleAlpha = 4,
    /// Colour type 6 — each pixel is an R,G,B triple followed by an
    /// alpha sample.
    TruecolorAlpha = 6,
}

impl ColourType {
    /// W3C PNG3 §6.1 "palette used" component bit of the colour-type
    /// integer (set on colour type 3 only).
    pub const PALETTE_USED_BIT: u8 = 1;
    /// W3C PNG3 §6.1 "truecolor used" component bit (set on colour
    /// types 2 and 6).
    pub const TRUECOLOR_USED_BIT: u8 = 2;
    /// W3C PNG3 §6.1 "alpha used" component bit (set on colour types
    /// 4 and 6).
    pub const ALPHA_USED_BIT: u8 = 4;

    /// Parse the raw IHDR colour-type byte into the typed enum. Values
    /// outside `{0, 2, 3, 4, 6}` are rejected — §6.1 / Table 9 lists
    /// no other valid combinations.
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Greyscale),
            2 => Some(Self::Truecolor),
            3 => Some(Self::IndexedColor),
            4 => Some(Self::GreyscaleAlpha),
            6 => Some(Self::TruecolorAlpha),
            _ => None,
        }
    }

    /// The on-wire byte for this colour type.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// `true` when the §6.1 "palette used" component bit is set
    /// (colour type 3 only).
    pub const fn palette_used(self) -> bool {
        self.as_byte() & Self::PALETTE_USED_BIT != 0
    }

    /// `true` when the §6.1 "truecolor used" component bit is set
    /// (colour types 2 and 6).
    pub const fn truecolor_used(self) -> bool {
        self.as_byte() & Self::TRUECOLOR_USED_BIT != 0
    }

    /// `true` when the §6.1 "alpha used" component bit is set
    /// (colour types 4 and 6).
    pub const fn alpha_used(self) -> bool {
        self.as_byte() & Self::ALPHA_USED_BIT != 0
    }

    /// Number of samples per pixel implied by the colour type.
    /// `3` for truecolor, `4` for truecolor-with-alpha, `2` for
    /// greyscale-with-alpha, `1` for greyscale and indexed.
    pub const fn channels(self) -> usize {
        match self {
            Self::Greyscale | Self::IndexedColor => 1,
            Self::GreyscaleAlpha => 2,
            Self::Truecolor => 3,
            Self::TruecolorAlpha => 4,
        }
    }

    /// Decode W3C PNG3 §11.2.1 Table 12 ("Allowed combinations of
    /// color type and bit depth"). `true` when the `(colour_type,
    /// bit_depth)` pair is one of the rows in that table:
    ///
    /// * colour type 0 (greyscale): 1, 2, 4, 8, 16
    /// * colour type 2 (truecolor): 8, 16
    /// * colour type 3 (indexed-color): 1, 2, 4, 8
    /// * colour type 4 (greyscale with alpha): 8, 16
    /// * colour type 6 (truecolor with alpha): 8, 16
    ///
    /// Every other pair is non-conforming — e.g. an attempt to
    /// encode RGB at 4-bit, or indexed at 16-bit.
    pub const fn allows_bit_depth(self, bit_depth: u8) -> bool {
        match self {
            Self::Greyscale => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
            Self::IndexedColor => matches!(bit_depth, 1 | 2 | 4 | 8),
            Self::Truecolor | Self::GreyscaleAlpha | Self::TruecolorAlpha => {
                matches!(bit_depth, 8 | 16)
            }
        }
    }

    /// `true` when colour type 3 (indexed) — the one row of Table 12
    /// that requires a `PLTE` chunk. Convenience wrapper around
    /// [`Self::palette_used`] kept for readability at call sites that
    /// branch on the chunk presence rather than the bit.
    pub const fn requires_plte(self) -> bool {
        matches!(self, Self::IndexedColor)
    }
}

/// Read one chunk starting at `buf[pos..]`, verify its CRC32, and return
/// the parsed `ChunkRef` + the updated position.
pub fn read_chunk<'a>(buf: &'a [u8], pos: usize) -> Result<(ChunkRef<'a>, usize)> {
    if pos + 8 > buf.len() {
        return Err(Error::invalid("PNG: truncated chunk header"));
    }
    let len = u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
    if len > MAX_CHUNK_LEN {
        return Err(Error::invalid(format!(
            "PNG: chunk length {len} exceeds maximum"
        )));
    }
    let type_start = pos + 4;
    let data_start = type_start + 4;
    let data_end = data_start
        .checked_add(len as usize)
        .ok_or_else(|| Error::invalid("PNG: chunk length overflow"))?;
    let crc_end = data_end + 4;
    if crc_end > buf.len() {
        return Err(Error::invalid("PNG: chunk extends past end of buffer"));
    }
    let mut chunk_type = [0u8; 4];
    chunk_type.copy_from_slice(&buf[type_start..type_start + 4]);
    let data = &buf[data_start..data_end];

    let declared = u32::from_be_bytes([
        buf[data_end],
        buf[data_end + 1],
        buf[data_end + 2],
        buf[data_end + 3],
    ]);
    let computed = crc32(&buf[type_start..data_end]);
    if declared != computed {
        return Err(Error::invalid(format!(
            "PNG: bad CRC on chunk {:?} (expected {:08X}, got {:08X})",
            std::str::from_utf8(&chunk_type).unwrap_or("????"),
            declared,
            computed
        )));
    }

    Ok((ChunkRef { chunk_type, data }, crc_end))
}

/// Write one chunk to `out`. Appends: length, type, data, CRC32.
pub fn write_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    // CRC over type + data.
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(chunk_type);
    crc_input.extend_from_slice(data);
    let c = crc32(&crc_input);
    out.extend_from_slice(&c.to_be_bytes());
}

/// Iterator over chunks in a PNG file buffer (starting after the magic).
pub struct ChunkIter<'a> {
    buf: &'a [u8],
    pos: usize,
    done: bool,
}

impl<'a> ChunkIter<'a> {
    pub fn new(buf: &'a [u8], start: usize) -> Self {
        Self {
            buf,
            pos: start,
            done: false,
        }
    }
}

impl<'a> Iterator for ChunkIter<'a> {
    type Item = Result<ChunkRef<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.pos >= self.buf.len() {
            return None;
        }
        match read_chunk(self.buf, self.pos) {
            Ok((c, next)) => {
                self.pos = next;
                if c.chunk_type == *b"IEND" {
                    self.done = true;
                }
                Some(Ok(c))
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrip() {
        let mut out = Vec::new();
        write_chunk(&mut out, b"IHDR", &[1, 2, 3, 4]);
        let (chunk, end) = read_chunk(&out, 0).unwrap();
        assert_eq!(&chunk.chunk_type, b"IHDR");
        assert_eq!(chunk.data, &[1, 2, 3, 4]);
        assert_eq!(end, out.len());
    }

    #[test]
    fn bad_crc_rejected() {
        let mut out = Vec::new();
        write_chunk(&mut out, b"IHDR", &[1, 2, 3, 4]);
        // Flip one CRC byte.
        let last = out.len() - 1;
        out[last] ^= 0x01;
        let err = read_chunk(&out, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    // §5.4 ("Chunk naming conventions") property bits — every assertion
    // below cross-checks the bit-5 case-flag interpretation against the
    // table's worked example or against a chunk that the spec itself
    // classifies into the property bucket under test.

    #[test]
    fn property_bits_match_w3c_png3_section_5_4_table_6_worked_example() {
        // §5.4 hypothetical chunk type `cHNk`:
        //   * lower-case first letter → ancillary bit set
        //   * upper-case second letter → public (private bit clear)
        //   * upper-case third letter → reserved bit clear
        //   * lower-case fourth letter → safe-to-copy
        let t = ChunkType::new(*b"cHNk");
        assert!(t.is_ancillary());
        assert!(!t.is_critical());
        assert!(!t.is_private());
        assert!(t.is_public());
        assert!(!t.is_reserved_bit_set());
        assert!(t.is_safe_to_copy());
        assert!(!t.is_unsafe_to_copy());
        assert!(t.is_well_formed_name());
        assert_eq!(t.as_str(), "cHNk");
    }

    #[test]
    fn critical_chunks_have_uppercase_first_letter() {
        for name in [b"IHDR", b"PLTE", b"IDAT", b"IEND"] {
            let t = ChunkType::new(*name);
            assert!(t.is_critical(), "{} should be critical", t.as_str());
            assert!(!t.is_ancillary());
            assert!(t.is_public(), "{} should be public", t.as_str());
            assert!(!t.is_reserved_bit_set());
            assert!(t.is_well_formed_name());
        }
    }

    #[test]
    fn ancillary_chunks_have_lowercase_first_letter() {
        // §11.3 catalogue — every chunk defined as ancillary by the
        // spec. All carry lowercase first letters (ancillary bit set);
        // the second-letter case differs across the catalogue and is
        // exercised by `apng_chunks_carry_private_second_letter` below.
        for name in [
            b"tRNS", b"cHRM", b"gAMA", b"iCCP", b"sBIT", b"sRGB", b"cICP", b"mDCV", b"cLLI",
            b"tEXt", b"zTXt", b"iTXt", b"bKGD", b"hIST", b"pHYs", b"sPLT", b"eXIf", b"tIME",
            b"acTL", b"fcTL", b"fdAT",
        ] {
            let t = ChunkType::new(*name);
            assert!(t.is_ancillary(), "{} should be ancillary", t.as_str());
            assert!(!t.is_critical());
            assert!(!t.is_reserved_bit_set());
            assert!(t.is_well_formed_name());
        }
    }

    #[test]
    fn public_vs_private_split_across_section_11_3_catalogue() {
        // Public second letter (uppercase): the §11.3 colour /
        // miscellaneous / text / timestamp chunks all use an uppercase
        // second letter, matching the §5.4 rule that public chunks are
        // "reserved for definition by the W3C".
        for name in [
            b"tRNS", b"cHRM", b"gAMA", b"iCCP", b"sBIT", b"sRGB", b"cICP", b"mDCV", b"cLLI",
            b"tEXt", b"zTXt", b"iTXt", b"bKGD", b"hIST", b"pHYs", b"sPLT", b"eXIf", b"tIME",
        ] {
            let t = ChunkType::new(*name);
            assert!(t.is_public(), "{} should be public", t.as_str());
        }
        // APNG animation chunks (§11.3.6) were originally a Mozilla
        // extension; their second letter is lowercase, so the §5.4
        // private bit reads as set even though W3C PNG 3rd Edition
        // now documents them. The property bit is "an inherent part
        // of the chunk type, and hence … fixed" (§5.4) — once minted
        // with a lowercase second letter the bit cannot be changed
        // without renaming the chunk.
        for name in [b"acTL", b"fcTL", b"fdAT"] {
            let t = ChunkType::new(*name);
            assert!(t.is_private(), "{} should read as private", t.as_str());
        }
    }

    #[test]
    fn safe_to_copy_bit_matches_spec_classification() {
        // §11.3.2 colour-space chunks are all unsafe-to-copy because
        // they change meaning when image-data is edited (§14.2): the
        // fourth-letter property bit is uppercase across the bucket.
        // `tIME` joins them because the last-modification time becomes
        // incorrect the moment image data is edited — uppercase `E`.
        for name in [b"cHRM", b"gAMA", b"iCCP", b"sBIT", b"sRGB", b"tIME"] {
            let t = ChunkType::new(*name);
            assert!(
                t.is_unsafe_to_copy(),
                "{} should be unsafe-to-copy",
                t.as_str()
            );
        }
        // Text + physical-dimensions chunks survive image edits
        // unchanged → fourth letter is lowercase.
        for name in [b"tEXt", b"zTXt", b"iTXt", b"pHYs"] {
            let t = ChunkType::new(*name);
            assert!(t.is_safe_to_copy(), "{} should be safe-to-copy", t.as_str());
        }
    }

    #[test]
    fn private_bit_distinguishes_hypothetical_private_chunk() {
        // §5.7.3 / §12.10.1 reserve lowercase-second-letter chunk names
        // for private (third-party) extensions. We never define one
        // ourselves, but the property bit should classify a synthesised
        // example correctly. Using a fully-letter name so well-formedness
        // passes.
        let t = ChunkType::new(*b"prVt");
        assert!(t.is_private());
        assert!(!t.is_public());
        assert!(t.is_ancillary());
    }

    #[test]
    fn reserved_bit_flags_nonconforming_third_letter_case() {
        // §5.4 "all chunk names shall have uppercase third letters" —
        // lowercase third letter signals a non-conforming datastream.
        let t = ChunkType::new(*b"abcd");
        assert!(t.is_reserved_bit_set());
        // Conforming names never trip the reserved bit.
        for name in [b"IHDR", b"tIME", b"iTXt", b"acTL"] {
            assert!(!ChunkType::new(*name).is_reserved_bit_set());
        }
    }

    #[test]
    fn well_formed_name_rejects_non_letter_bytes() {
        // Digits / punctuation / control bytes are not valid chunk-name
        // bytes per §13.1 "type names shall consist of letters".
        assert!(!ChunkType::new(*b"1HDR").is_well_formed_name());
        assert!(!ChunkType::new(*b"IH_R").is_well_formed_name());
        assert!(!ChunkType::new([0xFF, 0x00, 0x80, 0x7F]).is_well_formed_name());
        // ASCII-letter names — both cases — are accepted.
        assert!(ChunkType::new(*b"IHDR").is_well_formed_name());
        assert!(ChunkType::new(*b"abcd").is_well_formed_name());
        assert!(ChunkType::new(*b"AbCd").is_well_formed_name());
    }

    // §6.1 / Table 9 colour-type encoding + §11.2.1 / Table 12
    // allowed-combinations table — every assertion below cross-checks
    // the typed wrapper against the worked entries in those tables.

    #[test]
    fn colour_type_from_byte_accepts_section_6_1_table_9_values() {
        assert_eq!(ColourType::from_byte(0), Some(ColourType::Greyscale));
        assert_eq!(ColourType::from_byte(2), Some(ColourType::Truecolor));
        assert_eq!(ColourType::from_byte(3), Some(ColourType::IndexedColor));
        assert_eq!(ColourType::from_byte(4), Some(ColourType::GreyscaleAlpha));
        assert_eq!(ColourType::from_byte(6), Some(ColourType::TruecolorAlpha));
    }

    #[test]
    fn colour_type_from_byte_rejects_undefined_combinations() {
        // §6.1 / Table 9 does not list 1 ("palette without truecolor"),
        // 5, or 7 ("palette + alpha"). Values above 7 cannot be valid
        // either since the §6.1 bit components only span 0..=7.
        for b in [1u8, 5, 7, 8, 16, 127, 255] {
            assert_eq!(
                ColourType::from_byte(b),
                None,
                "byte {b} should not parse as a colour type"
            );
        }
    }

    #[test]
    fn colour_type_round_trips_through_byte() {
        for ct in [
            ColourType::Greyscale,
            ColourType::Truecolor,
            ColourType::IndexedColor,
            ColourType::GreyscaleAlpha,
            ColourType::TruecolorAlpha,
        ] {
            assert_eq!(ColourType::from_byte(ct.as_byte()), Some(ct));
        }
    }

    #[test]
    fn colour_type_component_bits_match_section_6_1_definition() {
        // §6.1 "the sum of the following values: 1 (palette used),
        // 2 (truecolor used) and 4 (alpha used)".
        // Greyscale = 0: none of the bits set.
        assert!(!ColourType::Greyscale.palette_used());
        assert!(!ColourType::Greyscale.truecolor_used());
        assert!(!ColourType::Greyscale.alpha_used());
        // Truecolor = 2: truecolor bit only.
        assert!(!ColourType::Truecolor.palette_used());
        assert!(ColourType::Truecolor.truecolor_used());
        assert!(!ColourType::Truecolor.alpha_used());
        // IndexedColor = 3 = 1 | 2: palette + truecolor (a palette
        // entry IS an RGB triple per §11.2.2).
        assert!(ColourType::IndexedColor.palette_used());
        assert!(ColourType::IndexedColor.truecolor_used());
        assert!(!ColourType::IndexedColor.alpha_used());
        // GreyscaleAlpha = 4: alpha bit only.
        assert!(!ColourType::GreyscaleAlpha.palette_used());
        assert!(!ColourType::GreyscaleAlpha.truecolor_used());
        assert!(ColourType::GreyscaleAlpha.alpha_used());
        // TruecolorAlpha = 6 = 2 | 4: truecolor + alpha.
        assert!(!ColourType::TruecolorAlpha.palette_used());
        assert!(ColourType::TruecolorAlpha.truecolor_used());
        assert!(ColourType::TruecolorAlpha.alpha_used());
    }

    #[test]
    fn colour_type_channels_match_section_4_5_pixel_layout() {
        // §4.5 / §6.1 pixel decompositions: greyscale + indexed are
        // 1 channel, greyscale-with-alpha is 2, truecolor is 3,
        // truecolor-with-alpha is 4. The numbers also fall out of
        // the §6.1 bit math: channels = (truecolor_used ? 3 : 1) +
        // (alpha_used ? 1 : 0).
        assert_eq!(ColourType::Greyscale.channels(), 1);
        assert_eq!(ColourType::Truecolor.channels(), 3);
        assert_eq!(ColourType::IndexedColor.channels(), 1);
        assert_eq!(ColourType::GreyscaleAlpha.channels(), 2);
        assert_eq!(ColourType::TruecolorAlpha.channels(), 4);
    }

    #[test]
    fn colour_type_allows_bit_depth_decodes_table_12_rows() {
        // Greyscale (row 1): 1, 2, 4, 8, 16 allowed; nothing else.
        for bd in [1u8, 2, 4, 8, 16] {
            assert!(ColourType::Greyscale.allows_bit_depth(bd));
        }
        for bd in [0u8, 3, 5, 6, 7, 9, 12, 32] {
            assert!(!ColourType::Greyscale.allows_bit_depth(bd));
        }
        // Truecolor (row 2): 8, 16 only.
        assert!(ColourType::Truecolor.allows_bit_depth(8));
        assert!(ColourType::Truecolor.allows_bit_depth(16));
        for bd in [1u8, 2, 4, 32] {
            assert!(!ColourType::Truecolor.allows_bit_depth(bd));
        }
        // IndexedColor (row 3): 1, 2, 4, 8 (no 16-bit indexed).
        for bd in [1u8, 2, 4, 8] {
            assert!(ColourType::IndexedColor.allows_bit_depth(bd));
        }
        for bd in [0u8, 3, 5, 7, 16, 32] {
            assert!(!ColourType::IndexedColor.allows_bit_depth(bd));
        }
        // GreyscaleAlpha (row 4): 8, 16 only.
        for bd in [8u8, 16] {
            assert!(ColourType::GreyscaleAlpha.allows_bit_depth(bd));
        }
        for bd in [1u8, 2, 4] {
            assert!(!ColourType::GreyscaleAlpha.allows_bit_depth(bd));
        }
        // TruecolorAlpha (row 5): 8, 16 only.
        for bd in [8u8, 16] {
            assert!(ColourType::TruecolorAlpha.allows_bit_depth(bd));
        }
        for bd in [1u8, 2, 4] {
            assert!(!ColourType::TruecolorAlpha.allows_bit_depth(bd));
        }
    }

    #[test]
    fn colour_type_requires_plte_matches_table_12_palette_row() {
        // Only colour type 3 (indexed) carries the "a PLTE chunk
        // shall appear" rider in Table 12.
        assert!(ColourType::IndexedColor.requires_plte());
        assert!(!ColourType::Greyscale.requires_plte());
        assert!(!ColourType::Truecolor.requires_plte());
        assert!(!ColourType::GreyscaleAlpha.requires_plte());
        assert!(!ColourType::TruecolorAlpha.requires_plte());
    }

    #[test]
    fn chunk_ref_type_code_round_trips_to_bytes() {
        // Verify the ChunkRef → ChunkType bridge so callers that hold a
        // borrowed chunk can interrogate the property bits without
        // copying the four bytes through a local.
        let mut out = Vec::new();
        write_chunk(&mut out, b"tEXt", b"keyword\0value");
        let (chunk, _) = read_chunk(&out, 0).unwrap();
        let code = chunk.type_code();
        assert_eq!(code.as_bytes(), b"tEXt");
        assert!(code.is_ancillary());
        assert!(code.is_public());
        assert!(code.is_safe_to_copy());
        // From / Into round-trip.
        let raw: [u8; 4] = code.into();
        assert_eq!(raw, *b"tEXt");
        assert_eq!(ChunkType::from(raw), code);
    }
}
