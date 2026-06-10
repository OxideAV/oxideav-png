//! APNG (Animated PNG) support — `acTL`, `fcTL`, `fdAT` chunks.
//!
//! Spec: <https://wiki.mozilla.org/APNG_Specification>.
//!
//! Layout (simplified): after `IHDR`, an `acTL` chunk announces the animation
//! (`num_frames` + `num_plays` = loop count, 0 = infinite). Each subsequent
//! animation frame is bracketed by an `fcTL` chunk (frame position + delay +
//! disposal/blend modes) and one or more data chunks (`IDAT` for the first
//! frame if it's also the default image, otherwise `fdAT`). Every `fcTL` and
//! `fdAT` starts with a 4-byte sequence number that monotonically increases
//! across the file.

use crate::error::{PngError as Error, Result};

/// `acTL` — animation control chunk (8 bytes total).
#[derive(Clone, Copy, Debug)]
pub struct Actl {
    pub num_frames: u32,
    /// 0 → loop forever.
    pub num_plays: u32,
}

impl Actl {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 8 {
            return Err(Error::invalid(format!(
                "PNG acTL: expected 8 bytes, got {}",
                data.len()
            )));
        }
        Ok(Self {
            num_frames: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
            num_plays: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        })
    }

    pub fn to_bytes(&self) -> [u8; 8] {
        let mut out = [0u8; 8];
        out[0..4].copy_from_slice(&self.num_frames.to_be_bytes());
        out[4..8].copy_from_slice(&self.num_plays.to_be_bytes());
        out
    }
}

/// Disposal method after a frame is displayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposal {
    /// No disposal — next frame drawn on top of this one.
    None = 0,
    /// Clear the frame region to fully transparent black before the next frame.
    Background = 1,
    /// Restore the frame region to its state before this frame was drawn.
    Previous = 2,
}

/// Blend mode for this frame over the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    /// Overwrite (all pixels including alpha replace the canvas).
    Source = 0,
    /// Standard "over" alpha blending.
    Over = 1,
}

/// `fcTL` — frame control chunk (26 bytes payload).
#[derive(Clone, Copy, Debug)]
pub struct Fctl {
    pub sequence_number: u32,
    pub width: u32,
    pub height: u32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub delay_num: u16,
    pub delay_den: u16,
    pub dispose_op: Disposal,
    pub blend_op: Blend,
}

impl Fctl {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 26 {
            return Err(Error::invalid(format!(
                "PNG fcTL: expected 26 bytes, got {}",
                data.len()
            )));
        }
        let sequence_number = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let width = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let height = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
        let x_offset = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
        let y_offset = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let delay_num = u16::from_be_bytes([data[20], data[21]]);
        let delay_den = u16::from_be_bytes([data[22], data[23]]);
        let dispose_op = match data[24] {
            0 => Disposal::None,
            1 => Disposal::Background,
            2 => Disposal::Previous,
            o => return Err(Error::invalid(format!("PNG fcTL: bad dispose_op {o}"))),
        };
        let blend_op = match data[25] {
            0 => Blend::Source,
            1 => Blend::Over,
            o => return Err(Error::invalid(format!("PNG fcTL: bad blend_op {o}"))),
        };
        // delay_den == 0 means 100 per the spec.
        let delay_den = if delay_den == 0 { 100 } else { delay_den };
        Ok(Self {
            sequence_number,
            width,
            height,
            x_offset,
            y_offset,
            delay_num,
            delay_den,
            dispose_op,
            blend_op,
        })
    }

    pub fn to_bytes(&self) -> [u8; 26] {
        let mut out = [0u8; 26];
        out[0..4].copy_from_slice(&self.sequence_number.to_be_bytes());
        out[4..8].copy_from_slice(&self.width.to_be_bytes());
        out[8..12].copy_from_slice(&self.height.to_be_bytes());
        out[12..16].copy_from_slice(&self.x_offset.to_be_bytes());
        out[16..20].copy_from_slice(&self.y_offset.to_be_bytes());
        out[20..22].copy_from_slice(&self.delay_num.to_be_bytes());
        out[22..24].copy_from_slice(&self.delay_den.to_be_bytes());
        out[24] = self.dispose_op as u8;
        out[25] = self.blend_op as u8;
        out
    }

    /// Enforce the W3C PNG 3rd Edition §11.3.6.1 frame-region constraints
    /// against the canvas (IHDR) dimensions.
    ///
    /// Two rules from the `fcTL` description apply:
    ///
    /// * "`width` and `height` … must be greater than zero." A zero-extent
    ///   frame names no pixels and cannot be rendered.
    /// * "This region may not fall outside of the default image; thus
    ///   `x_offset` plus `width` must not be greater than the IHDR `width`;
    ///   similarly `y_offset` plus `height` must not be greater than the
    ///   IHDR `height`."
    ///
    /// The two sums are computed in `u64` so the `u32 + u32` addition cannot
    /// wrap a frame whose offset/extent are individually in range but whose
    /// sum exceeds `u32::MAX` past the canvas bound.
    pub fn validate_within_canvas(&self, canvas_width: u32, canvas_height: u32) -> Result<()> {
        if self.width == 0 || self.height == 0 {
            return Err(Error::invalid(format!(
                "PNG fcTL: frame width/height must be greater than zero (got {}x{})",
                self.width, self.height
            )));
        }
        let right = self.x_offset as u64 + self.width as u64;
        if right > canvas_width as u64 {
            return Err(Error::invalid(format!(
                "PNG fcTL: x_offset ({}) + width ({}) = {} exceeds canvas width {}",
                self.x_offset, self.width, right, canvas_width
            )));
        }
        let bottom = self.y_offset as u64 + self.height as u64;
        if bottom > canvas_height as u64 {
            return Err(Error::invalid(format!(
                "PNG fcTL: y_offset ({}) + height ({}) = {} exceeds canvas height {}",
                self.y_offset, self.height, bottom, canvas_height
            )));
        }
        Ok(())
    }

    /// Delay converted to centiseconds (1/100s), matching our APNG time base.
    pub fn delay_centiseconds(&self) -> u32 {
        // delay_num / delay_den seconds → * 100.
        let num = self.delay_num as u64 * 100;
        (num / self.delay_den.max(1) as u64) as u32
    }
}

