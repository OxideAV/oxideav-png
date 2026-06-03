//! Criterion benchmarks for the PNG decoder hot paths.
//!
//! Round 154 (depth-mode benchmarks): `oxideav-png` is saturated at
//! 100% decode + encode per the workspace README — every IHDR colour
//! type at 8/16-bit, every PNG row filter, Adam7 interlace, APNG
//! disposal/blend state machine, and the full 9-chunk ancillary
//! metadata round-trip have shipped through rounds 1..r153. Per the
//! workspace "saturated → fuzz/bench/profile" memo, this round adds
//! criterion benches mirroring the bmp / gif / tiff shape so future
//! optimisation rounds can A/B-test changes to the decoder hot paths
//! against a stable r154 baseline.
//!
//! This file covers the **decoder**; sibling files cover `encode` and
//! `roundtrip`.
//!
//! Each scenario synthesises a fresh PNG on the fly with the public
//! encoder API and iterates [`decode_png`] / [`decode_png_to_rgba`] /
//! [`parse_metadata`] / [`decode_apng`] on the encoded bytes. No
//! fixture files are committed; the bench inputs are entirely
//! reproducible from the bench source.
//!
//!   - **decode_rgba_1920x1080**: 1920×1080 8-bit RGBA full-frame
//!     decode — the natural-image baseline. Exercises filter
//!     reconstruction (Sub / Up / Average / Paeth all in play) + the
//!     inflate hot path on a 1080p surface.
//!   - **decode_rgba_320x240**: 320×240 8-bit RGBA decode — the
//!     smaller "thumbnail" baseline that pairs with the encode bench
//!     of the same dimensions.
//!   - **decode_rgb24_640x480**: 640×480 8-bit RGB decode — the VGA
//!     case (3 bytes/pixel; no alpha).
//!   - **decode_gray8_512x512**: 512×512 8-bit grayscale decode —
//!     single-byte-per-pixel filter path.
//!   - **decode_gray16_512x512**: 512×512 16-bit grayscale decode —
//!     2-byte filter unit + big-endian sample assembly.
//!   - **decode_rgb48_512x512**: 512×512 16-bit RGB decode — 6 bytes
//!     per filter unit.
//!   - **decode_rgba64_320x240**: 320×240 16-bit RGBA decode — the
//!     widest filter unit (8 bytes).
//!   - **decode_pal8_320x240**: 320×240 palette-indexed decode —
//!     PLTE bytes copied through `extradata`; no decode-side palette
//!     expansion.
//!   - **decode_to_rgba_pal8_320x240**: same source as above but
//!     decoded through [`decode_png_to_rgba`], which expands the
//!     palette + applies tRNS on the way out.
//!   - **parse_metadata_rgba_320x240**: chunk-walk + CRC validation
//!     on a populated metadata block (sBIT/pHYs/tIME/bKGD/sRGB) with
//!     no IDAT inflate. Isolates the chunk-parser cost.
//!   - **decode_apng_4_frames_320x240**: 4-frame APNG decode (None
//!     dispose, Source blend) — exercises the disposal / blend state
//!     machine and per-frame inflate.
//!
//! Round 196 (depth-mode benches): adds **decode_apng_frame_scan**,
//! a parametric group sweeping multiple frame counts on small
//! (128×128) RGBA frames so the measured time tracks the per-frame
//! decode-loop cost (fcTL + fdAT sequence-number walk, the
//! disposal/blend state machine, and per-frame inflate setup) rather
//! than the per-pixel inflate work. Three parameters (2 / 8 / 32
//! frames) bracket the trivial / typical / long-loop range; throughput
//! is reported in decoded RGBA bytes so a frame-count scaling that's
//! sub-linear in the loop overhead surfaces as a throughput delta.
//!   - **decode_apng_frame_scan**: 128×128 RGBA APNG decoded for N
//!     ∈ {2, 8, 32} frames (None dispose, Source blend; default
//!     IDAT-as-first-frame layout).
//!
//! Run with:
//!     cargo bench -p oxideav-png --bench decode

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_png::{
    decode_apng, decode_png, decode_png_to_rgba, encode_apng, encode_png_image, parse_metadata,
    Phys, PhysUnit, PngImage, PngPixelFormat, RenderingIntent, Sbit, Srgb, Time,
};

