//! Decoder gamma handling — W3C PNG 3rd Edition §13.13 ("Decoder gamma
//! handling") / RFC 2083 §10.5.
//!
//! A `gAMA` chunk records the *file gamma* — the exponent that relates a
//! datastream sample to the desired display output intensity (RFC 2083
//! §4.2.3: the stored integer is "the image gamma times 100000"). To
//! reproduce correct tone, a viewer must undo that encoding and re-apply
//! the gamma of the display it is targeting. The codec proper leaves the
//! samples verbatim (it only round-trips the raw `gAMA` integer); this
//! module is the opt-in transform a caller invokes when it actually wants
//! gamma-corrected pixels for a known display.
//!
//! ## The §13.13 formula
//!
//! For an 8-bit sample `s` in `0..=255`, with `MAX = 255`:
//!
//! ```text
//! sample          = s / MAX                                   (0.0 ..= 1.0)
//! display_input    = sample ^ decoding_exponent
//! framebuf_sample  = floor(display_input * MAX + 0.5)
//! ```
//!
//! where the single merged exponent (§13.13) is
//!
//! ```text
//! decoding_exponent = user_exponent / (gamma_from_file * display_exponent)
//! ```
//!
//! `user_exponent` lets a caller darken (`> 1`) or lighten (`< 1`) the
//! mid-tones; it "defaults to 1.0" (§13.13). `display_exponent` describes
//! the target display's transfer function; "A display exponent of 2.2
//! should be used unless detailed calibration measurements are available"
//! (§13.13), so [`GammaParams::default`] picks `2.2`. `gamma_from_file` is
//! the `gAMA` value (e.g. `0.45455` for the common `1/2.2` encoding).
//!
//! Because there are only 256 possible 8-bit sample values, the transform
//! is realised as a 256-entry lookup table ("This requires only 256
//! calculations per image (for 8-bit accuracy), not one or three
//! calculations per pixel", §13.13) and then applied per channel.
//!
//! ## Alpha is never gamma-corrected
//!
//! "Gamma correction is not applied to the alpha channel … alpha is
//! always represented linearly" (W3C PNG3 §13.16). [`apply_to_rgba`]
//! therefore transforms only the R, G, B channels of an [`RgbaBitmap`]
//! and copies the alpha byte through untouched.
//!
//! ## Zero file gamma
//!
//! "A gAMA chunk containing zero is meaningless … Decoders should ignore
//! it" (§13.13). [`GammaParams::from_gama`] treats a zero `gAMA` as
//! "no usable file gamma" and returns `None`, so a caller does not divide
//! by zero; the caller then keeps the samples unchanged (or supplies a
//! default file gamma of its own choosing).

use crate::image::RgbaBitmap;
use crate::metadata::Gama;

/// Parameters for the §13.13 decoder gamma transform.
///
/// All three exponents are spec quantities:
/// * `file_gamma` — the `gAMA` value (image gamma), e.g. `0.45455`.
/// * `display_exponent` — the target display's transfer-function
///   exponent; `2.2` by default per §13.13.
/// * `user_exponent` — the optional viewer brightness control; `1.0`
///   (no adjustment) by default per §13.13.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GammaParams {
    /// The image gamma from the `gAMA` chunk (a positive float).
    pub file_gamma: f64,
    /// The display system's transfer-function exponent (`2.2` default).
    pub display_exponent: f64,
    /// The user brightness exponent (`1.0` default = no adjustment).
    pub user_exponent: f64,
}

impl Default for GammaParams {
    /// `display_exponent = 2.2`, `user_exponent = 1.0` (§13.13
    /// recommendations) and `file_gamma = 1/2.2 = 0.454545…` — the
    /// "reasonable default" the spec names for an image whose gamma is
    /// otherwise unknown (§13.13: when `gAMA`/`sRGB`/`iCCP` are all
    /// absent the viewer chooses a likely default).
    fn default() -> Self {
        Self {
            file_gamma: 1.0 / 2.2,
            display_exponent: 2.2,
            user_exponent: 1.0,
        }
    }
}

