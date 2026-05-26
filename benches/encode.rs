//! Criterion benchmarks for the PNG encoder hot paths.
//!
//! Round 154 (depth-mode benchmarks): the encoder owns the §12.8
//! min-sum-abs-delta filter heuristic, sub-image flattening across all
//! 8 supported pixel layouts (8 + 16-bit grayscale, RGB, RGBA, plus
//! 8-bit indexed and 8-bit Ya8), miniz_oxide-driven DEFLATE on the
//! filtered row stream, Adam7 seven-pass interlaced output, and APNG
//! `acTL` / `fcTL` / `fdAT` framing. These benches make each layer's
//! cost measurable so a future "Lever N+1" optimisation round can
//! A/B-compare against the r154 baseline.
//!
//! Scenarios (all freshly synthesised, no committed fixtures):
//!
//!   - **encode_rgba_1920x1080**: 1920×1080 8-bit RGBA encode — the
//!     1080p baseline.
//!   - **encode_rgba_320x240**: 320×240 8-bit RGBA encode — smaller
//!     "thumbnail" baseline.
//!   - **encode_rgb24_640x480**: 640×480 8-bit RGB encode.
//!   - **encode_gray8_512x512**: 512×512 8-bit grayscale encode.
//!   - **encode_gray16_512x512**: 512×512 16-bit grayscale encode.
//!   - **encode_rgb48_512x512**: 512×512 16-bit RGB encode.
//!   - **encode_rgba64_320x240**: 320×240 16-bit RGBA encode.
//!   - **encode_pal8_320x240**: 320×240 8-bit indexed encode (with
//!     PLTE write).
//!   - **encode_rgba_adam7_320x240**: 320×240 8-bit RGBA Adam7
//!     interlaced encode — exercises the seven-pass split.
//!   - **encode_apng_4_frames_320x240**: 4-frame 320×240 RGBA APNG
//!     encode — exercises `acTL` + per-frame `fcTL` / `fdAT` framing
//!     and the per-frame inflate.
//!
//! Run with:
//!     cargo bench -p oxideav-png --bench encode

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_png::{
    encode_apng, encode_png_image, encode_png_image_with_options, PngEncoderOptions, PngImage,
    PngPixelFormat,
};

fn xorshift_byte(state: &mut u32) -> u8 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state & 0xff) as u8
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
            data[idx] = (((base_x + base_y) / 2).min(255) as u8)
                .wrapping_add(xorshift_byte(&mut state) & 0x07);
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
            data[idx] = (((base_x + base_y) / 2).min(255) as u8)
                .wrapping_add(xorshift_byte(&mut state) & 0x07);
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
            let base_x = ((c * 255) / w.max(1)) as u32;
            data[r * w + c] =
                (base_x.min(255) as u8).wrapping_add(xorshift_byte(&mut state) & 0x07);
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

fn bench_encode_rgba_1920x1080(c: &mut Criterion) {
    let image = build_rgba(1920, 1080);
    let mut g = c.benchmark_group("encode_rgba_1920x1080");
    g.throughput(Throughput::Bytes((1920 * 1080 * 4) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("rgba/1920x1080"), |b| {
        b.iter(|| encode_png_image(criterion::black_box(&image)).expect("encode_png_image"));
    });
    g.finish();
}

fn bench_encode_rgba_320x240(c: &mut Criterion) {
    let image = build_rgba(320, 240);
    let mut g = c.benchmark_group("encode_rgba_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/320x240"), |b| {
        b.iter(|| encode_png_image(criterion::black_box(&image)).expect("encode_png_image"));
    });
    g.finish();
}

