//! PNG ancillary metadata chunks — `sBIT`, `pHYs`, `tIME`, `bKGD`,
//! `hIST`, `eXIf`, `sRGB`, `cICP`, `iCCP`, `gAMA`, `cHRM`, `sPLT`,
//! `tEXt`, `zTXt`, `iTXt`.
//!
//! All but `eXIf` and `sPLT` are short, fixed-layout chunks with no
//! embedded compression and no cross-chunk dependencies (with the single
//! exception that `hIST` must accompany a `PLTE` and match its length),
//! which makes them the natural first pass at round-tripping PNG
//! metadata through this codec. `eXIf` is variable-length but is treated
//! as an opaque TIFF-formatted blob: we validate only its byte-order
//! header and round-trip the bytes verbatim. `sPLT` is variable-length
//! and is the one chunk here that "Multiple OK? = Yes" (W3C PNG3 §5.6
//! Table 7); we parse each instance fully and require distinct palette
//! names across the datastream.
//!
//! Spec references (RFC 2083 = PNG 1.0; the W3C PNG 3rd edition preserves
//! the same layouts):
//!
//! - `sBIT` — RFC 2083 §4.2.6 "Significant bits". Per-channel bit counts
//!   indicating how many high-order bits of each stored sample carry real
//!   data; the rest were created by spec-mandated sample-depth scaling
//!   (RFC 2083 §9.1 / §10.4). One byte per channel of the IHDR colour type:
//!
//!   | Colour type | Channels | sBIT length |
//!   |-------------|----------|-------------|
//!   | 0 (grey)    | G        | 1           |
//!   | 2 (RGB)     | R G B    | 3           |
//!   | 3 (palette) | R G B    | 3           |
//!   | 4 (grey+α)  | G A      | 2           |
//!   | 6 (RGBA)    | R G B A  | 4           |
//!
//!   Each entry must be `1..=sample_depth` (8 for colour type 3, IHDR bit
//!   depth otherwise; RFC 2083 §4.2.6 final paragraph). `sBIT` must precede
//!   the first `IDAT` and the `PLTE` chunk.
//!
//! - `pHYs` — RFC 2083 §4.2.5 "Physical pixel dimensions". 9 bytes:
//!   `pixels_per_unit_x (u32 BE)`, `pixels_per_unit_y (u32 BE)`,
//!   `unit_specifier (u8: 0 = unknown / aspect-ratio only, 1 = metres)`.
//!   Must precede the first `IDAT`.
//!
//! - `tIME` — RFC 2083 §4.2.8 "Image last-modification time". 7 bytes:
//!   `year (u16 BE)`, `month`, `day`, `hour`, `minute`, `second` (all
//!   `u8`). UTC; `second` may legally be `60` for a leap second. No
//!   ordering constraint.
//!
//! - `bKGD` — RFC 2083 §4.2.1 / W3C PNG3 §11.3.4.1 "Background color".
//!   Layout depends on the IHDR colour type:
//!
//!   | Colour type      | Payload                                  | Length  |
//!   |------------------|------------------------------------------|---------|
//!   | 0, 4 (grey ±α)   | `grey: u16 BE`                           | 2 bytes |
//!   | 2, 6 (RGB ±α)    | `r: u16 BE, g: u16 BE, b: u16 BE`        | 6 bytes |
//!   | 3 (indexed)      | `palette_index: u8`                      | 1 byte  |
//!
//!   For sub-16-bit images "the least significant bits are used. Encoders
//!   should set the other bits to 0, and decoders must mask the other bits
//!   to 0 before the value is used" (W3C PNG3 §11.3.4.1 final paragraph).
//!   `bKGD` must follow `PLTE` and precede the first `IDAT`. Multiple
//!   `bKGD` chunks are forbidden.
//!
//! - `hIST` — RFC 2083 §4.2.4 / W3C PNG3 §11.3.4.2 "Image histogram".
//!   `2 × N` bytes, `N = PLTE entry count`. Each `u16 BE` is the
//!   approximate usage count for the matching palette index; only meaningful
//!   when a `PLTE` is present. `hIST` must follow `PLTE` and precede the
//!   first `IDAT`. Multiple `hIST` chunks are forbidden.
//!
//! - `eXIf` — W3C PNG3 §11.3.4.5 "Exchangeable Image File (Exif)
//!   Profile". Variable-length: the payload is an Exif/TIFF profile in
//!   the [CIPA-DC-008] §4.7.2 interoperability layout, **minus** the JPEG
//!   `APP1` marker, length field, and `"Exif\0"` ID code. PNG treats it
//!   as opaque metadata "concerning the original image data"; we neither
//!   parse nor interpret the TIFF directory, only round-trip it. The spec
//!   does require the first four bytes to be one of the two TIFF byte-order
//!   magic words — `49 49 2A 00` (`"II"`, little-endian, 16-bit `42`) or
//!   `4D 4D 00 2A` (`"MM"`, big-endian, 16-bit `42`) — "all other values
//!   are reserved" (§11.3.4.5.2). We reject any other header so a
//!   malformed blob can't masquerade as Exif. `eXIf` must precede the
//!   first `IDAT` (§5.6 Table 1) and only one is permitted.
//!
//! - `sRGB` — W3C PNG3 §11.3.2.5 "Standard RGB color space". A single
//!   byte naming the ICC rendering intent the image samples should be
//!   displayed with (`0` Perceptual / `1` Relative colorimetric /
//!   `2` Saturation / `3` Absolute colorimetric — Table 16). The
//!   chunk's mere presence asserts the samples conform to the sRGB
//!   colour space; values `4..=255` are reserved (§11.3.2.5) and
//!   rejected on parse. `sRGB` must precede `PLTE` and the first
//!   `IDAT` (§5.6 Table 1); only one is permitted.
//!
//! - `cICP` — W3C PNG3 §11.3.2.6 "Coding-independent code points for
//!   video signal type identification". Exactly four `u8` fields per
//!   Table 18: `color_primaries`, `transfer_function`,
//!   `matrix_coefficients`, `video_full_range_flag`. The first three
//!   re-use the registries from [ITU-T H.273]; the spec further pins
//!   `matrix_coefficients = 0` for PNG ("RGB is currently the only
//!   supported color model in PNG, and as such `Matrix Coefficients`
//!   shall be set to `0`"). `video_full_range_flag` is `0`
//!   (narrow-range / video-quantisation) or `1` (full-range / computer-
//!   graphics quantisation) — every other value is reserved by H.273
//!   §8.3. `cICP` must precede `PLTE` and `IDAT` (§5.6 Table 1) and
//!   only one is permitted; when present, it is the highest-precedence
//!   colour chunk (§4.3 Table 1).
//!
//! - `gAMA` — RFC 2083 §4.2.3 / W3C PNG3 §11.3.2.2 "Image gamma". One
//!   4-byte big-endian unsigned integer equal to the image gamma "times
//!   100000" (γ 0.45 ⇒ `45000`). The value is preserved verbatim — a
//!   stored `0` "is meaningless but … decoders should ignore it" (PNG3
//!   §11.3.2.2), which is a `should`, not a `shall`, so parse keeps the
//!   raw integer rather than rejecting it. Must precede `PLTE` and the
//!   first `IDAT`; only one is permitted.
//!
//! - `cHRM` — RFC 2083 §4.2.2 / W3C PNG3 §11.3.2.1 "Primary
//!   chromaticities and white point". Eight 4-byte big-endian unsigned
//!   integers — white-point x/y, red x/y, green x/y, blue x/y — each the
//!   1931 CIE x or y value "times 100000" (0.3127 ⇒ `31270`). 32 bytes
//!   total; other lengths are rejected. Must precede `PLTE` and the
//!   first `IDAT`; only one is permitted.
//!
//!   Both colour chunks are the lowest-precedence members of the §4.3
//!   "Color Chunk Priority" table (cICP `1` > iCCP `2` > sRGB `3` >
//!   cHRM/gAMA `4`); the encoder emits them after `cICP` / `sBIT` /
//!   `sRGB` but still before `PLTE`.
//!
//! - `sPLT` — W3C PNG3 §11.3.4.4 "Suggested palette". A standalone
//!   palette (independent of `PLTE`) that viewers may use to display a
//!   truecolour image on indexed-colour hardware. Layout per Table 25:
//!   a 1-79-byte palette name, a `NUL` separator, a 1-byte sample depth
//!   (`8` or `16`), then a sequence of entries. Each entry is `R G B A`
//!   samples — one byte each when the sample depth is `8`, two bytes
//!   (big-endian) each when `16` — followed by a 2-byte big-endian
//!   `frequency`. The entry stride is therefore 6 bytes (depth 8) or
//!   10 bytes (depth 16); the remaining payload after the sample-depth
//!   byte must divide evenly by that stride. The palette name shares
//!   the `tEXt` keyword restrictions (§11.3.3.1): printable Latin-1 in
//!   `0x20..=0x7E` / `0xA1..=0xFF`, no leading / trailing / consecutive
//!   spaces. `sPLT` "Before IDAT" (§5.6 Table 7) with no `PLTE`
//!   relationship; multiple `sPLT` chunks are permitted but each shall
//!   have a different palette name (we reject duplicate names on parse).
//!
//! - `iCCP` — W3C PNG3 §11.3.2.3 "Embedded ICC profile". A named ICC
//!   colour-management profile carried as an opaque zlib-compressed
//!   blob. Payload layout (§11.3.2.3 "The iCCP chunk contains"):
//!
//!   | Field                | Width     |
//!   |----------------------|-----------|
//!   | Profile name         | 1-79 B    |
//!   | NUL separator        | 1 B       |
//!   | Compression method   | 1 B       |
//!   | Compressed profile   | n B       |
//!
//!   "The only compression method defined in this specification is
//!   method 0 (zlib datastream with deflate compression)" — any other
//!   method byte is rejected. The profile name obeys the same rules as
//!   the `tEXt` keyword / `sPLT` palette name (§11.3.3.1: printable
//!   Latin-1 `0x20..=0x7E` / `0xA1..=0xFF`, 1-79 bytes, no leading /
//!   trailing / consecutive spaces). The decompressed profile is
//!   opaque to PNG — the ICC profile internals belong to [ICC.1] /
//!   [ISO_15076-1] and are not interpreted here; the codec validates
//!   only the zlib framing and round-trips the inflated bytes
//!   verbatim. The in-memory representation holds the *decompressed*
//!   profile so callers do not need to know that compression happened.
//!   `iCCP` must precede `PLTE` and `IDAT` (§5.6 Table 1); only one is
//!   permitted. In the §4.3 "Color Chunk Priority" table iCCP is rank
//!   `2`, between `cICP` (`1`) and `sRGB` (`3`); a datastream "should
//!   not" carry both `iCCP` and `sRGB` (§11.3.2.3 last paragraph) but
//!   the codec does not reject the combination — the spec is a
//!   `should`, not a `shall`. Empty profiles (`n = 0` plaintext) are
//!   permitted.
//!
//! - `zTXt` — RFC 2083 §4.2.10 "Compressed textual data" / W3C PNG3
//!   §11.3.3.3. Same semantics as `tEXt` (Latin-1 keyword + `NUL`
//!   separator + Latin-1 text body), but the body is zlib-compressed.
//!   The payload layout is:
//!
//!   | Field                   | Width     |
//!   |-------------------------|-----------|
//!   | Keyword                 | 1-79 B    |
//!   | NUL separator           | 1 B       |
//!   | Compression method      | 1 B       |
//!   | Compressed text         | n B       |
//!
//!   "The only value presently defined for [the compression-method byte]
//!   is 0 (deflate/inflate compression)" — any other value is rejected.
//!   The decompressed text is plain Latin-1 with the same `NUL`-forbidden
//!   rule as `tEXt` (the spec reserves `NUL` as the keyword separator).
//!   Keyword validation reuses the shared `tEXt` predicate. `zTXt` is
//!   one of two metadata chunks PNG allows to repeat without uniqueness
//!   constraints — "Any number of zTXt and tEXt chunks can appear in
//!   the same file" (§4.2.10) — so the decoder preserves file order and
//!   the encoder replays it via `Vec<Ztxt>`. Emitted before `IDAT`
//!   alongside `tEXt` (Table 1: "Multiple OK? Yes / Ordering: None").
//!
//! - `iTXt` — W3C PNG3 §11.3.3.4 "International textual data". The
//!   UTF-8 successor to `tEXt`: a Latin-1 keyword paired with a UTF-8
//!   language-tagged text body. Payload layout (§11.3.3.4 Table — "An
//!   iTXt chunk contains"):
//!
//!   | Field              | Width     |
//!   |--------------------|-----------|
//!   | Keyword            | 1-79 B    |
//!   | NUL separator      | 1 B       |
//!   | Compression flag   | 1 B       |
//!   | Compression method | 1 B       |
//!   | Language tag       | 0+ B      |
//!   | NUL separator      | 1 B       |
//!   | Translated keyword | 0+ B      |
//!   | NUL separator      | 1 B       |
//!   | Text               | 0+ B      |
//!
//!   The keyword obeys the same rules as `tEXt` (printable Latin-1,
//!   1-79 bytes, no leading / trailing / consecutive spaces). The
//!   compression flag is `0` for an uncompressed text body, `1` for a
//!   zlib-compressed body — "only the text field may be compressed"
//!   (§11.3.3.4); the compression method byte is `0` (deflate) when
//!   the flag is `1`. "For uncompressed text, encoders shall set the
//!   compression method to 0, and decoders shall ignore it"
//!   (§11.3.3.4) — so a non-zero method when the flag is `0` is
//!   accepted on parse and zeroed on re-emit. The language tag is a
//!   well-formed BCP47 tag (`en`, `en-GB`, `zh-Hans-CN`, …) or empty
//!   for "language unspecified"; the codec stores it as a `String` and
//!   does not validate against the IANA language-subtag registry
//!   (offline-only, and the spec frames it as `should`). The
//!   translated keyword and text are UTF-8 [rfc3629] and "neither
//!   shall contain a zero byte" (§11.3.3.4 ¶3) — the codec validates
//!   UTF-8 round-tripping (a `String` already enforces it) and rejects
//!   embedded `NUL` on parse. The text is not `NUL`-terminated; its
//!   length is derived from the chunk length. `iTXt` is the third
//!   metadata chunk PNG explicitly permits to repeat with identical
//!   keywords (alongside `tEXt` and `zTXt`); the decoder preserves
//!   file order and the encoder replays it via `Vec<Itxt>`. Emitted
//!   before `IDAT` (§5.6 Table 7) after `tEXt` / `zTXt` in the encoder
//!   so the lower-overhead chunks lead the textual stream.
//!
//! Every chunk here except `sPLT`, `tEXt`, `zTXt`, and `iTXt` is marked
//! "Multiple OK? No" in the PNG spec; we enforce that on parse —
//! duplicates are an `InvalidData` error. `sPLT` ("Multiple OK? Yes")
//! instead requires distinct palette names; a repeated name is the
//! `InvalidData` error. `tEXt`, `zTXt`, and `iTXt` may repeat freely,
//! with or without identical keywords (§4.2.7 ¶3 / §4.2.10 ¶6 /
//! §11.3.3.4).

use crate::error::{PngError as Error, Result};
use miniz_oxide::deflate::compress_to_vec_zlib;
use miniz_oxide::inflate::decompress_to_vec_zlib;

