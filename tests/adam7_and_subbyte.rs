//! Adam7 interlace decode + sub-8-bit (bit depth 1/2/4) indexed/grayscale
//! decode.
//!
//! We hand-build the PNG byte streams because the encoder doesn't produce
//! Adam7 or sub-byte output — decoding those formats is purely a decoder
//! capability.

use miniz_oxide::deflate::compress_to_vec_zlib;
use oxideav_core::{PixelFormat, TimeBase};

use oxideav_png::chunk::{write_chunk, PNG_MAGIC};
use oxideav_png::decoder::{decode_png_to_frame, Ihdr};

// ---- shared helpers -----------------------------------------------------

fn build_png_file(ihdr: &Ihdr, idat: &[u8], plte: Option<&[u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&PNG_MAGIC);
    write_chunk(&mut out, b"IHDR", &ihdr.to_bytes());
    if let Some(p) = plte {
        write_chunk(&mut out, b"PLTE", p);
    }
    write_chunk(&mut out, b"IDAT", idat);
    write_chunk(&mut out, b"IEND", &[]);
    out
}

// ---- Adam7 (8×8 RGB 8-bit) ----------------------------------------------

/// Pixel pattern (RGB8) used for the Adam7 test.
fn rgb_pattern(x: usize, y: usize) -> [u8; 3] {
    [(x * 31) as u8, (y * 31) as u8, ((x + y) * 15) as u8]
}

/// Table E.3 pass table. (starting_row, starting_col, row_spacing, col_spacing).
const PASS: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (0, 4, 8, 8),
    (4, 0, 8, 4),
    (0, 2, 4, 4),
    (2, 0, 4, 2),
    (0, 1, 2, 2),
    (1, 0, 2, 1),
];

fn pass_dims(img_w: usize, img_h: usize, pass: usize) -> (usize, usize) {
    let (sr, sc, rs, cs) = PASS[pass];
    let pw = if img_w > sc {
        (img_w - sc).div_ceil(cs)
    } else {
        0
    };
    let ph = if img_h > sr {
        (img_h - sr).div_ceil(rs)
    } else {
        0
    };
    (pw, ph)
}

#[test]
fn adam7_rgb_8x8_matches_noninterlaced() {
    let w = 8usize;
    let h = 8usize;

    // Build the zlib-compressed stream of seven passes, each row prefixed
    // with the filter byte (0 = None).
    let mut raw = Vec::new();
    for (pass, &(sr, sc, rs, cs)) in PASS.iter().enumerate() {
        let (pw, ph) = pass_dims(w, h, pass);
        if pw == 0 || ph == 0 {
            continue;
        }
        for py in 0..ph {
            raw.push(0); // filter: None
            for px in 0..pw {
                let x = sc + px * cs;
                let y = sr + py * rs;
                raw.extend_from_slice(&rgb_pattern(x, y));
            }
        }
    }
    let idat = compress_to_vec_zlib(&raw, 6);

    let ihdr = Ihdr {
        width: w as u32,
        height: h as u32,
        bit_depth: 8,
        colour_type: 2, // RGB
        compression: 0,
        filter: 0,
        interlace: 1, // Adam7
    };
    let png = build_png_file(&ihdr, &idat, None);

    let vf = decode_png_to_frame(&png, Some(0), TimeBase::new(1, 100)).expect("adam7 decode");
    assert_eq!(vf.format, PixelFormat::Rgb24);
    assert_eq!(vf.width as usize, w);
    assert_eq!(vf.height as usize, h);
    assert_eq!(vf.planes[0].stride, w * 3);

    // Verify every pixel round-trips.
    let data = &vf.planes[0].data;
    for y in 0..h {
        for x in 0..w {
            let px = &data[y * w * 3 + x * 3..y * w * 3 + x * 3 + 3];
            assert_eq!(
                px,
                rgb_pattern(x, y).as_slice(),
                "pixel mismatch at ({x},{y})"
            );
        }
    }
}

// ---- 2-bit indexed (16×16) ----------------------------------------------

#[test]
fn indexed_2bit_16x16_unpacks_correctly() {
    let w = 16usize;
    let h = 16usize;

    // Palette: 4 entries (black, red, green, blue).
    let palette: [u8; 12] = [
        0, 0, 0, //
        255, 0, 0, //
        0, 255, 0, //
        0, 0, 255, //
    ];

    // Build per-pixel palette indices (0..=3).
    let expected: Vec<u8> = (0..(w * h))
        .map(|i| {
            let x = i % w;
            let y = i / w;
            ((x + y) % 4) as u8
        })
        .collect();

    // Pack 4 pixels per byte, MSB-first.
    // row_bytes = ceil(w * 2 / 8) = w / 4.
    let row_bytes = (w * 2).div_ceil(8);
    assert_eq!(row_bytes, 4);
    let mut raw = Vec::with_capacity((1 + row_bytes) * h);
    for y in 0..h {
        raw.push(0); // filter: None
        for bx in 0..row_bytes {
            let p0 = expected[y * w + bx * 4];
            let p1 = expected[y * w + bx * 4 + 1];
            let p2 = expected[y * w + bx * 4 + 2];
            let p3 = expected[y * w + bx * 4 + 3];
            let byte = (p0 << 6) | (p1 << 4) | (p2 << 2) | p3;
            raw.push(byte);
        }
    }
    let idat = compress_to_vec_zlib(&raw, 6);

    let ihdr = Ihdr {
        width: w as u32,
        height: h as u32,
        bit_depth: 2,
        colour_type: 3, // indexed
        compression: 0,
        filter: 0,
        interlace: 0,
    };
    let png = build_png_file(&ihdr, &idat, Some(&palette));

    let vf = decode_png_to_frame(&png, Some(0), TimeBase::new(1, 100)).expect("2-bit decode");
    assert_eq!(vf.format, PixelFormat::Pal8);
    assert_eq!(vf.width as usize, w);
    assert_eq!(vf.height as usize, h);
    assert_eq!(vf.planes[0].stride, w);
    assert_eq!(vf.planes[0].data, expected);
}

#[test]
fn grayscale_4bit_scales_to_gray8() {
    // 4×2 grayscale 4-bit image. Values 0..=15 scale to 0..=255 (×17).
    let w = 4usize;
    let h = 2usize;
    let src_vals: [u8; 8] = [0, 1, 7, 15, 8, 4, 15, 0];
    // Pack two pixels per byte (high nibble first).
    let row_bytes = (w * 4).div_ceil(8); // = 2
    let mut raw = Vec::with_capacity((1 + row_bytes) * h);
    for y in 0..h {
        raw.push(0);
        for bx in 0..row_bytes {
            let p0 = src_vals[y * w + bx * 2];
            let p1 = src_vals[y * w + bx * 2 + 1];
            raw.push((p0 << 4) | p1);
        }
    }
    let idat = compress_to_vec_zlib(&raw, 6);
    let ihdr = Ihdr {
        width: w as u32,
        height: h as u32,
        bit_depth: 4,
        colour_type: 0,
        compression: 0,
        filter: 0,
        interlace: 0,
    };
    let png = build_png_file(&ihdr, &idat, None);
    let vf = decode_png_to_frame(&png, Some(0), TimeBase::new(1, 100)).expect("4-bit gray decode");
    assert_eq!(vf.format, PixelFormat::Gray8);
    let expected: Vec<u8> = src_vals.iter().map(|&v| v.wrapping_mul(17)).collect();
    assert_eq!(vf.planes[0].data, expected);
}
