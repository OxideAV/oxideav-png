//! Isolated micro-benchmarks for the per-row PNG filter loops.
//!
//! The whole-image decode/encode benches are dominated by DEFLATE
//! inflate/deflate cost (handled in `compcol`), which masks changes to
//! the §6 filter reconstruction (`unfilter_row`) and the §12.8 filter
//! heuristic (`choose_filter_heuristic` / `filter_row`). This harness
//! drives the filter functions directly over a full image's worth of
//! rows so an A/B of the filter loops is visible without inflate noise.
//!
//! Each scenario reconstructs / filters every row of a synthetic image
//! at a representative bpp (1 / 3 / 6 / 8 bytes per pixel), iterating
//! all five filter types so the head/body-split paths are all
//! exercised. Throughput is the raw filtered-byte count.
//!
//! Run with:
//!     cargo bench -p oxideav-png --bench filter

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_png::filter::{choose_filter_heuristic, filter_row, unfilter_row, FilterType};

const FILTERS: [FilterType; 5] = [
    FilterType::None,
    FilterType::Sub,
    FilterType::Up,
    FilterType::Average,
    FilterType::Paeth,
];

/// Deterministic pseudo-random fill so the Paeth/Average branches see a
/// realistic mix of predictor outcomes instead of a flat ramp.
fn fill(buf: &mut [u8], mut state: u32) {
    for b in buf.iter_mut() {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *b = (state & 0xff) as u8;
    }
}

fn bench_unfilter(c: &mut Criterion) {
    // (label, bytes-per-pixel, row_bytes, height)
    let cases = [
        ("gray8/512x512", 1usize, 512usize, 512usize),
        ("rgb24/640x480", 3, 640 * 3, 480),
        ("rgb48/512x512", 6, 512 * 6, 512),
        ("rgba64/512x512", 8, 512 * 8, 512),
    ];
    let mut g = c.benchmark_group("unfilter_row");
    for (label, bpp, row_bytes, height) in cases {
        // One filtered byte stream per filter type: the reconstruction
        // input is the *filtered* bytes, regenerated each iteration from
        // a fresh fill so the loop-carried Sub/Paeth state is identical
        // across samples.
        let mut filtered = vec![0u8; row_bytes];
        fill(&mut filtered, 0x9e37_79b9);
        g.throughput(Throughput::Bytes((row_bytes * height) as u64));
        g.bench_function(BenchmarkId::from_parameter(label), |b| {
            b.iter(|| {
                let mut prev = vec![0u8; row_bytes];
                let mut row = vec![0u8; row_bytes];
                for y in 0..height {
                    row.copy_from_slice(&filtered);
                    let f = FILTERS[y % FILTERS.len()];
                    unfilter_row(f, &mut row, &prev, bpp).unwrap();
                    prev.copy_from_slice(&row);
                }
                criterion::black_box(&prev);
            });
        });
    }
    g.finish();
}

fn bench_filter_heuristic(c: &mut Criterion) {
    let cases = [
        ("gray8/512x512", 1usize, 512usize, 512usize),
        ("rgb24/640x480", 3, 640 * 3, 480),
        ("rgb48/512x512", 6, 512 * 6, 512),
        ("rgba64/512x512", 8, 512 * 8, 512),
    ];
    let mut g = c.benchmark_group("choose_filter_heuristic");
    for (label, bpp, row_bytes, height) in cases {
        let mut rows = vec![0u8; row_bytes * height];
        fill(&mut rows, 0x1234_5678);
        g.throughput(Throughput::Bytes((row_bytes * height) as u64));
        g.bench_function(BenchmarkId::from_parameter(label), |b| {
            b.iter(|| {
                let mut scratch = vec![0u8; row_bytes];
                let zero = vec![0u8; row_bytes];
                let mut acc = 0u64;
                for y in 0..height {
                    let row = &rows[y * row_bytes..(y + 1) * row_bytes];
                    let prev = if y == 0 {
                        &zero[..]
                    } else {
                        &rows[(y - 1) * row_bytes..y * row_bytes]
                    };
                    let f = choose_filter_heuristic(row, prev, bpp, &mut scratch);
                    acc += f as u64;
                }
                criterion::black_box(acc);
            });
        });
    }
    g.finish();
}

fn bench_filter_row(c: &mut Criterion) {
    let cases = [
        ("gray8/512x512", 1usize, 512usize, 512usize),
        ("rgb48/512x512", 6, 512 * 6, 512),
        ("rgba64/512x512", 8, 512 * 8, 512),
    ];
    let mut g = c.benchmark_group("filter_row");
    for (label, bpp, row_bytes, height) in cases {
        let mut rows = vec![0u8; row_bytes * height];
        fill(&mut rows, 0xabcd_ef01);
        // 5 filter types * all rows.
        g.throughput(Throughput::Bytes(
            (row_bytes * height * FILTERS.len()) as u64,
        ));
        g.bench_function(BenchmarkId::from_parameter(label), |b| {
            b.iter(|| {
                let mut out = vec![0u8; row_bytes];
                let zero = vec![0u8; row_bytes];
                for y in 0..height {
                    let row = &rows[y * row_bytes..(y + 1) * row_bytes];
                    let prev = if y == 0 {
                        &zero[..]
                    } else {
                        &rows[(y - 1) * row_bytes..y * row_bytes]
                    };
                    for f in FILTERS {
                        filter_row(f, row, prev, bpp, &mut out);
                        criterion::black_box(&out);
                    }
                }
            });
        });
    }
    g.finish();
}

criterion_group!(
    benches,
    bench_unfilter,
    bench_filter_heuristic,
    bench_filter_row
);
criterion_main!(benches);
