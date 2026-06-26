//! APNG `acTL` placement / multiplicity (W3C PNG 3rd Edition §4.9.1 / §5.6).
//!
//! * §4.9.1: "To be recognized as an APNG, an acTL chunk must appear in the
//!   stream before any IDAT chunks." An acTL after the first IDAT is rejected.
//! * §5.6 Table (Ordering constraints): acTL "Multiple OK? No". A second acTL
//!   is rejected.

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

fn rgba8_frame(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut raw = Vec::new();
    for _ in 0..h {
        raw.extend_from_slice(&[0u8]);
        for _ in 0..w {
            raw.extend_from_slice(&rgba);
        }
    }
    zlib(&raw)
}

fn fctl_bytes(seq: u32, w: u32, h: u32) -> Vec<u8> {
    let mut fctl = Vec::new();
    fctl.extend_from_slice(&be32(seq));
    fctl.extend_from_slice(&be32(w));
    fctl.extend_from_slice(&be32(h));
    fctl.extend_from_slice(&be32(0));
    fctl.extend_from_slice(&be32(0));
    fctl.extend_from_slice(&(1u16).to_be_bytes());
    fctl.extend_from_slice(&(10u16).to_be_bytes());
    fctl.push(0);
    fctl.push(0);
    fctl
}

fn ihdr(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&be32(w));
    v.extend_from_slice(&be32(h));
    v.push(8);
    v.push(6);
    v.push(0);
    v.push(0);
    v.push(0);
    v
}

fn actl(num_frames: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&be32(num_frames));
    v.extend_from_slice(&be32(0));
    v
}

#[test]
fn actl_before_idat_accepted() {
    let w = 4u32;
    let h = 4u32;
    let f0 = rgba8_frame(w, h, [10, 20, 30, 255]);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr(w, h));
    write_chunk(&mut png, b"acTL", &actl(1));
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"IEND", &[]);
    assert!(parse_apng(&png).is_ok());
    assert!(decode_apng(&png).is_ok());
}

#[test]
fn actl_after_idat_rejected() {
    // acTL placed AFTER the IDAT — §4.9.1 requires it before any IDAT.
    let w = 4u32;
    let h = 4u32;
    let f0 = rgba8_frame(w, h, [10, 20, 30, 255]);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr(w, h));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"acTL", &actl(1));
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h));
    write_chunk(&mut png, b"IEND", &[]);
    assert!(parse_apng(&png).is_err());
    assert!(decode_apng(&png).is_err());
}

#[test]
fn duplicate_actl_rejected() {
    // Two acTL chunks — §5.6 "Multiple OK? No".
    let w = 4u32;
    let h = 4u32;
    let f0 = rgba8_frame(w, h, [10, 20, 30, 255]);
    let mut png = Vec::new();
    png.extend_from_slice(&SIG);
    write_chunk(&mut png, b"IHDR", &ihdr(w, h));
    write_chunk(&mut png, b"acTL", &actl(1));
    write_chunk(&mut png, b"acTL", &actl(1));
    write_chunk(&mut png, b"fcTL", &fctl_bytes(0, w, h));
    write_chunk(&mut png, b"IDAT", &f0);
    write_chunk(&mut png, b"IEND", &[]);
    assert!(parse_apng(&png).is_err());
    assert!(decode_apng(&png).is_err());
}