/// Cheap deterministic xorshift32 — keeps the bench inputs from being
/// trivially compressible / branch-predictable so the inflate side of
/// the bench actually exercises Huffman + distance code paths.
fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
}

fn natural_pattern_byte(r: usize, c: usize, h: usize, w: usize, state: &mut u32) -> u8 {
    let base_y = ((r * 255) / h.max(1)) as u32;
    let base_x = ((c * 255) / w.max(1)) as u32;
    let v = ((base_x + base_y) / 2).min(255) as u8;
    v.wrapping_add(xorshift_byte(state) & 0x07)
}

fn build_rgba(width: u32, height: u32) -> PngImage {
    let w = width as usize;
    let h = height as usize;
    let mut data = vec![0u8; w * h * 4];
    let mut state: u32 = 0x1234_5678;
    for r in 0..h {
        for c in 0..w {
            let idx = (r * w + c) * 4;
            let base_y = ((r * 255) / h.max(1)) as u32;
            let base_x = ((c * 255) / w.max(1)) as u32;
            data[idx] = natural_pattern_byte(r, c, h, w, &mut state);
            data[idx + 1] = base_y.min(255) as u8;
            data[idx + 2] = base_x.min(255) as u8;
            data[idx + 3] = 0xff;
        }
    }
    PngImage {
        width,
        height,
        pixel_format: PngPixelFormat::Rgba,
        stride: w * 4,
        data,
        palette: Vec::new(),
    }
}

fn build_rgb24(width: u32, height: u32) -> PngImage {
    let w = width as usize;
    let h = height as usize;
    let mut data = vec![0u8; w * h * 3];
    let mut state: u32 = 0x2345_6789;
    for r in 0..h {
        for c in 0..w {
            let idx = (r * w + c) * 3;
            let base_y = ((r * 255) / h.max(1)) as u32;
            let base_x = ((c * 255) / w.max(1)) as u32;
            data[idx] = natural_pattern_byte(r, c, h, w, &mut state);
            data[idx + 1] = base_y.min(255) as u8;
            data[idx + 2] = base_x.min(255) as u8;
        }
    }
    PngImage {
        width,
        height,
        pixel_format: PngPixelFormat::Rgb24,
        stride: w * 3,
        data,
        palette: Vec::new(),
    }
}

fn build_gray8(width: u32, height: u32) -> PngImage {
    let w = width as usize;
    let h = height as usize;
    let mut data = vec![0u8; w * h];
    let mut state: u32 = 0x3456_789a;
    for r in 0..h {
        for c in 0..w {
            data[r * w + c] = natural_pattern_byte(r, c, h, w, &mut state);
        }
    }
    PngImage {
        width,
        height,
        pixel_format: PngPixelFormat::Gray8,
        stride: w,
        data,
        palette: Vec::new(),
    }
}

fn build_gray16(width: u32, height: u32) -> PngImage {
    let w = width as usize;
    let h = height as usize;
    let mut data = vec![0u8; w * h * 2];
    for r in 0..h {
        for c in 0..w {
            let base = (((r + c) as u32) * 0x0101) & 0xffff;
            let idx = (r * w + c) * 2;
            // PngPixelFormat::Gray16Le — little-endian per channel.
            data[idx] = (base & 0xff) as u8;
            data[idx + 1] = (base >> 8) as u8;
        }
    }
    PngImage {
        width,
        height,
        pixel_format: PngPixelFormat::Gray16Le,
        stride: w * 2,
        data,
        palette: Vec::new(),
    }
}

