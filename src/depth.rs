//! Sample-depth scaling — W3C PNG 3rd Edition §12.4 ("Sample depth
//! scaling") and §13.12 ("Sample depth rescaling").
//!
//! PNG stores samples at one of the IHDR-allowed sample depths (1, 2, 4,
//! 8, or 16 bits). Real source data and real display hardware frequently
//! live at a *different* precision, so both the encoder and the decoder
//! need to move a sample from one depth to another. §12.4 covers the
//! encoder direction (scaling source data *up* into an allowed PNG
//! depth), §13.12 the decoder direction (scaling stored data *down* for a
//! lower-precision display, or recovering the significant bits an encoder
//! recorded in an `sBIT` chunk). This module implements the exact spec
//! formulas as small pure primitives plus a couple of decoder-side
//! conveniences over a decoded [`PngImage`].
//!
//! The codec proper never rescales on its own — a decoded [`PngImage`]
//! carries samples at the IHDR depth verbatim, exactly as the wire holds
//! them. These helpers are the opt-in stage a caller reaches for when it
//! wants an 8-bit buffer for a typical display, mirroring the [`crate::
//! gamma`] / [`crate::srgb`] "transform beside the codec" arrangement.
//!
//! ## The linear equation (§12.4 / §13.12)
//!
//! Both directions share one "most accurate" formula:
//!
//! ```text
//! output = floor((input * MAXOUTSAMPLE / MAXINSAMPLE) + 0.5)
//! ```
//!
//! where `input` ranges `0..=MAXINSAMPLE` and `output` ranges
//! `0..=MAXOUTSAMPLE`, and `MAXxSAMPLE = 2^depth - 1`. [`rescale_sample`]
//! is this equation for arbitrary in/out maxima, so it serves the up
//! direction (`MAXOUTSAMPLE > MAXINSAMPLE`), the down direction
//! (`MAXOUTSAMPLE < MAXINSAMPLE`, e.g. 16→8-bit for display), and the
//! identity.
//!
//! ## Left bit replication (§12.4)
//!
//! "A close approximation to the linear scaling method is achieved by
//! *left bit replication*, which is shifting the valid bits to begin in
//! the most significant bit and repeating the most significant bits into
//! the open bits." The spec's worked example scales the 5-bit value
//! `27` (`11011`) up to 8 bits, giving `222` (`11011110`) — the same
//! result the linear equation produces. "Left bit replication usually
//! gives the same value as linear scaling, and is never off by more than
//! one." [`scale_up_bit_replication`] implements it.
//!
//! ## Zero fill (§12.4)
//!
//! "A distinctly less accurate approximation is obtained by simply
//! left-shifting the input value and filling the low order bits with
//! zeroes." It cannot reproduce an all-ones maximum (so it darkens the
//! image slightly) and "shall not be used for alpha channel data".
//! [`scale_up_zero_fill`] implements it for completeness.
//!
//! ## Significant-bit recovery (§13.12)
//!
//! "When an `sBIT` chunk is present, the reference image data can be
//! recovered by shifting right to the sample depth specified by `sBIT`
//! … the encoder is required to have used a method that preserves the
//! high-order bits, so shifting always works." [`recover_sbit`] performs
//! that right shift, and the [`crate::image::PngImage`] conveniences pair
//! it with a rescale so a caller can go straight from a stored 16-bit
//! image + its `sBIT` to an accurate 8-bit display buffer.

use crate::image::{PngImage, PngPixelFormat};
use crate::metadata::Sbit;

/// Maximum sample value for a PNG sample depth: `2^bit_depth - 1`.
///
/// Defined for `bit_depth` in `1..=16` (the PNG sample-depth range); a
/// `bit_depth` of `0` yields `0` and any value `>= 16` saturates at
/// `65535` so the helper can never overflow a `u16`.
#[must_use]
pub fn max_sample(bit_depth: u8) -> u16 {
    if bit_depth == 0 {
        0
    } else if bit_depth >= 16 {
        u16::MAX
    } else {
        (1u16 << bit_depth) - 1
    }
}

