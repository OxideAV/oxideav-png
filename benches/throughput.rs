//! Megapixels-per-second table for real PNG files, with a stage
//! breakdown.
//!
//! A plain `fn main` harness (`harness = false`):
//!
//! ```sh
//! PNG_BENCH_FILES=photo.png,screenshot.png cargo bench --bench throughput
//! ```
//!
//! For every file named in `PNG_BENCH_FILES` (comma-separated; with
//! none set, a synthetic 2048×1536 photo-like RGB image and a
//! screenshot-like RGBA image are used) it prints:
//!
//! * **decode** — `decode` and `decode_rgba8` wall-clock and MP/s, and
//!   where the time goes: the zlib inflate of the `IDAT` stream alone,
//!   and the §9 filter reconstruction of the inflated rows alone;
//! * **encode** — wall-clock, MP/s and output size for a set of option
//!   bundles, each emitted stream decoded back and compared with the
//!   source, plus the filter-selection and row-filter stages alone.
//!
//! `PNG_BENCH_EFFORTS=1,3,8` replaces the encode bundles with those
//! `EncodeOptions::effort` values, `PNG_BENCH_GRID=1` with every DEFLATE
//! level under four row-filter policies, and `PNG_BENCH_OUT=dir` writes
//! each encoded file there (for size comparisons with other tools).
//!
//! `PNG_BENCH_REPS=n` sets the repetition count (default 5); the
//! **median** is reported, because the machine is rarely idle.
//! `PNG_BENCH_THREADS=1,8` adds thread budgets to the encode table and
//! `PNG_BENCH_ONLY=decode|encode` runs one half.

use std::time::Instant;

use oxideav_png::chunk::ChunkIter;
use oxideav_png::filter::{choose_filter_heuristic, filter_row, unfilter_row};
use oxideav_png::{
    EncodeOptions, FilterStrategy, FilterType, Ihdr, PngImage, PngPixelFormat, decode,
    decode_rgba8, encode,
};

fn reps() -> usize {
    std::env::var("PNG_BENCH_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
        .max(1)
}

/// Median wall-clock milliseconds of `reps()` runs of `f`.
fn median_ms<T>(mut f: impl FnMut() -> T) -> f64 {
    let mut times: Vec<f64> = (0..reps())
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(f());
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[times.len() / 2]
}

fn hash(x: u32, y: u32, c: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ c.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h
}

/// Smooth gradients plus a few LSBs of noise.
fn synthetic_photo(w: u32, h: u32) -> PngImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let base = match c {
                    0 => (x * 255 / w + y * 255 / h) / 2,
                    1 => y * 255 / h,
                    _ => 255 - x * 255 / w,
                };
                data.push((base + (hash(x, y, c) & 7)).min(255) as u8);
            }
        }
    }
    PngImage::from_rgb8(w, h, data).unwrap()
}

/// Flat panels, one-pixel rules and small high-contrast glyph-like
/// blocks on an opaque RGBA canvas.
fn synthetic_screenshot(w: u32, h: u32) -> PngImage {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let panel = (x / 320 + 3 * (y / 200)) % 5;
            let mut px = match panel {
                0 => [250, 250, 250],
                1 => [32, 34, 40],
                2 => [236, 240, 245],
                3 => [255, 255, 255],
                _ => [18, 92, 180],
            };
            if x % 320 == 0 || y % 200 == 0 {
                px = [128, 128, 128];
            } else if (y % 20) < 11 && hash(x / 2, y / 2, panel) & 3 == 0 && (x % 320) > 24 {
                px = [px[0] ^ 0xE0, px[1] ^ 0xE0, px[2] ^ 0xE0];
            }
            data.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    PngImage::from_rgba8(w, h, data).unwrap()
}

/// The concatenated `IDAT` payload and the header of a PNG file.
fn idat_stream(file: &[u8]) -> (Ihdr, Vec<u8>) {
    let mut ihdr = None;
    let mut idat = Vec::new();
    for c in ChunkIter::new(file, 8) {
        let c = c.unwrap();
        if c.is_type(b"IHDR") {
            ihdr = Some(Ihdr::parse(c.data).unwrap());
        } else if c.is_type(b"IDAT") {
            idat.extend_from_slice(c.data);
        }
    }
    (ihdr.unwrap(), idat)
}

