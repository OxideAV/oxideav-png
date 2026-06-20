//! sRGB transfer-function conversion — the IEC 61966-2-1 electro-optical
//! transfer function (EOTF) and its inverse, as referenced normatively by
//! W3C PNG 3rd Edition / ISO/IEC 15948 §11.3.2 ("sRGB" / the sRGB-default
//! colour space) and §13.
//!
//! ## What this is for
//!
//! A PNG carrying an `sRGB` chunk (or, by the §11.3.2 default-colour-space
//! convention, a chunk-less PNG a viewer chooses to treat as sRGB) stores
//! its samples *encoded* with the sRGB transfer function — perceptually
//! companded, not linear light. Several operations are only correct on
//! **linear-light** samples:
//!
//! * Alpha compositing / blending: "compositing … should be performed
//!   using linear … values" (W3C PNG3 §13). To composite an sRGB image
//!   over a background a viewer must first linearize, blend, then re-encode.
//! * Resampling / scaling, and any averaging of colour values.
//!
//! The codec proper leaves the wire samples verbatim. This module is the
//! opt-in transform a caller invokes when it actually needs linear-light
//! samples for one of those operations, and the inverse to get back to
//! storable / displayable sRGB bytes.
//!
//! ## The transform
//!
//! The forward direction (sRGB byte → linear) is the standard piecewise
//! EOTF of IEC 61966-2-1 evaluated for all 256 possible 8-bit inputs and
//! stored as a 256-entry Q16 table ([`crate::srgb_tables::SRGB_TO_LINEAR`],
//! `0..=65535` linear per 8-bit sRGB input). [`to_linear8`] is a single
//! table lookup.
//!
//! The inverse (linear → sRGB byte) uses the paired base / delta tables
//! ([`crate::srgb_tables::SRGB_BASE`] / [`SRGB_DELTA`]). The input is a
//! linear value in the range `0..=255*65535` (8-bit-scaled linear light);
//! the top 9 bits select a `(base, delta)` pair and the low 15 bits scale
//! the delta:
//!
//! ```text
//! i      = linear >> 15                       (0..=510, indexes 512-entry tables)
//! sRGB8  = (base[i] + ((linear & 0x7fff) * delta[i] >> 12)) >> 8
//! ```
//!
//! [`to_linear8`] then [`from_linear`] (with the linear value rescaled by
//! 255) reproduces the original sRGB byte exactly for every input — the two
//! tables are exact inverses at 8-bit precision.
//!
//! ## Alpha is never transferred
//!
//! "alpha is always represented linearly" (W3C PNG3 §13.16). The RGBA /
//! RGBA-palette helpers transfer only the R, G, B channels and pass the
//! alpha byte through untouched.

use crate::image::RgbaBitmap;
use crate::srgb_tables::{SRGB_BASE, SRGB_DELTA, SRGB_TO_LINEAR};

/// Maximum 8-bit-scaled linear value, `255 * 65535`. A linear sample passed
/// to [`from_linear`] must lie in `0..=MAX_SCALED_LINEAR`.
pub const MAX_SCALED_LINEAR: u32 = 255 * 65535;

/// Convert an 8-bit sRGB sample to a 16-bit linear value (Q16, `0..=65535`).
///
/// A single lookup into the IEC 61966-2-1 EOTF table
/// ([`crate::srgb_tables::SRGB_TO_LINEAR`]). `0u8 -> 0`, `255u8 -> 65535`.
#[inline]
pub fn to_linear8(srgb: u8) -> u16 {
    SRGB_TO_LINEAR[srgb as usize]
}

/// Convert an 8-bit-scaled linear value (`0..=255*65535`) back to an 8-bit
/// sRGB sample.
///
/// `linear` is linear light scaled so that an 8-bit-white input maps to
/// `255 * 65535`. Values above [`MAX_SCALED_LINEAR`] are clamped to the
/// table's last index, so an out-of-range caller value can never index out
/// of bounds. This is the exact inverse of [`to_linear8`] (with the linear
/// value rescaled by 255): `from_linear(to_linear8(s) as u32 * 255) == s`
/// for every `s`.
#[inline]
pub fn from_linear(linear: u32) -> u8 {
    let clamped = linear.min(MAX_SCALED_LINEAR);
    let i = (clamped >> 15) as usize;
    let base = SRGB_BASE[i] as u32;
    let delta = SRGB_DELTA[i] as u32;
    let frac = clamped & 0x7fff;
    (((base + ((frac * delta) >> 12)) >> 8) & 0xff) as u8
}

