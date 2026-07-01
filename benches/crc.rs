//! Isolated micro-benchmark for the PNG chunk CRC-32.
//!
//! Every PNG chunk carries a CRC-32 (RFC 2083 §5.5, ISO 3309 / IEEE 802.3
//! polynomial `0xEDB88320`) over its type + data bytes. The decoder
//! validates it on every chunk and the encoder computes it on every chunk
//! it emits, so for an IDAT-heavy image the CRC runs over essentially the
//! whole file — an O(file size) cost per decode and per encode. This
//! harness drives [`crc32`] over a range of buffer sizes so an A/B of the
//! CRC inner loop is visible in isolation from the surrounding chunk walk
//! and DEFLATE cost.
//!
//! Throughput is the raw byte count fed through the CRC.
//!
//! Run with:
//!     cargo bench -p oxideav-png --bench crc

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_png::filter::crc32;

/// Deterministic pseudo-random fill so the table-index distribution is
/// realistic (a flat ramp would hammer a narrow slice of the table).
fn fill(buf: &mut [u8], mut state: u32) {
    for b in buf.iter_mut() {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *b = (state & 0xff) as u8;
    }
}

fn bench_crc32(c: &mut Criterion) {
    // Sizes span a small chunk header (4 B type + tiny payload) up to a
    // multi-hundred-KiB IDAT split — the realistic per-chunk CRC range.
    let sizes = [
        ("8B", 8usize),
        ("64B", 64),
        ("1KiB", 1024),
        ("64KiB", 64 * 1024),
        ("1MiB", 1024 * 1024),
    ];
    let mut g = c.benchmark_group("crc32");
    for (label, n) in sizes {
        let mut buf = vec![0u8; n];
        fill(&mut buf, 0x9e37_79b9);
        g.throughput(Throughput::Bytes(n as u64));
        g.bench_function(BenchmarkId::from_parameter(label), |b| {
            b.iter(|| criterion::black_box(crc32(criterion::black_box(&buf))));
        });
    }
    g.finish();
}

criterion_group!(benches, bench_crc32);
criterion_main!(benches);
