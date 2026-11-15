//! Every colour type × bit depth of W3C PNG3 §11.2.1 Table 12, plain
//! and Adam7-interlaced, at every size from 1×1 to 9×9 (the sizes where
//! interlace passes are empty, one pixel wide, or end mid-byte).
//!
//! The decoder is checked against files this test builds by hand —
//! samples packed and interlaced by the test's own code from the staged
//! pass table, filter type 0, compressed with the workspace compression
//! crate — so the expected pixels never come from the crate under test.
//! The encoder is then checked by decoding what it writes, under every
//! row-filter policy.

use oxideav_png::chunk::write_chunk;
use oxideav_png::{
    EncodeOptions, FilterStrategy, FilterType, Palette, PngImage, PngPixelFormat, decode,
    decode_rgb8, decode_rgba8, encode,
};

/// (colour type, bit depth) pairs of Table 12.
const LAYOUTS: [(u8, u8); 15] = [
    (0, 1),
    (0, 2),
    (0, 4),
    (0, 8),
    (0, 16),
    (2, 8),
    (2, 16),
    (3, 1),
    (3, 2),
    (3, 4),
    (3, 8),
    (4, 8),
    (4, 16),
    (6, 8),
    (6, 16),
];

/// Adam7 passes as (x start, y start, x step, y step) — the staged
/// `adam7-pass-*` tables.
const PASSES: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

fn channels(colour_type: u8) -> usize {
    match colour_type {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        _ => 4,
    }
}

/// Sample `c` of pixel (x, y), within `depth` bits.
fn sample(x: usize, y: usize, c: usize, depth: u8, seed: usize) -> u16 {
    let v = (x * 7 + y * 13 + c * 5 + seed * 3 + x * y) as u32;
    let v = v.wrapping_mul(2_654_435_761) >> 13;
    (v & ((1u32 << depth) - 1)) as u16
}

/// Pack one row of pixels (`xs` = the x coordinates on this row).
fn pack_row(xs: &[usize], y: usize, ct: u8, depth: u8, seed: usize, out: &mut Vec<u8>) {
    let ch = channels(ct);
    if depth >= 8 {
        for &x in xs {
            for c in 0..ch {
                let s = sample(x, y, c, depth, seed);
                if depth == 16 {
                    out.extend_from_slice(&s.to_be_bytes());
                } else {
                    out.push(s as u8);
                }
            }
        }
        return;
    }
    // Sub-byte: leftmost pixel in the high-order bits.
    let (mut acc, mut bits) = (0u8, 0u8);
    for &x in xs {
        acc = (acc << depth) | sample(x, y, 0, depth, seed) as u8;
        bits += depth;
        if bits == 8 {
            out.push(acc);
            (acc, bits) = (0, 0);
        }
    }
    if bits > 0 {
        out.push(acc << (8 - bits));
    }
}

fn palette_rgba(depth: u8) -> Vec<[u8; 4]> {
    (0..1usize << depth)
        .map(|i| {
            let i = i as u8;
            [
                i.wrapping_mul(37),
                i.wrapping_mul(91) ^ 0x55,
                !i,
                if i.is_multiple_of(3) { i } else { 255 },
            ]
        })
        .collect()
}