/// `sBIT` payload (RFC 2083 §4.2.6).
///
/// Variants name the IHDR colour type whose sample layout determines the
/// chunk's length. Each `u8` is the count of significant bits, in
/// `1..=sample_depth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sbit {
    /// Colour type 0 (grayscale): `(grey,)`.
    Grayscale(u8),
    /// Colour type 2 (truecolour) or 3 (indexed): `(r, g, b)`.
    /// The two share a layout because for indexed images sBIT describes
    /// the palette entries' source bit depths, not the index.
    Rgb(u8, u8, u8),
    /// Colour type 4 (grayscale + alpha): `(grey, alpha)`.
    GrayscaleAlpha(u8, u8),
    /// Colour type 6 (RGBA): `(r, g, b, a)`.
    Rgba(u8, u8, u8, u8),
}

impl Sbit {
    /// Parse an sBIT chunk payload given the IHDR colour type so we can
    /// pick the right variant. The caller is responsible for also
    /// validating `colour_type` against the IHDR (we just trust whatever
    /// is passed in).
    ///
    /// `sample_depth` is checked when supplied: `1..=sample_depth` per
    /// RFC 2083 §4.2.6 final paragraph. `sample_depth` is 8 for indexed
    /// images and the IHDR `bit_depth` for every other colour type.
    pub fn parse(data: &[u8], colour_type: u8, sample_depth: u8) -> Result<Self> {
        let expected = match colour_type {
            0 => 1,
            2 | 3 => 3,
            4 => 2,
            6 => 4,
            other => {
                return Err(Error::invalid(format!(
                    "PNG sBIT: colour type {other} has no defined layout"
                )))
            }
        };
        if data.len() != expected {
            return Err(Error::invalid(format!(
                "PNG sBIT: colour type {colour_type} expected {expected} bytes, got {}",
                data.len()
            )));
        }
        let check = |v: u8| -> Result<u8> {
            if v == 0 || v > sample_depth {
                return Err(Error::invalid(format!(
                    "PNG sBIT: significant-bit count {v} not in 1..={sample_depth} \
                     (colour type {colour_type})"
                )));
            }
            Ok(v)
        };
        Ok(match colour_type {
            0 => Self::Grayscale(check(data[0])?),
            2 | 3 => Self::Rgb(check(data[0])?, check(data[1])?, check(data[2])?),
            4 => Self::GrayscaleAlpha(check(data[0])?, check(data[1])?),
            6 => Self::Rgba(
                check(data[0])?,
                check(data[1])?,
                check(data[2])?,
                check(data[3])?,
            ),
            _ => unreachable!(),
        })
    }

    /// Emit the on-wire payload (1/2/3/4 bytes depending on variant).
    pub fn to_bytes(&self) -> Vec<u8> {
        match *self {
            Self::Grayscale(g) => vec![g],
            Self::Rgb(r, g, b) => vec![r, g, b],
            Self::GrayscaleAlpha(g, a) => vec![g, a],
            Self::Rgba(r, g, b, a) => vec![r, g, b, a],
        }
    }
}

/// `pHYs` payload (RFC 2083 §4.2.5).
///
/// `pixels_per_unit_x` and `_y` are unsigned. `unit` selects the
/// interpretation per [`PhysUnit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Phys {
    pub pixels_per_unit_x: u32,
    pub pixels_per_unit_y: u32,
    pub unit: PhysUnit,
}

/// `pHYs.unit_specifier` per RFC 2083 §4.2.5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysUnit {
    /// `0` — unit is unknown; chunk defines aspect ratio only.
    Unknown,
    /// `1` — pixels per metre.
    Metre,
}

impl Phys {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 9 {
            return Err(Error::invalid(format!(
                "PNG pHYs: expected 9 bytes, got {}",
                data.len()
            )));
        }
        let pixels_per_unit_x = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let pixels_per_unit_y = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let unit = match data[8] {
            0 => PhysUnit::Unknown,
            1 => PhysUnit::Metre,
            other => {
                return Err(Error::invalid(format!(
                    "PNG pHYs: unknown unit specifier {other}"
                )))
            }
        };
        Ok(Self {
            pixels_per_unit_x,
            pixels_per_unit_y,
            unit,
        })
    }

    pub fn to_bytes(&self) -> [u8; 9] {
        let mut out = [0u8; 9];
        out[0..4].copy_from_slice(&self.pixels_per_unit_x.to_be_bytes());
        out[4..8].copy_from_slice(&self.pixels_per_unit_y.to_be_bytes());
        out[8] = match self.unit {
            PhysUnit::Unknown => 0,
            PhysUnit::Metre => 1,
        };
        out
    }

    /// Convenience: compute the physical pixel size in DPI (dots per
    /// inch). Returns `None` when [`Self::unit`] is `Unknown` because
    /// the spec then defines no absolute size. One inch is exactly
    /// 0.0254 m (RFC 2083 §4.2.5 "Conversion note").
    pub fn dpi(&self) -> Option<(f64, f64)> {
        match self.unit {
            PhysUnit::Unknown => None,
            PhysUnit::Metre => Some((
                self.pixels_per_unit_x as f64 * 0.0254,
                self.pixels_per_unit_y as f64 * 0.0254,
            )),
        }
    }
}

/// `tIME` payload (RFC 2083 §4.2.8). UTC. `second` may legally be 60
/// for a leap second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Time {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Time {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 7 {
            return Err(Error::invalid(format!(
                "PNG tIME: expected 7 bytes, got {}",
                data.len()
            )));
        }
        let year = u16::from_be_bytes([data[0], data[1]]);
        let month = data[2];
        let day = data[3];
        let hour = data[4];
        let minute = data[5];
        let second = data[6];
        // Range checks per RFC 2083 §4.2.8.
        if !(1..=12).contains(&month) {
            return Err(Error::invalid(format!(
                "PNG tIME: month {month} not 1..=12"
            )));
        }
        if !(1..=31).contains(&day) {
            return Err(Error::invalid(format!("PNG tIME: day {day} not 1..=31")));
        }
        if hour > 23 {
            return Err(Error::invalid(format!("PNG tIME: hour {hour} not 0..=23")));
        }
        if minute > 59 {
            return Err(Error::invalid(format!(
                "PNG tIME: minute {minute} not 0..=59"
            )));
        }
        // Spec explicitly allows `second = 60` for a leap second; `61` is
        // called out as a common error.
        if second > 60 {
            return Err(Error::invalid(format!(
                "PNG tIME: second {second} not 0..=60"
            )));
        }
        Ok(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    pub fn to_bytes(&self) -> [u8; 7] {
        let mut out = [0u8; 7];
        out[0..2].copy_from_slice(&self.year.to_be_bytes());
        out[2] = self.month;
        out[3] = self.day;
        out[4] = self.hour;
        out[5] = self.minute;
        out[6] = self.second;
        out
    }
}

/// `bKGD` payload (RFC 2083 §4.2.1 / W3C PNG3 §11.3.4.1).
///
/// The variant matches the IHDR colour type. Grayscale and RGB values are
/// stored as `u16` regardless of the image bit depth; for sub-16-bit
/// images the value occupies the low-order bits and the high-order bits
/// **must** be zero per W3C PNG3 §11.3.4.1 final paragraph. Parse
/// enforces that constraint when the caller supplies the IHDR bit depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bkgd {
    /// Colour types 0 and 4 (grayscale, ±α): grey level.
    Grayscale(u16),
    /// Colour types 2 and 6 (RGB, ±α): RGB samples.
    Rgb(u16, u16, u16),
    /// Colour type 3 (indexed): palette index.
    Palette(u8),
}

impl Bkgd {
    /// Parse a `bKGD` payload. `colour_type` selects the variant per
    /// W3C PNG3 §11.3.4.1 Table 22. `bit_depth` is the IHDR bit depth;
    /// it bounds-checks grayscale / RGB samples so that high bits beyond
    /// `bit_depth` are zero (PNG3 §11.3.4.1 final paragraph: "decoders
    /// must mask the other bits to 0 before the value is used" — we
    /// reject rather than silently mask so the encoder can't fabricate
    /// payloads that disagree with IHDR).
    pub fn parse(data: &[u8], colour_type: u8, bit_depth: u8) -> Result<Self> {
        let expected = match colour_type {
            0 | 4 => 2,
            2 | 6 => 6,
            3 => 1,
            other => {
                return Err(Error::invalid(format!(
                    "PNG bKGD: colour type {other} has no defined layout"
                )))
            }
        };
        if data.len() != expected {
            return Err(Error::invalid(format!(
                "PNG bKGD: colour type {colour_type} expected {expected} bytes, got {}",
                data.len()
            )));
        }
        // Cap = (2^bit_depth) - 1, computed in u32 so 16-bit doesn't
        // overflow u16's MAX.
        let cap_u16 = || -> u16 {
            if bit_depth >= 16 {
                u16::MAX
            } else {
                ((1u32 << bit_depth) - 1) as u16
            }
        };
        let check_sample = |v: u16| -> Result<u16> {
            let cap = cap_u16();
            if v > cap {
                return Err(Error::invalid(format!(
                    "PNG bKGD: sample {v} exceeds 2^{bit_depth} - 1 ({cap})"
                )));
            }
            Ok(v)
        };
        Ok(match colour_type {
            0 | 4 => Self::Grayscale(check_sample(u16::from_be_bytes([data[0], data[1]]))?),
            2 | 6 => Self::Rgb(
                check_sample(u16::from_be_bytes([data[0], data[1]]))?,
                check_sample(u16::from_be_bytes([data[2], data[3]]))?,
                check_sample(u16::from_be_bytes([data[4], data[5]]))?,
            ),
            3 => Self::Palette(data[0]),
            _ => unreachable!(),
        })
    }

    /// Emit the on-wire payload (1 / 2 / 6 bytes depending on variant).
    pub fn to_bytes(&self) -> Vec<u8> {
        match *self {
            Self::Grayscale(g) => g.to_be_bytes().to_vec(),
            Self::Rgb(r, g, b) => {
                let mut out = Vec::with_capacity(6);
                out.extend_from_slice(&r.to_be_bytes());
                out.extend_from_slice(&g.to_be_bytes());
                out.extend_from_slice(&b.to_be_bytes());
                out
            }
            Self::Palette(idx) => vec![idx],
        }
    }
}

/// `hIST` payload (RFC 2083 §4.2.4 / W3C PNG3 §11.3.4.2).
///
/// One `u16` frequency per `PLTE` entry. Zero means "palette index unused
/// in the image"; otherwise the value is the encoder's chosen proportional
/// count (any scale, RFC 2083 §4.2.4 "the exact scale factor is chosen by
/// the encoder").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hist {
    pub frequencies: Vec<u16>,
}

impl Hist {
    /// Parse an `hIST` chunk. `palette_entries` is the number of `PLTE`
    /// entries the host PNG declares; spec requires "exactly one entry
    /// for each entry in the PLTE chunk" (W3C PNG3 §11.3.4.2).
    pub fn parse(data: &[u8], palette_entries: usize) -> Result<Self> {
        if data.len() != palette_entries * 2 {
            return Err(Error::invalid(format!(
                "PNG hIST: expected {} bytes for {palette_entries} palette entries, got {}",
                palette_entries * 2,
                data.len()
            )));
        }
        let mut frequencies = Vec::with_capacity(palette_entries);
        for chunk in data.chunks_exact(2) {
            frequencies.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }
        Ok(Self { frequencies })
    }

    /// Emit the on-wire payload (`2 × len()` bytes).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.frequencies.len() * 2);
        for f in &self.frequencies {
            out.extend_from_slice(&f.to_be_bytes());
        }
        out
    }
}

/// `eXIf` payload (W3C PNG3 §11.3.4.5).
///
/// An opaque Exif/TIFF profile. The PNG spec defines no internal
/// structure beyond requiring a valid TIFF byte-order header, so we
/// store the raw bytes and round-trip them verbatim. The leading
/// four bytes are validated against the two legal TIFF magic words on
/// [`Self::parse`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exif {
    /// Raw profile bytes, exactly as they appear in the chunk (TIFF
    /// header onward; no JPEG `APP1` marker / length / `"Exif\0"` ID).
    pub data: Vec<u8>,
}

/// TIFF little-endian (Intel) byte-order header: `"II"` followed by the
/// 16-bit value `42` little-endian (W3C PNG3 §11.3.4.5.2).
const TIFF_LE_MAGIC: [u8; 4] = [0x49, 0x49, 0x2A, 0x00];
/// TIFF big-endian (Motorola) byte-order header: `"MM"` followed by the
/// 16-bit value `42` big-endian (W3C PNG3 §11.3.4.5.2).
const TIFF_BE_MAGIC: [u8; 4] = [0x4D, 0x4D, 0x00, 0x2A];

impl Exif {
    /// Parse an `eXIf` chunk payload. Rejects payloads shorter than the
    /// 4-byte TIFF header and any header that is not one of the two
    /// legal byte-order magic words (W3C PNG3 §11.3.4.5.2: "all other
    /// values are reserved for possible future definition"). The TIFF
    /// directory itself is not interpreted.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 4 {
            return Err(Error::invalid(format!(
                "PNG eXIf: payload too short ({} bytes) for a TIFF header",
                data.len()
            )));
        }
        let header = [data[0], data[1], data[2], data[3]];
        if header != TIFF_LE_MAGIC && header != TIFF_BE_MAGIC {
            return Err(Error::invalid(format!(
                "PNG eXIf: invalid TIFF byte-order header {header:02X?} \
                 (expected \"II\"/{TIFF_LE_MAGIC:02X?} or \"MM\"/{TIFF_BE_MAGIC:02X?})"
            )));
        }
        Ok(Self {
            data: data.to_vec(),
        })
    }

    /// Emit the on-wire payload (the raw profile bytes, unchanged).
    pub fn to_bytes(&self) -> Vec<u8> {
        self.data.clone()
    }
}

/// `sRGB.rendering_intent` per W3C PNG3 §11.3.2.5 Table 16.
///
/// Names the ICC rendering intent a conforming viewer should use when
/// displaying the (sRGB-space) image samples. The four values are the
/// only ones defined; `4..=255` are reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderingIntent {
    /// `0` — Perceptual: good adaptation to the output-device gamut at
    /// the expense of colorimetric accuracy (e.g. photographs).
    Perceptual,
    /// `1` — Relative colorimetric: colour-appearance matching relative
    /// to the output-device white point (e.g. logos).
    RelativeColorimetric,
    /// `2` — Saturation: preserves saturation at the expense of hue and
    /// lightness (e.g. charts and graphs).
    Saturation,
    /// `3` — Absolute colorimetric: preserves absolute colorimetry (e.g.
    /// proofs destined for a different output device).
    AbsoluteColorimetric,
}

impl RenderingIntent {
    /// Map the on-wire byte to a variant. Values `4..=255` are reserved
    /// (W3C PNG3 §11.3.2.5) and rejected.
    pub fn from_byte(value: u8) -> Result<Self> {
        Ok(match value {
            0 => Self::Perceptual,
            1 => Self::RelativeColorimetric,
            2 => Self::Saturation,
            3 => Self::AbsoluteColorimetric,
            other => {
                return Err(Error::invalid(format!(
                    "PNG sRGB: rendering intent {other} not in 0..=3 (reserved)"
                )))
            }
        })
    }

    /// The on-wire byte for this variant.
    pub fn to_byte(self) -> u8 {
        match self {
            Self::Perceptual => 0,
            Self::RelativeColorimetric => 1,
            Self::Saturation => 2,
            Self::AbsoluteColorimetric => 3,
        }
    }
}

/// `sRGB` payload (W3C PNG3 §11.3.2.5).
///
/// A one-byte chunk whose presence asserts the image samples conform to
/// the sRGB colour space; the byte selects the ICC [`RenderingIntent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Srgb {
    pub rendering_intent: RenderingIntent,
}

