//! APNG default-image `fcTL` restrictions and first-frame disposal
//! normalisation (W3C PNG 3rd Edition §11.3.5.1).
//!
//! Two normative rules are pinned here:
//!
//! * "The fcTL chunk corresponding to the default image, if it exists, has
//!   these restrictions: the x_offset and y_offset fields must be 0. The
//!   width and height fields must equal the corresponding fields from the
//!   IHDR chunk." The default-image fcTL is the one preceding the first IDAT.
//! * "If the first fcTL chunk uses a dispose_op of APNG_DISPOSE_OP_PREVIOUS
//!   it should be treated as APNG_DISPOSE_OP_BACKGROUND."

use oxideav_png::chunk::write_chunk;
use oxideav_png::{decode_apng, parse_apng};

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

/// Solid-colour 8-bit RGBA frame, filter None per row.
fn rgba8_frame(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut raw = Vec::new();
    for _ in 0..h {
        raw.extend_from_slice(&[0u8]); // filter None
        for _ in 0..w {
            raw.extend_from_slice(&rgba);
        }
    }
    zlib(&raw)
}

#[allow(clippy::too_many_arguments)]
fn fctl_bytes(seq: u32, w: u32, h: u32, x: u32, y: u32, dispose: u8, blend: u8) -> Vec<u8> {
    let mut fctl = Vec::new();
    fctl.extend_from_slice(&be32(seq));
    fctl.extend_from_slice(&be32(w));
    fctl.extend_from_slice(&be32(h));
    fctl.extend_from_slice(&be32(x));
    fctl.extend_from_slice(&be32(y));
    fctl.extend_from_slice(&(1u16).to_be_bytes());
    fctl.extend_from_slice(&(10u16).to_be_bytes());
    fctl.push(dispose);
    fctl.push(blend);
    fctl
}

fn ihdr_rgba8(w: u32, h: u32) -> Vec<u8> {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(8);
    ihdr.push(6); // RGBA
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);
    ihdr
}

/// Build a 2-frame APNG where the default image (IDAT) is the first frame,
/// with the supplied default-image fcTL geometry.
fn craft_default_first(canvas: u32, def_w: u32, def_h: u32, def_x: u32, def_y: u32) -> Vec<u8> {
    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(2));
    actl.extend_from_slice(&be32(0));
    let f0 = rgba8_frame(def_w.min(canvas), def_h.min(canvas), [10, 20, 30, 255]);
    let f1 = rgba8_frame(canvas, canvas, [40, 50, 60, 255]);
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2));
    fdat.extend_from_slice(&f1);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr_rgba8(canvas, canvas));
    write_chunk(&mut png, b"acTL", &actl);
    write_chunk(
        &mut png,
        b"fcTL",
        &fctl_bytes(0, def_w, def_h, def_x, def_y, 0, 0),
    );
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(
        &mut png,
        b"fcTL",
        &fctl_bytes(1, canvas, canvas, 0, 0, 0, 0),
    );
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
fn default_fctl_zero_offset_full_size_accepted() {
    // Default-image fcTL with x=0,y=0 and dims == IHDR is well-formed.
    let png = craft_default_first(4, 4, 4, 0, 0);
    assert!(parse_apng(&png).is_ok());
    assert!(decode_apng(&png).is_ok());
}

#[test]
fn default_fctl_nonzero_offset_rejected() {
    // §11.3.5.1: default-image fcTL "x_offset and y_offset fields must be 0".
    let png = craft_default_first(4, 4, 4, 1, 0);
    assert!(parse_apng(&png).is_err());
    let png = craft_default_first(4, 4, 4, 0, 2);
    assert!(parse_apng(&png).is_err());
}

#[test]
fn default_fctl_partial_dimensions_rejected() {
    // §11.3.5.1: default-image fcTL "width and height fields must equal the
    // corresponding fields from the IHDR chunk".
    let png = craft_default_first(4, 2, 4, 0, 0);
    assert!(parse_apng(&png).is_err());
    let png = craft_default_first(4, 4, 3, 0, 0);
    assert!(parse_apng(&png).is_err());
}

#[test]
fn separate_default_image_fctl_offset_allowed() {
    // When the static image is NOT the first frame (fcTL comes AFTER IDAT),
    // the first animation fcTL is a normal frame and may carry offsets — the
    // §11.3.5.1 default-image restriction applies only to the fcTL that
    // precedes IDAT, which here does not exist.
    let canvas = 8u32;
    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(1));
    actl.extend_from_slice(&be32(0));
    let still = rgba8_frame(canvas, canvas, [1, 2, 3, 255]);
    let frame = rgba8_frame(4, 4, [9, 9, 9, 255]);
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(1));
    fdat.extend_from_slice(&frame);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr_rgba8(canvas, canvas));
    write_chunk(&mut png, b"acTL", &actl);
    write_chunk(&mut png, b"IDAT", &still);
    // fcTL AFTER IDAT → static image is not part of the animation; this frame
    // may sit at a non-zero offset with a sub-canvas extent.
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, 4, 4, 2, 2, 0, 0));
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);
    assert!(parse_apng(&png).is_ok());
    assert!(decode_apng(&png).is_ok());
}

#[test]
fn first_frame_dispose_previous_treated_as_background() {
    // §11.3.5.1: a first-frame dispose_op of PREVIOUS is treated as
    // BACKGROUND. We build a 2-frame animation where frame 0 fills the whole
    // canvas opaque, disposes PREVIOUS, and frame 1 covers only a sub-region
    // with dispose NONE / blend SOURCE. Under the PREVIOUS→BACKGROUND
    // normalisation the whole frame-0 region is cleared to transparent black
    // before frame 1 draws, so the area frame 1 does NOT cover must read as
    // zero (cleared), not as the frame-0 colour.
    let canvas = 4u32;
    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(2));
    actl.extend_from_slice(&be32(0));
    let f0 = rgba8_frame(canvas, canvas, [200, 100, 50, 255]);
    // Frame 1: 2x2 at offset (0,0).
    let f1 = rgba8_frame(2, 2, [10, 20, 30, 255]);
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(2));
    fdat.extend_from_slice(&f1);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr_rgba8(canvas, canvas));
    write_chunk(&mut png, b"acTL", &actl);
    // Default-image fcTL: full canvas, dispose PREVIOUS (=2).
    write_chunk(
        &mut png,
        b"fcTL",
        &fctl_bytes(0, canvas, canvas, 0, 0, 2, 0),
    );
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"fcTL", &fctl_bytes(1, 2, 2, 0, 0, 0, 0));
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);

    let anim = decode_apng(&png).expect("decode");
    assert_eq!(anim.frames.len(), 2);
    let stride = anim.frames[1].image.stride;
    let pix = |x: usize, y: usize| -> [u8; 4] {
        let off = y * stride + x * 4;
        let d = &anim.frames[1].image.data;
        [d[off], d[off + 1], d[off + 2], d[off + 3]]
    };
    // Region frame 1 covered: its colour.
    assert_eq!(pix(0, 0), [10, 20, 30, 255]);
    // Region frame 0 covered but frame 1 did not: must be cleared (PREVIOUS
    // normalised to BACKGROUND cleared the whole frame-0 region), NOT the
    // frame-0 colour.
    assert_eq!(pix(3, 3), [0, 0, 0, 0], "should be cleared, not frame-0");
}