/// The §12.4 / §13.12 "most accurate" linear rescale:
/// `floor((input * max_out / max_in) + 0.5)`.
///
/// `input` is assumed to lie in `0..=max_in`; an over-range `input` is
/// clamped to `max_in` first so the result never exceeds `max_out`. A
/// `max_in` of `0` (a degenerate zero-bit depth) returns `0`. The
/// rounding half-up is computed in `u64` so no intermediate product can
/// overflow even at the 16-bit maxima.
///
/// This one equation covers scaling **up** (`max_out > max_in`), **down**
/// (`max_out < max_in`, e.g. reducing a 16-bit sample to 8 bits for
/// display), and the identity (`max_out == max_in`).
#[must_use]
pub fn rescale_sample(input: u16, max_in: u16, max_out: u16) -> u16 {
    if max_in == 0 {
        return 0;
    }
    let input = input.min(max_in) as u64;
    let max_out = max_out as u64;
    let max_in = max_in as u64;
    // floor(x + 0.5) with x = input*max_out/max_in, done in integers:
    // (2*input*max_out + max_in) / (2*max_in).
    let num = 2 * input * max_out + max_in;
    let den = 2 * max_in;
    (num / den) as u16
}

/// §12.4 left bit replication: scale `input` (valid in the low
/// `from_bits` bits) up to `to_bits` by shifting it to the top and
/// repeating the high-order bits into the freed low bits.
///
/// Matches the spec's worked example (`27` at 5 bits → `222` at 8 bits).
/// For `from_bits >= to_bits` the value is right-shifted (truncated) to
/// `to_bits` since there are no open bits to fill. `from_bits == 0`
/// yields `0`.
///
/// A PNG sample depth is one of `1, 2, 4, 8, 16` (RFC 2083 §11.2.2 /
/// W3C PNG 3rd Ed. §11.2.2 — "the allowed bit depths are 1, 2, 4, 8,
/// and 16"), so a sample never exceeds 16 bits. Any `from_bits` /
/// `to_bits` above `16` is saturated to `16` — the widest legal depth
/// and the width of the `u16` a sample lives in — so every shift stays
/// in range for arbitrary (e.g. fuzz-supplied) width arguments instead
/// of shifting past the `u16` / `u32` width.
#[must_use]
pub fn scale_up_bit_replication(input: u16, from_bits: u8, to_bits: u8) -> u16 {
    let from_bits = from_bits.min(16);
    let to_bits = to_bits.min(16);
    if from_bits == 0 || to_bits == 0 {
        return 0;
    }
    if from_bits >= to_bits {
        return (input >> (from_bits - to_bits)) & mask(to_bits);
    }
    let src = (input & mask(from_bits)) as u32;
    let mut result: u32 = 0;
    let mut filled: u8 = 0;
    while filled < to_bits {
        let take = from_bits.min(to_bits - filled);
        // The top `take` bits of the source pattern.
        let top = src >> (from_bits - take);
        result = (result << take) | top;
        filled += take;
    }
    (result & mask(to_bits) as u32) as u16
}

/// §12.4 zero-fill scaling: left-shift `input` from `from_bits` to
/// `to_bits`, filling the freed low-order bits with zeroes.
///
/// The spec's "distinctly less accurate" method — it cannot reproduce an
/// all-ones maximum (so it darkens slightly) and "shall not be used for
/// alpha channel data". Provided for completeness / measurement; the
/// [`PngImage`] conveniences here never use it. For `from_bits >=
/// to_bits` the value is right-shifted to `to_bits`.
///
/// As with [`scale_up_bit_replication`], `from_bits` / `to_bits` above
/// `16` — the widest legal PNG sample depth (RFC 2083 §11.2.2) and the
/// `u16` sample width — are saturated to `16`, keeping the shift within
/// the type width for any width argument.
#[must_use]
pub fn scale_up_zero_fill(input: u16, from_bits: u8, to_bits: u8) -> u16 {
    let from_bits = from_bits.min(16);
    let to_bits = to_bits.min(16);
    if from_bits == 0 || to_bits == 0 {
        return 0;
    }
    let masked = input & mask(from_bits);
    if from_bits >= to_bits {
        return masked >> (from_bits - to_bits);
    }
    masked << (to_bits - from_bits)
}