impl GammaParams {
    /// Build parameters from a parsed `gAMA` chunk, keeping the default
    /// `display_exponent` (2.2) and `user_exponent` (1.0).
    ///
    /// Returns `None` for a zero `gAMA` value: "A gAMA chunk containing
    /// zero is meaningless … Decoders should ignore it" (§13.13). A
    /// `None` result is the caller's cue to leave the samples unchanged
    /// rather than attempt a transform with no usable file gamma.
    pub fn from_gama(gama: Gama) -> Option<Self> {
        if gama.gamma_times_100000 == 0 {
            return None;
        }
        Some(Self {
            file_gamma: gama.gamma(),
            ..Self::default()
        })
    }

    /// The merged §13.13 decoding exponent:
    /// `user_exponent / (file_gamma * display_exponent)`.
    ///
    /// Returns `None` if either denominator factor is non-positive
    /// (a `pow` with a non-positive base/exponent combination would be
    /// meaningless, and a zero `file_gamma` would divide by zero).
    pub fn decoding_exponent(&self) -> Option<f64> {
        let denom = self.file_gamma * self.display_exponent;
        if denom <= 0.0 || !denom.is_finite() {
            return None;
        }
        let exp = self.user_exponent / denom;
        if exp.is_finite() {
            Some(exp)
        } else {
            None
        }
    }

    /// Build the 256-entry 8-bit gamma-correction lookup table.
    ///
    /// `table[s]` is the framebuffer value the §13.13 formula assigns to
    /// input sample `s`:
    ///
    /// ```text
    /// table[s] = floor((s / 255) ^ decoding_exponent * 255 + 0.5)
    /// ```
    ///
    /// "Zero raised to any positive power is zero" (§13.13), so
    /// `table[0] == 0` and `table[255] == 255` for any positive exponent.
    ///
    /// Returns `None` when [`Self::decoding_exponent`] is undefined; the
    /// caller then performs no transform.
    pub fn build_lut(&self) -> Option<[u8; 256]> {
        let exp = self.decoding_exponent()?;
        let mut lut = [0u8; 256];
        for (s, slot) in lut.iter_mut().enumerate() {
            // First line of §13.13: normalize to 0.0..=1.0.
            let sample = s as f64 / 255.0;
            // Merged second/third line: sample ^ decoding_exponent.
            let display_input = sample.powf(exp);
            // Fourth line: floor(display_input * MAX + 0.5).
            let fb = (display_input * 255.0 + 0.5).floor();
            // Defensive clamp; for a valid exponent fb is already 0..=255.
            *slot = fb.clamp(0.0, 255.0) as u8;
        }
        Some(lut)
    }
}

/// Apply §13.13 decoder gamma correction to an [`RgbaBitmap`] in place.
///
/// The R, G and B byte of every pixel is replaced by `lut[old]`; the
/// alpha byte is left untouched ("Gamma correction is not applied to the
/// alpha channel … alpha is always represented linearly", W3C PNG3
/// §13.16). For colour images "the entire calculation is performed
/// separately for R, G, and B values" (§13.13) — the same single LUT
/// drives all three because the transform is per-sample, not per-channel.
///
/// Returns `false` (and leaves the bitmap untouched) when `params` does
/// not yield a usable transform (zero / non-finite file gamma); `true`
/// when the correction was applied.
pub fn apply_to_rgba(bitmap: &mut RgbaBitmap, params: GammaParams) -> bool {
    let Some(lut) = params.build_lut() else {
        return false;
    };
    for px in bitmap.data.chunks_exact_mut(4) {
        px[0] = lut[px[0] as usize];
        px[1] = lut[px[1] as usize];
        px[2] = lut[px[2] as usize];
        // px[3] (alpha) deliberately untouched — §13.16.
    }
    true
}

