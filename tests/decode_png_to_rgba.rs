//! `decode_png_to_rgba` covers each input pixel format the standalone
//! decoder can produce, promoting them to a uniform 8-bit RGBA bitmap
//! ready to blit. Tests below build a synthetic source image per
//! pixel format, encode it via the standalone encoder, then re-decode
//! through `decode_png_to_rgba` and check the post-promotion bytes.
//!
//! Coverage matrix (one test per row):
//!   - Gray8     → (g,g,g,255)
//!   - Gray16Le  → (hi,hi,hi,255)
//!   - Rgb24     → (r,g,b,255)
//!   - Rgb48Le   → (r_hi,g_hi,b_hi,255)
//!   - Pal8      → PLTE lookup, no tRNS
//!   - Pal8 + tRNS → PLTE lookup with per-entry alpha
//!   - Ya8       → (g,g,g,a)
//!   - Rgba      → identity
//!   - Rgba64Le  → (r_hi,g_hi,b_hi,a_hi)

use oxideav_png::{decode_png_to_rgba, encode_png_image, PngImage, PngPixelFormat};

fn make(w: u32, h: u32, pf: PngPixelFormat, data: Vec<u8>, palette: Vec<u8>) -> PngImage {
    let bpp = pf.bytes_per_pixel();
    PngImage {
        width: w,
        height: h,
        pixel_format: pf,
        stride: w as usize * bpp,
        data,
        palette,
    }
}

#[test]
fn gray8_to_rgba_alpha_255_grey_replicated() {
    // 4x2 ramp.
    let raw: Vec<u8> = (0..8u8).map(|x| x * 10).collect();
    let img = make(4, 2, PngPixelFormat::Gray8, raw.clone(), Vec::new());
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    assert_eq!(rgba.width, 4);
    assert_eq!(rgba.height, 2);
    assert_eq!(rgba.data.len(), 4 * 2 * 4);
    for (i, &g) in raw.iter().enumerate() {
        assert_eq!(rgba.data[i * 4], g);
        assert_eq!(rgba.data[i * 4 + 1], g);
        assert_eq!(rgba.data[i * 4 + 2], g);
        assert_eq!(rgba.data[i * 4 + 3], 255);
    }
}

#[test]
fn gray16le_to_rgba_high_byte_replicated() {
    // 2x2, four 16-bit grays. Stored LE: (lo, hi).
    let samples: [u16; 4] = [0x0001, 0x00ff, 0x8000, 0xff00];
    let mut raw = Vec::with_capacity(8);
    for s in samples {
        raw.push((s & 0xff) as u8);
        raw.push((s >> 8) as u8);
    }
    let img = make(2, 2, PngPixelFormat::Gray16Le, raw, Vec::new());
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    assert_eq!(rgba.width, 2);
    assert_eq!(rgba.height, 2);
    assert_eq!(rgba.data.len(), 16);
    for (i, s) in samples.iter().enumerate() {
        let g = (s >> 8) as u8;
        assert_eq!(rgba.data[i * 4], g, "px{i} R");
        assert_eq!(rgba.data[i * 4 + 1], g, "px{i} G");
        assert_eq!(rgba.data[i * 4 + 2], g, "px{i} B");
        assert_eq!(rgba.data[i * 4 + 3], 255, "px{i} A");
    }
}

#[test]
fn rgb24_to_rgba_alpha_255() {
    // 2x2 RGB tiles.
    let raw: Vec<u8> = vec![
        255, 0, 0, // red
        0, 255, 0, // green
        0, 0, 255, // blue
        128, 64, 32, // brownish
    ];
    let img = make(2, 2, PngPixelFormat::Rgb24, raw.clone(), Vec::new());
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    assert_eq!(rgba.data.len(), 16);
    for i in 0..4 {
        assert_eq!(rgba.data[i * 4], raw[i * 3]);
        assert_eq!(rgba.data[i * 4 + 1], raw[i * 3 + 1]);
        assert_eq!(rgba.data[i * 4 + 2], raw[i * 3 + 2]);
        assert_eq!(rgba.data[i * 4 + 3], 255);
    }
}

#[test]
fn rgb48le_to_rgba_high_byte_per_channel() {
    // 1x2 RGB48 LE.
    let samples: [(u16, u16, u16); 2] = [(0x1122, 0x3344, 0x5566), (0xff00, 0x00ff, 0xa5a5)];
    let mut raw = Vec::with_capacity(12);
    for (r, g, b) in samples {
        raw.push((r & 0xff) as u8);
        raw.push((r >> 8) as u8);
        raw.push((g & 0xff) as u8);
        raw.push((g >> 8) as u8);
        raw.push((b & 0xff) as u8);
        raw.push((b >> 8) as u8);
    }
    let img = make(1, 2, PngPixelFormat::Rgb48Le, raw, Vec::new());
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    assert_eq!(rgba.data.len(), 8);
    for (i, (r, g, b)) in samples.iter().enumerate() {
        assert_eq!(rgba.data[i * 4], (r >> 8) as u8, "px{i} R");
        assert_eq!(rgba.data[i * 4 + 1], (g >> 8) as u8, "px{i} G");
        assert_eq!(rgba.data[i * 4 + 2], (b >> 8) as u8, "px{i} B");
        assert_eq!(rgba.data[i * 4 + 3], 255, "px{i} A");
    }
}

