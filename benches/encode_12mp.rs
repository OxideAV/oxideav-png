//! Production-scale encode / decode timing table at 4032×3024 (12 MP).
//!
//! Round 464: the HEIF → PNG conversion path spends almost all of its
//! time in the PNG encoder, so this harness measures the one thing that
//! matters for that path — wall-clock milliseconds and output bytes for
//! a 12 MP frame under every (pixel layout × DEFLATE level × filter
//! strategy) combination a caller can select — and prints a Markdown
//! table instead of Criterion's per-scenario statistics. It is a plain
//! `fn main` harness (`harness = false`) so it runs with
//!
//!     cargo bench -p oxideav-png --bench encode_12mp
//!
//! and takes an optional filter argument (`-- rgb24` runs only the
//! layouts whose name contains `rgb24`). Set `PNG_BENCH_REPS=n` to
//! change the repetition count (default 3, min-of-n reported),
//! `PNG_BENCH_LEVELS=1,3,6` to pick the DEFLATE levels, and
//! `PNG_BENCH_THREADS=1,8` to pick the thread budgets,
//! `PNG_BENCH_FILTERS=adaptive,paeth` to pick the filter strategies, and
//! `PNG_BENCH_RAW=/path/to/4032x3024.rgb` to replace the synthetic
//! "photo" content with a real 8-bit RGB24 raster (the other layouts
//! are derived from it: alpha = 255, gray = BT.601 luma, 16-bit =
//! `v << 8 | v`).
//!
//! Two synthetic contents are measured per layout:
//!
//! * **photo** — smooth low-frequency gradients plus per-pixel noise of
//!   a few LSBs, the texture a demosaiced camera frame has after
//!   colour conversion (the case the §12.8 heuristic and the DEFLATE
//!   level are tuned against);
//! * **flat** — a handful of large constant-colour regions, the
//!   screenshot / rendered-graphics case where rows repeat and the
//!   `Up` early-out dominates.
//!
//! Every emitted stream is decoded back and compared byte-for-byte
//! against the source so a speed-up that silently mis-encodes fails
//! loudly instead of producing a flattering number.

use std::time::Instant;

use oxideav_png::filter::{choose_filter_heuristic, filter_row};
use oxideav_png::{
    decode_png, encode_png_image_threaded, FilterStrategy, FilterType, PngEncoderOptions, PngImage,
    PngPixelFormat,
};

const WIDTH: u32 = 4032;
const HEIGHT: u32 = 3024;

#[inline]
fn hash(x: u32, y: u32, c: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B) ^ c.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h
}

/// Photographic-like sample in 0..=65535 for channel `c` at (x, y):
/// three overlapping gradients + a slow sinusoid-free "bump" + 4 LSBs
/// (of 8-bit) of noise.
#[inline]
fn photo_sample(x: u32, y: u32, c: u32) -> u16 {
    let gx = (x * 255 / WIDTH) as i32;
    let gy = (y * 255 / HEIGHT) as i32;
    let base = match c {
        0 => (gx + gy) / 2,
        1 => gy,
        2 => 255 - gx,
        _ => 255,
    };
    // A ring of brighter "subject" in the middle of the frame.
    let dx = x as i32 - WIDTH as i32 / 2;
    let dy = y as i32 - HEIGHT as i32 / 2;
    let r2 = (dx * dx + dy * dy) / 4096;
    let bump = if r2 < 600 { (600 - r2) / 8 } else { 0 };
    let noise = (hash(x, y, c) & 0x0f) as i32 - 8;
    let v8 = (base + bump + noise).clamp(0, 255) as u32;
    // Spread to 16 bits with low-order noise so 16-bit layouts are not
    // trivially compressible.
    ((v8 << 8) | (hash(x ^ 0x5555, y, c) & 0xff)) as u16
}

#[inline]
fn flat_sample(x: u32, y: u32, c: u32) -> u16 {
    let cell = ((x / 503) * 7 + (y / 311) * 3) % 11;
    let v8 = match c {
        0 => cell * 23,
        1 => 255 - cell * 19,
        2 => cell * 11 + 40,
        _ => 255,
    };
    ((v8 & 0xff) << 8) as u16 | (v8 & 0xff) as u16
}