/// Convert an 8-bit sRGB sample to an 8-bit-scaled linear value
/// (`0..=255*65535`) — the [`to_linear8`] Q16 value multiplied back up by
/// 255 so it lands in the domain [`from_linear`] consumes.
///
/// This is the round-trip-exact pairing: `from_linear(to_scaled_linear8(s))
/// == s` for all 256 inputs.
#[inline]
pub fn to_scaled_linear8(srgb: u8) -> u32 {
    to_linear8(srgb) as u32 * 255
}

/// Linearize the R, G, B channels of an [`RgbaBitmap`] in place into 16-bit
/// linear-light values, returning a parallel `Vec<u16>` of `width * height *
/// 4` samples (R, G, B linear in `0..=65535`; A copied through unchanged in
/// `0..=255`).
///
/// The source bitmap is left untouched — linearization widens 8-bit samples
/// to 16-bit, so the result cannot live in the 8-bit buffer. "alpha is
/// always represented linearly" (§13.16), so the alpha byte is copied
/// straight into the `u16` lane (still `0..=255`) rather than passed through
/// the EOTF.
pub fn linearize_rgba(bitmap: &RgbaBitmap) -> Vec<u16> {
    let mut out = Vec::with_capacity(bitmap.data.len());
    for px in bitmap.data.chunks_exact(4) {
        out.push(to_linear8(px[0]));
        out.push(to_linear8(px[1]));
        out.push(to_linear8(px[2]));
        // Alpha is linear already (§13.16): pass the 0..=255 byte through.
        out.push(px[3] as u16);
    }
    out
}