/// Parse a single `fdAT` chunk. Returns `(sequence_number, compressed_bytes)`.
/// The first 4 bytes are a sequence number; the rest is raw IDAT-equivalent
/// compressed data.
pub fn parse_fdat(data: &[u8]) -> Result<(u32, &[u8])> {
    if data.len() < 4 {
        return Err(Error::invalid("PNG fdAT: too short for sequence number"));
    }
    let seq = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    Ok((seq, &data[4..]))
}

/// Build an `fdAT` chunk payload (sequence_number + compressed data).
pub fn build_fdat(seq: u32, compressed: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + compressed.len());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(compressed);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fctl_roundtrip() {
        let f = Fctl {
            sequence_number: 5,
            width: 16,
            height: 8,
            x_offset: 0,
            y_offset: 0,
            delay_num: 25,
            delay_den: 100,
            dispose_op: Disposal::Background,
            blend_op: Blend::Over,
        };
        let bytes = f.to_bytes();
        let back = Fctl::parse(&bytes).unwrap();
        assert_eq!(back.sequence_number, 5);
        assert_eq!(back.delay_num, 25);
        assert_eq!(back.delay_den, 100);
        assert_eq!(back.dispose_op, Disposal::Background);
        assert_eq!(back.blend_op, Blend::Over);
        assert_eq!(f.delay_centiseconds(), 25);
    }

    /// Build an `fcTL` with the given region and an otherwise inert body so
    /// the region-bounds tests below read at a glance.
    fn region_fctl(x: u32, y: u32, w: u32, h: u32) -> Fctl {
        Fctl {
            sequence_number: 0,
            width: w,
            height: h,
            x_offset: x,
            y_offset: y,
            delay_num: 1,
            delay_den: 100,
            dispose_op: Disposal::None,
            blend_op: Blend::Source,
        }
    }

    #[test]
    fn validate_accepts_frame_flush_with_canvas() {
        // A frame whose region exactly fills the canvas is in bounds:
        // x_offset + width == IHDR width, y_offset + height == IHDR height.
        assert!(region_fctl(0, 0, 100, 60)
            .validate_within_canvas(100, 60)
            .is_ok());
        // A strictly-inside sub-region with non-zero offsets is fine too.
        assert!(region_fctl(10, 20, 80, 30)
            .validate_within_canvas(100, 60)
            .is_ok());
    }

    #[test]
    fn validate_rejects_zero_extent() {
        // §11.3.6.1: "width and height … must be greater than zero."
        assert!(region_fctl(0, 0, 0, 10)
            .validate_within_canvas(100, 60)
            .is_err());
        assert!(region_fctl(0, 0, 10, 0)
            .validate_within_canvas(100, 60)
            .is_err());
    }

    #[test]
    fn validate_rejects_region_outside_canvas() {
        // x_offset + width must not exceed IHDR width.
        assert!(region_fctl(50, 0, 60, 10)
            .validate_within_canvas(100, 60)
            .is_err());
        // y_offset + height must not exceed IHDR height.
        assert!(region_fctl(0, 40, 10, 30)
            .validate_within_canvas(100, 60)
            .is_err());
        // Off-by-one over the right edge is rejected (101 > 100).
        assert!(region_fctl(1, 0, 100, 10)
            .validate_within_canvas(100, 60)
            .is_err());
    }

    #[test]
    fn validate_offset_plus_extent_cannot_wrap() {
        // u32 addition would wrap to a tiny value; the u64 sum keeps the
        // out-of-bounds region rejected rather than slipping past the gate.
        assert!(region_fctl(u32::MAX, 0, 2, 1)
            .validate_within_canvas(u32::MAX, 1)
            .is_err());
    }

    #[test]
    fn actl_roundtrip() {
        let a = Actl {
            num_frames: 7,
            num_plays: 0,
        };
        let bytes = a.to_bytes();
        let back = Actl::parse(&bytes).unwrap();
        assert_eq!(back.num_frames, 7);
        assert_eq!(back.num_plays, 0);
    }
}
