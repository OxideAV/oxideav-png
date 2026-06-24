#![no_main]

//! Drive `encode_png_image_with_options` across the full
//! [`PngEncoderOptions`] matrix the existing `encode_decode_roundtrip`
//! target leaves untouched: Adam7 interlace (both the >=8-bit and the
//! sub-byte 1/2/4-bit pass layouts), caller-supplied sub-byte
//! `bit_depth` packing, every `FilterStrategy` variant (Adaptive,
//! `Fixed` x5, plus the whole-image exhaustive `Brute` search), and the
//! ancillary-metadata emission path. The default
//! round-trip target only ever passes `PngEncoderOptions::default()`
//! (non-interlaced, 8-bit, Adaptive, no metadata), so the interlaced
//! sub-image gather (`deflate_encode_pixels_adam7` /
//! `deflate_encode_pixels_adam7_subbyte`), the sub-byte MSB-first
//! packer, and the chunk-ordering metadata writers are reached for the
//! first time here.
//!
//! Strategy: derive a synthetic, internally-consistent [`PngImage`]
//! (dimensions, pixel format, deterministic pixel fill) plus an options
//! bundle from the fuzz bytes, then:
//!
//!   1. Call `encode_png_image_with_options`. The input is always a
//!      valid image with options the encoder must either accept or
//!      reject with an `Err` — it may never panic / abort / overflow.
//!      Options the encoder legitimately rejects (e.g. a sub-byte
//!      `bit_depth` on a truecolour source, or a pixel value outside
//!      the sub-byte range) yield `Err` and the harness returns; that
//!      is a contract path, not a crash.
//!   2. For the option combinations the encoder *accepts*, decode the
//!      emitted bytes with `decode_png` and `decode_png_to_rgba`.
//!      Neither may panic on the encoder's own output, and the decode
//!      must reproduce the image's width / height (the on-wire pixel
//!      format may legitimately differ from the source format for the
//!      sub-byte cases, so only dimensions are asserted, plus liveness).
//!
//! The harness deliberately keeps the *pixel data* loss-free-friendly
//! only where it asserts equality (it does not); the binding contract
//! under test is liveness of the option-bearing encode path plus
//! decode-liveness on its output, which is exactly the surface the
//! default-options round-trip never visits.

use libfuzzer_sys::fuzz_target;
use oxideav_png::{
    decode_png, decode_png_to_rgba, encode_png_image_with_options, FilterStrategy, FilterType,
    Gama, Phys, PhysUnit, PngEncoderOptions, PngImage, PngMetadata, PngPixelFormat, Text, Time,
};

/// Per-dimension cap. Interlaced encode walks all seven Adam7 passes
/// over the whole image, so keep the canvas small to leave the fuzzer
/// iteration headroom.
const MAX_DIM: u32 = 20;
const MIN_DIM: u32 = 1;

fuzz_target!(|data: &[u8]| {
    let Some((image, opts)) = Plan::build(data) else {
        return;
    };

    let encoded = match encode_png_image_with_options(&image, &opts) {
        // A rejected option bundle is a legitimate contract outcome
        // (sub-byte depth on a wrong source format, out-of-range
        // sub-byte sample, invalid metadata, ...). Not a crash.
        Err(_) => return,
        Ok(bytes) => bytes,
    };

    // The encoder accepted the options, so its output must decode
    // without panicking. The on-wire IHDR colour-type/bit-depth can
    // differ from the source `PngPixelFormat` for the sub-byte cases
    // (Gray8/Pal8 at depth 1/2/4 decode back as a packed plane), so we
    // assert only liveness + dimension preservation, not pixel-format
    // identity or pixel equality.
    match decode_png(&encoded) {
        Ok(decoded) => {
            assert_eq!(
                decoded.width, image.width,
                "decode width drift: {} != {}",
                decoded.width, image.width
            );
            assert_eq!(
                decoded.height, image.height,
                "decode height drift: {} != {}",
                decoded.height, image.height
            );
        }
        Err(e) => panic!("decode_png failed on accepted encoder output: {e}"),
    }

    // The one-shot RGBA promotion is a second, independent decode
    // surface (palette resolution, grayscale widening, 16->8 truncation,
    // sub-byte unpacking) over the same bytes — drive it for liveness.
    match decode_png_to_rgba(&encoded) {
        Ok(bitmap) => {
            assert_eq!(bitmap.width, image.width, "rgba decode width drift");
            assert_eq!(bitmap.height, image.height, "rgba decode height drift");
        }
        Err(e) => panic!("decode_png_to_rgba failed on accepted encoder output: {e}"),
    }
});

