//! Colour type 4 at bit depth 16 (gray + alpha, W3C PNG3 §11.2.1 IHDR;
//! §7.1: each pixel is a gray sample then an alpha sample, two bytes
//! each, most significant byte first) decodes to the native two-sample
//! `PngPixelFormat::Ya16Le` layout — little-endian gray then alpha,
//! 4 bytes per pixel — on every path: still, Adam7, APNG compositing
//! (the canvas is `Ya16Le` too), `decode_all`, `info`, and the
//! registry decoder / container stream parameters.
//!
//! Exact samples are checked against the planes the test builds the
//! files from and, when ImageMagick is installed, against its raw
//! dump (`magick in.png -depth 16 -endian LSB graya:-`), a black-box
//! reader.

use std::path::PathBuf;
use std::process::Command;

use oxideav_png::chunk::write_chunk;
use oxideav_png::{EncodeOptions, PngImage, PngPixelFormat, decode, decode_all, decode_apng};

const SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn zlib(raw: &[u8]) -> Vec<u8> {
    compcol::vec::compress_to_vec_with::<compcol::zlib::Zlib>(
        raw,
        compcol::zlib::EncoderConfig { level: 6 },
    )
    .expect("zlib compress")
}

fn ihdr(w: u32, h: u32, bit_depth: u8, colour_type: u8, interlace: u8) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&be32(w));
    v.extend_from_slice(&be32(h));
    v.extend_from_slice(&[bit_depth, colour_type, 0, 0, interlace]);
    v
}

/// Deterministic `(gray, alpha)` samples for a `w × h` region.
fn samples(w: u32, h: u32, seed: u32) -> Vec<[u16; 2]> {
    let mut s = 0x9E37_79B9u32 ^ seed;
    (0..w * h)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            [(s & 0xFFFF) as u16, (s >> 16) as u16 | 0x0101]
        })
        .collect()
}

/// Filtered (filter type None) scanlines, samples big-endian on the wire.
fn wire_rows(w: u32, px: &[[u16; 2]]) -> Vec<u8> {
    let mut raw = Vec::new();
    for row in px.chunks(w as usize) {
        raw.push(0);
        for [g, a] in row {
            raw.extend_from_slice(&g.to_be_bytes());
            raw.extend_from_slice(&a.to_be_bytes());
        }
    }
    raw
}

/// The `Ya16Le` plane bytes for `px`.
fn ya16le(px: &[[u16; 2]]) -> Vec<u8> {
    px.iter()
        .flat_map(|[g, a]| {
            let (g, a) = (g.to_le_bytes(), a.to_le_bytes());
            [g[0], g[1], a[0], a[1]]
        })
        .collect()
}

fn still_png(w: u32, h: u32, px: &[[u16; 2]]) -> Vec<u8> {
    let mut png = SIG.to_vec();
    write_chunk(&mut png, b"IHDR", &ihdr(w, h, 16, 4, 0));
    write_chunk(&mut png, b"IDAT", &zlib(&wire_rows(w, px)));
    write_chunk(&mut png, b"IEND", &[]);
    png
}