#[test]
fn pal8_no_trns_to_rgba_alpha_255() {
    // 4-entry palette: black / red / green / blue.
    let palette: Vec<u8> = vec![
        0, 0, 0, // 0 black
        255, 0, 0, // 1 red
        0, 255, 0, // 2 green
        0, 0, 255, // 3 blue
    ];
    // 2x2 picking each entry.
    let raw: Vec<u8> = vec![0, 1, 2, 3];
    let img = make(2, 2, PngPixelFormat::Pal8, raw, palette);
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    let expected: Vec<u8> = vec![
        0, 0, 0, 255, // black
        255, 0, 0, 255, // red
        0, 255, 0, 255, // green
        0, 0, 255, 255, // blue
    ];
    assert_eq!(rgba.data, expected);
}

#[test]
fn pal8_with_trns_to_rgba_per_entry_alpha() {
    // 4-entry palette + tRNS for first 3 entries (entry 3 stays opaque).
    // Layout: PLTE (12 bytes) || tRNS (3 bytes).
    let mut palette: Vec<u8> = vec![
        10, 20, 30, // 0
        40, 50, 60, // 1
        70, 80, 90, // 2
        200, 210, 220, // 3
    ];
    palette.extend_from_slice(&[0u8, 64, 200]); // tRNS for entries 0,1,2

    // 4 pixels, one per palette entry.
    let raw: Vec<u8> = vec![0, 1, 2, 3];
    let img = make(2, 2, PngPixelFormat::Pal8, raw, palette);
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    let expected: Vec<u8> = vec![
        10, 20, 30, 0, // entry 0, alpha 0
        40, 50, 60, 64, // entry 1, alpha 64
        70, 80, 90, 200, // entry 2, alpha 200
        200, 210, 220, 255, // entry 3, no tRNS → opaque
    ];
    assert_eq!(rgba.data, expected);
}

#[test]
fn ya8_to_rgba_grey_with_alpha() {
    // 2x1 grey+alpha pixels.
    let raw: Vec<u8> = vec![100, 50, 200, 255];
    let img = make(2, 1, PngPixelFormat::Ya8, raw, Vec::new());
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    let expected: Vec<u8> = vec![
        100, 100, 100, 50, // px0
        200, 200, 200, 255, // px1
    ];
    assert_eq!(rgba.data, expected);
}

#[test]
fn rgba_to_rgba_identity() {
    // 2x2 RGBA. Decoded bytes should match input verbatim.
    let raw: Vec<u8> = vec![
        255, 0, 0, 200, // red translucent
        0, 255, 0, 100, // green translucent
        0, 0, 255, 50, // blue translucent
        128, 128, 128, 255, // grey opaque
    ];
    let img = make(2, 2, PngPixelFormat::Rgba, raw.clone(), Vec::new());
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    assert_eq!(rgba.data, raw);
}

#[test]
fn rgba64le_to_rgba_high_byte_per_channel() {
    // 1x2 RGBA64 LE.
    let samples: [(u16, u16, u16, u16); 2] = [
        (0x1100, 0x2200, 0x3300, 0x4400),
        (0xff80, 0x80ff, 0x00ff, 0xff00),
    ];
    let mut raw = Vec::with_capacity(16);
    for (r, g, b, a) in samples {
        raw.push((r & 0xff) as u8);
        raw.push((r >> 8) as u8);
        raw.push((g & 0xff) as u8);
        raw.push((g >> 8) as u8);
        raw.push((b & 0xff) as u8);
        raw.push((b >> 8) as u8);
        raw.push((a & 0xff) as u8);
        raw.push((a >> 8) as u8);
    }
    let img = make(1, 2, PngPixelFormat::Rgba64Le, raw, Vec::new());
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    assert_eq!(rgba.data.len(), 8);
    for (i, (r, g, b, a)) in samples.iter().enumerate() {
        assert_eq!(rgba.data[i * 4], (r >> 8) as u8, "px{i} R");
        assert_eq!(rgba.data[i * 4 + 1], (g >> 8) as u8, "px{i} G");
        assert_eq!(rgba.data[i * 4 + 2], (b >> 8) as u8, "px{i} B");
        assert_eq!(rgba.data[i * 4 + 3], (a >> 8) as u8, "px{i} A");
    }
}

#[test]
fn rgba_bitmap_stride_helper() {
    // Tiny smoke-test of the stride() helper (always 4*width).
    let raw: Vec<u8> = vec![1; 16];
    let img = make(2, 2, PngPixelFormat::Rgba, raw, Vec::new());
    let bytes = encode_png_image(&img).expect("encode");
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    assert_eq!(rgba.stride(), 8);
}
