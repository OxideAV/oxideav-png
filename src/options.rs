//! Decode-side options: resource limits and strictness
//! ([`DecodeOptions`]). The encode-side [`crate::EncodeOptions`] lives
//! next to the encoder.

use crate::error::{PngError, Result};

/// Limits and strictness for [`crate::decode_with`] /
/// [`crate::decode_all_with`].
///
/// Every limit is checked against the header **before** any pixel
/// buffer is allocated, so a hostile `IHDR` fails with
/// [`PngError::LimitExceeded`] instead of committing memory. The
/// defaults are: no dimension / pixel-count limit, decoded planes
/// capped at [`DecodeOptions::DEFAULT_MAX_BYTES`] (1 GiB) per image,
/// `strict = false`.
///
/// `strict` selects how much of W3C PNG3's *should* language is
/// enforced on ancillary chunks:
///
/// * always (both modes): signature, every chunk CRC, critical-chunk
///   ordering (`IHDR` first, one `IHDR`, `PLTE` before `IDAT`,
///   consecutive `IDAT`, `IEND` last), IHDR validity (§11.2.1 Table 12),
///   `tRNS` validity, the exact filtered-stream size (decompression
///   bombs and truncations, §13.3), unknown critical chunks (§5.4);
/// * `strict = false` (default): a malformed or duplicated ancillary
///   colour / metadata chunk (`gAMA`, `cHRM`, `sRGB`, `cICP`, `iCCP`,
///   `eXIf`, XMP `iTXt`) is ignored and the pixels still decode, as
///   §13.1 recommends for ancillary chunks;
/// * `strict = true`: those chunks must parse and be unique, and the
///   §5.6 Table 7 ancillary ordering rules are enforced; any violation
///   is [`PngError::InvalidData`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecodeOptions {
    /// Reject images wider than this (pixels).
    pub max_width: Option<u32>,
    /// Reject images taller than this (pixels).
    pub max_height: Option<u32>,
    /// Reject images with more than this many pixels (`width ×
    /// height`).
    pub max_pixels: Option<u64>,
    /// Reject images whose decoded plane would exceed this many bytes
    /// (`stride × height` of the native layout, per image / frame).
    pub max_bytes: Option<u64>,
    /// Enforce the ancillary-chunk rules (see the type docs).
    pub strict: bool,
}

impl DecodeOptions {
    /// Default [`Self::max_bytes`]: 1 GiB of decoded plane per image.
    pub const DEFAULT_MAX_BYTES: u64 = 1 << 30;

    /// The defaults (see the type docs).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or lift with `None`) the width limit.
    pub fn with_max_width(mut self, max_width: impl Into<Option<u32>>) -> Self {
        self.max_width = max_width.into();
        self
    }

    /// Set (or lift with `None`) the height limit.
    pub fn with_max_height(mut self, max_height: impl Into<Option<u32>>) -> Self {
        self.max_height = max_height.into();
        self
    }

    /// Set (or lift with `None`) the pixel-count limit.
    pub fn with_max_pixels(mut self, max_pixels: impl Into<Option<u64>>) -> Self {
        self.max_pixels = max_pixels.into();
        self
    }

    /// Set (or lift with `None`) the decoded-bytes limit.
    pub fn with_max_bytes(mut self, max_bytes: impl Into<Option<u64>>) -> Self {
        self.max_bytes = max_bytes.into();
        self
    }

    /// Set strict mode.
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Lift every limit (`max_*` all `None`).
    pub fn unlimited(mut self) -> Self {
        self.max_width = None;
        self.max_height = None;
        self.max_pixels = None;
        self.max_bytes = None;
        self
    }

    /// Check a header's geometry against the limits. `bytes` is the
    /// decoded plane size the native layout implies.
    pub(crate) fn check(&self, width: u32, height: u32, bytes: u64) -> Result<()> {
        if let Some(m) = self.max_width {
            if width > m {
                return Err(PngError::limit(format!(
                    "PNG: width {width} exceeds max_width {m}"
                )));
            }
        }
        if let Some(m) = self.max_height {
            if height > m {
                return Err(PngError::limit(format!(
                    "PNG: height {height} exceeds max_height {m}"
                )));
            }
        }
        let pixels = u64::from(width) * u64::from(height);
        if let Some(m) = self.max_pixels {
            if pixels > m {
                return Err(PngError::limit(format!(
                    "PNG: {pixels} pixels exceed max_pixels {m}"
                )));
            }
        }
        if let Some(m) = self.max_bytes {
            if bytes > m {
                return Err(PngError::limit(format!(
                    "PNG: decoded plane of {bytes} bytes exceeds max_bytes {m}"
                )));
            }
        }
        Ok(())
    }
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            max_width: None,
            max_height: None,
            max_pixels: None,
            max_bytes: Some(Self::DEFAULT_MAX_BYTES),
            strict: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_fire_in_order() {
        let o = DecodeOptions::default()
            .with_max_width(10u32)
            .with_max_height(10u32)
            .with_max_pixels(50u64)
            .with_max_bytes(100u64);
        assert!(o.check(5, 5, 75).is_ok());
        assert!(matches!(o.check(11, 1, 1), Err(PngError::LimitExceeded(_))));
        assert!(matches!(o.check(1, 11, 1), Err(PngError::LimitExceeded(_))));
        assert!(matches!(o.check(8, 8, 1), Err(PngError::LimitExceeded(_))));
        assert!(matches!(
            o.check(5, 5, 101),
            Err(PngError::LimitExceeded(_))
        ));
        assert!(o.unlimited().check(u32::MAX, u32::MAX, u64::MAX).is_ok());
    }
}