fn build_rgb48(width: u32, height: u32) -> PngImage {
    let w = width as usize;
    let h = height as usize;
    let mut data = vec![0u8; w * h * 6];
    for r in 0..h {
        for c in 0..w {
            let idx = (r * w + c) * 6;
            let rr = (((r + c) as u32) * 0x0103) & 0xffff;
            let gg = (((r * 2 + c) as u32) * 0x0107) & 0xffff;
            let bb = (((r + c * 2) as u32) * 0x010d) & 0xffff;
            data[idx] = (rr & 0xff) as u8;
            data[idx + 1] = (rr >> 8) as u8;
            data[idx + 2] = (gg & 0xff) as u8;
            data[idx + 3] = (gg >> 8) as u8;
            data[idx + 4] = (bb & 0xff) as u8;
            data[idx + 5] = (bb >> 8) as u8;
        }
    }
    PngImage {
        width,
        height,
        pixel_format: PngPixelFormat::Rgb48Le,
        stride: w * 6,
        data,
        palette: Vec::new(),
    }
}

fn build_rgba64(width: u32, height: u32) -> PngImage {
    let w = width as usize;
    let h = height as usize;
    let mut data = vec![0u8; w * h * 8];
    for r in 0..h {
        for c in 0..w {
            let idx = (r * w + c) * 8;
            let rr = (((r + c) as u32) * 0x0103) & 0xffff;
            let gg = (((r * 2 + c) as u32) * 0x0107) & 0xffff;
            let bb = (((r + c * 2) as u32) * 0x010d) & 0xffff;
            data[idx] = (rr & 0xff) as u8;
            data[idx + 1] = (rr >> 8) as u8;
            data[idx + 2] = (gg & 0xff) as u8;
            data[idx + 3] = (gg >> 8) as u8;
            data[idx + 4] = (bb & 0xff) as u8;
            data[idx + 5] = (bb >> 8) as u8;
            data[idx + 6] = 0xff;
            data[idx + 7] = 0xff;
        }
    }
    PngImage {
        width,
        height,
        pixel_format: PngPixelFormat::Rgba64Le,
        stride: w * 8,
        data,
        palette: Vec::new(),
    }
}

fn build_pal8(width: u32, height: u32) -> PngImage {
    let w = width as usize;
    let h = height as usize;
    let mut data = vec![0u8; w * h];
    let mut state: u32 = 0x4567_89ab;
    for byte in data.iter_mut() {
        *byte = xorshift_byte(&mut state);
    }
    // 256-entry palette (full).
    let mut palette = Vec::with_capacity(256 * 3);
    for i in 0..256u16 {
        palette.push(i as u8);
        palette.push((i ^ 0x55) as u8);
        palette.push((i ^ 0xaa) as u8);
    }
    PngImage {
        width,
        height,
        pixel_format: PngPixelFormat::Pal8,
        stride: w,
        data,
        palette,
    }
}

fn encode_with_metadata(image: &PngImage) -> Vec<u8> {
    use oxideav_png::{PngEncoderOptions, PngMetadata};
    let metadata = PngMetadata {
        sbit: Some(Sbit::Rgba(8, 8, 8, 8)),
        phys: Some(Phys {
            pixels_per_unit_x: 2835,
            pixels_per_unit_y: 2835,
            unit: PhysUnit::Metre,
        }),
        time: Some(Time {
            year: 2026,
            month: 5,
            day: 26,
            hour: 12,
            minute: 0,
            second: 0,
        }),
        srgb: Some(Srgb {
            rendering_intent: RenderingIntent::Perceptual,
        }),
        ..Default::default()
    };
    let opts = PngEncoderOptions {
        interlace: false,
        metadata: Some(metadata),
        bit_depth: None,
    };
    oxideav_png::encode_png_image_with_options(image, &opts).expect("encode_png_image_with_options")
}