/// §13.12 significant-bit recovery: recover the reference image sample by
/// shifting `sample` (stored at `stored_bits`) right to the `sbit`
/// significant-bit depth an `sBIT` chunk recorded.
///
/// "The encoder is required to have used a method that preserves the
/// high-order bits, so shifting always works." The result lies in
/// `0..=2^sbit - 1`. `sbit >= stored_bits` (nothing to recover) returns
/// the sample unchanged; `sbit == 0` returns `0`.
///
/// `stored_bits` / `sbit` above `16` — a stored sample is at most the
/// widest legal PNG depth of 16 bits (RFC 2083 §11.2.2), the `u16`
/// sample width — are saturated to `16` so the right shift never exceeds
/// the type width for any width argument.
#[must_use]
pub fn recover_sbit(sample: u16, stored_bits: u8, sbit: u8) -> u16 {
    let stored_bits = stored_bits.min(16);
    let sbit = sbit.min(16);
    if sbit == 0 {
        return 0;
    }
    if sbit >= stored_bits {
        return sample;
    }
    sample >> (stored_bits - sbit)
}

/// Low `bits`-bit mask (`bits` in `1..=16`).
fn mask(bits: u8) -> u16 {
    max_sample(bits)
}

/// Accurately reduce a 16-bit [`PngImage`] to its 8-bit counterpart with
/// the §13.12 linear equation (`Gray16Le` → `Gray8`, `Rgb48Le` →
/// `Rgb24`, `Rgba64Le` → `Rgba`).
///
/// Every little-endian 16-bit sample — colour **and** alpha — is
/// rescaled `floor(v * 255 / 65535 + 0.5)`; this is a depth reduction,
/// distinct from gamma correction (§13.16 exempts alpha only from the
/// latter). Endpoints are exact: `0 → 0`, `65535 → 255`, and the
/// mid-scale `0x8000 → 128` rather than the low-byte-discard `128`'s
/// less-accurate cousin. Any per-row stride padding is dropped; the
/// returned image is tightly packed (`stride == width * out_bpp`).
///
/// A non-16-bit input (`Gray8` / `Rgb24` / `Pal8` / `Ya8` / `Rgba`) is
/// already at 8-bit depth, so it is returned as an unchanged clone.
#[must_use]
pub fn rescale_16bit_to_8bit(image: &PngImage) -> PngImage {
    rescale_16bit_to_8bit_inner(image, None)
}

/// Accurately reduce a 16-bit [`PngImage`] to 8 bits, first recovering
/// each channel's significant bits from an `sBIT` chunk (§13.12).
///
/// "Using `sBIT` to recover the original samples before scaling them to
/// suit the display often yields a more accurate display than ignoring
/// `sBIT`" (§13.12). For every colour / alpha channel whose recorded
/// significant-bit count `S` is below 16, the stored sample is first
/// shifted right to `S` bits ([`recover_sbit`]) and then linearly
/// rescaled from `2^S - 1` to `255`; a channel with `S == 16` (or absent
/// from the `sBIT` variant) takes the plain 16→8 linear path. The
/// `Sbit` channel layout is matched to the image's pixel format; a
/// mismatched variant simply falls back to plain rescaling per channel.
///
/// A non-16-bit input is returned as an unchanged clone, exactly as
/// [`rescale_16bit_to_8bit`].
#[must_use]
pub fn rescale_16bit_to_8bit_via_sbit(image: &PngImage, sbit: Sbit) -> PngImage {
    rescale_16bit_to_8bit_inner(image, Some(sbit))
}