/// A complete PNG file for the layout, by hand.
fn build_file(w: usize, h: usize, ct: u8, depth: u8, interlace: bool, seed: usize) -> Vec<u8> {
    let mut raw = Vec::new();
    if interlace {
        for (x0, y0, dx, dy) in PASSES {
            let xs: Vec<usize> = (x0..w).step_by(dx).collect();
            if xs.is_empty() {
                continue;
            }
            for y in (y0..h).step_by(dy) {
                raw.push(0);
                pack_row(&xs, y, ct, depth, seed, &mut raw);
            }
        }
    } else {
        let xs: Vec<usize> = (0..w).collect();
        for y in 0..h {
            raw.push(0);
            pack_row(&xs, y, ct, depth, seed, &mut raw);
        }
    }
    let mut out = vec![137, 80, 78, 71, 13, 10, 26, 10];
    let mut ihdr = (w as u32).to_be_bytes().to_vec();
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[depth, ct, 0, 0, u8::from(interlace)]);
    write_chunk(&mut out, b"IHDR", &ihdr);
    if ct == 3 {
        let pal = palette_rgba(depth);
        let plte: Vec<u8> = pal.iter().flat_map(|e| [e[0], e[1], e[2]]).collect();
        let trns: Vec<u8> = pal.iter().map(|e| e[3]).collect();
        write_chunk(&mut out, b"PLTE", &plte);
        write_chunk(&mut out, b"tRNS", &trns);
    }
    let z = compcol::vec::compress_to_vec::<compcol::zlib::Zlib>(&raw).unwrap();
    write_chunk(&mut out, b"IDAT", &z);
    write_chunk(&mut out, b"IEND", &[]);
    out
}

fn format_of(ct: u8, depth: u8) -> PngPixelFormat {
    match (ct, depth) {
        (0, 16) => PngPixelFormat::Gray16Le,
        (0, _) => PngPixelFormat::Gray8,
        (2, 8) => PngPixelFormat::Rgb24,
        (2, _) => PngPixelFormat::Rgb48Le,
        (3, _) => PngPixelFormat::Pal8,
        (4, 8) => PngPixelFormat::Ya8,
        (4, _) => PngPixelFormat::Ya16Le,
        (6, 8) => PngPixelFormat::Rgba,
        _ => PngPixelFormat::Rgba64Le,
    }
}

/// The decoded plane the layout must produce: sub-byte greyscale scaled
/// to 8 bits (×255 / ×85 / ×17), palette indices as they are, 16-bit
/// samples little-endian.
fn expected_plane(w: usize, h: usize, ct: u8, depth: u8, seed: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            for c in 0..channels(ct) {
                let s = sample(x, y, c, depth, seed);
                match (ct, depth) {
                    (_, 16) => out.extend_from_slice(&s.to_le_bytes()),
                    (0, 1) => out.push(s as u8 * 255),
                    (0, 2) => out.push(s as u8 * 85),
                    (0, 4) => out.push(s as u8 * 17),
                    _ => out.push(s as u8),
                }
            }
        }
    }
    out
}

/// 8-bit RGBA of the same pixels: 16-bit samples by their high byte.
fn expected_rgba(w: usize, h: usize, ct: u8, depth: u8, seed: usize) -> Vec<u8> {
    let pal = palette_rgba(depth.min(8));
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let s8 = |c: usize| -> u8 {
                let s = sample(x, y, c, depth, seed);
                match depth {
                    16 => (s >> 8) as u8,
                    1 => s as u8 * 255,
                    2 => s as u8 * 85,
                    4 => s as u8 * 17,
                    _ => s as u8,
                }
            };
            match ct {
                0 => out.extend_from_slice(&[s8(0), s8(0), s8(0), 255]),
                2 => out.extend_from_slice(&[s8(0), s8(1), s8(2), 255]),
                3 => out.extend_from_slice(&pal[sample(x, y, 0, depth, seed) as usize]),
                4 => out.extend_from_slice(&[s8(0), s8(0), s8(0), s8(1)]),
                _ => out.extend_from_slice(&[s8(0), s8(1), s8(2), s8(3)]),
            }
        }
    }
    out
}