fn bench_decode_rgba_1920x1080(c: &mut Criterion) {
    let image = build_rgba(1920, 1080);
    let bytes = encode_png_image(&image).expect("encode_png_image");
    let mut g = c.benchmark_group("decode_rgba_1920x1080");
    g.throughput(Throughput::Bytes((1920 * 1080 * 4) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("rgba/1920x1080"), |b| {
        b.iter(|| decode_png(criterion::black_box(&bytes)).expect("decode_png"));
    });
    g.finish();
}

fn bench_decode_rgba_320x240(c: &mut Criterion) {
    let image = build_rgba(320, 240);
    let bytes = encode_png_image(&image).expect("encode_png_image");
    let mut g = c.benchmark_group("decode_rgba_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/320x240"), |b| {
        b.iter(|| decode_png(criterion::black_box(&bytes)).expect("decode_png"));
    });
    g.finish();
}

fn bench_decode_rgb24_640x480(c: &mut Criterion) {
    let image = build_rgb24(640, 480);
    let bytes = encode_png_image(&image).expect("encode_png_image");
    let mut g = c.benchmark_group("decode_rgb24_640x480");
    g.throughput(Throughput::Bytes((640 * 480 * 3) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("rgb24/640x480"), |b| {
        b.iter(|| decode_png(criterion::black_box(&bytes)).expect("decode_png"));
    });
    g.finish();
}

fn bench_decode_gray8_512x512(c: &mut Criterion) {
    let image = build_gray8(512, 512);
    let bytes = encode_png_image(&image).expect("encode_png_image");
    let mut g = c.benchmark_group("decode_gray8_512x512");
    g.throughput(Throughput::Bytes((512 * 512) as u64));
    g.bench_function(BenchmarkId::from_parameter("gray8/512x512"), |b| {
        b.iter(|| decode_png(criterion::black_box(&bytes)).expect("decode_png"));
    });
    g.finish();
}

fn bench_decode_gray16_512x512(c: &mut Criterion) {
    let image = build_gray16(512, 512);
    let bytes = encode_png_image(&image).expect("encode_png_image");
    let mut g = c.benchmark_group("decode_gray16_512x512");
    g.throughput(Throughput::Bytes((512 * 512 * 2) as u64));
    g.bench_function(BenchmarkId::from_parameter("gray16/512x512"), |b| {
        b.iter(|| decode_png(criterion::black_box(&bytes)).expect("decode_png"));
    });
    g.finish();
}

fn bench_decode_rgb48_512x512(c: &mut Criterion) {
    let image = build_rgb48(512, 512);
    let bytes = encode_png_image(&image).expect("encode_png_image");
    let mut g = c.benchmark_group("decode_rgb48_512x512");
    g.throughput(Throughput::Bytes((512 * 512 * 6) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("rgb48/512x512"), |b| {
        b.iter(|| decode_png(criterion::black_box(&bytes)).expect("decode_png"));
    });
    g.finish();
}

fn bench_decode_rgba64_320x240(c: &mut Criterion) {
    let image = build_rgba64(320, 240);
    let bytes = encode_png_image(&image).expect("encode_png_image");
    let mut g = c.benchmark_group("decode_rgba64_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 8) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba64/320x240"), |b| {
        b.iter(|| decode_png(criterion::black_box(&bytes)).expect("decode_png"));
    });
    g.finish();
}

fn bench_decode_pal8_320x240(c: &mut Criterion) {
    let image = build_pal8(320, 240);
    let bytes = encode_png_image(&image).expect("encode_png_image");
    let mut g = c.benchmark_group("decode_pal8_320x240");
    g.throughput(Throughput::Bytes((320 * 240) as u64));
    g.bench_function(BenchmarkId::from_parameter("pal8/320x240"), |b| {
        b.iter(|| decode_png(criterion::black_box(&bytes)).expect("decode_png"));
    });
    g.finish();
}

fn bench_decode_to_rgba_pal8_320x240(c: &mut Criterion) {
    let image = build_pal8(320, 240);
    let bytes = encode_png_image(&image).expect("encode_png_image");
    let mut g = c.benchmark_group("decode_to_rgba_pal8_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("pal8/to-rgba/320x240"), |b| {
        b.iter(|| decode_png_to_rgba(criterion::black_box(&bytes)).expect("decode_png_to_rgba"));
    });
    g.finish();
}

