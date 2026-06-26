//! APNG compositing on 16-bit-per-channel canvases.
//!
//! The decoder composites every animation frame into a canvas applying
//! `blend_op` (`APNG_BLEND_OP_SOURCE` / `APNG_BLEND_OP_OVER`) and `dispose_op`
//! per W3C PNG 3rd Edition §11.3.5 (frame control) and §13.16 ("Alpha Channel
//! Processing", the non-premultiplied OVER referenced by
//! `APNG_BLEND_OP_OVER`). 16-bit RGBA (colour type 6, bit depth 16) and 16-bit
//! gray+alpha (colour type 4, bit depth 16 — expanded to RGBA64 internally)
//! both store little-endian per-channel samples on the composited canvas; this
//! test pins the OVER arithmetic at 16-bit precision.

use oxideav_png::chunk::write_chunk;
use oxideav_png::{decode_apng, PngPixelFormat};

const SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// zlib-compress a raw filtered scanline buffer.
fn zlib(raw: &[u8]) -> Vec<u8> {
    compcol::vec::compress_to_vec_with::<compcol::zlib::Zlib>(
        raw,
        compcol::zlib::EncoderConfig { level: 6 },
    )
    .expect("zlib compress")
}

/// Build a solid-colour 16-bit RGBA frame (`w x h`), every pixel the same
/// `(r,g,b,a)` 16-bit sample, filter None per row. Samples are emitted
/// big-endian per the PNG wire format.
fn rgba16_frame(w: u32, h: u32, rgba: [u16; 4]) -> Vec<u8> {
    let mut raw = Vec::new();
    for _ in 0..h {
        raw.extend_from_slice(&[0u8]); // filter None
        for _ in 0..w {
            for s in rgba {
                raw.extend_from_slice(&s.to_be_bytes());
            }
        }
    }
    zlib(&raw)
}

fn fctl_bytes(seq: u32, w: u32, h: u32, x: u32, y: u32, dispose: u8, blend: u8) -> Vec<u8> {
    let mut fctl = Vec::new();
    fctl.extend_from_slice(&be32(seq));
    fctl.extend_from_slice(&be32(w));
    fctl.extend_from_slice(&be32(h));
    fctl.extend_from_slice(&be32(x));
    fctl.extend_from_slice(&be32(y));
    fctl.extend_from_slice(&(1u16).to_be_bytes()); // delay_num
    fctl.extend_from_slice(&(10u16).to_be_bytes()); // delay_den
    fctl.push(dispose);
    fctl.push(blend);
    fctl
}

/// Pull the LE 16-bit channel value at pixel `(x,y)` channel `c` from a
/// composited RGBA64Le frame.
fn sample(img: &oxideav_png::ApngFrameImage, w: usize, x: usize, y: usize, c: usize) -> u16 {
    let off = (y * w + x) * 8 + c * 2;
    u16::from_le_bytes([img.image.data[off], img.image.data[off + 1]])
}

#[test]
fn rgba16_over_blends_at_16bit_precision() {
    // 2x2 canvas. Frame 0 = opaque deep red default image. Frame 1 = a
    // half-transparent green drawn OVER it. Result must be the 16-bit OVER
    // blend, NOT a Source overwrite.
    let w = 2u32;
    let h = 2u32;

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(16); // bit depth
    ihdr.push(6); // colour type RGBA
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);

    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(2)); // num_frames
    actl.extend_from_slice(&be32(0)); // infinite

    // Frame 0: opaque red (0xFFFF, 0, 0, 0xFFFF). Default image = frame 0.
    let f0 = rgba16_frame(w, h, [0xFFFF, 0x0000, 0x0000, 0xFFFF]);
    // Frame 1: green at half alpha (0, 0xFFFF, 0, 0x8000), blend OVER.
    let alpha = 0x8000u16;
    let f1 = rgba16_frame(w, h, [0x0000, 0xFFFF, 0x0000, alpha]);
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2)); // sequence number
    fdat.extend_from_slice(&f1);

    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h, 0, 0, 0, 0)); // dispose None, Source
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(1, w, h, 0, 0, 0, 1)); // dispose None, OVER
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);

    let anim = decode_apng(&png).expect("decode");
    assert_eq!(anim.pixel_format, PngPixelFormat::Rgba64Le);
    assert_eq!(anim.frames.len(), 2);

    // Frame 0 canvas: pure red.
    assert_eq!(sample(&anim.frames[0], w as usize, 0, 0, 0), 0xFFFF);
    assert_eq!(sample(&anim.frames[0], w as usize, 0, 0, 1), 0x0000);

    // Frame 1 OVER red:
    //   inv = 65535 - 32768 = 32767
    //   R: (0*32768 + 65535*32767 + 32767)/65535 = 32767
    //   G: (65535*32768 + 0*32767 + 32767)/65535 = 32768
    //   B: 0
    //   A: 32768 + (65535*32767 + 32767)/65535 = 32768 + 32767 = 65535
    let inv = 65535u64 - alpha as u64;
    // R foreground is 0, G background is 0 — written out explicitly so the
    // OVER arithmetic reads against the implementation, but the erasing
    // multiply-by-zero terms are dropped to keep clippy's `erasing_op` quiet.
    let exp_r = ((0xFFFFu64 * inv + 32767) / 65535) as u16;
    let exp_g = ((0xFFFFu64 * alpha as u64 + 32767) / 65535) as u16;
    let exp_a = (alpha as u64 + (0xFFFFu64 * inv + 32767) / 65535) as u16;
    assert_eq!(sample(&anim.frames[1], w as usize, 0, 0, 0), exp_r, "R");
    assert_eq!(sample(&anim.frames[1], w as usize, 0, 0, 1), exp_g, "G");
    assert_eq!(sample(&anim.frames[1], w as usize, 0, 0, 2), 0x0000, "B");
    assert_eq!(sample(&anim.frames[1], w as usize, 0, 0, 3), exp_a, "A");
    // Must NOT be a Source overwrite (that would give pure green a=0x8000).
    assert_ne!(sample(&anim.frames[1], w as usize, 0, 0, 1), 0xFFFF);
}