impl Srgb {
    /// Parse an `sRGB` chunk payload (exactly one byte; the rendering
    /// intent). Rejects any other length and any reserved intent value.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 1 {
            return Err(Error::invalid(format!(
                "PNG sRGB: expected 1 byte, got {}",
                data.len()
            )));
        }
        Ok(Self {
            rendering_intent: RenderingIntent::from_byte(data[0])?,
        })
    }

    /// Emit the on-wire payload (the single rendering-intent byte).
    pub fn to_bytes(&self) -> [u8; 1] {
        [self.rendering_intent.to_byte()]
    }
}

/// `cICP` payload (W3C PNG3 §11.3.2.6).
///
/// Carries the four [ITU-T H.273] coding-independent code points that
/// classify the image's signal characteristics:
///
/// * `color_primaries` — chromaticity coordinates of the source RGB
///   primaries and the white point (H.273 §8.1, e.g. `1` = BT.709,
///   `9` = BT.2020, `12` = SMPTE EG 432-1 / Display P3).
/// * `transfer_function` — the opto-electronic transfer characteristic
///   used when the image was authored (H.273 §8.2, e.g. `1` = BT.709,
///   `13` = sRGB / IEC 61966-2-1, `16` = SMPTE ST 2084 / PQ,
///   `18` = ARIB STD-B67 / HLG).
/// * `matrix_coefficients` — colour-component derivation matrix
///   identifier (H.273 §8.3). PNG fixes this at `0` ("Identity / RGB")
///   per §11.3.2.6 ("RGB is currently the only supported color model in
///   PNG, and as such Matrix Coefficients shall be set to `0`"). Parse
///   rejects any other value.
/// * `video_full_range_flag` — `0` = narrow-range (video-quantised,
///   e.g. BT.709 reference black `64` / nominal peak `940` for 10-bit),
///   `1` = full-range (the conventional `0..=2^N-1` quantisation used by
///   most computer-graphics PNGs). H.273 §8.3 reserves every other
///   value; parse rejects anything outside `0..=1`.
///
/// The spec carries no constraints on the `color_primaries` /
/// `transfer_function` bytes beyond their definitions in H.273, so the
/// parser preserves any caller-supplied byte (round-trip integrity)
/// even when it names a "Reserved" H.273 code-point. Validation of
/// those two bytes against the H.273 registry is left to consumers that
/// need stricter checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cicp {
    pub color_primaries: u8,
    pub transfer_function: u8,
    pub matrix_coefficients: u8,
    pub video_full_range_flag: u8,
}

impl Cicp {
    /// Parse a `cICP` chunk payload. The chunk is exactly four bytes
    /// (Table 18); other lengths, `matrix_coefficients != 0`, and
    /// `video_full_range_flag` outside `0..=1` are all rejected.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 4 {
            return Err(Error::invalid(format!(
                "PNG cICP: expected 4 bytes, got {}",
                data.len()
            )));
        }
        let color_primaries = data[0];
        let transfer_function = data[1];
        let matrix_coefficients = data[2];
        let video_full_range_flag = data[3];
        // PNG3 §11.3.2.6: PNG is RGB only, so the H.273 matrix
        // coefficients value must be 0 (identity / RGB).
        if matrix_coefficients != 0 {
            return Err(Error::invalid(format!(
                "PNG cICP: matrix_coefficients {matrix_coefficients} must be 0 (PNG is RGB-only)",
            )));
        }
        // H.273 §8.3 / PNG3 §11.3.2.6 Note: only 0 (narrow range) and
        // 1 (full range) are defined; everything else is reserved.
        if video_full_range_flag > 1 {
            return Err(Error::invalid(format!(
                "PNG cICP: video_full_range_flag {video_full_range_flag} not in 0..=1 (reserved)",
            )));
        }
        Ok(Self {
            color_primaries,
            transfer_function,
            matrix_coefficients,
            video_full_range_flag,
        })
    }

    /// Emit the on-wire payload (four bytes, in spec order).
    pub fn to_bytes(&self) -> [u8; 4] {
        [
            self.color_primaries,
            self.transfer_function,
            self.matrix_coefficients,
            self.video_full_range_flag,
        ]
    }
}

/// `gAMA` payload (RFC 2083 §4.2.3 / W3C PNG3 §11.3.2.2).
///
/// A single 4-byte unsigned integer that is the image gamma "times
/// 100000" (RFC 2083 §4.2.3: "a gamma of 0.45 would be stored as the
/// integer 45000"). The field names the gamma of the source device that
/// produced the image with respect to the original scene; a viewer is
/// "strongly encouraged" to compensate (RFC 2083 §2.7).
///
/// The on-wire value is stored verbatim in [`Self::gamma_times_100000`]
/// rather than as a float, so a round-trip is byte-exact. A stored value
/// of `0` "is meaningless but could appear by mistake. Decoders should
/// ignore it" (W3C PNG3 §11.3.2.2); we honour that "should" by
/// round-tripping the raw integer (any interpretation / discard is the
/// caller's choice) and only reject a malformed *length*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gama {
    /// The image gamma multiplied by 100000 (so `45000` == γ 0.45).
    pub gamma_times_100000: u32,
}

impl Gama {
    /// Parse a `gAMA` chunk payload (exactly four bytes, big-endian).
    /// Other lengths are rejected. The value is preserved verbatim — the
    /// spec's "decoders should ignore a zero gamma" is a recommendation,
    /// not a parse-time `shall`, so a `0` payload still round-trips.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 4 {
            return Err(Error::invalid(format!(
                "PNG gAMA: expected 4 bytes, got {}",
                data.len()
            )));
        }
        Ok(Self {
            gamma_times_100000: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        })
    }

    /// Emit the on-wire payload (the 4-byte big-endian gamma integer).
    pub fn to_bytes(&self) -> [u8; 4] {
        self.gamma_times_100000.to_be_bytes()
    }

    /// Convenience: the gamma as a floating-point value
    /// (`gamma_times_100000 / 100000.0`). RFC 2083 §4.2.3 defines the
    /// stored integer as "gamma times 100000".
    pub fn gamma(&self) -> f64 {
        self.gamma_times_100000 as f64 / 100_000.0
    }
}

/// `cHRM` payload (RFC 2083 §4.2.2 / W3C PNG3 §11.3.2.1).
///
/// The 1931 CIE x,y chromaticities of the white point and the red,
/// green, and blue display primaries, allowing device-independent colour
/// matching. Eight 4-byte big-endian unsigned integers, each "the x or y
/// value times 100000" (RFC 2083 §4.2.2: "a value of 0.3127 would be
/// stored as the integer 31270"), in the fixed order white-point x/y,
/// red x/y, green x/y, blue x/y.
///
/// Values are stored verbatim as the on-wire `× 100000` integers so a
/// round-trip is byte-exact; [`Self::white_point`] / [`Self::red`] /
/// [`Self::green`] / [`Self::blue`] return the `(x, y)` pair as floats.
/// `cHRM` "is allowed in all PNG files, although it is of little value
/// for grayscale images" (RFC 2083 §4.2.2); the codec carries it for any
/// colour type and leaves that judgement to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrm {
    pub white_point_x: u32,
    pub white_point_y: u32,
    pub red_x: u32,
    pub red_y: u32,
    pub green_x: u32,
    pub green_y: u32,
    pub blue_x: u32,
    pub blue_y: u32,
}

impl Chrm {
    /// Parse a `cHRM` chunk payload (exactly 32 bytes: eight 4-byte
    /// big-endian unsigned integers). Other lengths are rejected.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 32 {
            return Err(Error::invalid(format!(
                "PNG cHRM: expected 32 bytes, got {}",
                data.len()
            )));
        }
        let be = |i: usize| u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        Ok(Self {
            white_point_x: be(0),
            white_point_y: be(4),
            red_x: be(8),
            red_y: be(12),
            green_x: be(16),
            green_y: be(20),
            blue_x: be(24),
            blue_y: be(28),
        })
    }

    /// Emit the on-wire payload (32 bytes, in spec order).
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, v) in [
            self.white_point_x,
            self.white_point_y,
            self.red_x,
            self.red_y,
            self.green_x,
            self.green_y,
            self.blue_x,
            self.blue_y,
        ]
        .iter()
        .enumerate()
        {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// The white-point chromaticity as `(x, y)` floats (each stored
    /// integer / 100000, per RFC 2083 §4.2.2).
    pub fn white_point(&self) -> (f64, f64) {
        (
            self.white_point_x as f64 / 100_000.0,
            self.white_point_y as f64 / 100_000.0,
        )
    }

    /// The red-primary chromaticity as `(x, y)` floats.
    pub fn red(&self) -> (f64, f64) {
        (self.red_x as f64 / 100_000.0, self.red_y as f64 / 100_000.0)
    }

    /// The green-primary chromaticity as `(x, y)` floats.
    pub fn green(&self) -> (f64, f64) {
        (
            self.green_x as f64 / 100_000.0,
            self.green_y as f64 / 100_000.0,
        )
    }

    /// The blue-primary chromaticity as `(x, y)` floats.
    pub fn blue(&self) -> (f64, f64) {
        (
            self.blue_x as f64 / 100_000.0,
            self.blue_y as f64 / 100_000.0,
        )
    }
}

/// One entry of an [`Splt`] suggested palette (W3C PNG3 §11.3.4.4
/// Table 25).
///
/// The four colour samples and the frequency are kept as `u16`
/// regardless of the parent palette's sample depth. For an 8-bit
/// palette the samples occupy `0..=255`; for 16-bit they use the full
/// `u16` range. `frequency` is always a `u16` ("proportional to the
/// fraction of the pixels … for which that palette entry is the closest
/// match", §11.3.4.4); zero is a valid value meaning "least important".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpltEntry {
    pub red: u16,
    pub green: u16,
    pub blue: u16,
    pub alpha: u16,
    pub frequency: u16,
}

/// `sPLT` payload (W3C PNG3 §11.3.4.4).
///
/// A named suggested palette independent of `PLTE`. [`Self::sample_depth`]
/// is `8` or `16` and fixes the on-wire width of each colour sample
/// (the entries are always stored here as `u16`); for an 8-bit palette
/// the [`SpltEntry`] samples must fit in `0..=255`. The palette name
/// obeys the `tEXt` keyword rules (§11.3.3.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splt {
    /// Palette name (1-79 printable Latin-1 bytes; no leading / trailing
    /// / consecutive spaces). Stored as a `String`; on the wire it is
    /// the raw Latin-1 bytes, so every byte is `< 0x100` and maps 1:1 to
    /// a `char` in `U+0020..=U+00FF`.
    pub name: String,
    /// `8` or `16` — the per-sample bit width on the wire.
    pub sample_depth: u8,
    /// Palette entries, in the spec-mandated decreasing-frequency order
    /// (we preserve whatever order the input used; the encoder writes
    /// them back unchanged).
    pub entries: Vec<SpltEntry>,
}

/// Validate an `sPLT` palette name / `tEXt` keyword (W3C PNG3 §11.3.3.1
/// / §11.3.4.4): 1-79 bytes, printable Latin-1 (`0x20..=0x7E` or
/// `0xA1..=0xFF`), no leading / trailing / consecutive spaces. Returns
/// the validated raw bytes on success.
fn validate_keyword(name: &str, context: &str) -> Result<Vec<u8>> {
    // The name is Latin-1 on the wire: every char must be a single byte
    // (`U+0000..=U+00FF`). Reject anything that wouldn't round-trip to a
    // single byte before we even check the printable ranges.
    let mut bytes = Vec::with_capacity(name.len());
    for ch in name.chars() {
        let cp = ch as u32;
        if cp > 0xFF {
            return Err(Error::invalid(format!(
                "PNG {context}: name char U+{cp:04X} is not Latin-1 (single-byte)"
            )));
        }
        bytes.push(cp as u8);
    }
    if bytes.is_empty() || bytes.len() > 79 {
        return Err(Error::invalid(format!(
            "PNG {context}: name length {} not in 1..=79",
            bytes.len()
        )));
    }
    for &b in &bytes {
        let printable = (0x20..=0x7E).contains(&b) || (0xA1..=0xFF).contains(&b);
        if !printable {
            return Err(Error::invalid(format!(
                "PNG {context}: name byte 0x{b:02X} not printable Latin-1 \
                 (0x20..=0x7E or 0xA1..=0xFF)"
            )));
        }
    }
    // No leading / trailing / consecutive spaces (0x20).
    if bytes.first() == Some(&0x20) || bytes.last() == Some(&0x20) {
        return Err(Error::invalid(format!(
            "PNG {context}: name has a leading or trailing space"
        )));
    }
    if bytes.windows(2).any(|w| w == [0x20, 0x20]) {
        return Err(Error::invalid(format!(
            "PNG {context}: name has consecutive spaces"
        )));
    }
    Ok(bytes)
}

impl Splt {
    /// Parse an `sPLT` chunk payload (Table 25): palette name, `NUL`,
    /// sample depth, then 6- or 10-byte entries. Rejects an invalid
    /// palette name, a sample depth other than `8` / `16`, a missing
    /// `NUL` separator, and an entry-region length that is not a
    /// multiple of the per-entry stride.
    pub fn parse(data: &[u8]) -> Result<Self> {
        // Palette name runs up to (but not including) the first NUL.
        let nul = data
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::invalid("PNG sPLT: missing NUL separator after palette name"))?;
        let name_bytes = &data[..nul];
        let name_str: String = name_bytes.iter().map(|&b| b as char).collect();
        // Re-run the keyword validator over the parsed name (length /
        // printable / spacing rules).
        validate_keyword(&name_str, "sPLT")?;

        // After the NUL: 1 sample-depth byte, then the entry region.
        let rest = &data[nul + 1..];
        let sample_depth = *rest
            .first()
            .ok_or_else(|| Error::invalid("PNG sPLT: missing sample-depth byte"))?;
        let stride = match sample_depth {
            8 => 6,
            16 => 10,
            other => {
                return Err(Error::invalid(format!(
                    "PNG sPLT: sample depth {other} must be 8 or 16"
                )))
            }
        };
        let entry_region = &rest[1..];
        if entry_region.len() % stride != 0 {
            return Err(Error::invalid(format!(
                "PNG sPLT: entry region {} bytes not a multiple of {stride} (sample depth {sample_depth})",
                entry_region.len()
            )));
        }
        let mut entries = Vec::with_capacity(entry_region.len() / stride);
        for e in entry_region.chunks_exact(stride) {
            let entry = if sample_depth == 8 {
                SpltEntry {
                    red: e[0] as u16,
                    green: e[1] as u16,
                    blue: e[2] as u16,
                    alpha: e[3] as u16,
                    frequency: u16::from_be_bytes([e[4], e[5]]),
                }
            } else {
                SpltEntry {
                    red: u16::from_be_bytes([e[0], e[1]]),
                    green: u16::from_be_bytes([e[2], e[3]]),
                    blue: u16::from_be_bytes([e[4], e[5]]),
                    alpha: u16::from_be_bytes([e[6], e[7]]),
                    frequency: u16::from_be_bytes([e[8], e[9]]),
                }
            };
            entries.push(entry);
        }
        Ok(Self {
            name: name_str,
            sample_depth,
            entries,
        })
    }

    /// Emit the on-wire payload (palette name, `NUL`, sample depth,
    /// then the 6- / 10-byte entries). Validates the palette name, the
    /// sample depth, and — for an 8-bit palette — that every sample fits
    /// in a single byte, so the encoder can't silently truncate.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let name_bytes = validate_keyword(&self.name, "sPLT")?;
        if self.sample_depth != 8 && self.sample_depth != 16 {
            return Err(Error::invalid(format!(
                "PNG sPLT: sample depth {} must be 8 or 16",
                self.sample_depth
            )));
        }
        let mut out = Vec::with_capacity(name_bytes.len() + 2 + self.entries.len() * 10);
        out.extend_from_slice(&name_bytes);
        out.push(0); // NUL separator.
        out.push(self.sample_depth);
        for e in &self.entries {
            if self.sample_depth == 8 {
                for (sample, label) in [
                    (e.red, "red"),
                    (e.green, "green"),
                    (e.blue, "blue"),
                    (e.alpha, "alpha"),
                ] {
                    if sample > 0xFF {
                        return Err(Error::invalid(format!(
                            "PNG sPLT: {label} sample {sample} exceeds 255 for an 8-bit palette"
                        )));
                    }
                    out.push(sample as u8);
                }
                out.extend_from_slice(&e.frequency.to_be_bytes());
            } else {
                out.extend_from_slice(&e.red.to_be_bytes());
                out.extend_from_slice(&e.green.to_be_bytes());
                out.extend_from_slice(&e.blue.to_be_bytes());
                out.extend_from_slice(&e.alpha.to_be_bytes());
                out.extend_from_slice(&e.frequency.to_be_bytes());
            }
        }
        Ok(out)
    }
}