#[test]
fn decoder_reads_every_layout_at_every_small_size() {
    let mut files = 0usize;
    for (ct, depth) in LAYOUTS {
        for interlace in [false, true] {
            for h in 1..=9usize {
                for w in 1..=9usize {
                    let seed = w + 10 * h;
                    let file = build_file(w, h, ct, depth, interlace, seed);
                    let tag = format!("ct {ct} depth {depth} interlace {interlace} {w}x{h}");
                    let img = decode(&file).unwrap_or_else(|e| panic!("{tag}: {e}"));
                    assert_eq!((img.width(), img.height()), (w as u32, h as u32), "{tag}");
                    assert_eq!(img.format(), format_of(ct, depth), "{tag}");
                    assert_eq!(
                        img.as_bytes().unwrap(),
                        expected_plane(w, h, ct, depth, seed),
                        "{tag}"
                    );
                    let rgba = expected_rgba(w, h, ct, depth, seed);
                    assert_eq!(decode_rgba8(&file).unwrap().as_bytes(), rgba, "{tag} rgba");
                    let rgb: Vec<u8> = rgba
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|p| [p[0], p[1], p[2]])
                        .collect();
                    assert_eq!(decode_rgb8(&file).unwrap().as_bytes(), rgb, "{tag} rgb");
                    files += 1;
                }
            }
        }
    }
    assert_eq!(files, 15 * 2 * 81);
}

#[test]
fn encoder_round_trips_every_layout_at_every_small_size() {
    let strategies = [
        FilterStrategy::Adaptive,
        FilterStrategy::Entropy,
        FilterStrategy::Fixed(FilterType::None),
        FilterStrategy::Fixed(FilterType::Sub),
        FilterStrategy::Fixed(FilterType::Up),
        FilterStrategy::Fixed(FilterType::Average),
        FilterStrategy::Fixed(FilterType::Paeth),
    ];
    for (ct, depth) in LAYOUTS {
        for interlace in [false, true] {
            for h in 1..=9usize {
                for w in 1..=9usize {
                    let seed = 3 * w + h;
                    // The image the encoder takes for this layout: one
                    // byte per sample for the sub-byte depths, the
                    // samples unscaled.
                    let mut data = Vec::new();
                    for y in 0..h {
                        for x in 0..w {
                            for c in 0..channels(ct) {
                                let s = sample(x, y, c, depth, seed);
                                if depth == 16 {
                                    data.extend_from_slice(&s.to_le_bytes());
                                } else {
                                    data.push(s as u8);
                                }
                            }
                        }
                    }
                    let pf = format_of(ct, depth);
                    let stride = w * pf.bytes_per_pixel();
                    let mut image = PngImage::packed(w as u32, h as u32, pf, stride, data).unwrap();
                    if ct == 3 {
                        image = image.with_palette(Palette::new(palette_rgba(depth)));
                    }
                    // What a decode of the written file must give.
                    let mut expected = image.clone();
                    if ct == 0 && depth < 8 {
                        let scale = [0u8, 255, 85, 0, 17][depth as usize];
                        expected = PngImage::packed(
                            w as u32,
                            h as u32,
                            pf,
                            stride,
                            image
                                .as_bytes()
                                .unwrap()
                                .iter()
                                .map(|&v| v * scale)
                                .collect(),
                        )
                        .unwrap();
                    }
                    for (i, strategy) in strategies.iter().enumerate() {
                        let mut opts = EncodeOptions::new()
                            .with_interlace(interlace)
                            .with_filter_strategy(*strategy)
                            .with_level(1 + (i as u8 + depth) % 9);
                        if depth < 8 {
                            opts = opts.with_bit_depth(depth);
                        }
                        let tag = format!(
                            "ct {ct} depth {depth} interlace {interlace} {w}x{h} {strategy:?}"
                        );
                        let file = encode(&image, &opts).unwrap_or_else(|e| panic!("{tag}: {e}"));
                        assert_eq!(file[24], depth, "{tag}: IHDR bit depth");
                        assert_eq!(file[25], ct, "{tag}: IHDR colour type");
                        assert_eq!(file[28], u8::from(interlace), "{tag}: IHDR interlace");
                        let back = decode(&file).unwrap_or_else(|e| panic!("{tag}: {e}"));
                        assert!(back == expected, "{tag}: pixels differ");
                    }
                    // Every effort writes the same pixels.
                    for effort in 1..=9u8 {
                        let mut opts = EncodeOptions::new()
                            .with_interlace(interlace)
                            .with_effort(effort);
                        if depth < 8 {
                            opts = opts.with_bit_depth(depth);
                        }
                        if !(w + h + effort as usize).is_multiple_of(5) {
                            continue; // a fifth of the grid per effort
                        }
                        let file = encode(&image, &opts).unwrap();
                        assert!(decode(&file).unwrap() == expected, "effort {effort}");
                    }
                }
            }
        }
    }
}