fn bench_decode(name: &str, file: &[u8]) {
    let (ihdr, idat) = idat_stream(file);
    let mp = ihdr.width as f64 * ihdr.height as f64 / 1e6;
    let layout = format!(
        "ct {} / {}-bit{}",
        ihdr.colour_type,
        ihdr.bit_depth,
        if ihdr.interlace != 0 { " Adam7" } else { "" }
    );
    let t_decode = median_ms(|| decode(file).unwrap());
    let t_rgba = median_ms(|| decode_rgba8(file).unwrap());
    let t_inflate =
        median_ms(|| compcol::vec::decompress_to_vec::<compcol::zlib::Zlib>(&idat).unwrap());
    let mut line = format!(
        "| {name} | {}×{} {layout} | {:.1} | {:.0} | {:.1} | {:.0} | {:.1} |",
        ihdr.width,
        ihdr.height,
        t_decode,
        mp / t_decode * 1e3,
        t_rgba,
        mp / t_rgba * 1e3,
        t_inflate
    );
    if ihdr.interlace == 0 {
        let filtered = compcol::vec::decompress_to_vec::<compcol::zlib::Zlib>(&idat).unwrap();
        let row = ihdr.row_bytes().unwrap();
        let bpp = ihdr.bpp_for_filter().unwrap();
        let mut hist = [0usize; 5];
        for r in filtered.chunks_exact(row + 1) {
            hist[r[0].min(4) as usize] += 1;
        }
        let t_unfilter = median_ms(|| {
            let mut prev = vec![0u8; row];
            let mut cur = vec![0u8; row];
            for r in filtered.chunks_exact(row + 1) {
                cur.copy_from_slice(&r[1..]);
                unfilter_row(FilterType::from_u8(r[0]).unwrap(), &mut cur, &prev, bpp).unwrap();
                std::mem::swap(&mut prev, &mut cur);
            }
            prev[0]
        });
        line.push_str(&format!(
            " {t_unfilter:.1} | {}/{}/{}/{}/{} |",
            hist[0], hist[1], hist[2], hist[3], hist[4]
        ));
    } else {
        line.push_str(" — | — |");
    }
    println!("{line}");
}

fn bench_encode(name: &str, image: &PngImage, bundles: &[(String, EncodeOptions)]) {
    let mp = image.width() as f64 * image.height() as f64 / 1e6;
    let raw = image.as_bytes().unwrap();
    for (label, opts) in bundles {
        let mut bytes = Vec::new();
        let t = median_ms(|| bytes = encode(image, opts).unwrap());
        assert!(
            decode(&bytes).unwrap() == *image,
            "{name} {label}: round trip"
        );
        if let Ok(dir) = std::env::var("PNG_BENCH_OUT") {
            let file = format!("{dir}/{name}-{}.png", label.replace([' ', ','], "_"));
            std::fs::write(file, &bytes).expect("write PNG_BENCH_OUT file");
        }
        println!(
            "| {name} | {label} | {t:.1} | {:.1} | {} | {:.2} % |",
            mp / t * 1e3,
            bytes.len(),
            bytes.len() as f64 * 100.0 / raw.len() as f64
        );
    }
    // Stage: the §12.8 selection and the filter itself over every row.
    let row = image.stride();
    let bpp = image.bytes_per_pixel();
    if image.format().bits_per_sample() == 8 {
        let zero = vec![0u8; row];
        let mut scratch = vec![0u8; row];
        let t_pick = median_ms(|| {
            let mut prev: &[u8] = &zero;
            let mut acc = 0usize;
            for r in raw.chunks_exact(row) {
                acc += choose_filter_heuristic(r, prev, bpp, &mut scratch) as usize;
                prev = r;
            }
            acc
        });
        let mut out = vec![0u8; row];
        let t_paeth = median_ms(|| {
            let mut prev: &[u8] = &zero;
            for r in raw.chunks_exact(row) {
                filter_row(FilterType::Paeth, r, prev, bpp, &mut out);
                prev = r;
            }
            out[0]
        });
        println!(
            "| {name} | stage: §12.8 selection {t_pick:.1} ms, Paeth filter_row {t_paeth:.1} ms | | | | |"
        );
    }
}