/// `tEXt` payload (RFC 2083 §4.2.7 / W3C PNG3 §11.3.3.3).
///
/// Latin-1 keyword + null separator + Latin-1 text. The text is stored
/// raw on the wire as Latin-1 bytes, can contain any byte in the
/// `U+0001..=U+00FF` range (no null — the spec reserves null as the
/// keyword separator), can be any length from zero up to whatever the
/// surrounding chunk's payload allows, and is not null-terminated
/// (chunk length is the only end marker).
///
/// The keyword obeys the standard PNG text-keyword rules (RFC 2083
/// §4.2.7): 1-79 printable Latin-1 bytes (`0x20..=0x7E` or
/// `0xA1..=0xFF`), no leading / trailing / consecutive spaces, no null
/// byte, case-sensitive. The same predicate that gates `sPLT` palette
/// names applies here verbatim.
///
/// `tEXt` is the most permissive metadata chunk PNG defines: any number
/// of `tEXt` chunks may appear, and more than one chunk with the same
/// keyword is permitted (RFC 2083 §4.2.7 paragraph 3 — "more than one
/// with the same keyword is permissible"). The decoder preserves the
/// file's order, and the encoder emits them in `Vec` order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    /// 1-79 printable Latin-1 bytes; no leading / trailing / consecutive
    /// spaces, no null. Stored as a `String` whose codepoints all fit
    /// in `U+0020..=U+00FF`.
    pub keyword: String,
    /// Zero or more Latin-1 bytes. Stored as a `String` whose
    /// codepoints all fit in `U+0001..=U+00FF` (no null — the spec
    /// reserves null as the keyword separator).
    pub text: String,
}

impl Text {
    /// Parse a `tEXt` chunk payload (RFC 2083 §4.2.7): keyword bytes,
    /// `NUL` separator, then text bytes (no trailing null — chunk
    /// length is the only end marker).
    pub fn parse(data: &[u8]) -> Result<Self> {
        let nul = data
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::invalid("PNG tEXt: missing NUL separator after keyword"))?;
        let keyword_bytes = &data[..nul];
        let keyword_str: String = keyword_bytes.iter().map(|&b| b as char).collect();
        // Reuse the shared keyword validator (length / printable /
        // spacing rules; same rules cited by sPLT).
        validate_keyword(&keyword_str, "tEXt")?;

        // Everything after the NUL is the text payload. Latin-1 bytes
        // (each byte maps 1:1 to a `char` in `U+0001..=U+00FF`); no
        // further `NUL` permitted in the text per the §4.2.7
        // requirement that "neither the keyword nor the text string can
        // contain a null character."
        let text_bytes = &data[nul + 1..];
        if text_bytes.contains(&0) {
            return Err(Error::invalid(
                "PNG tEXt: text string contains a NUL byte (only the keyword separator may)",
            ));
        }
        let text_str: String = text_bytes.iter().map(|&b| b as char).collect();
        Ok(Self {
            keyword: keyword_str,
            text: text_str,
        })
    }

    /// Emit the on-wire payload (keyword bytes, `NUL`, text bytes).
    /// Re-validates the keyword and checks that every text codepoint
    /// fits in Latin-1 and isn't a `NUL` — so a malformed `Text` value
    /// can't silently corrupt the output PNG.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let keyword_bytes = validate_keyword(&self.keyword, "tEXt")?;
        let mut text_bytes = Vec::with_capacity(self.text.len());
        for ch in self.text.chars() {
            let cp = ch as u32;
            if cp > 0xFF {
                return Err(Error::invalid(format!(
                    "PNG tEXt: text char U+{cp:04X} is not Latin-1 (single-byte)"
                )));
            }
            if cp == 0 {
                return Err(Error::invalid(
                    "PNG tEXt: text string contains a NUL (reserved as keyword separator)",
                ));
            }
            text_bytes.push(cp as u8);
        }
        let mut out = Vec::with_capacity(keyword_bytes.len() + 1 + text_bytes.len());
        out.extend_from_slice(&keyword_bytes);
        out.push(0);
        out.extend_from_slice(&text_bytes);
        Ok(out)
    }
}

/// `zTXt` payload (RFC 2083 §4.2.10 / W3C PNG3 §11.3.3.3).
///
/// Semantically equivalent to [`Text`] (Latin-1 keyword + Latin-1 text
/// body), but the text body is zlib-compressed on the wire. The
/// in-memory representation holds the *decompressed* text so callers do
/// not need to know that compression happened; [`Self::parse`] inflates
/// the wire bytes and [`Self::to_bytes`] re-compresses them.
///
/// The on-wire payload layout (§4.2.10 "A zTXt chunk contains"):
///
/// ```text
///     Keyword:            1-79 bytes (character string)
///     Null separator:     1 byte
///     Compression method: 1 byte
///     Compressed text:    n bytes
/// ```
///
/// The keyword and `NUL` separator obey the same rules as [`Text`]
/// (RFC 2083 §4.2.7 — printable Latin-1, 1-79 bytes, no leading /
/// trailing / consecutive spaces). The compression-method byte is
/// validated against the spec-defined `0` (`zlib` / deflate); any
/// other value is an `InvalidData` error per "The only value
/// presently defined for it is 0". The decompressed Latin-1 text must
/// not contain a `NUL` ("the spec reserves `NUL` as the keyword
/// separator"); the decoder enforces this.
///
/// `zTXt` is one of two metadata chunks PNG explicitly permits to
/// repeat: "Any number of zTXt and tEXt chunks can appear in the same
/// file" (§4.2.10 ¶6). The decoder preserves file order and the
/// encoder replays it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ztxt {
    /// 1-79 printable Latin-1 bytes; no leading / trailing / consecutive
    /// spaces, no `NUL`. Stored as a `String` whose codepoints all fit
    /// in `U+0020..=U+00FF`.
    pub keyword: String,
    /// Decompressed Latin-1 text. Stored as a `String` whose codepoints
    /// all fit in `U+0001..=U+00FF` (no `NUL` — the spec reserves it as
    /// the keyword separator). Empty text is permitted (the chunk's
    /// compressed-text field "n bytes" allows `n = 0` worth of plaintext
    /// after inflate, which `compress_to_vec_zlib` represents as a
    /// 2-byte zlib stored block).
    pub text: String,
}

impl Ztxt {
    /// `zlib`/deflate is the only compression method PNG defines for
    /// `zTXt` per RFC 2083 §4.2.10 ("The only value presently defined
    /// for it is 0 (deflate/inflate compression)"). PNG3 §11.3.3.3
    /// repeats the same constraint.
    pub const COMPRESSION_METHOD_DEFLATE: u8 = 0;

    /// Parse a `zTXt` chunk payload (RFC 2083 §4.2.10): keyword bytes,
    /// `NUL` separator, compression-method byte, then the
    /// zlib-compressed text bytes. Validates the keyword, rejects any
    /// compression method other than `0`, decompresses the body, and
    /// rejects a `NUL` in the decompressed text.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let nul = data
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::invalid("PNG zTXt: missing NUL separator after keyword"))?;
        let keyword_bytes = &data[..nul];
        let keyword_str: String = keyword_bytes.iter().map(|&b| b as char).collect();
        validate_keyword(&keyword_str, "zTXt")?;

        // After the NUL: 1 byte of compression method, then the
        // compressed text. §4.2.10 reserves any value other than 0.
        let rest = &data[nul + 1..];
        let method = *rest
            .first()
            .ok_or_else(|| Error::invalid("PNG zTXt: missing compression-method byte"))?;
        if method != Self::COMPRESSION_METHOD_DEFLATE {
            return Err(Error::invalid(format!(
                "PNG zTXt: unknown compression method {method} (only 0 = deflate is defined)"
            )));
        }
        let compressed = &rest[1..];

        // §4.2.10: "For compression method 0, this datastream adheres to
        // the zlib datastream format." Inflate it; surface a decode
        // error rather than panicking on a tampered chunk.
        let decompressed = decompress_to_vec_zlib(compressed)
            .map_err(|e| Error::invalid(format!("PNG zTXt: zlib decompression failed: {e:?}")))?;

        // Decompressed text obeys the same NUL-forbidden rule as tEXt
        // (RFC 2083 §4.2.10: "Decompression of this datastream yields
        // Latin-1 text that is identical to the text that would be
        // stored in an equivalent tEXt chunk", and §4.2.7 forbids NUL
        // in the text).
        if decompressed.contains(&0) {
            return Err(Error::invalid(
                "PNG zTXt: decompressed text contains a NUL byte \
                 (only the keyword separator may)",
            ));
        }
        let text_str: String = decompressed.iter().map(|&b| b as char).collect();
        Ok(Self {
            keyword: keyword_str,
            text: text_str,
        })
    }

    /// Emit the on-wire payload (keyword bytes, `NUL`, compression
    /// method, zlib-compressed text). Re-validates the keyword and
    /// every text codepoint (Latin-1 single-byte, no `NUL`) — so a
    /// malformed `Ztxt` value cannot silently corrupt the output PNG.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let keyword_bytes = validate_keyword(&self.keyword, "zTXt")?;
        let mut text_bytes = Vec::with_capacity(self.text.len());
        for ch in self.text.chars() {
            let cp = ch as u32;
            if cp > 0xFF {
                return Err(Error::invalid(format!(
                    "PNG zTXt: text char U+{cp:04X} is not Latin-1 (single-byte)"
                )));
            }
            if cp == 0 {
                return Err(Error::invalid(
                    "PNG zTXt: text string contains a NUL (reserved as keyword separator)",
                ));
            }
            text_bytes.push(cp as u8);
        }
        // miniz_oxide's default level (6) matches the encoder's IDAT
        // compression level — we don't yet expose a per-chunk knob, and
        // the spec leaves the choice entirely to the encoder.
        let compressed = compress_to_vec_zlib(&text_bytes, 6);
        let mut out = Vec::with_capacity(keyword_bytes.len() + 2 + compressed.len());
        out.extend_from_slice(&keyword_bytes);
        out.push(0); // NUL separator.
        out.push(Self::COMPRESSION_METHOD_DEFLATE);
        out.extend_from_slice(&compressed);
        Ok(out)
    }
}

/// `iCCP` payload (W3C PNG3 §11.3.2.3).
///
/// An embedded ICC colour-management profile. The on-wire layout is a
/// Latin-1 profile name (`tEXt`-keyword rules) + `NUL` separator +
/// 1-byte compression method + zlib-compressed profile bytes. PNG defines
/// only method `0` (zlib / deflate); the codec rejects any other value
/// per §11.3.2.3 "The only compression method defined in this
/// specification is method 0".
///
/// PNG treats the inflated profile as an opaque blob: §11.3.2.3 cites
/// [ICC.1] and [ISO_15076-1] for its internal structure and the codec
/// does not interpret it. The in-memory representation stores the
/// *decompressed* profile so callers do not need to know that the
/// chunk is compressed on the wire; [`Self::parse`] inflates and
/// [`Self::to_bytes`] re-compresses.
///
/// Only one `iCCP` chunk is permitted per datastream (§5.6 Table 1:
/// "Multiple OK? No") and it must precede `PLTE` and the first `IDAT`.
/// In the §4.3 "Color Chunk Priority" table `iCCP` is rank `2`, between
/// `cICP` (`1`) and `sRGB` (`3`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Iccp {
    /// Profile name (1-79 printable Latin-1 bytes; no leading /
    /// trailing / consecutive spaces, no `NUL`). Stored as a `String`
    /// whose codepoints all fit in `U+0020..=U+00FF`.
    pub name: String,
    /// Decompressed ICC profile bytes. Opaque to the codec — round-
    /// tripped verbatim. May be empty (the spec permits `n = 0`
    /// compressed bytes, which inflate to an empty profile).
    pub profile: Vec<u8>,
}

impl Iccp {
    /// `zlib`/deflate is the only compression method PNG defines for
    /// `iCCP` per W3C PNG3 §11.3.2.3 ("The only compression method
    /// defined in this specification is method 0").
    pub const COMPRESSION_METHOD_DEFLATE: u8 = 0;

    /// Parse an `iCCP` chunk payload: profile-name bytes, `NUL`
    /// separator, compression-method byte, then zlib-compressed
    /// profile. Validates the name against the shared keyword
    /// predicate, rejects any compression method other than `0`, and
    /// inflates the body via `miniz_oxide`.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let nul = data
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::invalid("PNG iCCP: missing NUL separator after profile name"))?;
        let name_bytes = &data[..nul];
        let name_str: String = name_bytes.iter().map(|&b| b as char).collect();
        validate_keyword(&name_str, "iCCP")?;

        let rest = &data[nul + 1..];
        let method = *rest
            .first()
            .ok_or_else(|| Error::invalid("PNG iCCP: missing compression-method byte"))?;
        if method != Self::COMPRESSION_METHOD_DEFLATE {
            return Err(Error::invalid(format!(
                "PNG iCCP: unknown compression method {method} (only 0 = deflate is defined)"
            )));
        }
        let compressed = &rest[1..];
        let profile = decompress_to_vec_zlib(compressed)
            .map_err(|e| Error::invalid(format!("PNG iCCP: zlib decompression failed: {e:?}")))?;
        Ok(Self {
            name: name_str,
            profile,
        })
    }

    /// Emit the on-wire payload (profile-name bytes, `NUL`,
    /// compression method, zlib-compressed profile). Re-validates the
    /// profile name — so a malformed `Iccp` value cannot silently
    /// corrupt the output PNG.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let name_bytes = validate_keyword(&self.name, "iCCP")?;
        // miniz_oxide's default level (6) matches the encoder's IDAT
        // compression level — we don't yet expose a per-chunk knob, and
        // the spec leaves the choice entirely to the encoder.
        let compressed = compress_to_vec_zlib(&self.profile, 6);
        let mut out = Vec::with_capacity(name_bytes.len() + 2 + compressed.len());
        out.extend_from_slice(&name_bytes);
        out.push(0); // NUL separator.
        out.push(Self::COMPRESSION_METHOD_DEFLATE);
        out.extend_from_slice(&compressed);
        Ok(out)
    }
}

