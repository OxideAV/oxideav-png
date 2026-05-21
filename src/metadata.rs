//! PNG ancillary metadata chunks — `sBIT`, `pHYs`, `tIME`.
//!
//! All three are short, fixed-layout chunks with no embedded compression and
//! no cross-chunk dependencies, which makes them the natural first pass at
//! round-tripping PNG metadata through this codec.
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
//! All three chunks may freely appear in the file; the wider PNG spec only
//! says "may appear at most once". We enforce that on parse — duplicates
//! are an `InvalidData` error.

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
}

impl PngMetadata {
    /// True when no metadata chunks are populated. Used by the encoder
    /// as a quick "nothing to emit" check.
    pub fn is_empty(&self) -> bool {
        self.sbit.is_none() && self.phys.is_none() && self.time.is_none()
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
    }
}