/// Per-channel significant-bit counts `[colour0, colour1, colour2,
/// alpha]` for a 16-bit pixel format, drawn from an `sBIT` variant. A
/// `16` entry (or a channel the variant does not describe) means "plain
/// linear rescale, no recovery".
fn sbit_channel_bits(format: PngPixelFormat, sbit: Option<Sbit>) -> [u8; 4] {
    let default = [16u8; 4];
    let Some(sbit) = sbit else { return default };
    match (format, sbit) {
        (PngPixelFormat::Gray16Le, Sbit::Grayscale(g)) => [g, 16, 16, 16],
        (PngPixelFormat::Gray16Le, Sbit::GrayscaleAlpha(g, _)) => [g, 16, 16, 16],
        (PngPixelFormat::Rgb48Le, Sbit::Rgb(r, g, b)) => [r, g, b, 16],
        (PngPixelFormat::Rgba64Le, Sbit::Rgba(r, g, b, a)) => [r, g, b, a],
        // Variant doesn't match the format: no recovery, plain rescale.
        _ => default,
    }
}

fn rescale_16bit_to_8bit_inner(image: &PngImage, sbit: Option<Sbit>) -> PngImage {
    // (out_format, colour_sample_count, has_alpha)
    let (out_format, colour_samples, has_alpha) = match image.pixel_format {
        PngPixelFormat::Gray16Le => (PngPixelFormat::Gray8, 1usize, false),
        PngPixelFormat::Rgb48Le => (PngPixelFormat::Rgb24, 3, false),
        PngPixelFormat::Rgba64Le => (PngPixelFormat::Rgba, 3, true),
        // Already 8-bit: identity clone.
        PngPixelFormat::Gray8
        | PngPixelFormat::Rgb24
        | PngPixelFormat::Pal8
        | PngPixelFormat::Ya8
        | PngPixelFormat::Rgba => return image.clone(),
    };
    let samples_per_pixel = colour_samples + usize::from(has_alpha);
    let bits = sbit_channel_bits(image.pixel_format, sbit);
    let width = image.width as usize;
    let height = image.height as usize;
    let in_bpp = image.bytes_per_pixel(); // samples_per_pixel * 2
    let out_bpp = out_format.bytes_per_pixel();
    let out_stride = width * out_bpp;
    let mut out = vec![0u8; out_stride * height];

    let rescale_channel = |v: u16, ch: usize| -> u8 {
        let s = bits[ch];
        if s >= 16 {
            rescale_sample(v, u16::MAX, 255) as u8
        } else {
            let recovered = recover_sbit(v, 16, s);
            rescale_sample(recovered, max_sample(s), 255) as u8
        }
    };

    for y in 0..height {
        let in_row = &image.data[y * image.stride..];
        let out_row = &mut out[y * out_stride..y * out_stride + out_stride];
        for x in 0..width {
            let in_px = &in_row[x * in_bpp..x * in_bpp + in_bpp];
            let out_px = &mut out_row[x * out_bpp..x * out_bpp + out_bpp];
            for ch in 0..samples_per_pixel {
                let v = u16::from_le_bytes([in_px[ch * 2], in_px[ch * 2 + 1]]);
                out_px[ch] = rescale_channel(v, ch);
            }
        }
    }

    PngImage {
        width: image.width,
        height: image.height,
        pixel_format: out_format,
        stride: out_stride,
        data: out,
        palette: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_sample_matches_spec_depths() {
        assert_eq!(max_sample(1), 1);
        assert_eq!(max_sample(2), 3);
        assert_eq!(max_sample(4), 15);
        assert_eq!(max_sample(5), 31);
        assert_eq!(max_sample(8), 255);
        assert_eq!(max_sample(16), 65535);
        // Degenerate guards.
        assert_eq!(max_sample(0), 0);
        assert_eq!(max_sample(17), 65535);
    }

    #[test]
    fn rescale_linear_worked_example() {
        // §12.4: 5-bit 27 scaled up to 8 bits is 222.
        assert_eq!(rescale_sample(27, 31, 255), 222);
    }

    #[test]
    fn rescale_linear_endpoints_and_midscale() {
        // Up direction.
        assert_eq!(rescale_sample(0, 31, 255), 0);
        assert_eq!(rescale_sample(31, 31, 255), 255);
        // Down direction 16→8.
        assert_eq!(rescale_sample(0, 65535, 255), 0);
        assert_eq!(rescale_sample(65535, 65535, 255), 255);
        // 0x8000 is the mid-scale value; half-up rounding gives 128.
        assert_eq!(rescale_sample(0x8000, 65535, 255), 128);
        assert_eq!(rescale_sample(0x7FFF, 65535, 255), 127);
        // Linear rounding is strictly more accurate than a byte-discard
        // shift: 0x01FF (511) shifts down to 1 but rounds up to 2.
        assert_eq!(0x01FFu16 >> 8, 1);
        assert_eq!(rescale_sample(0x01FF, 65535, 255), 2);
    }

    #[test]
    fn rescale_clamps_over_range_and_zero_maxin() {
        assert_eq!(rescale_sample(9999, 31, 255), 255);
        assert_eq!(rescale_sample(5, 0, 255), 0);
    }

    #[test]
    fn bit_replication_worked_example() {
        // §12.4: 27 (11011) at 5 bits → 222 (11011110) at 8 bits.
        assert_eq!(scale_up_bit_replication(27, 5, 8), 222);
    }

    #[test]
    fn bit_replication_endpoints() {
        for from in 1u8..=8 {
            assert_eq!(scale_up_bit_replication(0, from, 8), 0);
            // All-ones at any depth replicates to all-ones (reproduces
            // white exactly, unlike zero-fill).
            let all_ones = max_sample(from);
            assert_eq!(
                scale_up_bit_replication(all_ones, from, 8),
                255,
                "from={from}"
            );
        }
        // 1-bit up to 16-bit: 1 → 0xFFFF.
        assert_eq!(scale_up_bit_replication(1, 1, 16), 0xFFFF);
        assert_eq!(scale_up_bit_replication(0, 1, 16), 0);
    }

    #[test]
    fn bit_replication_never_off_by_more_than_one_from_linear() {
        // §12.4: "never off by more than one".
        for from in 1u8..=8 {
            let max_in = max_sample(from);
            for to in from..=16 {
                let max_out = max_sample(to);
                for v in 0..=max_in {
                    let lin = rescale_sample(v, max_in, max_out) as i32;
                    let rep = scale_up_bit_replication(v, from, to) as i32;
                    assert!(
                        (lin - rep).abs() <= 1,
                        "from={from} to={to} v={v}: linear={lin} rep={rep}"
                    );
                }
            }
        }
    }

    #[test]
    fn zero_fill_cannot_reproduce_white() {
        // 5-bit 31 zero-filled to 8 bits is 0xF8 = 248, not 255.
        assert_eq!(scale_up_zero_fill(31, 5, 8), 248);
        assert_eq!(scale_up_zero_fill(27, 5, 8), 216);
        assert_eq!(scale_up_zero_fill(0, 5, 8), 0);
    }

    #[test]
    fn recover_sbit_round_trips_bit_replication_and_zero_fill() {
        // §13.12: an encoder that preserved the high-order S bits (all the
        // §12.4 methods do) lets a decoder shift right to recover the
        // reference sample exactly.
        for s in 1u8..=8 {
            for v in 0..=max_sample(s) {
                let up_rep = scale_up_bit_replication(v, s, 8);
                assert_eq!(recover_sbit(up_rep, 8, s), v, "bitrep s={s} v={v}");
                let up_zero = scale_up_zero_fill(v, s, 8);
                assert_eq!(recover_sbit(up_zero, 8, s), v, "zerofill s={s} v={v}");
            }
        }
        // Into 16 bits too.
        for s in 1u8..=16 {
            let v = max_sample(s);
            let up = scale_up_bit_replication(v, s, 16);
            assert_eq!(recover_sbit(up, 16, s), v, "16-bit s={s}");
        }
    }

    #[test]
    fn out_of_range_depths_never_shift_overflow() {
        // Regression: fuzz `decode` target crash-17b883b6… drove the
        // sample-depth primitives with attacker-controlled width bytes
        // (`from_bits = 0x0d = 13`, `to_bits = 0x50 = 80`), overflowing
        // `masked << (to_bits - from_bits)` (= `<< 67`) in
        // `scale_up_zero_fill`, and the sibling shifts in
        // `scale_up_bit_replication` / `recover_sbit`. A PNG sample depth
        // is at most 16 (RFC 2083 §11.2.2), so widths saturate to 16 and
        // every shift stays inside the u16/u32 width. Just returning is
        // the assertion — a shift-overflow would panic here.
        let sample = 0x470a; // data[0..2] of the crash vector.
        let _ = scale_up_zero_fill(sample, 13, 80);
        let _ = scale_up_bit_replication(sample, 13, 80);
        let _ = recover_sbit(sample, 13, 80);
        // Sweep the whole u8 width domain for all three: none may panic.
        for from in 0u8..=255 {
            for to in 0u8..=255 {
                let _ = scale_up_zero_fill(sample, from, to);
                let _ = scale_up_bit_replication(sample, from, to);
                let _ = recover_sbit(sample, from, to);
            }
        }
    }

    #[test]
    fn over_16_widths_saturate_to_16() {
        // Out-of-range widths behave exactly as the widest legal depth
        // (16), never as a wrapped shift producing a different sample.
        for w in [17u8, 80, 200, 255] {
            assert_eq!(
                scale_up_zero_fill(0x1234, w, 8),
                scale_up_zero_fill(0x1234, 16, 8),
                "zero_fill from_bits={w}"
            );
            assert_eq!(
                scale_up_bit_replication(0x1234, 5, w),
                scale_up_bit_replication(0x1234, 5, 16),
                "bit_replication to_bits={w}"
            );
            assert_eq!(
                recover_sbit(0x1234, w, 5),
                recover_sbit(0x1234, 16, 5),
                "recover_sbit stored_bits={w}"
            );
        }
    }

    #[test]
    fn recover_sbit_edge_cases() {
        assert_eq!(recover_sbit(1234, 16, 0), 0);
        assert_eq!(recover_sbit(1234, 16, 16), 1234);
        assert_eq!(recover_sbit(1234, 16, 20), 1234);
        // 0xB6 (222) at 8 bits, 5 significant → 27.
        assert_eq!(recover_sbit(222, 8, 5), 27);
    }

    fn png16(format: PngPixelFormat, w: u32, h: u32, samples: &[u16]) -> PngImage {
        let mut data = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        let bpp = format.bytes_per_pixel();
        PngImage {
            width: w,
            height: h,
            pixel_format: format,
            stride: w as usize * bpp,
            data,
            palette: Vec::new(),
        }
    }

    #[test]
    fn rescale_gray16_to_gray8() {
        let img = png16(PngPixelFormat::Gray16Le, 3, 1, &[0, 0x8000, 0xFFFF]);
        let out = rescale_16bit_to_8bit(&img);
        assert_eq!(out.pixel_format, PngPixelFormat::Gray8);
        assert_eq!(out.stride, 3);
        assert_eq!(out.data, vec![0, 128, 255]);
    }

    #[test]
    fn rescale_rgb48_to_rgb24() {
        let img = png16(PngPixelFormat::Rgb48Le, 1, 1, &[0, 0x8000, 0xFFFF]);
        let out = rescale_16bit_to_8bit(&img);
        assert_eq!(out.pixel_format, PngPixelFormat::Rgb24);
        assert_eq!(out.data, vec![0, 128, 255]);
    }

    #[test]
    fn rescale_rgba64_to_rgba_alpha_rescaled_linearly() {
        // Alpha IS depth-reduced (this is not gamma correction).
        let img = png16(PngPixelFormat::Rgba64Le, 1, 1, &[0xFFFF, 0, 0x8000, 0x8000]);
        let out = rescale_16bit_to_8bit(&img);
        assert_eq!(out.pixel_format, PngPixelFormat::Rgba);
        assert_eq!(out.data, vec![255, 0, 128, 128]);
    }

    #[test]
    fn rescale_drops_stride_padding() {
        // Two 1-pixel Gray16 rows with 4 bytes of trailing padding each.
        let mut data = Vec::new();
        data.extend_from_slice(&0xFFFFu16.to_le_bytes());
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // padding
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // padding
        let img = PngImage {
            width: 1,
            height: 2,
            pixel_format: PngPixelFormat::Gray16Le,
            stride: 6,
            data,
            palette: Vec::new(),
        };
        let out = rescale_16bit_to_8bit(&img);
        assert_eq!(out.stride, 1);
        assert_eq!(out.data, vec![255, 0]);
    }

    #[test]
    fn rescale_8bit_is_identity_clone() {
        for fmt in [
            PngPixelFormat::Gray8,
            PngPixelFormat::Rgb24,
            PngPixelFormat::Rgba,
            PngPixelFormat::Ya8,
        ] {
            let bpp = fmt.bytes_per_pixel();
            let img = PngImage {
                width: 2,
                height: 1,
                pixel_format: fmt,
                stride: 2 * bpp,
                data: (0..(2 * bpp) as u8).collect(),
                palette: Vec::new(),
            };
            let out = rescale_16bit_to_8bit(&img);
            assert_eq!(out.pixel_format, fmt);
            assert_eq!(out.data, img.data);
        }
    }

    #[test]
    fn rescale_via_sbit_recovers_before_scaling() {
        // A Gray16 sample bit-replicated up from a 5-bit source value 27.
        let stored = scale_up_bit_replication(27, 5, 16);
        let img = png16(PngPixelFormat::Gray16Le, 1, 1, &[stored]);
        // Plain path: rescale the full 16-bit value.
        let plain = rescale_16bit_to_8bit(&img);
        // sBIT path: recover to 5 bits (=27) then scale 27@5 → 8 = 222.
        let via = rescale_16bit_to_8bit_via_sbit(&img, Sbit::Grayscale(5));
        assert_eq!(via.data, vec![222]);
        // The plain 16→8 of a bit-replicated 5-bit value is within one of
        // the sBIT-accurate answer (both derive from the same source).
        assert!((plain.data[0] as i32 - 222).abs() <= 1);
    }

    #[test]
    fn rescale_via_sbit_full_depth_channel_is_plain() {
        // S == 16 on every channel ⇒ identical to the plain path.
        let img = png16(PngPixelFormat::Rgb48Le, 1, 1, &[0x1234, 0x8000, 0xFFFF]);
        let plain = rescale_16bit_to_8bit(&img);
        let via = rescale_16bit_to_8bit_via_sbit(&img, Sbit::Rgb(16, 16, 16));
        assert_eq!(plain.data, via.data);
    }

    #[test]
    fn rescale_via_sbit_mismatched_variant_falls_back() {
        // A Grayscale sBIT on an Rgb48 image can't describe the channels,
        // so every channel takes the plain path.
        let img = png16(PngPixelFormat::Rgb48Le, 1, 1, &[0, 0x8000, 0xFFFF]);
        let plain = rescale_16bit_to_8bit(&img);
        let via = rescale_16bit_to_8bit_via_sbit(&img, Sbit::Grayscale(5));
        assert_eq!(plain.data, via.data);
    }
}
