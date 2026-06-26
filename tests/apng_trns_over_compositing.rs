//! APNG `APNG_BLEND_OP_OVER` on alpha-less canvas formats (colour types 0/2/3)
//! whose transparency comes from a `tRNS` chunk.
//!
//! W3C PNG3 §11.3.6.2 says an OVER frame "should be composited onto the output
//! buffer based on its alpha". For colour types 0 (grayscale), 2 (truecolour)
//! and 3 (indexed) the alpha is not in the pixel — it is carried by `tRNS`
//! (RFC 2083 §4.2.9). Because the composited canvas is itself one of these
//! alpha-less formats, an OVER pixel can only be fully transparent (leave the
//! canvas) or fully written. These tests pin that binary behaviour: a
//! tRNS-keyed transparent source pixel must leave the canvas, an opaque one
//! must overwrite.

use oxideav_png::chunk::write_chunk;
use oxideav_png::decode_apng;

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

fn fctl_bytes(seq: u32, w: u32, h: u32, blend: u8) -> Vec<u8> {
    let mut fctl = Vec::new();
    fctl.extend_from_slice(&be32(seq));
    fctl.extend_from_slice(&be32(w));
    fctl.extend_from_slice(&be32(h));
    fctl.extend_from_slice(&be32(0));
    fctl.extend_from_slice(&be32(0));
    fctl.extend_from_slice(&(1u16).to_be_bytes());
    fctl.extend_from_slice(&(10u16).to_be_bytes());
    fctl.push(0); // dispose None
    fctl.push(blend);
    fctl
}

fn actl(num_frames: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&be32(num_frames));
    v.extend_from_slice(&be32(0));
    v
}

/// One filter-None scanline buffer from a per-pixel byte producer.
fn rows(
    w: u32,
    h: u32,
    bytes_per_pixel: usize,
    mut px: impl FnMut(u32, u32) -> Vec<u8>,
) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..h {
        raw.extend_from_slice(&[0u8]); // filter None
        for x in 0..w {
            let b = px(x, y);
            assert_eq!(b.len(), bytes_per_pixel);
            raw.extend_from_slice(&b);
        }
    }
    zlib(&raw)
}

#[test]
fn palette_over_skips_transparent_index() {
    // 2x1 indexed canvas. PLTE: idx0 = red, idx1 = green. tRNS: idx0 alpha 0
    // (transparent), idx1 alpha 255 (opaque, implied — tail length 1).
    // Frame 0 (default): both pixels idx1 (green), Source.
    // Frame 1: both pixels idx0 (transparent), OVER → must leave canvas green.
    let w = 2u32;
    let h = 1u32;
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(8); // bit depth
    ihdr.push(3); // colour type indexed
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    let plte = vec![255, 0, 0, 0, 255, 0]; // idx0 red, idx1 green
    let trns = vec![0u8]; // idx0 alpha 0

    let f0 = rows(w, h, 1, |_, _| vec![1]); // idx1 green
    let f1 = rows(w, h, 1, |_, _| vec![0]); // idx0 transparent
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2));
    fdat.extend_from_slice(&f1);

    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl(2));
    write_chunk(&mut png, b"PLTE", &plte);
    write_chunk(&mut png, b"tRNS", &trns);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h, 0));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(1, w, h, 1)); // OVER
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);

    let anim = decode_apng(&png).expect("decode");
    // Canvas is Pal8: stride = w, each byte is an index.
    let stride = anim.frames[1].image.stride;
    let d = &anim.frames[1].image.data;
    // Frame 1 OVER: transparent idx0 source leaves canvas at idx1 (green).
    assert_eq!(d[0], 1, "transparent OVER pixel should keep idx1");
    assert_eq!(d[stride - 1], 1, "second pixel also keeps idx1");
}

#[test]
fn palette_over_writes_opaque_index() {
    // Same palette/tRNS but frame 1 uses idx1 (opaque green) OVER onto an
    // idx0-filled (red) canvas → must overwrite to idx1.
    let w = 2u32;
    let h = 1u32;
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(8);
    ihdr.push(3);
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    let plte = vec![255, 0, 0, 0, 255, 0];
    let trns = vec![0u8];
    let f0 = rows(w, h, 1, |_, _| vec![0]); // idx0 (transparent-keyed but drawn opaque on default)
    let f1 = rows(w, h, 1, |_, _| vec![1]); // idx1 opaque
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2));
    fdat.extend_from_slice(&f1);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl(2));
    write_chunk(&mut png, b"PLTE", &plte);
    write_chunk(&mut png, b"tRNS", &trns);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h, 0));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(1, w, h, 1));
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);
    let anim = decode_apng(&png).expect("decode");
    let d = &anim.frames[1].image.data;
    assert_eq!(d[0], 1, "opaque OVER pixel should overwrite to idx1");
}

#[test]
fn grayscale_over_skips_keyed_sample() {
    // 8-bit grayscale, tRNS keys gray 0x00 as transparent.
    // Frame 0: all gray 0x80. Frame 1: all gray 0x00 (keyed transparent) OVER
    // → canvas must stay 0x80.
    let w = 2u32;
    let h = 1u32;
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(8);
    ihdr.push(0); // grayscale
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    let trns = vec![0u8, 0u8]; // gray 0x0000 (2 bytes for ct0)
    let f0 = rows(w, h, 1, |_, _| vec![0x80]);
    let f1 = rows(w, h, 1, |_, _| vec![0x00]);
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2));
    fdat.extend_from_slice(&f1);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl(2));
    write_chunk(&mut png, b"tRNS", &trns);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h, 0));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(1, w, h, 1));
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);
    let anim = decode_apng(&png).expect("decode");
    let d = &anim.frames[1].image.data;
    assert_eq!(d[0], 0x80, "keyed-transparent gray should leave canvas");
}

#[test]
fn truecolour_over_skips_keyed_rgb() {
    // 8-bit RGB, tRNS keys (0,0,0) as transparent.
    // Frame 0: all (10,20,30). Frame 1: all (0,0,0) keyed → OVER must keep
    // frame-0 colour. A non-keyed pixel overwrites.
    let w = 2u32;
    let h = 1u32;
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(8);
    ihdr.push(2); // truecolour
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    // tRNS ct2: 6 bytes, (r,g,b) each big-endian u16 → key (0,0,0).
    let trns = vec![0u8; 6];
    let f0 = rows(w, h, 3, |_, _| vec![10, 20, 30]);
    // Frame 1: pixel 0 keyed (0,0,0); pixel 1 opaque (99,88,77).
    let f1 = rows(w, h, 3, |x, _| {
        if x == 0 {
            vec![0, 0, 0]
        } else {
            vec![99, 88, 77]
        }
    });
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2));
    fdat.extend_from_slice(&f1);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl(2));
    write_chunk(&mut png, b"tRNS", &trns);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h, 0));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(1, w, h, 1));
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);
    let anim = decode_apng(&png).expect("decode");
    let d = &anim.frames[1].image.data;
    // Pixel 0: keyed transparent → keeps frame-0 (10,20,30).
    assert_eq!(&d[0..3], &[10, 20, 30], "keyed RGB should leave canvas");
    // Pixel 1: opaque → overwrites to (99,88,77).
    assert_eq!(&d[3..6], &[99, 88, 77], "opaque RGB should overwrite");
}