/// Apply §13.13 correction using the file gamma from a `gAMA` chunk and
/// the default display (2.2) / user (1.0) exponents.
///
/// Convenience wrapper around [`GammaParams::from_gama`] +
/// [`apply_to_rgba`]. Returns `false` (no change) for a zero / absent
/// usable file gamma.
pub fn apply_gama_to_rgba(bitmap: &mut RgbaBitmap, gama: Gama) -> bool {
    match GammaParams::from_gama(gama) {
        Some(params) => apply_to_rgba(bitmap, params),
        None => false,
    }
}

/// Apply §13.13 decoder gamma correction to an indexed image's palette
/// in place — the spec's explicit "one-time correction of the palette"
/// optimisation.
///
/// "For an indexed-color image, a one-time correction of the palette is
/// sufficient, unless the image uses transparency and is being displayed
/// against a nonuniform background" (W3C PNG3 §13.13). Rather than gamma-
/// correcting every output pixel, a viewer corrects the (typically much
/// smaller) palette once and then resolves indices into the already-
/// corrected entries.
///
/// `palette` is the [`crate::image::PngImage::palette`] layout for a
/// `Pal8` image: a run of `PLTE` `R, G, B` triples optionally followed by
/// a `tRNS` alpha tail. `plte_len` is the byte length of the `PLTE`
/// portion (a multiple of 3); bytes at and beyond `plte_len` are the
/// linear `tRNS` alpha values and are left untouched ("alpha is always
/// represented linearly", §13.16). A `plte_len` that is not a multiple of
/// 3, or that runs past the buffer, is clamped to the largest whole-
/// triple prefix that fits so a malformed length can never index out of
/// bounds.
///
/// Returns `false` (and leaves the palette untouched) when `params` does
/// not yield a usable transform (zero / non-finite file gamma); `true`
/// when the correction was applied.
pub fn apply_to_palette(palette: &mut [u8], plte_len: usize, params: GammaParams) -> bool {
    let Some(lut) = params.build_lut() else {
        return false;
    };
    // Clamp to the largest whole-triple prefix that actually fits, so a
    // malformed plte_len (not a multiple of 3, or past the buffer) is
    // defensive rather than a panic. tRNS alpha bytes live at/after
    // plte_len and are §13.16-linear: never gamma-corrected.
    let rgb_bytes = plte_len.min(palette.len());
    let triples_end = rgb_bytes - (rgb_bytes % 3);
    for entry in palette[..triples_end].chunks_exact_mut(3) {
        entry[0] = lut[entry[0] as usize];
        entry[1] = lut[entry[1] as usize];
        entry[2] = lut[entry[2] as usize];
    }
    true
}