#[test]
fn rgba16_over_fully_opaque_is_overwrite() {
    // A fully-opaque (a=0xFFFF) OVER frame replaces the canvas exactly.
    let w = 1u32;
    let h = 1u32;
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(16);
    ihdr.push(6);
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(2));
    actl.extend_from_slice(&be32(0));
    let f0 = rgba16_frame(w, h, [0x1234, 0x5678, 0x9abc, 0xFFFF]);
    let f1 = rgba16_frame(w, h, [0xdead, 0xbeef, 0xcafe, 0xFFFF]);
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2));
    fdat.extend_from_slice(&f1);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h, 0, 0, 0, 0));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(1, w, h, 0, 0, 0, 1));
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);
    let anim = decode_apng(&png).expect("decode");
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 0), 0xdead);
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 1), 0xbeef);
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 2), 0xcafe);
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 3), 0xFFFF);
}

#[test]
fn rgba16_over_fully_transparent_leaves_canvas() {
    // An a=0 OVER frame leaves the canvas untouched.
    let w = 1u32;
    let h = 1u32;
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(16);
    ihdr.push(6);
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(2));
    actl.extend_from_slice(&be32(0));
    let f0 = rgba16_frame(w, h, [0x1111, 0x2222, 0x3333, 0xFFFF]);
    let f1 = rgba16_frame(w, h, [0xFFFF, 0xFFFF, 0xFFFF, 0x0000]);
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2));
    fdat.extend_from_slice(&f1);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h, 0, 0, 0, 0));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(1, w, h, 0, 0, 0, 1));
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);
    let anim = decode_apng(&png).expect("decode");
    // Canvas unchanged: still frame-0 colour.
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 0), 0x1111);
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 1), 0x2222);
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 2), 0x3333);
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 3), 0xFFFF);
}

#[test]
fn gray_alpha16_over_composites() {
    // Colour type 4 / bit depth 16 (gray + alpha) is expanded to RGBA64
    // internally; OVER must still blend at 16-bit precision. Frame 0 = opaque
    // gray 0x4000, frame 1 = gray 0xC000 at half alpha OVER.
    let w = 1u32;
    let h = 1u32;
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(16);
    ihdr.push(4); // colour type gray+alpha
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(2));
    actl.extend_from_slice(&be32(0));

    // ya16 frame: (gray, alpha) big-endian, filter None.
    let ya16 = |gray: u16, a: u16| -> Vec<u8> {
        let mut raw = vec![0u8]; // filter None
        raw.extend_from_slice(&gray.to_be_bytes());
        raw.extend_from_slice(&a.to_be_bytes());
        zlib(&raw)
    };
    let f0 = ya16(0x4000, 0xFFFF);
    let alpha = 0x8000u16;
    let f1 = ya16(0xC000, alpha);
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2));
    fdat.extend_from_slice(&f1);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h, 0, 0, 0, 0));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(1, w, h, 0, 0, 0, 1));
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);
    let anim = decode_apng(&png).expect("decode");
    let inv = 65535u64 - alpha as u64;
    let exp = ((0xC000u64 * alpha as u64 + 0x4000u64 * inv + 32767) / 65535) as u16;
    // Gray is replicated across R/G/B; all three must equal the blended gray.
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 0), exp);
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 1), exp);
    assert_eq!(sample(&anim.frames[1], 1, 0, 0, 2), exp);
}