/// `tRNS` on the alpha-less layouts: the keyed sample is transparent
/// on every decode path, at every bit depth (the key is given at the
/// file's bit depth, W3C PNG3 §11.3.1.1).
#[test]
fn keyed_transparency_holds_at_every_depth_on_every_path() {
    for (ct, depth) in LAYOUTS.into_iter().filter(|(ct, _)| matches!(ct, 0 | 2)) {
        for interlace in [false, true] {
            for (w, h) in [(1usize, 1usize), (5, 3), (9, 9)] {
                let seed = w * h + depth as usize;
                let mut file = build_file(w, h, ct, depth, interlace, seed);
                // The key: the samples of the last pixel.
                let key: Vec<u16> = (0..channels(ct))
                    .map(|c| sample(w - 1, h - 1, c, depth, seed))
                    .collect();
                let trns: Vec<u8> = key.iter().flat_map(|k| k.to_be_bytes()).collect();
                let mut chunk = Vec::new();
                write_chunk(&mut chunk, b"tRNS", &trns);
                // After the 8-byte signature and the 25-byte IHDR chunk.
                file.splice(33..33, chunk);
                let mut expected = expected_rgba(w, h, ct, depth, seed);
                let mut transparent = 0;
                for y in 0..h {
                    for x in 0..w {
                        let same =
                            (0..channels(ct)).all(|c| sample(x, y, c, depth, seed) == key[c]);
                        if same {
                            expected[(y * w + x) * 4 + 3] = 0;
                            transparent += 1;
                        }
                    }
                }
                assert!(transparent >= 1);
                let tag = format!("ct {ct} depth {depth} interlace {interlace} {w}x{h}");
                assert_eq!(decode_rgba8(&file).unwrap().as_bytes(), expected, "{tag}");
                let image = decode(&file).unwrap();
                assert_eq!(image.to_rgba8(), expected, "{tag} via decode");
                // And the image re-encodes to a file that says the same.
                let again = encode(&image, &EncodeOptions::new()).unwrap();
                assert_eq!(
                    decode_rgba8(&again).unwrap().as_bytes(),
                    expected,
                    "{tag} re-encoded"
                );
            }
        }
    }
}

/// `decode_rgba8` / `decode_rgb8` hold their output — four / three
/// bytes per pixel whatever the file's layout — to the default 1 GiB
/// limit before allocating it: a 1-bit image of 17000 × 17000 pixels
/// is a 36 MB plane and a 1.1 GiB RGBA buffer.
#[test]
fn packed_decodes_check_their_output_size_before_allocating() {
    let mut out = vec![137, 80, 78, 71, 13, 10, 26, 10];
    let mut ihdr = 17_000u32.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&17_000u32.to_be_bytes());
    ihdr.extend_from_slice(&[1, 0, 0, 0, 0]);
    write_chunk(&mut out, b"IHDR", &ihdr);
    // A stream that would fail later anyway: the limit fires first.
    write_chunk(&mut out, b"IDAT", &[0x78, 0x9c, 3, 0, 0, 0, 0, 1]);
    write_chunk(&mut out, b"IEND", &[]);
    let err = decode_rgba8(&out).unwrap_err();
    assert!(
        matches!(err, oxideav_png::PngError::LimitExceeded(_)),
        "{err:?}"
    );
    // RGB of the same image is 867 MB: within the limit, so the decode
    // goes on to the (truncated) pixel stream and fails there instead.
    let err = decode_rgb8(&out).unwrap_err();
    assert!(
        matches!(err, oxideav_png::PngError::InvalidData(_)),
        "{err:?}"
    );
}