/// `iTXt` payload (W3C PNG3 §11.3.3.4).
///
/// The UTF-8 internationalised successor to `tEXt`. Carries a Latin-1
/// keyword paired with a UTF-8 language-tagged text body, optionally
/// zlib-compressed. The on-wire payload is:
///
/// ```text
///     Keyword:            1-79 bytes (Latin-1, tEXt rules)
///     NUL separator:      1 byte
///     Compression flag:   1 byte (0 = uncompressed, 1 = compressed)
///     Compression method: 1 byte (only 0 = deflate defined)
///     Language tag:       0+ bytes (BCP47, NUL-terminated)
///     NUL separator:      1 byte
///     Translated keyword: 0+ bytes (UTF-8, NUL-terminated)
///     NUL separator:      1 byte
///     Text:               0+ bytes (UTF-8, length from chunk length)
/// ```
///
/// The keyword obeys the shared `tEXt` predicate. "For uncompressed
/// text, encoders shall set the compression method to 0, and decoders
/// shall ignore it" (§11.3.3.4) — the codec accepts any method byte
/// when the flag is `0` and zeroes it on re-emit. When the flag is `1`
/// the method must be `0` (deflate). The translated keyword and text
/// are UTF-8 and "neither shall contain a zero byte" (§11.3.3.4) —
/// `String` already enforces UTF-8 round-tripping, and the codec
/// rejects embedded `NUL` on parse.
///
/// `iTXt` is the third metadata chunk PNG explicitly permits to repeat
/// with identical keywords (alongside `tEXt` and `zTXt`). The decoder
/// preserves file order and the encoder replays it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Itxt {
    /// 1-79 printable Latin-1 bytes; no leading / trailing /
    /// consecutive spaces, no `NUL`. Same predicate as `tEXt`.
    pub keyword: String,
    /// `true` when the on-wire text body was zlib-compressed (encoder
    /// re-compresses on emit). Translated keyword and language tag are
    /// always emitted uncompressed regardless of this flag — only the
    /// text field may be compressed (§11.3.3.4).
    pub compressed: bool,
    /// Language tag (BCP47). May be empty for "language unspecified".
    /// ASCII / printable bytes; no `NUL` (the wire format would not
    /// parse otherwise). Not validated against the IANA language-subtag
    /// registry — that requires online lookup, and the spec frames the
    /// subtag-registry constraint as `must` for encoders but as a
    /// decoder discretion call.
    pub language_tag: String,
    /// Translated keyword (UTF-8). May be empty. No `NUL`.
    pub translated_keyword: String,
    /// Text body (UTF-8). May be empty. No `NUL`. Length is derived
    /// from the chunk length on the wire; the text is not
    /// `NUL`-terminated.
    pub text: String,
}

impl Itxt {
    /// `zlib`/deflate is the only compression method PNG defines for
    /// `iTXt` per W3C PNG3 §11.3.3.4 ("The only compression method
    /// defined in this specification is 0").
    pub const COMPRESSION_METHOD_DEFLATE: u8 = 0;

    /// Parse an `iTXt` chunk payload (§11.3.3.4): keyword bytes, `NUL`,
    /// compression flag, compression method, language tag, `NUL`,
    /// translated keyword, `NUL`, text. Validates the keyword, the
    /// compression flag / method combination, the UTF-8 encoding of
    /// the translated keyword and text, and the no-`NUL` rule on the
    /// translated keyword and text bodies.
    pub fn parse(data: &[u8]) -> Result<Self> {
        // Keyword: bytes up to first NUL.
        let k_nul = data
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::invalid("PNG iTXt: missing NUL separator after keyword"))?;
        let keyword_bytes = &data[..k_nul];
        let keyword_str: String = keyword_bytes.iter().map(|&b| b as char).collect();
        validate_keyword(&keyword_str, "iTXt")?;

        // After keyword NUL: 1 flag + 1 method + language tag + NUL +
        // translated keyword + NUL + text (rest).
        let rest = &data[k_nul + 1..];
        if rest.len() < 2 {
            return Err(Error::invalid(
                "PNG iTXt: truncated payload (missing compression flag/method)",
            ));
        }
        let flag = rest[0];
        let method = rest[1];
        let compressed = match flag {
            0 => false,
            1 => true,
            other => {
                return Err(Error::invalid(format!(
                    "PNG iTXt: unknown compression flag {other} (must be 0 or 1)"
                )))
            }
        };
        // §11.3.3.4: "For uncompressed text, encoders shall set the
        // compression method to 0, and decoders shall ignore it." So we
        // only police the method byte when the flag is 1.
        if compressed && method != Self::COMPRESSION_METHOD_DEFLATE {
            return Err(Error::invalid(format!(
                "PNG iTXt: unknown compression method {method} (only 0 = deflate is defined)"
            )));
        }
        let after_method = &rest[2..];

        // Language tag: up to next NUL.
        let lang_nul = after_method
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::invalid("PNG iTXt: missing NUL separator after language tag"))?;
        let lang_bytes = &after_method[..lang_nul];
        // BCP47 tags are ASCII letters / digits / hyphens; the spec
        // frames the subtag-registry constraint as encoder-side, and
        // the decoder is told the tag is case-insensitive. We don't
        // validate against the IANA registry (offline only) but we do
        // require ASCII bytes — a non-ASCII language tag is malformed
        // BCP47 and any future strict validator would reject it.
        for &b in lang_bytes {
            if !b.is_ascii() {
                return Err(Error::invalid(format!(
                    "PNG iTXt: language tag byte 0x{b:02X} not ASCII (BCP47 tags are ASCII)"
                )));
            }
        }
        let language_tag: String = lang_bytes.iter().map(|&b| b as char).collect();
        let after_lang = &after_method[lang_nul + 1..];

        // Translated keyword: UTF-8, up to next NUL.
        let tk_nul = after_lang.iter().position(|&b| b == 0).ok_or_else(|| {
            Error::invalid("PNG iTXt: missing NUL separator after translated keyword")
        })?;
        let tk_bytes = &after_lang[..tk_nul];
        let translated_keyword = std::str::from_utf8(tk_bytes)
            .map_err(|e| {
                Error::invalid(format!(
                    "PNG iTXt: translated keyword is not valid UTF-8: {e}"
                ))
            })?
            .to_string();
        let text_region = &after_lang[tk_nul + 1..];

        // Text: rest of payload, possibly zlib-compressed.
        let text_bytes_owned: Vec<u8>;
        let text_bytes: &[u8] = if compressed {
            text_bytes_owned = decompress_to_vec_zlib(text_region).map_err(|e| {
                Error::invalid(format!("PNG iTXt: zlib decompression failed: {e:?}"))
            })?;
            &text_bytes_owned
        } else {
            text_region
        };
        // §11.3.3.4: "neither shall contain a zero byte" — applied to
        // the decompressed text and to the translated keyword (already
        // checked above by virtue of the NUL terminator).
        if text_bytes.contains(&0) {
            return Err(Error::invalid(
                "PNG iTXt: text contains a NUL byte (forbidden per §11.3.3.4)",
            ));
        }
        let text = std::str::from_utf8(text_bytes)
            .map_err(|e| Error::invalid(format!("PNG iTXt: text is not valid UTF-8: {e}")))?
            .to_string();

        Ok(Self {
            keyword: keyword_str,
            compressed,
            language_tag,
            translated_keyword,
            text,
        })
    }

    /// Emit the on-wire payload. Re-validates the keyword, the
    /// language tag (ASCII), and the no-`NUL`-in-translated-keyword /
    /// no-`NUL`-in-text rules; deflates the text body when
    /// [`Self::compressed`] is `true`. The compression method byte is
    /// always emitted as `0` ("the only value presently defined for
    /// it", §11.3.3.4); when [`Self::compressed`] is `false` the byte
    /// is ignored by decoders per the spec but we still emit `0` for
    /// determinism.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let keyword_bytes = validate_keyword(&self.keyword, "iTXt")?;
        // Language tag: ASCII only (BCP47 is ASCII by construction).
        // Empty is permitted (= "language unspecified", §11.3.3.4).
        for ch in self.language_tag.chars() {
            let cp = ch as u32;
            if cp > 0x7F {
                return Err(Error::invalid(format!(
                    "PNG iTXt: language tag char U+{cp:04X} not ASCII"
                )));
            }
            if cp == 0 {
                return Err(Error::invalid(
                    "PNG iTXt: language tag contains a NUL (reserved as field separator)",
                ));
            }
        }
        // Translated keyword: UTF-8 (already enforced by String), no NUL.
        if self.translated_keyword.contains('\0') {
            return Err(Error::invalid(
                "PNG iTXt: translated keyword contains a NUL byte (forbidden per §11.3.3.4)",
            ));
        }
        // Text: UTF-8, no NUL.
        if self.text.contains('\0') {
            return Err(Error::invalid(
                "PNG iTXt: text contains a NUL byte (forbidden per §11.3.3.4)",
            ));
        }

        let text_bytes = self.text.as_bytes();
        let text_payload: Vec<u8> = if self.compressed {
            compress_to_vec_zlib(text_bytes, 6)
        } else {
            text_bytes.to_vec()
        };

        let mut out = Vec::with_capacity(
            keyword_bytes.len()
                + 4
                + self.language_tag.len()
                + self.translated_keyword.len()
                + text_payload.len(),
        );
        out.extend_from_slice(&keyword_bytes);
        out.push(0); // NUL after keyword.
        out.push(if self.compressed { 1 } else { 0 });
        out.push(Self::COMPRESSION_METHOD_DEFLATE);
        out.extend_from_slice(self.language_tag.as_bytes());
        out.push(0); // NUL after language tag.
        out.extend_from_slice(self.translated_keyword.as_bytes());
        out.push(0); // NUL after translated keyword.
        out.extend_from_slice(&text_payload);
        Ok(out)
    }
}

/// Bundle of metadata chunks that round-trip through the encoder.
///
/// Populated by [`crate::parse_metadata`] on decode and consumed by
/// [`crate::PngEncoderOptions::metadata`] on encode. Any `None` field is
/// simply omitted from the output PNG; the `splt`, `texts`, `ztxts`, and
/// `itxts` `Vec`s are omitted when empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PngMetadata {
    pub sbit: Option<Sbit>,
    pub phys: Option<Phys>,
    pub time: Option<Time>,
    pub bkgd: Option<Bkgd>,
    pub hist: Option<Hist>,
    pub exif: Option<Exif>,
    pub srgb: Option<Srgb>,
    pub cicp: Option<Cicp>,
    /// Embedded ICC profile (`iCCP`, W3C PNG3 §11.3.2.3). Carries an
    /// opaque ICC.1 profile blob alongside its 1-79-byte Latin-1
    /// profile name; the codec validates only the chunk framing
    /// (keyword rules, deflate compression method, zlib round-trip)
    /// and leaves the profile internals to the caller.
    pub iccp: Option<Iccp>,
    /// Image gamma (`gAMA`, RFC 2083 §4.2.3 / W3C PNG3 §11.3.2.2).
    pub gama: Option<Gama>,
    /// Primary chromaticities + white point (`cHRM`, RFC 2083 §4.2.2 /
    /// W3C PNG3 §11.3.2.1).
    pub chrm: Option<Chrm>,
    /// Zero or more suggested palettes (`sPLT`, W3C PNG3 §11.3.4.4). The
    /// PNG spec permits multiple instances as long as each has a
    /// distinct palette name; the decoder enforces that and the encoder
    /// emits them in `Vec` order.
    pub splt: Vec<Splt>,
    /// Zero or more textual annotations (`tEXt`, RFC 2083 §4.2.7). PNG
    /// permits any number, and more than one with the same keyword is
    /// allowed — this is one of three metadata chunks where the decoder
    /// does NOT enforce keyword uniqueness (the others are `zTXt` and
    /// `iTXt`). File order is preserved on decode and replayed on
    /// encode.
    pub texts: Vec<Text>,
    /// Zero or more zlib-compressed textual annotations (`zTXt`,
    /// RFC 2083 §4.2.10). Carries the same Latin-1 keyword + text pair
    /// as [`Self::texts`] but with the body compressed on the wire —
    /// "recommended for storing large blocks of text". Same multi-
    /// instance / repeated-keyword rules as `tEXt`. File order is
    /// preserved on decode and replayed on encode.
    pub ztxts: Vec<Ztxt>,
    /// Zero or more internationalised textual annotations (`iTXt`, W3C
    /// PNG3 §11.3.3.4). UTF-8 translated keyword + UTF-8 text body,
    /// optionally zlib-compressed, paired with a BCP47 language tag
    /// and the same Latin-1 keyword as `tEXt`. Same multi-instance /
    /// repeated-keyword rules as `tEXt` and `zTXt`; file order is
    /// preserved on decode and replayed on encode.
    pub itxts: Vec<Itxt>,
}

