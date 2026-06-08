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
