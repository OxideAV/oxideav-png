//! Micro-benchmarks for the §12.4 / §13.12 sample-depth scaling loops.
//!
//! [`rescale_16bit_to_8bit`] and [`rescale_16bit_to_8bit_via_sbit`] walk
//! every colour / alpha sample of a decoded 16-bit image, so on a large
//! frame they are an O(pixels) pass a viewer runs before display. This
//! harness drives them over full images at representative sizes (isolated
//! from any decode / inflate cost, which the whole-image benches already
//! cover) so an A/B of the rescale inner loop is visible on its own.
//!
//! Throughput is counted in input (16-bit) sample bytes processed.
//!
//! Run with:
//!     cargo bench -p oxideav-png --bench depth

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_png::depth::{rescale_16bit_to_8bit, rescale_16bit_to_8bit_via_sbit};
use oxideav_png::image::{PngImage, PngPixelFormat};
use oxideav_png::metadata::Sbit;

/// Deterministic pseudo-random 16-bit fill so the rounding branch of the
/// linear equation sees a realistic spread of sample values.
fn fill16(count: usize, mut state: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(count * 2);
    for _ in 0..count {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out.extend_from_slice(&((state & 0xffff) as u16).to_le_bytes());
    }
    out
}

fn image(format: PngPixelFormat, w: u32, h: u32) -> PngImage {
    let bpp = format.bytes_per_pixel();
    let samples = w as usize * h as usize * (bpp / 2);
    PngImage {
        width: w,
        height: h,
        pixel_format: format,
        stride: w as usize * bpp,
        data: fill16(samples, 0x9e37_79b9),
        palette: Vec::new(),
    }
}

fn bench_rescale(c: &mut Criterion) {
    // (label, format, width, height, sBIT variant for the via arm)
    let cases = [
        (
            "gray16/512x512",
            PngPixelFormat::Gray16Le,
            512u32,
            512u32,
            Sbit::Grayscale(12),
        ),
        (
            "rgb48/512x512",
            PngPixelFormat::Rgb48Le,
            512,
            512,
            Sbit::Rgb(12, 12, 12),
        ),
        (
            "rgba64/320x240",
            PngPixelFormat::Rgba64Le,
            320,
            240,
            Sbit::Rgba(12, 12, 12, 12),
        ),
    ];

    let mut plain = c.benchmark_group("rescale_16bit_to_8bit");
    for (label, fmt, w, h, _) in cases {
        let img = image(fmt, w, h);
        plain.throughput(Throughput::Bytes(img.data.len() as u64));
        plain.bench_function(BenchmarkId::from_parameter(label), |b| {
            b.iter(|| criterion::black_box(rescale_16bit_to_8bit(criterion::black_box(&img))));
        });
    }
    plain.finish();

    let mut via = c.benchmark_group("rescale_16bit_to_8bit_via_sbit");
    for (label, fmt, w, h, sbit) in cases {
        let img = image(fmt, w, h);
        via.throughput(Throughput::Bytes(img.data.len() as u64));
        via.bench_function(BenchmarkId::from_parameter(label), |b| {
            b.iter(|| {
                criterion::black_box(rescale_16bit_to_8bit_via_sbit(
                    criterion::black_box(&img),
                    sbit,
                ))
            });
        });
    }
    via.finish();
}

criterion_group!(benches, bench_rescale);
criterion_main!(benches);