fn bench_parse_metadata_rgba_320x240(c: &mut Criterion) {
    let image = build_rgba(320, 240);
    let bytes = encode_with_metadata(&image);
    let mut g = c.benchmark_group("parse_metadata_rgba_320x240");
    // Throughput = whole PNG length, since parse_metadata walks the
    // full chunk list (CRC included). Excludes the IDAT inflate cost.
    g.throughput(Throughput::Bytes(bytes.len() as u64));
    g.bench_function(BenchmarkId::from_parameter("metadata/320x240"), |b| {
        b.iter(|| parse_metadata(criterion::black_box(&bytes)).expect("parse_metadata"));
    });
    g.finish();
}

fn bench_decode_apng_4_frames_320x240(c: &mut Criterion) {
    // Four 320×240 RGBA frames, 4 cs delay each — exercises fcTL/fdAT
    // walking + per-frame inflate + the disposal/blend state machine.
    let frames = (0..4).map(|_| build_rgba(320, 240)).collect::<Vec<_>>();
    let bytes = encode_apng(&frames, /* delay_cs */ 4, /* num_plays */ 0).expect("encode_apng");
    let mut g = c.benchmark_group("decode_apng_4_frames_320x240");
    g.throughput(Throughput::Bytes((4 * 320 * 240 * 4) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("apng/4x320x240"), |b| {
        b.iter(|| decode_apng(criterion::black_box(&bytes)).expect("decode_apng"));
    });
    g.finish();
}

/// Parametric APNG decode-loop throughput across multiple frame
/// counts. Small 128×128 RGBA frames are used deliberately so the
/// per-pixel inflate cost is small relative to the per-frame loop
/// overhead (fcTL+fdAT sequence-number walk, disposal/blend state
/// reset, per-frame inflate setup). Sweeps 2 / 8 / 32 frames so a
/// future change that, say, makes the per-frame setup amortise
/// differently is visible as a scaling shift between the three
/// points rather than just a single 4-frame magnitude.
fn bench_decode_apng_frame_scan(c: &mut Criterion) {
    const W: u32 = 128;
    const H: u32 = 128;
    // RGBA decoded-byte footprint per frame; throughput reported in
    // total output bytes so a flatter time/frame curve shows as
    // higher throughput at the larger frame counts.
    const FRAME_RGBA_BYTES: u64 = (W as u64) * (H as u64) * 4;

    let mut g = c.benchmark_group("decode_apng_frame_scan");
    g.sample_size(10);
    for &n in &[2usize, 8, 32] {
        let frames = (0..n).map(|_| build_rgba(W, H)).collect::<Vec<_>>();
        let bytes = encode_apng(&frames, /* delay_cs */ 4, /* num_plays */ 0).expect("encode_apng");
        g.throughput(Throughput::Bytes(FRAME_RGBA_BYTES * (n as u64)));
        g.bench_function(
            BenchmarkId::from_parameter(format!("apng/{n}x{W}x{H}")),
            |b| {
                b.iter(|| decode_apng(criterion::black_box(&bytes)).expect("decode_apng"));
            },
        );
    }
    g.finish();
}

criterion_group!(
    benches,
    bench_decode_rgba_1920x1080,
    bench_decode_rgba_320x240,
    bench_decode_rgb24_640x480,
    bench_decode_gray8_512x512,
    bench_decode_gray16_512x512,
    bench_decode_rgb48_512x512,
    bench_decode_rgba64_320x240,
    bench_decode_pal8_320x240,
    bench_decode_to_rgba_pal8_320x240,
    bench_parse_metadata_rgba_320x240,
    bench_decode_apng_4_frames_320x240,
    bench_decode_apng_frame_scan,
);
criterion_main!(benches);
