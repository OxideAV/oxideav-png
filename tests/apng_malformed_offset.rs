//! Regression: a malformed APNG `fcTL` may carry an `x_offset` /
//! `y_offset` (or sub-frame width/height) that places the frame entirely
//! outside the canvas. The PNG spec (§ APNG fcTL) says these must lie
//! within the canvas, but a hostile stream need not honour it. The
//! decoder must reject / clamp such frames gracefully, never panic.
//!
//! Found by the `decode` cargo-fuzz target's attack-surface analysis:
//! `clear_region` (Background-disposal clear) turned an out-of-canvas
//! `x_offset` into a byte offset past the canvas buffer and indexed an
//! (even empty) slice out of bounds. Fixed in `decoder::clear_region`.

use oxideav_png::chunk::write_chunk;
use oxideav_png::decode_apng;

const SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// Build a single-frame 6x6 RGBA APNG whose `fcTL` is patched with the
/// supplied offsets / disposal so we can drive the out-of-canvas paths.
fn craft_apng(x_offset: u32, y_offset: u32, dispose_op: u8) -> Vec<u8> {
    let w = 6u32;
    let h = 6u32;

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(8); // bit depth
    ihdr.push(6); // colour type RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace

    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(1)); // num_frames
    actl.extend_from_slice(&be32(0)); // num_plays

    let mut fctl = Vec::new();
    fctl.extend_from_slice(&be32(0)); // sequence number
    fctl.extend_from_slice(&be32(w)); // frame width
    fctl.extend_from_slice(&be32(h)); // frame height
    fctl.extend_from_slice(&be32(x_offset)); // x_offset (hostile)
    fctl.extend_from_slice(&be32(y_offset)); // y_offset (hostile)
    fctl.extend_from_slice(&(1u16).to_be_bytes()); // delay_num
    fctl.extend_from_slice(&(10u16).to_be_bytes()); // delay_den
    fctl.push(dispose_op); // dispose_op
    fctl.push(0); // blend_op = Source

    // Valid zlib IDAT: 6 rows of (1 filter byte + 6*4 data) = 150 bytes.
    let row_bytes = (w * 4) as usize;
    let mut raw = Vec::new();
    for _ in 0..h {
        raw.push(0u8); // filter None
        raw.extend(std::iter::repeat(128u8).take(row_bytes));
    }
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);

    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl);
    write_chunk(&mut png, b"fcTL", &fctl);
    write_chunk(&mut png, b"IDAT", &idat);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
fn apng_xoffset_past_canvas_background_dispose_does_not_panic() {
    // x_offset 1000 >> canvas width 6, Background disposal (op 1) — the
    // path that previously indexed `canvas[4000..4000]` and panicked.
    let png = craft_apng(1000, 0, 1);
    // Must return (Ok or Err), never panic.
    let _ = decode_apng(&png);
}

#[test]
fn apng_yoffset_past_canvas_background_dispose_does_not_panic() {
    let png = craft_apng(0, 1000, 1);
    let _ = decode_apng(&png);
}

#[test]
fn apng_both_offsets_past_canvas_all_dispose_ops_do_not_panic() {
    for dispose_op in 0..=2u8 {
        let png = craft_apng(5000, 5000, dispose_op);
        let _ = decode_apng(&png);
    }
}