impl PngMetadata {
    /// True when no metadata chunks are populated. Used by the encoder
    /// as a quick "nothing to emit" check.
    pub fn is_empty(&self) -> bool {
        self.sbit.is_none()
            && self.phys.is_none()
            && self.time.is_none()
            && self.bkgd.is_none()
            && self.hist.is_none()
            && self.exif.is_none()
            && self.srgb.is_none()
            && self.cicp.is_none()
            && self.iccp.is_none()
            && self.gama.is_none()
            && self.chrm.is_none()
            && self.splt.is_empty()
            && self.texts.is_empty()
            && self.ztxts.is_empty()
            && self.itxts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sbit_grayscale_roundtrip() {
        let s = Sbit::Grayscale(7);
        let b = s.to_bytes();
        assert_eq!(b, vec![7]);
        let back = Sbit::parse(&b, 0, 8).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn sbit_rgba_roundtrip() {
        let s = Sbit::Rgba(8, 8, 8, 5);
        let b = s.to_bytes();
        assert_eq!(b, vec![8, 8, 8, 5]);
        let back = Sbit::parse(&b, 6, 8).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn sbit_rejects_zero_significant_bits() {
        // 0 is forbidden per RFC 2083 §4.2.6 last paragraph.
        let bad = vec![0u8];
        let err = Sbit::parse(&bad, 0, 8).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn sbit_rejects_over_sample_depth() {
        // 9 > IHDR bit_depth 8 → invalid.
        let bad = vec![9u8];
        let err = Sbit::parse(&bad, 0, 8).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn sbit_rejects_wrong_length() {
        // colour type 6 needs 4 bytes; supplying 3 is invalid.
        let err = Sbit::parse(&[8, 8, 8], 6, 8).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn sbit_palette_uses_three_byte_layout() {
        // Per §4.2.6, indexed images carry sBIT for the palette entries'
        // R/G/B source bit depths. sample_depth is fixed at 8 in this
        // case because palette entries are 8-bit per the spec.
        let s = Sbit::Rgb(5, 6, 5);
        let b = s.to_bytes();
        assert_eq!(b, vec![5, 6, 5]);
        let back = Sbit::parse(&b, 3, 8).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn phys_roundtrip_metres() {
        let p = Phys {
            pixels_per_unit_x: 2835,
            pixels_per_unit_y: 2835,
            unit: PhysUnit::Metre,
        };
        let b = p.to_bytes();
        assert_eq!(&b[..4], &2835u32.to_be_bytes());
        assert_eq!(b[8], 1);
        let back = Phys::parse(&b).unwrap();
        assert_eq!(back, p);
        // 2835 px/m ≈ 72.009 DPI (the canonical "72 DPI" default).
        let (dx, dy) = back.dpi().unwrap();
        assert!((dx - 72.009).abs() < 0.01);
        assert!((dy - 72.009).abs() < 0.01);
    }

    #[test]
    fn phys_roundtrip_unknown_unit_means_aspect_only() {
        let p = Phys {
            pixels_per_unit_x: 4,
            pixels_per_unit_y: 3,
            unit: PhysUnit::Unknown,
        };
        let b = p.to_bytes();
        assert_eq!(b[8], 0);
        let back = Phys::parse(&b).unwrap();
        assert_eq!(back, p);
        // §4.2.5: "When the unit specifier is 0, the pHYs chunk defines
        // pixel aspect ratio only; the actual size of the pixels remains
        // unspecified."
        assert!(back.dpi().is_none());
    }

    #[test]
    fn phys_rejects_bad_unit() {
        let mut bad = [0u8; 9];
        bad[8] = 2; // only 0 / 1 defined.
        let err = Phys::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn phys_rejects_wrong_length() {
        let err = Phys::parse(&[0; 8]).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn time_roundtrip() {
        let t = Time {
            year: 2026,
            month: 5,
            day: 20,
            hour: 14,
            minute: 30,
            second: 45,
        };
        let b = t.to_bytes();
        assert_eq!(&b[..2], &2026u16.to_be_bytes());
        let back = Time::parse(&b).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn time_accepts_leap_second_sixty() {
        // §4.2.8 specifically calls out 60-as-leap-second-not-61.
        let t = Time {
            year: 2016,
            month: 12,
            day: 31,
            hour: 23,
            minute: 59,
            second: 60,
        };
        let b = t.to_bytes();
        let back = Time::parse(&b).unwrap();
        assert_eq!(back.second, 60);
    }

    #[test]
    fn time_rejects_second_sixty_one() {
        let bad = [0x07, 0xE4, 1, 1, 0, 0, 61]; // second = 61 (forbidden).
        let err = Time::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn time_rejects_month_zero() {
        let bad = [0x07, 0xE4, 0, 1, 0, 0, 0];
        let err = Time::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn time_rejects_hour_twenty_four() {
        let bad = [0x07, 0xE4, 1, 1, 24, 0, 0];
        let err = Time::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn metadata_is_empty_when_all_none() {
        let m = PngMetadata::default();
        assert!(m.is_empty());
        let m2 = PngMetadata {
            sbit: Some(Sbit::Grayscale(8)),
            ..Default::default()
        };
        assert!(!m2.is_empty());
        let m3 = PngMetadata {
            bkgd: Some(Bkgd::Palette(0)),
            ..Default::default()
        };
        assert!(!m3.is_empty());
        let m4 = PngMetadata {
            hist: Some(Hist {
                frequencies: vec![1, 2, 3],
            }),
            ..Default::default()
        };
        assert!(!m4.is_empty());
        let m5 = PngMetadata {
            exif: Some(Exif {
                data: TIFF_LE_MAGIC.to_vec(),
            }),
            ..Default::default()
        };
        assert!(!m5.is_empty());
        let m6 = PngMetadata {
            srgb: Some(Srgb {
                rendering_intent: RenderingIntent::Perceptual,
            }),
            ..Default::default()
        };
        assert!(!m6.is_empty());
        let m7 = PngMetadata {
            cicp: Some(Cicp {
                color_primaries: 1,
                transfer_function: 13,
                matrix_coefficients: 0,
                video_full_range_flag: 1,
            }),
            ..Default::default()
        };
        assert!(!m7.is_empty());
        let m8 = PngMetadata {
            gama: Some(Gama {
                gamma_times_100000: 45_000,
            }),
            ..Default::default()
        };
        assert!(!m8.is_empty());
        let m9 = PngMetadata {
            chrm: Some(Chrm {
                white_point_x: 31_270,
                white_point_y: 32_900,
                red_x: 64_000,
                red_y: 33_000,
                green_x: 30_000,
                green_y: 60_000,
                blue_x: 15_000,
                blue_y: 6_000,
            }),
            ..Default::default()
        };
        assert!(!m9.is_empty());
    }

    #[test]
    fn bkgd_grayscale_roundtrip() {
        // Colour type 0, 8-bit: grey level 200 in low byte, MSB zero.
        let b = Bkgd::Grayscale(200);
        let raw = b.to_bytes();
        assert_eq!(raw, vec![0x00, 0xC8]); // big-endian 0x00C8 == 200.
        let back = Bkgd::parse(&raw, 0, 8).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn bkgd_grayscale_16bit_full_range() {
        // 16-bit grey 0xFFFF is fine.
        let b = Bkgd::Grayscale(0xFFFF);
        let raw = b.to_bytes();
        let back = Bkgd::parse(&raw, 0, 16).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn bkgd_grayscale_rejects_value_above_bitdepth_cap() {
        // 8-bit cap is 0xFF; 0x0100 must be rejected per PNG3
        // §11.3.4.1 "decoders must mask the other bits to 0".
        let raw = 0x0100u16.to_be_bytes();
        let err = Bkgd::parse(&raw, 0, 8).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn bkgd_rgb_roundtrip_8bit() {
        // 8-bit RGB background ⇒ payload is `00 R 00 G 00 B`.
        let b = Bkgd::Rgb(0x80, 0x40, 0x10);
        let raw = b.to_bytes();
        assert_eq!(raw, vec![0x00, 0x80, 0x00, 0x40, 0x00, 0x10]);
        let back = Bkgd::parse(&raw, 2, 8).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn bkgd_rgb_rejects_sample_above_bitdepth_cap() {
        // 4-bit cap = 15; supplying 16 must fail.
        let bad = [0x00, 0x10, 0x00, 0x00, 0x00, 0x00];
        let err = Bkgd::parse(&bad, 2, 4).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn bkgd_palette_roundtrip() {
        let b = Bkgd::Palette(7);
        let raw = b.to_bytes();
        assert_eq!(raw, vec![7]);
        let back = Bkgd::parse(&raw, 3, 8).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn bkgd_rejects_wrong_payload_length() {
        // Colour type 6 wants 6 bytes; only 4 supplied.
        let err = Bkgd::parse(&[0; 4], 6, 8).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn bkgd_rejects_unknown_colour_type() {
        let err = Bkgd::parse(&[0; 2], 5, 8).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn hist_roundtrip() {
        let h = Hist {
            frequencies: vec![0, 1, 65535, 12345],
        };
        let raw = h.to_bytes();
        assert_eq!(raw.len(), 8);
        let back = Hist::parse(&raw, 4).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn hist_rejects_length_mismatch_against_palette() {
        // 3 PLTE entries but payload covers only 2 → reject.
        let bad = [0u8; 4];
        let err = Hist::parse(&bad, 3).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn hist_empty_palette_is_zero_bytes() {
        // Zero palette entries ⇒ zero hIST bytes. The parser should
        // happily accept an empty payload in that case (still pointless
        // but not a spec violation in itself).
        let h = Hist::parse(&[], 0).unwrap();
        assert!(h.frequencies.is_empty());
    }

    #[test]
    fn exif_little_endian_roundtrip() {
        // "II", 42 LE, then an opaque (here trivial) TIFF body. We don't
        // interpret the body; it just rides along verbatim.
        let mut raw = TIFF_LE_MAGIC.to_vec();
        raw.extend_from_slice(&[0x08, 0x00, 0x00, 0x00, 0xDE, 0xAD]);
        let e = Exif::parse(&raw).unwrap();
        assert_eq!(e.to_bytes(), raw);
        assert_eq!(e.data, raw);
    }

    #[test]
    fn exif_big_endian_roundtrip() {
        // "MM", 42 BE header.
        let mut raw = TIFF_BE_MAGIC.to_vec();
        raw.extend_from_slice(&[0x00, 0x00, 0x00, 0x08]);
        let e = Exif::parse(&raw).unwrap();
        assert_eq!(e.to_bytes(), raw);
    }

    #[test]
    fn exif_accepts_bare_four_byte_header() {
        // The minimal legal payload is the 4-byte byte-order header.
        let e = Exif::parse(&TIFF_LE_MAGIC).unwrap();
        assert_eq!(e.data, TIFF_LE_MAGIC);
    }

    #[test]
    fn exif_rejects_short_payload() {
        // Fewer than 4 bytes can't carry a TIFF header.
        let err = Exif::parse(&[0x49, 0x49, 0x2A]).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn exif_rejects_bad_byte_order_magic() {
        // §11.3.4.5.2: only "II"/42 and "MM"/42 are valid; everything
        // else is reserved and must be rejected. JFIF's "JFIF" header is
        // a representative wrong value.
        let bad = [0x4A, 0x46, 0x49, 0x46, 0x00];
        let err = Exif::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn exif_rejects_wrong_endianness_marker() {
        // "II" but with a big-endian 42 (00 2A) — the spec pins the
        // 42 marker to match the declared byte order, so this is invalid.
        let bad = [0x49, 0x49, 0x00, 0x2A];
        let err = Exif::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn srgb_roundtrips_every_defined_intent() {
        // All four Table 16 intents survive byte → variant → byte.
        for (byte, intent) in [
            (0u8, RenderingIntent::Perceptual),
            (1, RenderingIntent::RelativeColorimetric),
            (2, RenderingIntent::Saturation),
            (3, RenderingIntent::AbsoluteColorimetric),
        ] {
            let s = Srgb {
                rendering_intent: intent,
            };
            assert_eq!(s.to_bytes(), [byte]);
            let back = Srgb::parse(&[byte]).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn srgb_rejects_reserved_intent() {
        // §11.3.2.5: only 0..=3 are defined; 4 is reserved.
        let err = Srgb::parse(&[4]).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
        let err = Srgb::parse(&[255]).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn srgb_rejects_wrong_length() {
        // The chunk is exactly one byte; zero or two bytes is invalid.
        assert!(matches!(
            Srgb::parse(&[]).unwrap_err(),
            Error::InvalidData(_)
        ));
        assert!(matches!(
            Srgb::parse(&[0, 0]).unwrap_err(),
            Error::InvalidData(_)
        ));
    }

    #[test]
    fn cicp_bt709_narrow_range_example_roundtrip() {
        // §11.3.2.6 Example 3 — narrow-range BT.709 image.
        let raw = [0x01, 0x01, 0x00, 0x00];
        let c = Cicp::parse(&raw).unwrap();
        assert_eq!(c.color_primaries, 1);
        assert_eq!(c.transfer_function, 1);
        assert_eq!(c.matrix_coefficients, 0);
        assert_eq!(c.video_full_range_flag, 0);
        assert_eq!(c.to_bytes(), raw);
    }

    #[test]
    fn cicp_bt2100_pq_full_range_example_roundtrip() {
        // §11.3.2.6 Example 1 — BT.2100 PQ, full-range.
        let raw = [0x09, 0x10, 0x00, 0x01];
        let c = Cicp::parse(&raw).unwrap();
        assert_eq!(c.color_primaries, 9);
        assert_eq!(c.transfer_function, 16);
        assert_eq!(c.matrix_coefficients, 0);
        assert_eq!(c.video_full_range_flag, 1);
        assert_eq!(c.to_bytes(), raw);
    }

    #[test]
    fn cicp_display_p3_example_roundtrip() {
        // §11.3.2.6 Example 4 — Display P3 (full-range).
        let raw = [0x0C, 0x0D, 0x00, 0x01];
        let c = Cicp::parse(&raw).unwrap();
        assert_eq!(c.color_primaries, 12);
        assert_eq!(c.transfer_function, 13);
        assert_eq!(c.matrix_coefficients, 0);
        assert_eq!(c.video_full_range_flag, 1);
        assert_eq!(c.to_bytes(), raw);
    }

    #[test]
    fn cicp_rejects_nonzero_matrix_coefficients() {
        // §11.3.2.6: "RGB is currently the only supported color model in
        // PNG, and as such Matrix Coefficients shall be set to 0."
        let bad = [1u8, 13, 1, 1];
        let err = Cicp::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn cicp_rejects_reserved_video_range_flag() {
        // H.273 §8.3 reserves anything outside 0..=1.
        let bad = [1u8, 13, 0, 2];
        let err = Cicp::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
        let bad = [1u8, 13, 0, 255];
        let err = Cicp::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn cicp_rejects_wrong_length() {
        // Exactly four bytes per Table 18.
        assert!(matches!(
            Cicp::parse(&[1, 13, 0]).unwrap_err(),
            Error::InvalidData(_)
        ));
        assert!(matches!(
            Cicp::parse(&[1, 13, 0, 1, 0]).unwrap_err(),
            Error::InvalidData(_)
        ));
        assert!(matches!(
            Cicp::parse(&[]).unwrap_err(),
            Error::InvalidData(_)
        ));
    }

    #[test]
    fn gama_roundtrip() {
        // RFC 2083 §4.2.3: a gamma of 0.45 is stored as the integer
        // 45000.
        let g = Gama {
            gamma_times_100000: 45_000,
        };
        let b = g.to_bytes();
        assert_eq!(b, 45_000u32.to_be_bytes());
        let back = Gama::parse(&b).unwrap();
        assert_eq!(back, g);
        assert!((back.gamma() - 0.45).abs() < 1e-9);
    }

    #[test]
    fn gama_preserves_zero_verbatim() {
        // PNG3 §11.3.2.2: a zero gamma "is meaningless … decoders should
        // ignore it" — a SHOULD, not a SHALL, so parse keeps the raw
        // integer and round-trips it intact.
        let b = 0u32.to_be_bytes();
        let g = Gama::parse(&b).unwrap();
        assert_eq!(g.gamma_times_100000, 0);
        assert_eq!(g.to_bytes(), b);
    }

    #[test]
    fn gama_preserves_full_u32_range() {
        let g = Gama {
            gamma_times_100000: u32::MAX,
        };
        let back = Gama::parse(&g.to_bytes()).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn gama_rejects_wrong_length() {
        assert!(matches!(
            Gama::parse(&[0, 0, 0]).unwrap_err(),
            Error::InvalidData(_)
        ));
        assert!(matches!(
            Gama::parse(&[0, 0, 0, 0, 0]).unwrap_err(),
            Error::InvalidData(_)
        ));
        assert!(matches!(
            Gama::parse(&[]).unwrap_err(),
            Error::InvalidData(_)
        ));
    }

    #[test]
    fn chrm_roundtrip_srgb_d65_primaries() {
        // RFC 2083 §4.2.2 worked example: a chromaticity of 0.3127 is
        // stored as 31270. These are the canonical sRGB / Rec.709
        // primaries + D65 white point (× 100000).
        let c = Chrm {
            white_point_x: 31_270,
            white_point_y: 32_900,
            red_x: 64_000,
            red_y: 33_000,
            green_x: 30_000,
            green_y: 60_000,
            blue_x: 15_000,
            blue_y: 6_000,
        };
        let b = c.to_bytes();
        assert_eq!(b.len(), 32);
        // White-point x lands in the first BE word.
        assert_eq!(&b[..4], &31_270u32.to_be_bytes());
        // Blue y lands in the last BE word.
        assert_eq!(&b[28..], &6_000u32.to_be_bytes());
        let back = Chrm::parse(&b).unwrap();
        assert_eq!(back, c);
        let (wx, wy) = back.white_point();
        assert!((wx - 0.31270).abs() < 1e-9);
        assert!((wy - 0.32900).abs() < 1e-9);
        assert_eq!(back.red(), (0.64, 0.33));
        assert_eq!(back.green(), (0.30, 0.60));
        assert_eq!(back.blue(), (0.15, 0.06));
    }

    #[test]
    fn chrm_field_order_is_white_red_green_blue() {
        // Distinct value per field confirms the BE word offsets map to
        // the right struct member (spec layout: WP x/y, R x/y, G x/y,
        // B x/y).
        let c = Chrm {
            white_point_x: 1,
            white_point_y: 2,
            red_x: 3,
            red_y: 4,
            green_x: 5,
            green_y: 6,
            blue_x: 7,
            blue_y: 8,
        };
        let b = c.to_bytes();
        for (i, expect) in (1u32..=8).enumerate() {
            assert_eq!(&b[i * 4..i * 4 + 4], &expect.to_be_bytes());
        }
        assert_eq!(Chrm::parse(&b).unwrap(), c);
    }

    #[test]
    fn chrm_rejects_wrong_length() {
        assert!(matches!(
            Chrm::parse(&[0u8; 31]).unwrap_err(),
            Error::InvalidData(_)
        ));
        assert!(matches!(
            Chrm::parse(&[0u8; 33]).unwrap_err(),
            Error::InvalidData(_)
        ));
        assert!(matches!(
            Chrm::parse(&[]).unwrap_err(),
            Error::InvalidData(_)
        ));
    }

    #[test]
    fn splt_8bit_roundtrip() {
        // Two-entry 8-bit palette; entries in decreasing-frequency order.
        let s = Splt {
            name: "256 color including Macintosh default".to_string(),
            sample_depth: 8,
            entries: vec![
                SpltEntry {
                    red: 0xFF,
                    green: 0x80,
                    blue: 0x00,
                    alpha: 0xFF,
                    frequency: 60000,
                },
                SpltEntry {
                    red: 0x10,
                    green: 0x20,
                    blue: 0x30,
                    alpha: 0x40,
                    frequency: 0,
                },
            ],
        };
        let raw = s.to_bytes().unwrap();
        // name(37) + NUL(1) + depth(1) + 2 entries × 6 = 51 bytes.
        assert_eq!(raw.len(), 37 + 1 + 1 + 12);
        // The depth byte sits right after the NUL terminator.
        assert_eq!(raw[37], 0);
        assert_eq!(raw[38], 8);
        let back = Splt::parse(&raw).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn splt_16bit_roundtrip() {
        // 16-bit palette: each sample is a big-endian u16 → 10-byte stride.
        let s = Splt {
            name: "Optimal 512".to_string(),
            sample_depth: 16,
            entries: vec![SpltEntry {
                red: 0xDEAD,
                green: 0xBEEF,
                blue: 0x1234,
                alpha: 0xFFFF,
                frequency: 0xABCD,
            }],
        };
        let raw = s.to_bytes().unwrap();
        // name(11) + NUL(1) + depth(1) + 1 entry × 10 = 23 bytes.
        assert_eq!(raw.len(), 11 + 1 + 1 + 10);
        assert_eq!(raw[12], 16);
        // First sample big-endian DEAD.
        assert_eq!(&raw[13..15], &[0xDE, 0xAD]);
        let back = Splt::parse(&raw).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn splt_empty_entry_list_roundtrip() {
        // A palette with a name but zero entries is structurally legal
        // (the entry region after the depth byte is empty, which divides
        // evenly by any stride).
        let s = Splt {
            name: "empty".to_string(),
            sample_depth: 8,
            entries: vec![],
        };
        let raw = s.to_bytes().unwrap();
        assert_eq!(raw.len(), 5 + 1 + 1);
        let back = Splt::parse(&raw).unwrap();
        assert_eq!(back, s);
        assert!(back.entries.is_empty());
    }

    #[test]
    fn splt_parse_rejects_missing_nul() {
        // No NUL separator anywhere → can't delimit the palette name.
        let bad = b"name-with-no-nul-and-then-bytes".to_vec();
        let err = Splt::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn splt_parse_rejects_bad_sample_depth() {
        // "n\0" + depth 4 (only 8 / 16 are legal).
        let bad = [b'n', 0x00, 4];
        let err = Splt::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn splt_parse_rejects_entry_length_not_multiple_of_stride() {
        // depth 8 ⇒ stride 6; supply 5 trailing bytes (not a multiple).
        let mut bad = vec![b'n', 0x00, 8];
        bad.extend_from_slice(&[1, 2, 3, 4, 5]);
        let err = Splt::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn splt_parse_rejects_missing_sample_depth_byte() {
        // Name + NUL but no sample-depth byte at all.
        let bad = [b'n', 0x00];
        let err = Splt::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn splt_rejects_empty_name() {
        // §11.3.3.1: keywords (and sPLT names) are 1..=79 bytes.
        let bad = [0x00, 8]; // empty name, NUL, depth.
        let err = Splt::parse(&bad).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
        // …and the encoder rejects it too.
        let s = Splt {
            name: String::new(),
            sample_depth: 8,
            entries: vec![],
        };
        assert!(matches!(s.to_bytes().unwrap_err(), Error::InvalidData(_)));
    }

    #[test]
    fn splt_rejects_name_over_79_bytes() {
        let s = Splt {
            name: "x".repeat(80),
            sample_depth: 8,
            entries: vec![],
        };
        assert!(matches!(s.to_bytes().unwrap_err(), Error::InvalidData(_)));
    }

    #[test]
    fn splt_rejects_name_with_control_char() {
        // 0x1F is below the printable 0x20 floor.
        let s = Splt {
            name: "bad\u{1F}name".to_string(),
            sample_depth: 8,
            entries: vec![],
        };
        assert!(matches!(s.to_bytes().unwrap_err(), Error::InvalidData(_)));
    }

    #[test]
    fn splt_rejects_name_in_latin1_gap() {
        // 0x80..=0xA0 are reserved (the gap between the two printable
        // Latin-1 ranges). U+00A0 NO-BREAK SPACE is explicitly excluded.
        let s = Splt {
            name: "a\u{A0}b".to_string(),
            sample_depth: 8,
            entries: vec![],
        };
        assert!(matches!(s.to_bytes().unwrap_err(), Error::InvalidData(_)));
    }

    #[test]
    fn splt_rejects_leading_trailing_consecutive_spaces() {
        for bad_name in [" lead", "trail ", "two  spaces"] {
            let s = Splt {
                name: bad_name.to_string(),
                sample_depth: 8,
                entries: vec![],
            };
            assert!(
                matches!(s.to_bytes().unwrap_err(), Error::InvalidData(_)),
                "expected reject for {bad_name:?}"
            );
        }
        // A single interior space is fine.
        let ok = Splt {
            name: "one space".to_string(),
            sample_depth: 8,
            entries: vec![],
        };
        assert!(ok.to_bytes().is_ok());
    }

    #[test]
    fn splt_rejects_non_latin1_name_char() {
        // A multi-byte char can't be a single Latin-1 byte.
        let s = Splt {
            name: "café\u{1F600}".to_string(),
            sample_depth: 8,
            entries: vec![],
        };
        assert!(matches!(s.to_bytes().unwrap_err(), Error::InvalidData(_)));
    }

    #[test]
    fn splt_encode_rejects_8bit_sample_over_255() {
        // An 8-bit palette can't carry a sample value of 256+.
        let s = Splt {
            name: "ok".to_string(),
            sample_depth: 8,
            entries: vec![SpltEntry {
                red: 256,
                green: 0,
                blue: 0,
                alpha: 0,
                frequency: 0,
            }],
        };
        assert!(matches!(s.to_bytes().unwrap_err(), Error::InvalidData(_)));
    }

    #[test]
    fn splt_high_latin1_name_roundtrips() {
        // 0xC9 = É is in the upper printable Latin-1 range (0xA1..=0xFF).
        let s = Splt {
            name: "\u{C9}".to_string(),
            sample_depth: 8,
            entries: vec![],
        };
        let raw = s.to_bytes().unwrap();
        // First byte is the raw Latin-1 0xC9.
        assert_eq!(raw[0], 0xC9);
        let back = Splt::parse(&raw).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn metadata_is_empty_accounts_for_splt() {
        let mut m = PngMetadata::default();
        assert!(m.is_empty());
        m.splt.push(Splt {
            name: "p".to_string(),
            sample_depth: 8,
            entries: vec![],
        });
        assert!(!m.is_empty());
    }

    #[test]
    fn cicp_preserves_reserved_color_primaries_byte() {
        // The first two bytes index into H.273 registries that include
        // "Reserved" entries; parse intentionally round-trips any byte
        // (no enum gating) so the encoder can faithfully rewrite caller-
        // supplied values, even forward-compatible ones.
        let raw = [0xFE, 0xFE, 0x00, 0x01];
        let c = Cicp::parse(&raw).unwrap();
        assert_eq!(c.color_primaries, 0xFE);
        assert_eq!(c.transfer_function, 0xFE);
        assert_eq!(c.to_bytes(), raw);
    }

    #[test]
    fn text_simple_roundtrip() {
        let t = Text {
            keyword: "Title".to_string(),
            text: "Sunrise over the Pacific".to_string(),
        };
        let raw = t.to_bytes().unwrap();
        // keyword bytes (5) + NUL + text bytes (24) = 30
        assert_eq!(raw.len(), 5 + 1 + 24);
        assert_eq!(&raw[0..5], b"Title");
        assert_eq!(raw[5], 0);
        assert_eq!(&raw[6..], b"Sunrise over the Pacific");
        let back = Text::parse(&raw).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn text_empty_text_string_roundtrip() {
        // RFC 2083 §4.2.7: "The text string can be of any length from
        // zero bytes up to the maximum permissible chunk size..."
        let t = Text {
            keyword: "Comment".to_string(),
            text: String::new(),
        };
        let raw = t.to_bytes().unwrap();
        // keyword (7) + NUL + 0 text bytes = 8
        assert_eq!(raw, [b'C', b'o', b'm', b'm', b'e', b'n', b't', 0]);
        let back = Text::parse(&raw).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn text_high_latin1_byte_roundtrip() {
        // RFC 2083 §4.2.7: "Both keyword and text are interpreted
        // according to the ISO 8859-1 (Latin-1) character set". Text
        // can carry any Latin-1 byte; cover one in the 0xA1..=0xFF
        // band so the codepath that maps `u8 -> char` is exercised.
        let t = Text {
            keyword: "Author".to_string(),
            // 'é' is U+00E9, single Latin-1 byte 0xE9.
            text: "Renée".to_string(),
        };
        let raw = t.to_bytes().unwrap();
        // The encoded text portion should contain the Latin-1 byte
        // 0xE9 ('é') in position 3 of the text portion ("Ren'é'e").
        assert!(raw.contains(&0xE9));
        let back = Text::parse(&raw).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn text_embedded_newline_allowed() {
        // §4.2.7: "Newlines in the text string should be represented by
        // a single linefeed character (decimal 10)." The decoder must
        // accept LF in the text payload — it's not a NUL, so it doesn't
        // truncate the string.
        let t = Text {
            keyword: "Description".to_string(),
            text: "line one\nline two".to_string(),
        };
        let raw = t.to_bytes().unwrap();
        let back = Text::parse(&raw).unwrap();
        assert_eq!(back, t);
        assert!(back.text.contains('\n'));
    }

    #[test]
    fn text_keyword_at_79_bytes_accepted() {
        // §4.2.7: keyword is "1-79 bytes". 79 is the inclusive upper
        // bound; 80 is rejected.
        let kw_79 = "k".repeat(79);
        let t = Text {
            keyword: kw_79.clone(),
            text: "x".to_string(),
        };
        let raw = t.to_bytes().unwrap();
        let back = Text::parse(&raw).unwrap();
        assert_eq!(back.keyword, kw_79);
    }

    #[test]
    fn text_keyword_at_80_bytes_rejected() {
        let kw_80 = "k".repeat(80);
        let t = Text {
            keyword: kw_80,
            text: "x".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn text_empty_keyword_rejected() {
        // §4.2.7: keyword "must be at least one character".
        let t = Text {
            keyword: String::new(),
            text: "anything".to_string(),
        };
        assert!(t.to_bytes().is_err());
        // And the same on parse — an empty keyword byte sequence shows
        // up as the NUL being at offset 0.
        let raw = b"\0anything";
        assert!(Text::parse(raw).is_err());
    }

    #[test]
    fn text_keyword_with_leading_space_rejected() {
        let t = Text {
            keyword: " Leading".to_string(),
            text: "x".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn text_keyword_with_consecutive_spaces_rejected() {
        let t = Text {
            keyword: "Two  Spaces".to_string(),
            text: "x".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn text_keyword_with_non_breaking_space_rejected() {
        // §4.2.7 paragraph 4: "the non-breaking space (code 160) is
        // not permitted in keywords, since it is visually
        // indistinguishable from an ordinary space."
        let t = Text {
            keyword: "Bad\u{00A0}NBSP".to_string(),
            text: "x".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn text_keyword_with_control_char_rejected() {
        // Outside the printable bands (0x20..=0x7E or 0xA1..=0xFF).
        // 0x7F (DEL) is in neither.
        let t = Text {
            keyword: "Bad\u{007F}".to_string(),
            text: "x".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn text_text_containing_nul_rejected_on_parse() {
        // §4.2.7: "Neither the keyword nor the text string can contain
        // a null character." Two NULs in the chunk payload means the
        // text string itself contains a NUL — reject the parse.
        let raw = b"kw\0first\0second";
        assert!(Text::parse(raw).is_err());
    }

    #[test]
    fn text_text_containing_nul_rejected_on_encode() {
        let t = Text {
            keyword: "k".to_string(),
            text: "bad\u{0000}null".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn text_text_with_non_latin1_codepoint_rejected_on_encode() {
        // §4.2.7 fixes the text to Latin-1; a codepoint above 0xFF
        // can't be written without lossy conversion, which the encoder
        // refuses.
        let t = Text {
            keyword: "k".to_string(),
            // U+0100 — first codepoint outside Latin-1.
            text: "\u{0100}".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn text_parse_missing_nul_rejected() {
        let raw = b"NoSeparator";
        assert!(Text::parse(raw).is_err());
    }

    #[test]
    fn metadata_is_empty_accounts_for_texts() {
        let mut m = PngMetadata::default();
        assert!(m.is_empty());
        m.texts.push(Text {
            keyword: "k".to_string(),
            text: String::new(),
        });
        assert!(!m.is_empty());
    }

    // ---- zTXt -------------------------------------------------------------

    #[test]
    fn ztxt_roundtrip_simple() {
        // §4.2.10: keyword + NUL + compression method (0) + zlib body.
        let z = Ztxt {
            keyword: "Description".to_string(),
            text: "A compressed description of the image.".to_string(),
        };
        let raw = z.to_bytes().unwrap();
        // On-wire layout sanity: keyword bytes, NUL, method byte (0),
        // then a zlib stream (starts with 0x78 for compression level
        // 6 — CMF byte = 0x78, FLG follows). The exact zlib header
        // depends on the dictionary / level, but the first byte of any
        // zlib stream with CINFO=7 / CM=8 (the only combination
        // miniz_oxide produces) is 0x78. We assert the layout is
        // "keyword || NUL || 0 || zlib", not the level-specific bytes.
        let keyword_bytes = b"Description";
        assert_eq!(&raw[..keyword_bytes.len()], keyword_bytes);
        assert_eq!(raw[keyword_bytes.len()], 0);
        assert_eq!(raw[keyword_bytes.len() + 1], 0);
        // zlib magic byte (CMF = 0x78 for CM=8/CINFO=7 — the only mode
        // miniz_oxide emits).
        assert_eq!(raw[keyword_bytes.len() + 2], 0x78);
        let back = Ztxt::parse(&raw).unwrap();
        assert_eq!(back, z);
    }

    #[test]
    fn ztxt_roundtrip_empty_text() {
        // §4.2.10 doesn't forbid n=0 compressed bytes (after inflate,
        // empty plaintext). Make sure the round-trip survives.
        let z = Ztxt {
            keyword: "Empty".to_string(),
            text: String::new(),
        };
        let raw = z.to_bytes().unwrap();
        let back = Ztxt::parse(&raw).unwrap();
        assert_eq!(back, z);
        assert!(back.text.is_empty());
    }

    #[test]
    fn ztxt_large_text_compresses() {
        // The whole point of zTXt vs tEXt: large bodies compress.
        // "recommended for storing large blocks of text" (§4.2.10).
        // A 2000-byte run of one character must serialise to far fewer
        // bytes than its tEXt-equivalent encoding (keyword + NUL +
        // 2000 text bytes).
        let z = Ztxt {
            keyword: "Bulk".to_string(),
            text: "A".repeat(2000),
        };
        let raw = z.to_bytes().unwrap();
        // Keyword (4) + NUL (1) + method (1) + zlib stream. zlib of 2000
        // identical bytes compresses to well under 100 bytes; allow
        // 200 for headroom against future miniz_oxide tuning.
        assert!(
            raw.len() < 200,
            "zTXt of 2000 identical chars should compress well (got {} bytes)",
            raw.len()
        );
        let back = Ztxt::parse(&raw).unwrap();
        assert_eq!(back, z);
    }

    #[test]
    fn ztxt_rejects_unknown_compression_method() {
        // §4.2.10: "The only value presently defined for it is 0".
        // Hand-build a payload with method = 1 and confirm parse rejects.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"kw");
        raw.push(0); // NUL
        raw.push(1); // bogus compression method
        raw.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert!(Ztxt::parse(&raw).is_err());
    }

    #[test]
    fn ztxt_rejects_missing_method_byte() {
        // Payload ends exactly at the NUL — no method byte at all.
        let raw = b"kw\0";
        assert!(Ztxt::parse(raw).is_err());
    }

    #[test]
    fn ztxt_rejects_missing_nul() {
        // No NUL separator means no keyword terminator → parse error.
        let raw = b"NoSeparator";
        assert!(Ztxt::parse(raw).is_err());
    }

    #[test]
    fn ztxt_rejects_corrupted_zlib_stream() {
        // Method byte is 0 but the compressed body is garbage; inflate
        // must fail and the parse must surface an InvalidData error
        // (no panic, no infinite loop).
        let mut raw = Vec::new();
        raw.extend_from_slice(b"kw");
        raw.push(0);
        raw.push(0); // method = deflate
        raw.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(Ztxt::parse(&raw).is_err());
    }

    #[test]
    fn ztxt_rejects_decompressed_nul() {
        // §4.2.10 + §4.2.7: the decompressed text must obey the same
        // "no NUL" rule as tEXt. Build a valid zTXt by deflating a NUL
        // ourselves and confirm parse rejects.
        let payload = b"valid\0invalid";
        let compressed = compress_to_vec_zlib(payload, 6);
        let mut raw = Vec::new();
        raw.extend_from_slice(b"kw");
        raw.push(0);
        raw.push(0);
        raw.extend_from_slice(&compressed);
        assert!(Ztxt::parse(&raw).is_err());
    }

    #[test]
    fn ztxt_encode_rejects_non_latin1_text() {
        // Same Latin-1-only rule as tEXt: U+0100 is the first codepoint
        // outside Latin-1 and must be rejected on encode.
        let z = Ztxt {
            keyword: "k".to_string(),
            text: "\u{0100}".to_string(),
        };
        assert!(z.to_bytes().is_err());
    }

    #[test]
    fn ztxt_encode_rejects_nul_in_text() {
        let z = Ztxt {
            keyword: "k".to_string(),
            text: "bad\u{0000}null".to_string(),
        };
        assert!(z.to_bytes().is_err());
    }

    #[test]
    fn ztxt_keyword_validation_shares_text_rules() {
        // Reuses validate_keyword — leading space / consecutive spaces
        // / non-breaking space / length / printable rules must all
        // apply identically to zTXt.
        for bad in [" Leading", "Two  Spaces", "Bad\u{00A0}NBSP", ""] {
            let z = Ztxt {
                keyword: bad.to_string(),
                text: "x".to_string(),
            };
            assert!(z.to_bytes().is_err(), "expected to reject keyword {bad:?}");
        }
        let too_long = "k".repeat(80);
        let z = Ztxt {
            keyword: too_long,
            text: "x".to_string(),
        };
        assert!(z.to_bytes().is_err());
    }

    #[test]
    fn metadata_is_empty_accounts_for_ztxts() {
        let mut m = PngMetadata::default();
        assert!(m.is_empty());
        m.ztxts.push(Ztxt {
            keyword: "k".to_string(),
            text: String::new(),
        });
        assert!(!m.is_empty());
    }

    // ---- iCCP -------------------------------------------------------------

    #[test]
    fn iccp_roundtrip_simple() {
        // §11.3.2.3: name + NUL + method (0) + zlib body.
        let p = Iccp {
            name: "sRGB IEC61966-2.1".to_string(),
            profile: b"\x00\x00\x02\x10ADBE".to_vec(),
        };
        let raw = p.to_bytes().unwrap();
        let name_bytes = b"sRGB IEC61966-2.1";
        assert_eq!(&raw[..name_bytes.len()], name_bytes);
        assert_eq!(raw[name_bytes.len()], 0);
        assert_eq!(raw[name_bytes.len() + 1], 0); // method = deflate
                                                  // zlib magic byte (CMF = 0x78 for CM=8/CINFO=7).
        assert_eq!(raw[name_bytes.len() + 2], 0x78);
        let back = Iccp::parse(&raw).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn iccp_roundtrip_empty_profile() {
        // n = 0 plaintext is permitted; the inflated body is empty.
        let p = Iccp {
            name: "Empty".to_string(),
            profile: Vec::new(),
        };
        let raw = p.to_bytes().unwrap();
        let back = Iccp::parse(&raw).unwrap();
        assert_eq!(back, p);
        assert!(back.profile.is_empty());
    }

    #[test]
    fn iccp_rejects_unknown_compression_method() {
        // §11.3.2.3: "The only compression method defined in this
        // specification is method 0". Hand-build a payload with method=1
        // and confirm parse rejects.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"P");
        raw.push(0);
        raw.push(1); // bogus method
        raw.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert!(Iccp::parse(&raw).is_err());
    }

    #[test]
    fn iccp_rejects_missing_method_byte() {
        let raw = b"P\0";
        assert!(Iccp::parse(raw).is_err());
    }

    #[test]
    fn iccp_rejects_missing_nul() {
        let raw = b"NoSeparator";
        assert!(Iccp::parse(raw).is_err());
    }

    #[test]
    fn iccp_rejects_corrupted_zlib_stream() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"P");
        raw.push(0);
        raw.push(0);
        raw.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(Iccp::parse(&raw).is_err());
    }

    #[test]
    fn iccp_rejects_invalid_name() {
        // Leading space → invalid per the shared keyword predicate.
        let p = Iccp {
            name: " Leading".to_string(),
            profile: vec![1, 2, 3],
        };
        assert!(p.to_bytes().is_err());
    }

    #[test]
    fn iccp_large_profile_compresses() {
        // ICC profiles in the wild are kilobytes-sized; a 4 KB run of
        // one byte compresses very well via zlib deflate.
        let p = Iccp {
            name: "Bulk".to_string(),
            profile: vec![0x42; 4096],
        };
        let raw = p.to_bytes().unwrap();
        assert!(
            raw.len() < 200,
            "iCCP of 4096 identical bytes should compress to << 200 wire bytes (got {})",
            raw.len()
        );
        let back = Iccp::parse(&raw).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn metadata_is_empty_accounts_for_iccp() {
        let mut m = PngMetadata::default();
        assert!(m.is_empty());
        m.iccp = Some(Iccp {
            name: "k".to_string(),
            profile: Vec::new(),
        });
        assert!(!m.is_empty());
    }

    // ---- iTXt -------------------------------------------------------------

    #[test]
    fn itxt_roundtrip_uncompressed() {
        // §11.3.3.4: keyword + NUL + flag + method + lang + NUL + tk +
        // NUL + text. Uncompressed; method byte ignored on parse but
        // emitted as 0.
        let t = Itxt {
            keyword: "Title".to_string(),
            compressed: false,
            language_tag: "en".to_string(),
            translated_keyword: String::new(),
            text: "A description of the image.".to_string(),
        };
        let raw = t.to_bytes().unwrap();
        // Layout sanity.
        let kw = b"Title";
        assert_eq!(&raw[..kw.len()], kw);
        assert_eq!(raw[kw.len()], 0);
        assert_eq!(raw[kw.len() + 1], 0); // flag = 0 (uncompressed)
        assert_eq!(raw[kw.len() + 2], 0); // method = 0
        let back = Itxt::parse(&raw).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn itxt_roundtrip_compressed() {
        let t = Itxt {
            keyword: "Description".to_string(),
            compressed: true,
            language_tag: "en-US".to_string(),
            translated_keyword: String::new(),
            text: "A".repeat(1000),
        };
        let raw = t.to_bytes().unwrap();
        // Compressed payload should be way smaller than the 1000-byte
        // text plus headers.
        assert!(
            raw.len() < 200,
            "iTXt(compressed) of 1000 identical chars should compress to << 200 wire bytes (got {})",
            raw.len()
        );
        let back = Itxt::parse(&raw).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn itxt_roundtrip_with_translated_keyword_and_utf8() {
        // Translated keyword + non-ASCII UTF-8 text body (Japanese
        // characters above the BMP basic ASCII range).
        let t = Itxt {
            keyword: "Title".to_string(),
            compressed: false,
            language_tag: "ja".to_string(),
            translated_keyword: "題".to_string(),
            text: "日本語のテキスト".to_string(),
        };
        let raw = t.to_bytes().unwrap();
        let back = Itxt::parse(&raw).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn itxt_roundtrip_empty_language_and_translated_keyword() {
        // §11.3.3.4: "If the language tag is empty, the language is
        // unspecified." The translated keyword is also permitted to be
        // empty. Text may also be empty.
        let t = Itxt {
            keyword: "Title".to_string(),
            compressed: false,
            language_tag: String::new(),
            translated_keyword: String::new(),
            text: String::new(),
        };
        let raw = t.to_bytes().unwrap();
        let back = Itxt::parse(&raw).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn itxt_rejects_unknown_compression_flag() {
        // §11.3.3.4: compression flag is 0 or 1; any other value is
        // malformed.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"k");
        raw.push(0); // NUL after keyword
        raw.push(2); // bogus flag
        raw.push(0); // method
        raw.push(0); // NUL after lang
        raw.push(0); // NUL after translated kw
                     // text: empty
        assert!(Itxt::parse(&raw).is_err());
    }

    #[test]
    fn itxt_rejects_unknown_compression_method_when_compressed() {
        // When flag = 1 the method byte must be 0 (deflate).
        let mut raw = Vec::new();
        raw.extend_from_slice(b"k");
        raw.push(0);
        raw.push(1); // compressed
        raw.push(7); // bogus method
        raw.push(0); // NUL after lang
        raw.push(0); // NUL after translated kw
        raw.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert!(Itxt::parse(&raw).is_err());
    }

    #[test]
    fn itxt_ignores_method_byte_when_uncompressed() {
        // "For uncompressed text, encoders shall set the compression
        // method to 0, and decoders shall ignore it" (§11.3.3.4). A
        // bogus method byte alongside flag=0 must still parse.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"k");
        raw.push(0);
        raw.push(0); // flag = uncompressed
        raw.push(99); // bogus method — decoder ignores
        raw.push(0); // NUL after lang
        raw.push(0); // NUL after translated kw
        raw.extend_from_slice(b"hello");
        let parsed = Itxt::parse(&raw).unwrap();
        assert_eq!(parsed.text, "hello");
        assert!(!parsed.compressed);
    }

    #[test]
    fn itxt_rejects_missing_keyword_nul() {
        let raw = b"NoSeparator";
        assert!(Itxt::parse(raw).is_err());
    }

    #[test]
    fn itxt_rejects_missing_language_nul() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"k");
        raw.push(0);
        raw.push(0);
        raw.push(0);
        // No NUL after language tag.
        raw.extend_from_slice(b"en-US");
        assert!(Itxt::parse(&raw).is_err());
    }

    #[test]
    fn itxt_rejects_missing_translated_keyword_nul() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"k");
        raw.push(0);
        raw.push(0);
        raw.push(0);
        raw.push(0); // NUL after lang
        raw.extend_from_slice(b"NoNul"); // translated keyword without NUL
        assert!(Itxt::parse(&raw).is_err());
    }

    #[test]
    fn itxt_rejects_corrupted_zlib_when_compressed() {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"k");
        raw.push(0);
        raw.push(1); // compressed
        raw.push(0);
        raw.push(0); // NUL after lang
        raw.push(0); // NUL after translated kw
        raw.extend_from_slice(&[0xFF; 5]);
        assert!(Itxt::parse(&raw).is_err());
    }

    #[test]
    fn itxt_rejects_invalid_keyword() {
        let t = Itxt {
            keyword: " Leading".to_string(),
            compressed: false,
            language_tag: "en".to_string(),
            translated_keyword: String::new(),
            text: "x".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn itxt_rejects_non_ascii_language_tag_on_encode() {
        // BCP47 tags are ASCII by construction; reject non-ASCII.
        let t = Itxt {
            keyword: "k".to_string(),
            compressed: false,
            language_tag: "é".to_string(),
            translated_keyword: String::new(),
            text: "x".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn itxt_rejects_non_ascii_language_tag_on_decode() {
        // Same rule on the parse side.
        let mut raw = Vec::new();
        raw.extend_from_slice(b"k");
        raw.push(0);
        raw.push(0);
        raw.push(0);
        raw.push(0xC3); // first byte of UTF-8 é
        raw.push(0xA9);
        raw.push(0); // NUL after lang
        raw.push(0); // NUL after translated kw
                     // empty text
        assert!(Itxt::parse(&raw).is_err());
    }

    #[test]
    fn itxt_rejects_nul_in_translated_keyword_on_encode() {
        let t = Itxt {
            keyword: "k".to_string(),
            compressed: false,
            language_tag: "en".to_string(),
            translated_keyword: "bad\u{0000}null".to_string(),
            text: "x".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn itxt_rejects_nul_in_text_on_encode() {
        let t = Itxt {
            keyword: "k".to_string(),
            compressed: false,
            language_tag: "en".to_string(),
            translated_keyword: String::new(),
            text: "bad\u{0000}null".to_string(),
        };
        assert!(t.to_bytes().is_err());
    }

    #[test]
    fn itxt_rejects_nul_in_decompressed_text() {
        // Build a compressed iTXt whose inflated body contains a NUL —
        // the codec must reject it per §11.3.3.4 "neither shall
        // contain a zero byte".
        let payload = b"hello\0world";
        let compressed = compress_to_vec_zlib(payload, 6);
        let mut raw = Vec::new();
        raw.extend_from_slice(b"k");
        raw.push(0);
        raw.push(1); // compressed
        raw.push(0);
        raw.push(0); // NUL after lang
        raw.push(0); // NUL after translated kw
        raw.extend_from_slice(&compressed);
        assert!(Itxt::parse(&raw).is_err());
    }

    #[test]
    fn metadata_is_empty_accounts_for_itxts() {
        let mut m = PngMetadata::default();
        assert!(m.is_empty());
        m.itxts.push(Itxt {
            keyword: "k".to_string(),
            compressed: false,
            language_tag: String::new(),
            translated_keyword: String::new(),
            text: String::new(),
        });
        assert!(!m.is_empty());
    }
}