fn bench_encode_rgb24_640x480(c: &mut Criterion) {
    let image = build_rgb24(640, 480);
    let mut g = c.benchmark_group("encode_rgb24_640x480");
    g.throughput(Throughput::Bytes((640 * 480 * 3) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("rgb24/640x480"), |b| {
        b.iter(|| encode_png_image(criterion::black_box(&image)).expect("encode_png_image"));
    });
    g.finish();
}

fn bench_encode_gray8_512x512(c: &mut Criterion) {
    let image = build_gray8(512, 512);
    let mut g = c.benchmark_group("encode_gray8_512x512");
    g.throughput(Throughput::Bytes((512 * 512) as u64));
    g.bench_function(BenchmarkId::from_parameter("gray8/512x512"), |b| {
        b.iter(|| encode_png_image(criterion::black_box(&image)).expect("encode_png_image"));
    });
    g.finish();
}

fn bench_encode_gray16_512x512(c: &mut Criterion) {
    let image = build_gray16(512, 512);
    let mut g = c.benchmark_group("encode_gray16_512x512");
    g.throughput(Throughput::Bytes((512 * 512 * 2) as u64));
    g.bench_function(BenchmarkId::from_parameter("gray16/512x512"), |b| {
        b.iter(|| encode_png_image(criterion::black_box(&image)).expect("encode_png_image"));
    });
    g.finish();
}

fn bench_encode_rgb48_512x512(c: &mut Criterion) {
    let image = build_rgb48(512, 512);
    let mut g = c.benchmark_group("encode_rgb48_512x512");
    g.throughput(Throughput::Bytes((512 * 512 * 6) as u64));
    g.sample_size(20);
    g.bench_function(BenchmarkId::from_parameter("rgb48/512x512"), |b| {
        b.iter(|| encode_png_image(criterion::black_box(&image)).expect("encode_png_image"));
    });
    g.finish();
}

fn bench_encode_rgba64_320x240(c: &mut Criterion) {
    let image = build_rgba64(320, 240);
    let mut g = c.benchmark_group("encode_rgba64_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 8) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba64/320x240"), |b| {
        b.iter(|| encode_png_image(criterion::black_box(&image)).expect("encode_png_image"));
    });
    g.finish();
}

fn bench_encode_pal8_320x240(c: &mut Criterion) {
    let image = build_pal8(320, 240);
    let mut g = c.benchmark_group("encode_pal8_320x240");
    g.throughput(Throughput::Bytes((320 * 240) as u64));
    g.bench_function(BenchmarkId::from_parameter("pal8/320x240"), |b| {
        b.iter(|| encode_png_image(criterion::black_box(&image)).expect("encode_png_image"));
    });
    g.finish();
}

fn bench_encode_rgba_adam7_320x240(c: &mut Criterion) {
    let image = build_rgba(320, 240);
    let opts = PngEncoderOptions {
        interlace: true,
        metadata: None,
    };
    let mut g = c.benchmark_group("encode_rgba_adam7_320x240");
    g.throughput(Throughput::Bytes((320 * 240 * 4) as u64));
    g.bench_function(BenchmarkId::from_parameter("rgba/adam7/320x240"), |b| {
        b.iter(|| {
            encode_png_image_with_options(criterion::black_box(&image), &opts)
                .expect("encode_png_image_with_options")
        });
    });
    g.finish();
}

fn bench_encode_apng_4_frames_320x240(c: &mut Criterion) {
    let frames = (0..4).map(|_| build_rgba(320, 240)).collect::<Vec<_>>();
    let mut g = c.benchmark_group("encode_apng_4_frames_320x240");
    g.throughput(Throughput::Bytes((4 * 320 * 240 * 4) as u64));
    g.sample_size(10);
    g.bench_function(BenchmarkId::from_parameter("apng/4x320x240"), |b| {
        b.iter(|| encode_apng(criterion::black_box(&frames), 4, 0).expect("encode_apng"));
    });
    g.finish();
}

criterion_group!(
    benches,
    bench_encode_rgba_1920x1080,
    bench_encode_rgba_320x240,
    bench_encode_rgb24_640x480,
    bench_encode_gray8_512x512,
    bench_encode_gray16_512x512,
    bench_encode_rgb48_512x512,
    bench_encode_rgba64_320x240,
    bench_encode_pal8_320x240,
    bench_encode_rgba_adam7_320x240,
    bench_encode_apng_4_frames_320x240,
);
criterion_main!(benches);