fn main() {
    let only = std::env::var("PNG_BENCH_ONLY").unwrap_or_default();
    let threads: Vec<usize> = std::env::var("PNG_BENCH_THREADS")
        .ok()
        .map(|v| v.split(',').filter_map(|t| t.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1]);
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
    match std::env::var("PNG_BENCH_FILES") {
        Ok(list) if !list.trim().is_empty() => {
            for path in list.split(',') {
                let path = path.trim();
                let name = std::path::Path::new(path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string());
                inputs.push((
                    name,
                    std::fs::read(path).expect("read PNG_BENCH_FILES entry"),
                ));
            }
        }
        _ => {
            let fast = EncodeOptions::new();
            inputs.push((
                "synthetic photo".into(),
                encode(&synthetic_photo(2048, 1536), &fast).unwrap(),
            ));
            inputs.push((
                "synthetic screenshot".into(),
                encode(&synthetic_screenshot(2048, 1536), &fast).unwrap(),
            ));
        }
    }

    if only != "encode" {
        println!(
            "| file | layout | decode ms | MP/s | decode_rgba8 ms | MP/s | inflate alone ms | unfilter alone ms | rows None/Sub/Up/Avg/Paeth |"
        );
        println!("|---|---|---:|---:|---:|---:|---:|---:|---|");
        for (name, file) in &inputs {
            bench_decode(name, file);
        }
        println!();
    }
    if only != "decode" {
        let mut bundles: Vec<(String, EncodeOptions)> = Vec::new();
        for &t in &threads {
            let tag = if t > 1 {
                format!(", {t} threads")
            } else {
                String::new()
            };
            for (label, opts) in encode_bundles() {
                bundles.push((format!("{label}{tag}"), opts.with_threads(t)));
            }
        }
        println!("| file | options | encode ms | MP/s | bytes | of raw |");
        println!("|---|---|---:|---:|---:|---:|");
        for (name, file) in &inputs {
            let image = decode(file).unwrap();
            if image.format() == PngPixelFormat::Pal8 && image.palette.is_none() {
                continue;
            }
            bench_encode(name, &image, &bundles);
        }
    }
}

/// The option bundles of the encode table. `PNG_BENCH_GRID=1` replaces
/// them with every DEFLATE level under the two per-row selections and
/// two fixed filters (the measurement behind the effort ladder).
fn encode_bundles() -> Vec<(String, EncodeOptions)> {
    let fixed = |f| FilterStrategy::Fixed(f);
    if let Ok(list) = std::env::var("PNG_BENCH_EFFORTS") {
        return list
            .split(',')
            .filter_map(|e| e.trim().parse::<u8>().ok())
            .map(|e| (format!("effort {e}"), EncodeOptions::new().with_effort(e)))
            .collect();
    }
    if std::env::var("PNG_BENCH_GRID").is_ok() {
        let mut grid = Vec::new();
        for level in 1..=9u8 {
            for (name, strategy) in [
                ("None", fixed(FilterType::None)),
                ("Sub", fixed(FilterType::Sub)),
                ("adaptive", FilterStrategy::Adaptive),
                ("entropy", FilterStrategy::Entropy),
            ] {
                grid.push((
                    format!("level {level}, {name}"),
                    EncodeOptions::new()
                        .with_level(level)
                        .with_filter_strategy(strategy),
                ));
            }
        }
        return grid;
    }
    vec![
        (
            "level 1, filter None".into(),
            EncodeOptions::new()
                .with_level(1)
                .with_filter_strategy(fixed(FilterType::None)),
        ),
        (
            "level 1, filter Sub".into(),
            EncodeOptions::new()
                .with_level(1)
                .with_filter_strategy(fixed(FilterType::Sub)),
        ),
        (
            "level 1, filter Paeth".into(),
            EncodeOptions::new()
                .with_level(1)
                .with_filter_strategy(fixed(FilterType::Paeth)),
        ),
        (
            "level 1, adaptive".into(),
            EncodeOptions::new().with_level(1),
        ),
        ("default (level 2, adaptive)".into(), EncodeOptions::new()),
        (
            "level 4, adaptive".into(),
            EncodeOptions::new().with_level(4),
        ),
        (
            "level 6, adaptive".into(),
            EncodeOptions::new().with_level(6),
        ),
        (
            "level 2, entropy".into(),
            EncodeOptions::new().with_filter_strategy(FilterStrategy::Entropy),
        ),
        (
            "level 6, entropy".into(),
            EncodeOptions::new()
                .with_level(6)
                .with_filter_strategy(FilterStrategy::Entropy),
        ),
        (
            "level 9, adaptive".into(),
            EncodeOptions::new().with_level(9),
        ),
        (
            "level 9, entropy".into(),
            EncodeOptions::new()
                .with_level(9)
                .with_filter_strategy(FilterStrategy::Entropy),
        ),
    ]
}