/// Fuzz-derived synthetic encode plan: an internally-consistent image
/// plus an options bundle. Both are constructed so the *image* is always
/// well-formed; only the *options* may be ones the encoder rejects (that
/// rejection is part of the surface under test).
struct Plan;

impl Plan {
    fn build(data: &[u8]) -> Option<(PngImage, PngEncoderOptions)> {
        // Layout of the leading control bytes (everything else is unused
        // entropy; pixels are synthesised deterministically from a seed).
        //   [0] width-1 selector
        //   [1] height-1 selector
        //   [2] pixel-format selector
        //   [3] options flags (interlace / metadata)
        //   [4] filter-strategy selector
        //   [5] bit-depth selector
        //   [6] pixel seed
        if data.len() < 7 {
            return None;
        }

        let width = MIN_DIM + (data[0] as u32 % (MAX_DIM - MIN_DIM + 1));
        let height = MIN_DIM + (data[1] as u32 % (MAX_DIM - MIN_DIM + 1));
        let format = pixel_format_for(data[2]);
        let flags = data[3];
        let filter_strategy = filter_strategy_for(data[4]);
        // bit_depth selector: 0 => None (8-bit implied), else map to
        // 1/2/4/8/16. 16 is intentionally included so the encoder's
        // "16 is rejected" path is exercised; non-power-of-two values
        // (3/5/6/7) exercise the validity rejection too.
        let bit_depth = match data[5] % 6 {
            0 => None,
            1 => Some(1u8),
            2 => Some(2),
            3 => Some(4),
            4 => Some(8),
            _ => Some(16),
        };
        let seed = data[6];

        let image = solid_image(width, height, format, seed, bit_depth);

        let metadata = if flags & 0b0000_0010 != 0 {
            Some(sample_metadata())
        } else {
            None
        };

        // compression_level selector packed into the high nibble of the
        // flags byte: 0 => None (= encoder default 6); 1..=9 => that
        // explicit level; 10..=15 exercise the out-of-range rejection
        // path (encode error ahead of the wire, a contract outcome, not
        // a crash).
        let compression_level = match flags >> 4 {
            0 => None,
            n => Some(n),
        };

        let opts = PngEncoderOptions {
            interlace: flags & 0b0000_0001 != 0,
            metadata,
            bit_depth,
            filter_strategy,
            compression_level,
        };

        Some((image, opts))
    }
}

/// Map a fuzz byte to a pixel format. All eight formats are reachable so
/// the sub-byte `bit_depth` rejection path (depth set on a non-Gray8 /
/// non-Pal8 source) is exercised alongside the accept path.
fn pixel_format_for(sel: u8) -> PngPixelFormat {
    match sel % 8 {
        0 => PngPixelFormat::Gray8,
        1 => PngPixelFormat::Gray16Le,
        2 => PngPixelFormat::Rgb24,
        3 => PngPixelFormat::Rgb48Le,
        4 => PngPixelFormat::Pal8,
        5 => PngPixelFormat::Ya8,
        6 => PngPixelFormat::Rgba,
        _ => PngPixelFormat::Rgba64Le,
    }
}

/// Map a fuzz byte to a filter strategy: Adaptive, all five Fixed
/// variants, plus the whole-image exhaustive `Brute` search, uniform
/// over a 0..=6 modulo. `Brute` funnels the budget into the
/// six-candidate compare loop (`brute_compress`) and, on the sub-byte
/// paths, the pack-once / filter-six split — code the Adaptive/Fixed
/// arms never exercise.
fn filter_strategy_for(sel: u8) -> FilterStrategy {
    match sel % 7 {
        0 => FilterStrategy::Adaptive,
        1 => FilterStrategy::Fixed(FilterType::None),
        2 => FilterStrategy::Fixed(FilterType::Sub),
        3 => FilterStrategy::Fixed(FilterType::Up),
        4 => FilterStrategy::Fixed(FilterType::Average),
        5 => FilterStrategy::Fixed(FilterType::Paeth),
        _ => FilterStrategy::Brute,
    }
}

