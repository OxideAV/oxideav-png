//! Regression: APNG shared `fcTL` / `fdAT` sequence-number validation.
//!
//! W3C PNG 3rd Edition §4.9.2 ("Sequence numbers") makes three `shall`
//! statements about the single sequence shared by `fcTL` and `fdAT`:
//!
//! * "The first `fcTL` chunk shall contain sequence number 0".
//! * "the sequence numbers in the remaining `fcTL` and `fdAT` chunks shall be
//!   in ascending order, with no gaps or duplicates."
//! * §4.9.1: "Decoders shall treat out-of-order APNG chunks as an error."
//!
//! and §4.9 says of `acTL.num_frames`: "0 is not a valid value." A hostile or
//! buggy stream need not honour any of these, so `parse_apng` / `decode_apng`
//! reject the crafted streams below with an `Err` (and never panic). The
//! well-formed control stream decodes cleanly.

use oxideav_png::chunk::write_chunk;
use oxideav_png::{decode_apng, parse_apng};

const SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// One frame's worth of zlib-compressed RGBA pixel data for a `w x h` image,
/// solid mid-grey, filter type None on every row.
fn frame_idat(w: u32, h: u32) -> Vec<u8> {
    let row_bytes = (w * 4) as usize;
    let mut raw = Vec::new();
    for _ in 0..h {
        raw.push(0u8); // filter None
        raw.extend(std::iter::repeat(128u8).take(row_bytes));
    }
    compcol::vec::compress_to_vec_with::<compcol::zlib::Zlib>(
        &raw,
        compcol::zlib::EncoderConfig { level: 6 },
    )
    .expect("zlib compress")
}

fn fctl_bytes(seq: u32, w: u32, h: u32) -> Vec<u8> {
    let mut fctl = Vec::new();
    fctl.extend_from_slice(&be32(seq)); // sequence number
    fctl.extend_from_slice(&be32(w)); // frame width
    fctl.extend_from_slice(&be32(h)); // frame height
    fctl.extend_from_slice(&be32(0)); // x_offset
    fctl.extend_from_slice(&be32(0)); // y_offset
    fctl.extend_from_slice(&(1u16).to_be_bytes()); // delay_num
    fctl.extend_from_slice(&(10u16).to_be_bytes()); // delay_den
    fctl.push(0); // dispose_op = None
    fctl.push(0); // blend_op = Source
    fctl
}

/// Build a 2-frame 4x4 RGBA APNG where the default image (`IDAT`) is also the
/// first animation frame. The caller supplies the four sequence numbers used
/// by, in file order: fcTL #0, fcTL #1, fdAT (frame 2 data), and `num_frames`.
fn craft_two_frame(seq_fctl0: u32, seq_fctl1: u32, seq_fdat: u32, num_frames: u32) -> Vec<u8> {
    let w = 4u32;
    let h = 4u32;

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(w));
    ihdr.extend_from_slice(&be32(h));
    ihdr.push(8); // bit depth
    ihdr.push(6); // colour type RGBA
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace

    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(num_frames));
    actl.extend_from_slice(&be32(0)); // num_plays = infinite

    let idat = frame_idat(w, h);

    // fdAT = 4-byte sequence number + the frame's compressed bytes.
    let mut fdat = Vec::new();
    fdat.extend_from_slice(&be32(seq_fdat));
    fdat.extend_from_slice(&frame_idat(w, h));

    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr);
    write_chunk(&mut png, b"acTL", &actl);
    // Frame 1: fcTL then the shared IDAT (default image doubles as frame 1).
    write_chunk(&mut png, b"fcTL", &fctl_bytes(seq_fctl0, w, h));
    write_chunk(&mut png, b"IDAT", &idat);
    // Frame 2: fcTL then fdAT.
    write_chunk(&mut png, b"fcTL", &fctl_bytes(seq_fctl1, w, h));
    write_chunk(&mut png, b"fdAT", &fdat);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
fn wellformed_sequence_decodes() {
    // fcTL(0) IDAT fcTL(1) fdAT(2) — contiguous-ascending, num_frames = 2.
    let png = craft_two_frame(0, 1, 2, 2);
    let info = parse_apng(&png).expect("parse");
    assert_eq!(info.frames.len(), 2);
    let img = decode_apng(&png).expect("decode");
    assert_eq!(img.frames.len(), 2);
}

#[test]
fn first_fctl_must_be_zero() {
    // §4.9.2: "The first fcTL chunk shall contain sequence number 0." Here it
    // starts at 1 (with the rest shifted up to stay contiguous).
    let png = craft_two_frame(1, 2, 3, 2);
    assert!(parse_apng(&png).is_err());
    assert!(decode_apng(&png).is_err());
}

#[test]
fn sequence_gap_rejected() {
    // fcTL(0) fcTL(1) fdAT(3) — skips 2: "no gaps".
    let png = craft_two_frame(0, 1, 3, 2);
    assert!(parse_apng(&png).is_err());
    assert!(decode_apng(&png).is_err());
}

#[test]
fn sequence_duplicate_rejected() {
    // fcTL(0) fcTL(1) fdAT(1) — duplicate of 1: "no duplicates".
    let png = craft_two_frame(0, 1, 1, 2);
    assert!(parse_apng(&png).is_err());
    assert!(decode_apng(&png).is_err());
}

#[test]
fn sequence_descending_rejected() {
    // fcTL(0) fcTL(2) fdAT(1) — out-of-order descending step.
    let png = craft_two_frame(0, 2, 1, 2);
    assert!(parse_apng(&png).is_err());
    assert!(decode_apng(&png).is_err());
}

#[test]
fn actl_num_frames_zero_rejected() {
    // §4.9: "0 is not a valid value" for num_frames.
    let png = craft_two_frame(0, 1, 2, 0);
    assert!(parse_apng(&png).is_err());
    assert!(decode_apng(&png).is_err());
}
