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
//! ## Bit-depth-general formula and the 16-bit path
//!
//! The §13.13 formula is written in terms of the sample depth, not a
//! fixed 8-bit width: `sample = integer_sample / (2^sampledepth - 1.0)`
//! and `framebuf_sample = floor(display_input * MAX_FRAMEBUF_SAMPLE +
//! 0.5)`, where `MAX_FRAMEBUF_SAMPLE` is "the maximum value of a frame
//! buffer sample (255 for 8-bit, 31 for 5-bit, etc)". The 8-bit
//! [`GammaParams::build_lut`] specialises `MAX = 255`; [`GammaParams::
//! build_lut16`] specialises `MAX = 65535` for 16-bit samples and
//! [`apply_to_png16`] runs it across the colour channels of a
//! [`PngImage`]'s `Gray16Le` / `Rgb48Le` / `Rgba64Le` little-endian
//! buffer. The same merged decoding exponent drives both widths — the
//! transform is per-sample, so only the normalisation and frame-buffer
//! denominators change with the depth.
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

use crate::image::{PngImage, PngPixelFormat, RgbaBitmap};
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

    /// Build the 65536-entry 16-bit gamma-correction lookup table — the
    /// §13.13 transform specialised to a 16-bit sample depth.
    ///
    /// The §13.13 formula is written for an arbitrary sample depth:
    /// `sample = integer_sample / (2^sampledepth - 1.0)` and
    /// `framebuf_sample = floor(display_input * MAX_FRAMEBUF_SAMPLE +
    /// 0.5)`. For 16-bit samples `MAX_FRAMEBUF_SAMPLE = 65535`, so:
    ///
    /// ```text
    /// table[s] = floor((s / 65535) ^ decoding_exponent * 65535 + 0.5)
    /// ```
    ///
    /// As with the 8-bit table the endpoints are fixed for any positive
    /// exponent: `table[0] == 0` ("Zero raised to any positive power is
    /// zero", §13.13) and `table[65535] == 65535`. The table is boxed to
    /// the heap (128 KiB) so it is never materialised on the stack.
    ///
    /// Returns `None` when [`Self::decoding_exponent`] is undefined; the
    /// caller then performs no transform.
    pub fn build_lut16(&self) -> Option<Box<[u16; 65536]>> {
        let exp = self.decoding_exponent()?;
        let mut lut = vec![0u16; 65536].into_boxed_slice();
        for (s, slot) in lut.iter_mut().enumerate() {
            // §13.13 line 1: normalize to 0.0..=1.0 over the 16-bit range.
            let sample = s as f64 / 65535.0;
            // Merged lines 2/3: sample ^ decoding_exponent.
            let display_input = sample.powf(exp);
            // Line 4: floor(display_input * MAX_FRAMEBUF_SAMPLE + 0.5).
            let fb = (display_input * 65535.0 + 0.5).floor();
            // Defensive clamp; for a valid exponent fb is already 0..=65535.
            *slot = fb.clamp(0.0, 65535.0) as u16;
        }
        // The boxed slice is exactly 65536 long, so the conversion to the
        // fixed-size array type is infallible by construction.
        lut.try_into().ok()
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

/// Apply §13.13 decoder gamma correction to a 16-bit [`PngImage`] in
/// place — the bit-depth-general transform specialised to a 16-bit
/// sample depth.
///
/// Operates on the three little-endian 16-bit pixel formats:
/// * [`PngPixelFormat::Gray16Le`] — one colour sample per pixel.
/// * [`PngPixelFormat::Rgb48Le`] — three colour samples (R, G, B).
/// * [`PngPixelFormat::Rgba64Le`] — three colour samples plus a linear
///   alpha sample that is left untouched ("alpha is always represented
///   linearly", W3C PNG3 §13.16).
///
/// Each 16-bit colour sample is read from its two little-endian wire
/// bytes, replaced by `lut[old]`, and written back little-endian. "For
/// color images, the entire calculation is performed separately for R, G,
/// and B values" (§13.13) — the same single LUT drives every colour
/// channel because the transform is per-sample.
///
/// Any non-16-bit format ([`PngPixelFormat::Gray8`] / `Rgb24` / `Pal8` /
/// `Ya8` / `Rgba`) is left untouched and the function returns `false`:
/// the 8-bit appliers ([`apply_to_rgba`] / [`apply_to_palette`]) cover
/// those widths. A `stride` wider than `width * bytes_per_pixel` (the
/// caller-supplied-input case [`PngImage::stride`] documents) is honoured
/// — only the live `width` samples of each row are corrected and any
/// trailing padding bytes are skipped.
///
/// Returns `false` (and leaves the image untouched) when `params` does
/// not yield a usable transform (zero / non-finite file gamma) or when
/// the pixel format is not one of the 16-bit layouts; `true` when the
/// correction was applied.
pub fn apply_to_png16(image: &mut PngImage, params: GammaParams) -> bool {
    // Colour samples per pixel; the alpha sample (if any) is excluded so
    // it stays §13.16-linear.
    let colour_samples = match image.pixel_format {
        PngPixelFormat::Gray16Le => 1usize,
        PngPixelFormat::Rgb48Le => 3,
        PngPixelFormat::Rgba64Le => 3,
        // 8-bit / palette / sub-16-bit formats are not this path's job.
        PngPixelFormat::Gray8
        | PngPixelFormat::Rgb24
        | PngPixelFormat::Pal8
        | PngPixelFormat::Ya8
        | PngPixelFormat::Rgba => return false,
    };
    let Some(lut) = params.build_lut16() else {
        return false;
    };
    let bpp = image.bytes_per_pixel();
    let width = image.width as usize;
    let row_colour_bytes = width * colour_samples * 2;
    let stride = image.stride;
    for row in image.data.chunks_mut(stride) {
        // Only the live colour bytes of the row are corrected; the alpha
        // tail of each pixel and any stride padding are skipped.
        if row.len() < row_colour_bytes {
            break;
        }
        for pixel in row[..width * bpp].chunks_exact_mut(bpp) {
            for sample in pixel[..colour_samples * 2].chunks_exact_mut(2) {
                let v = u16::from_le_bytes([sample[0], sample[1]]);
                let corrected = lut[v as usize].to_le_bytes();
                sample[0] = corrected[0];
                sample[1] = corrected[1];
            }
            // The Rgba64Le alpha sample (last 2 bytes) is deliberately
            // left untouched — §13.16.
        }
    }
    true
}

/// Apply §13.13 16-bit correction using the file gamma from a `gAMA`
/// chunk and the default display (2.2) / user (1.0) exponents.
///
/// Convenience wrapper around [`GammaParams::from_gama`] +
/// [`apply_to_png16`]. Returns `false` (no change) for a zero / absent
/// usable file gamma or a non-16-bit pixel format.
pub fn apply_gama_to_png16(image: &mut PngImage, gama: Gama) -> bool {
    match GammaParams::from_gama(gama) {
        Some(params) => apply_to_png16(image, params),
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

    fn png16(format: PngPixelFormat, width: u32, samples_le: &[u16]) -> PngImage {
        let mut data = Vec::with_capacity(samples_le.len() * 2);
        for &s in samples_le {
            data.extend_from_slice(&s.to_le_bytes());
        }
        let bpp = format.bytes_per_pixel();
        PngImage {
            width,
            height: (data.len() / (width as usize * bpp)) as u32,
            pixel_format: format,
            stride: width as usize * bpp,
            data,
            palette: Vec::new(),
        }
    }

    fn samples_le(image: &PngImage) -> Vec<u16> {
        image
            .data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[test]
    fn lut16_endpoints_and_identity() {
        // Identity exponent (file*display == user) maps every value to
        // itself; endpoints fixed for any positive exponent.
        let identity = GammaParams {
            file_gamma: 0.5,
            display_exponent: 2.0,
            user_exponent: 1.0,
        };
        let lut = identity.build_lut16().unwrap();
        for s in [0usize, 1, 255, 256, 12345, 40000, 65534, 65535] {
            assert_eq!(lut[s] as usize, s, "identity LUT16 should map s -> s");
        }
        // Non-identity exponent: endpoints still pinned.
        let other = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let lut = other.build_lut16().unwrap();
        assert_eq!(lut[0], 0, "0 maps to 0");
        assert_eq!(lut[65535], 65535, "65535 maps to 65535");
    }

    #[test]
    fn lut16_matches_spec_rounding() {
        // floor((s/65535)^e * 65535 + 0.5) hand-computed at sample points.
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let e = params.decoding_exponent().unwrap();
        let lut = params.build_lut16().unwrap();
        for s in [1usize, 257, 32768, 50000, 65534] {
            let expected = (((s as f64 / 65535.0).powf(e)) * 65535.0 + 0.5).floor() as u16;
            assert_eq!(lut[s], expected, "mismatch at s={s}");
        }
    }

    #[test]
    fn lut16_consistent_with_8bit_endpoints() {
        // The two widths share the merged decoding exponent; the 16-bit
        // table's full-scale value matches the 8-bit table's at the same
        // *normalised* position (both fix their own MAX -> MAX).
        let params = GammaParams::from_gama(Gama {
            gamma_times_100000: 45455,
        })
        .unwrap();
        let lut8 = params.build_lut().unwrap();
        let lut16 = params.build_lut16().unwrap();
        // 0 and full-scale agree under each width's own MAX.
        assert_eq!(lut8[0], 0);
        assert_eq!(lut16[0], 0);
        assert_eq!(lut8[255], 255);
        assert_eq!(lut16[65535], 65535);
    }

    #[test]
    fn png16_gray_corrected_per_lut() {
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let lut = params.build_lut16().unwrap();
        let mut img = png16(PngPixelFormat::Gray16Le, 3, &[0, 32768, 65535]);
        assert!(apply_to_png16(&mut img, params));
        assert_eq!(
            samples_le(&img),
            vec![lut[0], lut[32768], lut[65535]],
            "every Gray16 sample maps through the LUT"
        );
    }

    #[test]
    fn png16_rgb48_all_channels_corrected() {
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let lut = params.build_lut16().unwrap();
        // 2 RGB pixels: (R,G,B) samples.
        let src = [100u16, 20000, 65535, 1, 40000, 32768];
        let mut img = png16(PngPixelFormat::Rgb48Le, 2, &src);
        assert!(apply_to_png16(&mut img, params));
        let expected: Vec<u16> = src.iter().map(|&s| lut[s as usize]).collect();
        assert_eq!(samples_le(&img), expected);
    }

    #[test]
    fn png16_rgba64_alpha_is_never_corrected() {
        // §13.16: alpha is linear. The 4th sample of each RGBA64 pixel
        // must survive verbatim while R/G/B map through the LUT.
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let lut = params.build_lut16().unwrap();
        // 2 pixels: R,G,B,A each.
        let src = [10u16, 20000, 65535, 12345, 7, 40000, 100, 54321];
        let mut img = png16(PngPixelFormat::Rgba64Le, 2, &src);
        assert!(apply_to_png16(&mut img, params));
        let got = samples_le(&img);
        // RGB corrected …
        assert_eq!(got[0], lut[10]);
        assert_eq!(got[1], lut[20000]);
        assert_eq!(got[2], lut[65535]);
        assert_eq!(got[4], lut[7]);
        assert_eq!(got[5], lut[40000]);
        assert_eq!(got[6], lut[100]);
        // … alpha verbatim.
        assert_eq!(got[3], 12345, "pixel 0 alpha untouched");
        assert_eq!(got[7], 54321, "pixel 1 alpha untouched");
    }

    #[test]
    fn png16_rejects_non_16bit_formats() {
        let params = GammaParams::default();
        for fmt in [
            PngPixelFormat::Gray8,
            PngPixelFormat::Rgb24,
            PngPixelFormat::Pal8,
            PngPixelFormat::Ya8,
            PngPixelFormat::Rgba,
        ] {
            let bpp = fmt.bytes_per_pixel();
            let mut img = PngImage {
                width: 2,
                height: 1,
                pixel_format: fmt,
                stride: 2 * bpp,
                data: vec![1u8; 2 * bpp],
                palette: Vec::new(),
            };
            let before = img.data.clone();
            assert!(
                !apply_to_png16(&mut img, params),
                "{fmt:?} is not the 16-bit path's job"
            );
            assert_eq!(img.data, before, "{fmt:?} bytes unchanged");
        }
    }

    #[test]
    fn png16_honours_wider_stride_padding() {
        // A caller-supplied stride wider than width*bpp must leave the
        // trailing padding bytes untouched while still correcting the
        // live samples.
        let params = GammaParams {
            file_gamma: 0.45455,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        let lut = params.build_lut16().unwrap();
        // 1 Gray16 sample per row (2 bytes) + 4 padding bytes; 2 rows.
        let mut data = Vec::new();
        data.extend_from_slice(&20000u16.to_le_bytes());
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        data.extend_from_slice(&40000u16.to_le_bytes());
        data.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        let mut img = PngImage {
            width: 1,
            height: 2,
            pixel_format: PngPixelFormat::Gray16Le,
            stride: 6,
            data,
            palette: Vec::new(),
        };
        assert!(apply_to_png16(&mut img, params));
        // Row 0 sample corrected; its padding intact.
        assert_eq!(&img.data[0..2], &lut[20000].to_le_bytes());
        assert_eq!(&img.data[2..6], &[0xAA, 0xBB, 0xCC, 0xDD]);
        // Row 1 sample corrected; its padding intact.
        assert_eq!(&img.data[6..8], &lut[40000].to_le_bytes());
        assert_eq!(&img.data[8..12], &[0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn png16_zero_gama_is_a_no_op() {
        let gama = Gama {
            gamma_times_100000: 0,
        };
        let mut img = png16(PngPixelFormat::Rgb48Le, 1, &[10, 20000, 65535]);
        let before = img.data.clone();
        assert!(!apply_gama_to_png16(&mut img, gama));
        assert_eq!(img.data, before, "zero gAMA leaves 16-bit image unchanged");
    }

    #[test]
    fn png16_non_positive_factors_yield_no_transform() {
        let bad = GammaParams {
            file_gamma: 0.0,
            display_exponent: 2.2,
            user_exponent: 1.0,
        };
        assert!(bad.build_lut16().is_none());
        let mut img = png16(PngPixelFormat::Gray16Le, 2, &[1234, 56789]);
        let before = img.data.clone();
        assert!(!apply_to_png16(&mut img, bad));
        assert_eq!(img.data, before);
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