/// Apply §13.13 palette correction using the file gamma from a `gAMA`
/// chunk and the default display (2.2) / user (1.0) exponents.
///
/// Convenience wrapper around [`GammaParams::from_gama`] +
/// [`apply_to_palette`]. Returns `false` (no change) for a zero / absent
/// usable file gamma.
pub fn apply_gama_to_palette(palette: &mut [u8], plte_len: usize, gama: Gama) -> bool {
    match GammaParams::from_gama(gama) {
        Some(params) => apply_to_palette(palette, plte_len, params),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(pixels: &[[u8; 4]]) -> RgbaBitmap {
        let mut data = Vec::with_capacity(pixels.len() * 4);
        for p in pixels {
            data.extend_from_slice(p);
        }
        RgbaBitmap {
            width: pixels.len() as u32,
            height: 1,
            data,
        }
    }

    #[test]
    fn identity_exponent_is_a_no_op() {
        // file_gamma * display_exponent == user_exponent  =>  exponent 1.
        let params = GammaParams {
            file_gamma: 0.5,
            display_exponent: 2.0,
            user_exponent: 1.0,
        };
        assert_eq!(params.decoding_exponent(), Some(1.0));
        let lut = params.build_lut().unwrap();
        for (s, &v) in lut.iter().enumerate() {
            assert_eq!(v as usize, s, "identity LUT should map s -> s");
        }
    }

    #[test]
    fn endpoints_are_fixed_for_any_positive_exponent() {
        // §13.13: "Zero raised to any positive power is zero"; and 1^e == 1.
        for &(fg, de, ue) in &[
            (0.45455, 2.2, 1.0),
            (1.0, 2.2, 1.0),
            (0.5, 1.0, 0.8),
            (0.45455, 2.2, 1.5),
        ] {
            let params = GammaParams {
                file_gamma: fg,
                display_exponent: de,
                user_exponent: ue,
            };
            let lut = params.build_lut().unwrap();
            assert_eq!(lut[0], 0, "0 maps to 0");
            assert_eq!(lut[255], 255, "255 maps to 255");
        }
    }

    #[test]
    fn decoding_exponent_matches_spec_formula() {
        // §13.13: decoding_exponent = user / (file_gamma * display).
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let expected = 1.0 / (0.45455 * 2.2);
        let got = params.decoding_exponent().unwrap();
        assert!((got - expected).abs() < 1e-12);
    }

    #[test]
    fn sample_value_matches_spec_rounding() {
        // Hand-compute one mid-tone against the §13.13 formula:
        // floor((s/255)^e * 255 + 0.5).
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let e = params.decoding_exponent().unwrap();
        let lut = params.build_lut().unwrap();
        for s in [1usize, 50, 128, 200, 254] {
            let expected = (((s as f64 / 255.0).powf(e)) * 255.0 + 0.5).floor() as u8;
            assert_eq!(lut[s], expected, "mismatch at s={s}");
        }
    }

    #[test]
    fn alpha_channel_is_never_touched() {
        // §13.16: gamma is not applied to alpha.
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let mut bm = bitmap(&[[10, 128, 250, 7], [0, 255, 64, 200]]);
        let alphas: Vec<u8> = bm.data.chunks_exact(4).map(|c| c[3]).collect();
        assert!(apply_to_rgba(&mut bm, params));
        let new_alphas: Vec<u8> = bm.data.chunks_exact(4).map(|c| c[3]).collect();
        assert_eq!(alphas, new_alphas, "alpha bytes must survive verbatim");
    }

    #[test]
    fn rgb_channels_use_the_same_lut() {
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let lut = params.build_lut().unwrap();
        let mut bm = bitmap(&[[64, 64, 64, 255]]);
        assert!(apply_to_rgba(&mut bm, params));
        // A grey input pixel stays grey: identical samples map identically.
        assert_eq!(bm.data[0], lut[64]);
        assert_eq!(bm.data[1], lut[64]);
        assert_eq!(bm.data[2], lut[64]);
        assert_eq!(bm.data[0], bm.data[1]);
        assert_eq!(bm.data[1], bm.data[2]);
    }

    #[test]
    fn darkening_vs_lightening_via_user_exponent() {
        // user_exponent > 1 darkens mid-tones, < 1 lightens them (§13.13).
        let base = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let dark = GammaParams {
            user_exponent: 1.5,
            ..base
        };
        let light = GammaParams {
            user_exponent: 0.7,
            ..base
        };
        let mid = 128usize;
        let b = base.build_lut().unwrap()[mid];
        let d = dark.build_lut().unwrap()[mid];
        let l = light.build_lut().unwrap()[mid];
        assert!(d < b, "user_exponent > 1 darkens mid-tone ({d} !< {b})");
        assert!(l > b, "user_exponent < 1 lightens mid-tone ({l} !> {b})");
    }

    #[test]
    fn zero_gama_is_ignored() {
        // §13.13: a zero gAMA is meaningless; decoders should ignore it.
        let gama = Gama {
            gamma_times_100000: 0,
        };
        assert_eq!(GammaParams::from_gama(gama), None);
        let mut bm = bitmap(&[[10, 128, 250, 255]]);
        let before = bm.data.clone();
        assert!(
            !apply_gama_to_rgba(&mut bm, gama),
            "zero gAMA: no transform"
        );
        assert_eq!(bm.data, before, "samples unchanged for zero gAMA");
    }

    #[test]
    fn palette_correction_matches_the_rgba_lut() {
        // §13.13: "a one-time correction of the palette is sufficient".
        // Every PLTE triple must map through the same LUT the full-colour
        // path uses, channel by channel.
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let lut = params.build_lut().unwrap();
        // Three PLTE entries, no tRNS tail.
        let mut palette = vec![0, 50, 128, 200, 255, 10, 64, 192, 7];
        assert!(apply_to_palette(&mut palette, 9, params));
        let expected: Vec<u8> = [0, 50, 128, 200, 255, 10, 64, 192, 7]
            .iter()
            .map(|&b| lut[b as usize])
            .collect();
        assert_eq!(palette, expected);
    }

    #[test]
    fn palette_trns_alpha_tail_is_never_corrected() {
        // §13.16: alpha is always linear. The tRNS bytes at/after plte_len
        // must survive a palette gamma pass byte-for-byte.
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let lut = params.build_lut().unwrap();
        // 2 PLTE triples (6 bytes) + a 2-byte tRNS alpha tail.
        let mut palette = vec![10, 20, 30, 40, 50, 60, 128, 200];
        assert!(apply_to_palette(&mut palette, 6, params));
        // RGB corrected …
        assert_eq!(
            &palette[..6],
            &[lut[10], lut[20], lut[30], lut[40], lut[50], lut[60]]
        );
        // … alpha tail verbatim.
        assert_eq!(&palette[6..], &[128, 200]);
    }

    #[test]
    fn palette_malformed_len_is_clamped_not_panicked() {
        let params = GammaParams::default();
        // plte_len past the buffer + not a multiple of 3: must clamp to the
        // largest whole-triple prefix (3 bytes here) without panicking.
        let mut palette = vec![1, 2, 3, 4, 5];
        assert!(apply_to_palette(&mut palette, 100, params));
        let lut = params.build_lut().unwrap();
        // First triple corrected; the trailing partial pair (4,5) left as-is.
        assert_eq!(&palette[..3], &[lut[1], lut[2], lut[3]]);
        assert_eq!(&palette[3..], &[4, 5]);
    }

    #[test]
    fn palette_zero_gama_leaves_palette_unchanged() {
        let gama = Gama {
            gamma_times_100000: 0,
        };
        let mut palette = vec![10, 128, 250, 7];
        let before = palette.clone();
        assert!(!apply_gama_to_palette(&mut palette, 3, gama));
        assert_eq!(palette, before);
    }

    #[test]
    fn palette_identity_exponent_is_a_no_op() {
        let params = GammaParams {
            file_gamma: 0.5,
            display_exponent: 2.0,
            user_exponent: 1.0,
        };
        let mut palette = vec![0, 50, 128, 200, 255, 17];
        let before = palette.clone();
        assert!(apply_to_palette(&mut palette, 6, params));
        assert_eq!(
            palette, before,
            "identity exponent must not alter the palette"
        );
    }

    #[test]
    fn from_gama_uses_chunk_value() {
        // 45455 == 1/2.2 (the example value in §11.3.2.2).
        let gama = Gama {
            gamma_times_100000: 45455,
        };
        let params = GammaParams::from_gama(gama).unwrap();
        assert!((params.file_gamma - 0.45455).abs() < 1e-9);
        assert_eq!(params.display_exponent, 2.2);
        assert_eq!(params.user_exponent, 1.0);
    }

    #[test]
    fn non_positive_factors_yield_no_transform() {
        let bad = [
            GammaParams {
                file_gamma: 0.0,
                display_exponent: 2.2,
                user_exponent: 1.0,
            },
            GammaParams {
                file_gamma: 0.45455,
                display_exponent: 0.0,
                user_exponent: 1.0,
            },
        ];
        for p in bad {
            assert_eq!(p.decoding_exponent(), None);
            assert_eq!(p.build_lut(), None);
            let mut bm = bitmap(&[[1, 2, 3, 4]]);
            let before = bm.data.clone();
            assert!(!apply_to_rgba(&mut bm, p));
            assert_eq!(bm.data, before);
        }
    }
}