/// Composite a foreground [`RgbaBitmap`] over an opaque sRGB background
/// colour using linear-light blending (W3C PNG3 §13), writing the result
/// back into `bitmap` as opaque (`alpha = 255`) sRGB pixels.
///
/// For each foreground pixel `(fr, fg, fb, fa)` over background `(br, bg,
/// bb)` the per-channel result is the standard source-over composite
/// performed in linear light:
///
/// ```text
/// out_linear = fg_linear * (fa / 255) + bg_linear * (1 - fa / 255)
/// ```
///
/// then re-encoded to sRGB. Foreground and background are linearized via
/// [`to_scaled_linear8`] (8-bit-scaled linear), blended with the 8-bit alpha
/// fraction, and the result passed back through [`from_linear`]. A fully
/// opaque foreground pixel (`fa == 255`) reproduces the foreground colour
/// exactly; a fully transparent one (`fa == 0`) reproduces the background.
///
/// The result is opaque everywhere (alpha forced to 255) — the image now
/// stands on a known background, which is exactly the §13 "displaying the
/// image against a background" operation.
pub fn composite_over_background(bitmap: &mut RgbaBitmap, bg: [u8; 3]) {
    // Background linearized once (8-bit-scaled linear); constant per image.
    let bg_lin = [
        to_scaled_linear8(bg[0]),
        to_scaled_linear8(bg[1]),
        to_scaled_linear8(bg[2]),
    ];
    for px in bitmap.data.chunks_exact_mut(4) {
        let fa = px[3] as u32; // 0..=255
        let inv = 255 - fa;
        for c in 0..3 {
            let fg_lin = to_scaled_linear8(px[c]);
            // Weighted sum in linear light. Both terms are <= 255*65535*255,
            // which fits in u64; the /255 returns to the 0..=255*65535 domain.
            let blended =
                ((fg_lin as u64 * fa as u64 + bg_lin[c] as u64 * inv as u64) / 255) as u32;
            px[c] = from_linear(blended);
        }
        px[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_match_iec_61966() {
        // The EOTF fixes the endpoints: 0 -> 0, 255 -> 65535 (full scale).
        assert_eq!(to_linear8(0), 0);
        assert_eq!(to_linear8(255), 65535);
        // First two steps follow the linear segment slope (1/12.92): the
        // libpng Q16 table records 20 and 40 (IEC 61966-2-1 §F).
        assert_eq!(to_linear8(1), 20);
        assert_eq!(to_linear8(2), 40);
    }

    #[test]
    fn monotonic_nondecreasing() {
        // The sRGB EOTF is strictly increasing; the integer table is at
        // least non-decreasing across all 256 inputs.
        for s in 1u16..=255 {
            assert!(
                to_linear8(s as u8) >= to_linear8((s - 1) as u8),
                "non-monotonic at {s}"
            );
        }
    }

    #[test]
    fn round_trip_exact_all_bytes() {
        // The defining property: linearize then de-linearize recovers every
        // 8-bit sRGB value exactly (the base/delta tables are exact inverses
        // of the EOTF table at 8-bit precision).
        for s in 0u16..=255 {
            let s = s as u8;
            let lin = to_scaled_linear8(s);
            assert_eq!(from_linear(lin), s, "round-trip failed at {s}");
        }
    }

    #[test]
    fn from_linear_clamps_overflow() {
        // A linear value past the table's domain saturates to white rather
        // than indexing out of bounds.
        assert_eq!(from_linear(MAX_SCALED_LINEAR), 255);
        assert_eq!(from_linear(u32::MAX), 255);
        assert_eq!(from_linear(0), 0);
    }

    #[test]
    fn linearize_passes_alpha_through() {
        let bmp = RgbaBitmap {
            width: 2,
            height: 1,
            data: vec![0, 128, 255, 7, 255, 0, 128, 200],
        };
        let lin = linearize_rgba(&bmp);
        // R,G,B linearized; alpha (4th lane) copied as the raw 0..=255 byte.
        assert_eq!(lin[0], to_linear8(0));
        assert_eq!(lin[1], to_linear8(128));
        assert_eq!(lin[2], to_linear8(255));
        assert_eq!(lin[3], 7);
        assert_eq!(lin[7], 200);
    }

    #[test]
    fn composite_opaque_foreground_is_identity() {
        // A fully opaque foreground over any background reproduces the
        // foreground colour exactly (alpha=255 => weight 1.0 on fg).
        let mut bmp = RgbaBitmap {
            width: 3,
            height: 1,
            data: vec![10, 90, 200, 255, 0, 0, 0, 255, 255, 255, 255, 255],
        };
        composite_over_background(&mut bmp, [33, 77, 111]);
        assert_eq!(bmp.data[0..4], [10, 90, 200, 255]);
        assert_eq!(bmp.data[4..8], [0, 0, 0, 255]);
        assert_eq!(bmp.data[8..12], [255, 255, 255, 255]);
    }

    #[test]
    fn composite_transparent_foreground_is_background() {
        // A fully transparent foreground (alpha=0) reproduces the
        // background colour exactly everywhere.
        let mut bmp = RgbaBitmap {
            width: 2,
            height: 1,
            data: vec![200, 50, 9, 0, 1, 2, 3, 0],
        };
        composite_over_background(&mut bmp, [64, 128, 192]);
        assert_eq!(bmp.data[0..4], [64, 128, 192, 255]);
        assert_eq!(bmp.data[4..8], [64, 128, 192, 255]);
    }

    #[test]
    fn composite_half_alpha_blends_in_linear_light() {
        // 50% white over black: in linear light the midpoint is 0.5 linear,
        // which re-encodes to ~188 sRGB (NOT the gamma-space 127/8). This is
        // the whole point of compositing in linear space (§13).
        let mut bmp = RgbaBitmap {
            width: 1,
            height: 1,
            data: vec![255, 255, 255, 128],
        };
        composite_over_background(&mut bmp, [0, 0, 0]);
        // 128/255 of full-white linear, re-encoded. Must be well above the
        // naive sRGB-space average (128) — confirms linear-light blending.
        let v = bmp.data[0];
        assert!(v > 180 && v < 195, "linear-light half-white was {v}");
        assert_eq!(bmp.data[3], 255);
    }
}