/// Optional real-photo source (`PNG_BENCH_RAW`): 4032×3024 RGB24 raw.
fn load_raw() -> Option<Vec<u8>> {
    let path = std::env::var("PNG_BENCH_RAW").ok()?;
    let raw = std::fs::read(&path).expect("read PNG_BENCH_RAW");
    assert_eq!(
        raw.len(),
        WIDTH as usize * HEIGHT as usize * 3,
        "PNG_BENCH_RAW must be {WIDTH}x{HEIGHT} RGB24"
    );
    Some(raw)
}

fn build(pf: PngPixelFormat, photo: bool, raw: Option<&[u8]>) -> PngImage {
    let (channels, sixteen) = match pf {
        PngPixelFormat::Gray8 => (1, false),
        PngPixelFormat::Gray16Le => (1, true),
        PngPixelFormat::Rgb24 => (3, false),
        PngPixelFormat::Rgb48Le => (3, true),
        PngPixelFormat::Rgba => (4, false),
        PngPixelFormat::Rgba64Le => (4, true),
        PngPixelFormat::Ya8 => (2, false),
        PngPixelFormat::Pal8 => (1, false),
    };
    let bpp = channels * if sixteen { 2 } else { 1 };
    let stride = WIDTH as usize * bpp;
    let mut data = vec![0u8; stride * HEIGHT as usize];
    for y in 0..HEIGHT {
        let row = &mut data[y as usize * stride..(y as usize + 1) * stride];
        for x in 0..WIDTH {
            for c in 0..channels {
                let s = match (photo, raw) {
                    (true, Some(raw)) => {
                        let p = &raw[(y as usize * WIDTH as usize + x as usize) * 3..][..3];
                        let v8 = match (channels, c) {
                            (1, _) => {
                                ((77 * p[0] as u32 + 150 * p[1] as u32 + 29 * p[2] as u32) >> 8)
                                    as u16
                            }
                            (_, 3) => 255,
                            (_, c) => p[c] as u16,
                        };
                        (v8 << 8) | v8
                    }
                    (true, None) => photo_sample(x, y, c as u32),
                    (false, _) => flat_sample(x, y, c as u32),
                };
                let i = x as usize * bpp + c * if sixteen { 2 } else { 1 };
                if sixteen {
                    row[i] = (s & 0xff) as u8;
                    row[i + 1] = (s >> 8) as u8;
                } else {
                    row[i] = (s >> 8) as u8;
                }
            }
        }
    }
    PngImage {
        width: WIDTH,
        height: HEIGHT,
        pixel_format: pf,
        stride,
        data,
        palette: Vec::new(),
    }
}

