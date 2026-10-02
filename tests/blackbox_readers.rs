//! Black-box reader gate for the round-464 streaming / segmented
//! encoder: every option combination the encoder exposes is written
//! to a temporary file and handed to independent PNG readers that are
//! driven purely as binaries — ImageMagick's `magick` (raw sample dump
//! compared byte-for-byte against the source plane, plus `identify`)
//! and macOS `sips` (header parse) — on top of the crate's own decoder.
//!
//! The segmented IDAT stream (independent sync-flushed DEFLATE
//! segments inside one RFC 1950 container written by the crate) is
//! exactly the kind of thing a reader could reject while our own
//! inflater accepts it, so this gate is the one that matters for
//! "everything we write decodes everywhere".
//!
//! Each reader is optional: when its binary is not on `PATH` the
//! corresponding check is skipped with a message (CI hosts without
//! ImageMagick still pass), but the crate's own decoder always runs.

use std::path::PathBuf;
use std::process::Command;

use oxideav_png::{
    decode_png, encode_png_image_threaded, FilterStrategy, FilterType, PngEncoderOptions, PngImage,
    PngPixelFormat,
};

fn have(bin: &str, probe: &str) -> bool {
    Command::new(bin)
        .arg(probe)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Deterministic photographic-like plane (gradients + noise + repeated
/// rows so every filter type and the identical-row early-out fire).
fn synth(width: u32, height: u32, pf: PngPixelFormat) -> PngImage {
    let bpp = pf.bytes_per_pixel();
    let row_bytes = width as usize * bpp;
    let mut data = vec![0u8; row_bytes * height as usize];
    let mut s: u32 = 0x1357_9BDF ^ (width << 3) ^ height;
    for y in 0..height as usize {
        for x in 0..row_bytes {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let base = ((x / bpp) as u32 * 255 / width.max(1)) as u8;
            data[y * row_bytes + x] = base
                .wrapping_add((y as u8).wrapping_mul(5))
                .wrapping_add((s & 0x0f) as u8);
        }
        if y % 5 == 4 {
            let (prev, cur) = data.split_at_mut(y * row_bytes);
            cur[..row_bytes].copy_from_slice(&prev[(y - 1) * row_bytes..]);
        }
    }
    if pf == PngPixelFormat::Rgba || pf == PngPixelFormat::Ya8 {
        // Varying alpha, never fully transparent (some readers
        // premultiply or drop colour under alpha = 0).
        for px in data.chunks_exact_mut(bpp) {
            px[bpp - 1] = 64 + (px[0] >> 1);
        }
    }
    if pf == PngPixelFormat::Rgba64Le {
        for px in data.chunks_exact_mut(8) {
            px[6] = 0x40 + (px[0] >> 1);
            px[7] = 0xC0;
        }
    }
    PngImage::new(width, height, pf, row_bytes, data).with_palette(Vec::new())
}

/// ImageMagick raw-dump spec for a layout: (`magick` output format,
/// extra args). 16-bit layouts are dumped little-endian so they match
/// the crate's `*Le` plane layout directly.
fn magick_spec(pf: PngPixelFormat) -> Option<(&'static str, Vec<&'static str>)> {
    Some(match pf {
        PngPixelFormat::Rgb24 => ("rgb:-", vec!["-depth", "8"]),
        PngPixelFormat::Rgba => ("rgba:-", vec!["-depth", "8"]),
        PngPixelFormat::Gray8 => ("gray:-", vec!["-depth", "8"]),
        PngPixelFormat::Ya8 => ("graya:-", vec!["-depth", "8"]),
        PngPixelFormat::Gray16Le => ("gray:-", vec!["-depth", "16", "-endian", "LSB"]),
        PngPixelFormat::Rgb48Le => ("rgb:-", vec!["-depth", "16", "-endian", "LSB"]),
        PngPixelFormat::Rgba64Le => ("rgba:-", vec!["-depth", "16", "-endian", "LSB"]),
        _ => return None,
    })
}

struct Readers {
    magick: bool,
    sips: bool,
}

fn gate(readers: &Readers, label: &str, img: &PngImage, png: &[u8]) {
    // 1. Our own decoder, byte-exact.
    let back = decode_png(png).unwrap_or_else(|e| panic!("{label}: own decode failed: {e}"));
    assert_eq!(back.pixel_format, img.pixel_format, "{label}");
    assert_eq!(back.data, img.data, "{label}: own decode pixel mismatch");

    if !readers.magick && !readers.sips {
        return;
    }
    let dir = std::env::temp_dir().join(format!("oxideav-png-blackbox-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path: PathBuf = dir.join(format!("{}.png", label.replace(['/', ' '], "_")));
    std::fs::write(&path, png).expect("write png");

    if readers.magick {
        // 2a. `magick identify` must parse the file cleanly (exit 0).
        let id = Command::new("magick")
            .args(["identify", "-quiet"])
            .arg(&path)
            .output()
            .expect("run magick identify");
        assert!(
            id.status.success(),
            "{label}: magick identify failed: {}",
            String::from_utf8_lossy(&id.stderr)
        );
        // 2b. Raw sample dump must equal the source plane.
        if let Some((fmt, extra)) = magick_spec(img.pixel_format) {
            let mut cmd = Command::new("magick");
            cmd.arg(&path).args(&extra).arg(fmt);
            let out = cmd.output().expect("run magick dump");
            assert!(
                out.status.success(),
                "{label}: magick dump failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                out.stdout.len(),
                img.data.len(),
                "{label}: magick dump length"
            );
            assert!(
                out.stdout == img.data,
                "{label}: magick dump pixel mismatch"
            );
        }
    }
    if readers.sips {
        // 3. `sips` (macOS ImageIO) must read the header and report the
        //    declared geometry.
        let out = Command::new("sips")
            .args(["-g", "pixelWidth", "-g", "pixelHeight"])
            .arg(&path)
            .output()
            .expect("run sips");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && text.contains(&format!("pixelWidth: {}", img.width)),
            "{label}: sips rejected the file: {text} {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn every_option_combination_reads_back_in_black_box_readers() {
    let readers = Readers {
        magick: have("magick", "-version"),
        sips: have("sips", "-h"),
    };
    if !readers.magick {
        eprintln!("blackbox_readers: `magick` not found — ImageMagick checks skipped");
    }
    if !readers.sips {
        eprintln!("blackbox_readers: `sips` not found — ImageIO checks skipped");
    }

    let layouts = [
        PngPixelFormat::Rgb24,
        PngPixelFormat::Rgba,
        PngPixelFormat::Gray8,
        PngPixelFormat::Ya8,
        PngPixelFormat::Gray16Le,
        PngPixelFormat::Rgb48Le,
        PngPixelFormat::Rgba64Le,
    ];
    let filters = [
        ("adaptive", FilterStrategy::Adaptive),
        ("paeth", FilterStrategy::Fixed(FilterType::Paeth)),
        ("sub", FilterStrategy::Fixed(FilterType::Sub)),
        ("none", FilterStrategy::Fixed(FilterType::None)),
    ];
    let mut combos = 0usize;
    for pf in layouts {
        // Rows sized so the non-interlaced stream spans ≥ 3 segments
        // (≈ 2.5 MB of wire rows) — the multi-segment framing is what
        // the external readers must accept.
        let width = 1024u32;
        let height = (2_500_000 / (width as usize * pf.bytes_per_pixel())).max(32) as u32;
        let img = synth(width, height, pf);
        for level in [1u8, 2, 6] {
            for (fname, strategy) in filters {
                for interlace in [false, true] {
                    if interlace && level == 6 {
                        continue; // keep the matrix quick; level 1/2 cover Adam7
                    }
                    let opts = PngEncoderOptions::default()
                        .with_interlace(interlace)
                        .with_filter_strategy(strategy)
                        .with_compression_level(Some(level));
                    let png = encode_png_image_threaded(&img, &opts, 8).expect("encode");
                    let label = format!(
                        "{pf:?}/l{level}/{fname}/{}",
                        if interlace { "adam7" } else { "rows" }
                    );
                    gate(&readers, &label, &img, &png);
                    combos += 1;
                }
            }
        }
    }
    // Brute once per layout family (expensive: six deflates each).
    for pf in [PngPixelFormat::Rgb24, PngPixelFormat::Gray16Le] {
        let img = synth(640, 400, pf);
        let opts = PngEncoderOptions::default()
            .with_filter_strategy(FilterStrategy::Brute)
            .with_compression_level(Some(1));
        let png = encode_png_image_threaded(&img, &opts, 4).expect("encode");
        gate(&readers, &format!("{pf:?}/brute"), &img, &png);
        combos += 1;
    }
    eprintln!("blackbox_readers: {combos} combinations gated");
}