/// Build a deterministic single-colour image for `format`. When a
/// sub-byte `bit_depth` is requested for a Gray8 / Pal8 source, every
/// sample byte is masked into `0..=(1 << depth) - 1` so the encoder's
/// sub-byte range check accepts it (the harness still feeds the
/// out-of-range / wrong-format depth combinations because `bit_depth`
/// is applied to *every* format selection, exercising the rejection
/// arms when the source is not Gray8 / Pal8).
fn solid_image(
    width: u32,
    height: u32,
    format: PngPixelFormat,
    seed: u8,
    bit_depth: Option<u8>,
) -> PngImage {
    let bpp = format.bytes_per_pixel();
    let stride = width as usize * bpp;
    let pixel_count = width as usize * height as usize;

    // For Gray8 / Pal8 with a valid sub-byte depth, clamp the single
    // sample into range so the in-range accept path is reachable.
    let sample = match (format, bit_depth) {
        (PngPixelFormat::Gray8 | PngPixelFormat::Pal8, Some(d @ (1 | 2 | 4))) => {
            let max = (1u16 << d) - 1;
            (seed as u16 & max) as u8
        }
        _ => seed,
    };

    let pixel: Vec<u8> = match format {
        PngPixelFormat::Gray8 => vec![sample],
        PngPixelFormat::Gray16Le => {
            let v = (seed as u16) | ((seed.wrapping_mul(3) as u16) << 8);
            vec![(v & 0xFF) as u8, (v >> 8) as u8]
        }
        PngPixelFormat::Rgb24 => vec![seed, seed.wrapping_mul(3), seed.wrapping_mul(7)],
        PngPixelFormat::Rgb48Le => {
            let r = (seed as u16) | ((seed.wrapping_mul(3) as u16) << 8);
            let g = (seed.wrapping_mul(5) as u16) | ((seed.wrapping_mul(11) as u16) << 8);
            let b = (seed.wrapping_mul(7) as u16) | ((seed.wrapping_mul(13) as u16) << 8);
            vec![
                (r & 0xFF) as u8,
                (r >> 8) as u8,
                (g & 0xFF) as u8,
                (g >> 8) as u8,
                (b & 0xFF) as u8,
                (b >> 8) as u8,
            ]
        }
        // Pal8 byte is a palette index; keep it small so it indexes the
        // synthesised palette below.
        PngPixelFormat::Pal8 => vec![sample],
        PngPixelFormat::Ya8 => vec![seed, 0xFF],
        PngPixelFormat::Rgba => vec![seed, seed.wrapping_mul(3), seed.wrapping_mul(7), 0xFF],
        PngPixelFormat::Rgba64Le => vec![
            seed,
            seed.wrapping_mul(3),
            seed.wrapping_mul(5),
            seed.wrapping_mul(7),
            seed.wrapping_mul(11),
            seed.wrapping_mul(13),
            0xFF,
            0xFF,
        ],
    };

    let mut buf = Vec::with_capacity(stride * height as usize);
    for _ in 0..pixel_count {
        buf.extend_from_slice(&pixel);
    }

    // Palette for Pal8. The encoder treats `image.palette` as a single
    // `PLTE || tRNS` blob and derives the PLTE entry count from the
    // frame's max index (`(max_idx + 1) * 3` bytes); any trailing bytes
    // become a `tRNS` alpha table. The harness fills every pixel with
    // the same `sample` index, so `max_idx == sample`. Size the palette
    // to *exactly* `(sample + 1) * 3` bytes — no trailing bytes — so the
    // encoder writes a PLTE that covers the one used index and emits no
    // (mis-sized) tRNS chunk, keeping the encoder output decodable.
    let palette = if format == PngPixelFormat::Pal8 {
        let entries = sample as usize + 1;
        let mut p = Vec::with_capacity(entries * 3);
        for i in 0..entries {
            let v = i as u8;
            p.extend_from_slice(&[v, v.wrapping_mul(3), v.wrapping_mul(7)]);
        }
        p
    } else {
        Vec::new()
    };

    PngImage {
        width,
        height,
        pixel_format: format,
        stride,
        data: buf,
        palette,
    }
}

/// A small, always-valid ancillary-metadata bundle. Exercises the
/// `tEXt` / `pHYs` / `tIME` / `gAMA` chunk-ordering + framing writers
/// in `encode_png_image_with_options` without needing the fuzzer to
/// synthesise each chunk's internal constraints (keyword spacing,
/// date ranges, ...) which would mostly land in rejection paths.
fn sample_metadata() -> PngMetadata {
    let mut meta = PngMetadata::default();
    meta.texts = vec![Text {
        keyword: "Comment".to_string(),
        text: "fuzz".to_string(),
    }];
    meta.phys = Some(Phys {
        pixels_per_unit_x: 2835,
        pixels_per_unit_y: 2835,
        unit: PhysUnit::Metre,
    });
    meta.time = Some(Time {
        year: 2026,
        month: 6,
        day: 14,
        hour: 12,
        minute: 0,
        second: 0,
    });
    meta.gama = Some(Gama {
        gamma_times_100000: 45455,
    });
    meta
}