fn have_magick() -> bool {
    Command::new("magick")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// ImageMagick's 16-bit little-endian gray+alpha dump of `png`, or
/// `None` when `magick` is not installed.
fn magick_graya16(label: &str, png: &[u8]) -> Option<Vec<u8>> {
    if !have_magick() {
        eprintln!("gray_alpha16_decode: `magick` not found — {label} oracle skipped");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("oxideav-png-ya16-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path: PathBuf = dir.join(format!("{label}.png"));
    std::fs::write(&path, png).expect("write png");
    let out = Command::new("magick")
        .arg(&path)
        .args(["-depth", "16", "-endian", "LSB", "graya:-"])
        .output()
        .expect("run magick");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{label}: magick failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(out.stdout)
}

#[test]
fn still_decodes_to_native_ya16le() {
    let (w, h) = (13u32, 7u32);
    let px = samples(w, h, 1);
    let png = still_png(w, h, &px);
    let want = ya16le(&px);

    let img = decode(&png).unwrap();
    assert_eq!(img.format, PngPixelFormat::Ya16Le);
    assert_eq!(img.stride(), w as usize * 4);
    assert_eq!(img.as_bytes().unwrap(), want);

    let info = oxideav_png::info(&png).unwrap();
    assert_eq!(info.format, PngPixelFormat::Ya16Le);
    assert!(info.has_alpha);

    if let Some(oracle) = magick_graya16("still", &png) {
        assert_eq!(oracle, want, "magick graya dump");
    }

    // The 8-bit conversions still read the high bytes, gray replicated.
    let rgba = oxideav_png::decode_rgba8(&png).unwrap();
    for (p, [g, a]) in rgba.data.as_chunks::<4>().0.iter().zip(&px) {
        let (g, a) = ((g >> 8) as u8, (a >> 8) as u8);
        assert_eq!(*p, [g, g, g, a]);
    }
}

#[test]
fn adam7_decodes_to_native_ya16le() {
    let (w, h) = (19u32, 11u32);
    let px = samples(w, h, 2);
    let want = ya16le(&px);
    let src = PngImage::packed(w, h, PngPixelFormat::Ya16Le, w as usize * 4, want.clone()).unwrap();
    let png = oxideav_png::encode(&src, &EncodeOptions::default().with_interlace(true)).unwrap();
    // IHDR: bit depth 16, colour type 4, interlace method 1.
    assert_eq!((png[24], png[25], png[28]), (16, 4, 1));

    let img = decode(&png).unwrap();
    assert_eq!(img.format, PngPixelFormat::Ya16Le);
    assert_eq!(img.as_bytes().unwrap(), want);

    if let Some(oracle) = magick_graya16("adam7", &png) {
        assert_eq!(oracle, want, "magick graya dump of the Adam7 file");
    }
}

#[test]
fn encode_decode_round_trip_is_lossless() {
    for (w, h, interlace) in [(1u32, 1u32, false), (64, 33, false), (33, 64, true)] {
        let bytes = ya16le(&samples(w, h, w ^ h));
        let src = PngImage::packed(w, h, PngPixelFormat::Ya16Le, w as usize * 4, bytes).unwrap();
        let png =
            oxideav_png::encode(&src, &EncodeOptions::default().with_interlace(interlace)).unwrap();
        let back = decode(&png).unwrap();
        assert_eq!(back.format, PngPixelFormat::Ya16Le);
        assert_eq!(back.as_bytes(), src.as_bytes(), "{w}x{h} adam7={interlace}");
    }
}

fn fctl(seq: u32, w: u32, h: u32, x: u32, y: u32, dispose: u8, blend: u8) -> Vec<u8> {
    let mut v = Vec::new();
    for n in [seq, w, h, x, y] {
        v.extend_from_slice(&be32(n));
    }
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&10u16.to_be_bytes());
    v.extend_from_slice(&[dispose, blend]);
    v
}

fn fdat(seq: u32, idat: &[u8]) -> Vec<u8> {
    let mut v = be32(seq).to_vec();
    v.extend_from_slice(idat);
    v
}

/// `src OVER dst` for an opaque buffer pixel (W3C PNG3 §13.16
/// `output = alpha · foreground + (1 − alpha) · background`), rounded.
fn over_opaque(src: [u16; 2], dst: u16) -> [u16; 2] {
    let (cs, a, cd) = (u64::from(src[0]), u64::from(src[1]), u64::from(dst));
    let num = cs * a + cd * (65535 - a);
    [((2 * num + 65535) / (2 * 65535)) as u16, 0xFFFF]
}

#[test]
fn apng_composites_on_a_ya16le_canvas() {
    // 4×3 canvas.
    //  frame 0: the default image, opaque, full canvas, SOURCE, dispose NONE;
    //  frame 1: 2×2 at (1,1), translucent, OVER, dispose BACKGROUND;
    //  frame 2: 1×1 at (0,0), translucent, SOURCE, dispose NONE.
    let (cw, ch) = (4u32, 3u32);
    let mut base = samples(cw, ch, 3);
    for p in &mut base {
        p[1] = 0xFFFF;
    }
    let f1 = samples(2, 2, 4);
    let f2 = samples(1, 1, 5);
    let (idat0, idat1, idat2) = (
        zlib(&wire_rows(cw, &base)),
        zlib(&wire_rows(2, &f1)),
        zlib(&wire_rows(1, &f2)),
    );

    let mut png = SIG.to_vec();
    write_chunk(&mut png, b"IHDR", &ihdr(cw, ch, 16, 4, 0));
    let mut actl = be32(3).to_vec();
    actl.extend_from_slice(&be32(0));
    write_chunk(&mut png, b"acTL", &actl);
    write_chunk(&mut png, b"fcTL", &fctl(0, cw, ch, 0, 0, 0, 0));
    write_chunk(&mut png, b"IDAT", &idat0);
    write_chunk(&mut png, b"fcTL", &fctl(1, 2, 2, 1, 1, 1, 1));
    write_chunk(&mut png, b"fdAT", &fdat(2, &idat1));
    write_chunk(&mut png, b"fcTL", &fctl(3, 1, 1, 0, 0, 0, 0));
    write_chunk(&mut png, b"fdAT", &fdat(4, &idat2));
    write_chunk(&mut png, b"IEND", &[]);

    // Black-box samples of each frame's own pixels: the default image is
    // the file's IDAT, and each fdAT payload repackaged as a still of the
    // fcTL size (§11.3.6.3: frames inherit the IHDR colour type / depth).
    if let Some(oracle) = magick_graya16("apng-default", &png) {
        assert_eq!(oracle, ya16le(&base), "default image");
    }
    for (label, (w, h, px)) in [("apng-f1", (2, 2, &f1)), ("apng-f2", (1, 1, &f2))] {
        if let Some(oracle) = magick_graya16(label, &still_png(w, h, px)) {
            assert_eq!(oracle, ya16le(px), "{label}");
        }
    }

    // Expected canvases.
    let at = |x: u32, y: u32| (y * cw + x) as usize;
    let shown0 = base.clone();
    let mut shown1 = base.clone();
    for y in 0..2 {
        for x in 0..2 {
            let i = at(x + 1, y + 1);
            shown1[i] = over_opaque(f1[(y * 2 + x) as usize], base[i][0]);
        }
    }
    // Frame 1's region is cleared to transparent black, then frame 2
    // replaces pixel (0,0) outright.
    let mut shown2 = shown1.clone();
    for y in 0..2 {
        for x in 0..2 {
            shown2[at(x + 1, y + 1)] = [0, 0];
        }
    }
    shown2[0] = f2[0];
    let want = [ya16le(&shown0), ya16le(&shown1), ya16le(&shown2)];

    let anim = decode_apng(&png).unwrap();
    assert_eq!(anim.pixel_format, PngPixelFormat::Ya16Le);
    assert_eq!(anim.frames.len(), 3);
    for (i, f) in anim.frames.iter().enumerate() {
        assert_eq!(f.image.format, PngPixelFormat::Ya16Le, "frame {i}");
        assert_eq!(f.image.stride(), cw as usize * 4, "frame {i}");
        assert_eq!(f.image.planes[0].data, want[i], "frame {i}");
    }

    let all = decode_all(&png).unwrap();
    assert_eq!(all.len(), 3);
    for (i, f) in all.iter().enumerate() {
        assert_eq!(f.image.format, PngPixelFormat::Ya16Le);
        assert_eq!(f.image.planes[0].data, want[i], "decode_all frame {i}");
    }
}

#[test]
fn other_layouts_are_unchanged() {
    for (bd, ct, pf) in [
        (8u8, 4u8, PngPixelFormat::Ya8),
        (16, 6, PngPixelFormat::Rgba64Le),
        (8, 6, PngPixelFormat::Rgba),
        (16, 0, PngPixelFormat::Gray16Le),
        (16, 2, PngPixelFormat::Rgb48Le),
    ] {
        let bpp = pf.bytes_per_pixel();
        let (w, h) = (5u32, 3u32);
        let mut raw = Vec::new();
        for y in 0..h {
            raw.push(0);
            raw.extend((0..w as usize * bpp).map(|i| (i as u8).wrapping_mul(37) ^ y as u8));
        }
        let mut png = SIG.to_vec();
        write_chunk(&mut png, b"IHDR", &ihdr(w, h, bd, ct, 0));
        write_chunk(&mut png, b"IDAT", &zlib(&raw));
        write_chunk(&mut png, b"IEND", &[]);
        let img = decode(&png).unwrap();
        assert_eq!(img.format, pf, "ct {ct} / {bd}-bit");
        assert_eq!(img.stride(), w as usize * bpp);
    }
}

#[cfg(feature = "registry")]
#[test]
fn registry_path_reports_and_emits_ya16le() {
    use std::io::Cursor;

    use oxideav_core::{Frame, PixelFormat, RuntimeContext};

    let mut ctx = RuntimeContext::new();
    oxideav_png::register(&mut ctx);
    let (w, h) = (6u32, 4u32);
    let px = samples(w, h, 6);
    let png = still_png(w, h, &px);

    let mut dm = ctx
        .containers
        .open_demuxer("png", Box::new(Cursor::new(png.clone())), &ctx.codecs)
        .unwrap();
    let params = dm.streams()[0].params.clone();
    assert_eq!(params.pixel_format, Some(PixelFormat::Ya16Le));
    let mut dec = ctx.codecs.first_decoder(&params).unwrap();
    let pkt = dm.next_packet().unwrap();
    dec.send_packet(&pkt).unwrap();
    let Frame::Video(v) = dec.receive_frame().unwrap() else {
        panic!("video frame");
    };
    assert_eq!(v.planes.len(), 1);
    assert_eq!(v.planes[0].stride, w as usize * 4);
    assert_eq!(v.planes[0].data, ya16le(&px));

    let frame = oxideav_png::decode_png_to_frame(&png, Some(0)).unwrap();
    assert_eq!(frame.planes[0].data, ya16le(&px));
    assert_eq!(
        oxideav_png::to_core_pixel_format(decode(&png).unwrap().format),
        PixelFormat::Ya16Le
    );
}
