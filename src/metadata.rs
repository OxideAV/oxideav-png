//! PNG ancillary metadata chunks — `sBIT`, `pHYs`, `tIME`, `bKGD`,
//! `hIST`, `eXIf`, `sRGB`.
//!
//! All but `eXIf` are short, fixed-layout chunks with no embedded
//! compression and no cross-chunk dependencies (with the single
//! exception that `hIST` must accompany a `PLTE` and match its length),
//! which makes them the natural first pass at round-tripping PNG
//! metadata through this codec. `eXIf` is variable-length but is treated
//! as an opaque TIFF-formatted blob: we validate only its byte-order
//! header and round-trip the bytes verbatim.
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
//! All chunks here are marked "Multiple OK? No" in the PNG spec; we
//! enforce that on parse — duplicates are an `InvalidData` error.

use crate::error::{PngError as Error, Result};

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

/// Bundle of metadata chunks that round-trip through the encoder.
///
/// Populated by [`crate::parse_metadata`] on decode and consumed by
/// [`crate::PngEncoderOptions::metadata`] on encode. Any `None` field is
/// simply omitted from the output PNG.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PngMetadata {
    pub sbit: Option<Sbit>,
    pub phys: Option<Phys>,
    pub time: Option<Time>,
    pub bkgd: Option<Bkgd>,
    pub hist: Option<Hist>,
    pub exif: Option<Exif>,
    pub srgb: Option<Srgb>,
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
}