/// Stage breakdown for one layout: §12.8 heuristic alone, a fixed
/// `Paeth` `filter_row` pass alone, and the zlib pass over the already
/// filtered stream at each level — so the encode total can be
/// attributed. Printed when `PNG_BENCH_STAGES` is set.
fn stages(name: &str, img: &PngImage, levels: &[Option<u8>]) {
    use compcol::zlib::{EncoderConfig, Zlib};
    let bpp = img.bytes_per_pixel();
    let rb = img.stride;
    let h = img.height as usize;
    let zero = vec![0u8; rb];
    let mut scratch = vec![0u8; rb];
    let t = Instant::now();
    let mut picks = [0usize; 5];
    for y in 0..h {
        let row = &img.data[y * rb..(y + 1) * rb];
        let prev = if y == 0 {
            &zero[..]
        } else {
            &img.data[(y - 1) * rb..y * rb]
        };
        picks[choose_filter_heuristic(row, prev, bpp, &mut scratch) as usize] += 1;
    }
    let heur_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut filtered = vec![0u8; (rb + 1) * h];
    let t = Instant::now();
    for y in 0..h {
        let row = &img.data[y * rb..(y + 1) * rb];
        let prev = if y == 0 {
            &zero[..]
        } else {
            &img.data[(y - 1) * rb..y * rb]
        };
        let dst = &mut filtered[y * (rb + 1)..(y + 1) * (rb + 1)];
        dst[0] = FilterType::Paeth as u8;
        filter_row(FilterType::Paeth, row, prev, bpp, &mut dst[1..]);
    }
    let paeth_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "| {name} | stages | heuristic {heur_ms:.0} ms (picks none/sub/up/avg/paeth = {picks:?}) | paeth filter_row {paeth_ms:.0} ms |"
    );
    for &level in levels {
        let level = level.unwrap_or(6);
        let t = Instant::now();
        let z = compcol::vec::compress_to_vec_with::<Zlib>(&filtered, EncoderConfig { level })
            .expect("zlib");
        let ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "| {name} | stages | zlib level {level} over paeth stream | {ms:.0} ms | {} bytes |",
            z.len()
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("--"))
        .collect();
    let reps: usize = std::env::var("PNG_BENCH_REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let layouts = [
        ("rgb24", PngPixelFormat::Rgb24),
        ("rgba", PngPixelFormat::Rgba),
        ("gray8", PngPixelFormat::Gray8),
        ("rgb48", PngPixelFormat::Rgb48Le),
        ("gray16", PngPixelFormat::Gray16Le),
    ];
    let levels: Vec<Option<u8>> = std::env::var("PNG_BENCH_LEVELS")
        .ok()
        .map(|s| s.split(',').map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![Some(1), Some(2), Some(3), Some(4), Some(6), Some(9)]);
    let thread_budgets: Vec<usize> = std::env::var("PNG_BENCH_THREADS")
        .ok()
        .map(|s| s.split(',').filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1]);
    let filter_names = std::env::var("PNG_BENCH_FILTERS").unwrap_or_default();
    let filters: Vec<(&str, FilterStrategy)> = [
        ("adaptive", FilterStrategy::Adaptive),
        ("paeth", FilterStrategy::Fixed(FilterType::Paeth)),
        ("sub", FilterStrategy::Fixed(FilterType::Sub)),
        ("none", FilterStrategy::Fixed(FilterType::None)),
    ]
    .into_iter()
    .filter(|(n, _)| filter_names.is_empty() || filter_names.split(',').any(|f| f.trim() == *n))
    .collect();
    let raw = load_raw();
    let photo_label = if raw.is_some() { "real" } else { "photo" };
    println!(
        "| layout | content | level | filter | threads | encode ms | bytes | ratio | decode ms |"
    );
    println!("|---|---|---|---|---:|---:|---:|---:|---:|");
    for (name, pf) in layouts {
        if !args.is_empty() && !args.iter().any(|a| name.contains(a.as_str())) {
            continue;
        }
        for photo in [true, false] {
            let img = build(pf, photo, raw.as_deref());
            if std::env::var_os("PNG_BENCH_STAGES").is_some() {
                stages(name, &img, &levels);
            }
            let raw_len = img.data.len();
            for &level in &levels {
                for (fname, strategy) in &filters {
                    for &threads in &thread_budgets {
                        let opts = PngEncoderOptions {
                            compression_level: level,
                            filter_strategy: *strategy,
                            ..Default::default()
                        };
                        let mut best_enc = f64::MAX;
                        let mut best_dec = f64::MAX;
                        let mut bytes = 0usize;
                        for _ in 0..reps {
                            let t = Instant::now();
                            let png =
                                encode_png_image_threaded(&img, &opts, threads).expect("encode");
                            best_enc = best_enc.min(t.elapsed().as_secs_f64() * 1e3);
                            bytes = png.len();
                            let t = Instant::now();
                            let back = decode_png(&png).expect("decode");
                            best_dec = best_dec.min(t.elapsed().as_secs_f64() * 1e3);
                            assert_eq!(back.data, img.data, "{name} {fname} round-trip mismatch");
                        }
                        println!(
                        "| {name} | {} | {} | {fname} | {threads} | {best_enc:.0} | {bytes} | {:.1}% | {best_dec:.0} |",
                        if photo { photo_label } else { "flat" },
                        level.map_or("default".to_string(), |l| l.to_string()),
                        bytes as f64 * 100.0 / raw_len as f64
                    );
                    }
                }
            }
        }
    }
}
