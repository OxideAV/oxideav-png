//! Coverage for `tRNS` keyed-transparency application during
//! `decode_png_to_rgba` (RFC 2083 §4.2.9):
//!
//! * Colour type 0 (grayscale, 8 / 16-bit) — single transparent gray
//!   sample. The match is done at the bit depth's value, before the
//!   16→8 promotion drops the low byte.
//! * Colour type 2 (RGB, 8 / 16-bit) — single transparent RGB triple.
//! * Colour type 3 — already covered by `decode_png_to_rgba.rs`.
//! * Colour type 4 / 6 — `tRNS` is prohibited (decoder rejects the
//!   stream).
//! * Length policing — ct=0 must be 2 bytes, ct=2 must be 6 bytes.
//! * Sample-bounds policing — ct=0/2 sample must fit `bit_depth`.
//!
//! The standalone `encode_png_image` doesn't emit `tRNS` for ct=0/2
//! (the API only carries `tRNS` alongside `Pal8`). To exercise the
//! decode path the tests below take an encoder-produced PNG and splice
//! a hand-written `tRNS` chunk in just before the first `IDAT`,
//! recomputing CRCs via `chunk::write_chunk` so the resulting stream
//! is structurally a real PNG.

use oxideav_png::{
    chunk::{write_chunk, PNG_MAGIC},
    decode_png_to_rgba, encode_png_image, PngImage, PngPixelFormat,
};

fn make(w: u32, h: u32, pf: PngPixelFormat, data: Vec<u8>) -> PngImage {
    let bpp = pf.bytes_per_pixel();
    PngImage {
        width: w,
        height: h,
        pixel_format: pf,
        stride: w as usize * bpp,
        data,
        palette: Vec::new(),
    }
}

/// Splice a new chunk with the given type + payload immediately before
/// the first `IDAT` in an encoder-produced PNG stream.
///
/// The PNG chunk layout is `len(4) || type(4) || data(N) || crc(4)`;
/// chunks concatenate end-to-end after the 8-byte magic. We walk by
/// length until we hit `IDAT`, then re-emit everything-before + the new
/// chunk + everything-from-IDAT-onwards. CRC reuse is safe because the
/// untouched chunks' raw bytes (type+data+CRC) are copied verbatim and
/// the new chunk's CRC is freshly produced by `write_chunk`.
fn splice_chunk_before_idat(png: &[u8], chunk_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    assert_eq!(&png[..8], &PNG_MAGIC);
    let mut out = Vec::with_capacity(png.len() + 12 + payload.len());
    out.extend_from_slice(&PNG_MAGIC);
    let mut pos = 8;
    let mut spliced = false;
    while pos < png.len() {
        let len = u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
        let next = pos + 8 + len + 4;
        let chunk_type_here = &png[pos + 4..pos + 8];
        if !spliced && chunk_type_here == b"IDAT" {
            write_chunk(&mut out, chunk_type, payload);
            spliced = true;
        }
        out.extend_from_slice(&png[pos..next]);
        pos = next;
    }
    assert!(spliced, "input stream had no IDAT");
    out
}

// ---- ct=0 (grayscale) ---------------------------------------------------

#[test]
fn gray8_trns_marks_matching_pixels_transparent() {
    // 4x1 samples: [0x50, 0x60, 0x70, 0x80]; tRNS keys gray=0x60 → pixel
    // index 1 must come out alpha-0.
    let raw: Vec<u8> = (0..4u8).map(|x| x * 0x10 + 0x50).collect();
    let img = make(4, 1, PngPixelFormat::Gray8, raw.clone());
    let png = encode_png_image(&img).expect("encode");
    // tRNS for ct=0 is a single 2-byte BE gray value at the image bit
    // depth (here 8 → high byte = 0).
    let trns_payload = [0x00, 0x60];
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &trns_payload);

    let rgba = decode_png_to_rgba(&spliced).expect("decode");
    let alphas: Vec<u8> = (0..4).map(|i| rgba.data[i * 4 + 3]).collect();
    assert_eq!(alphas, vec![255, 0, 255, 255], "{:?}", rgba.data);
    // Colour bytes are still the source gray replicated 3×.
    for (i, &g) in raw.iter().enumerate() {
        assert_eq!(rgba.data[i * 4], g);
        assert_eq!(rgba.data[i * 4 + 1], g);
        assert_eq!(rgba.data[i * 4 + 2], g);
    }
}

#[test]
fn gray16le_trns_compares_both_bytes() {
    // 4x1; samples 0x0001, 0x0002, 0x0001, 0xff00. tRNS keys 0x0001 →
    // pixels 0 + 2 must be transparent. 0x0002 must stay opaque (this
    // is the §4.2.9 note explicitly: "if the grayscale level 0x0001 is
    // specified to be transparent, it would be incorrect to compare
    // only the high-order byte and decide that 0x0002 is also
    // transparent").
    let samples: [u16; 4] = [0x0001, 0x0002, 0x0001, 0xff00];
    let mut raw = Vec::with_capacity(8);
    for s in samples {
        raw.push((s & 0xff) as u8);
        raw.push((s >> 8) as u8);
    }
    let img = make(4, 1, PngPixelFormat::Gray16Le, raw);
    let png = encode_png_image(&img).expect("encode");
    let trns_payload = [0x00, 0x01]; // BE 16-bit
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &trns_payload);

    let rgba = decode_png_to_rgba(&spliced).expect("decode");
    let alphas: Vec<u8> = (0..4).map(|i| rgba.data[i * 4 + 3]).collect();
    assert_eq!(
        alphas,
        vec![0, 255, 0, 255],
        "spec §4.2.9: compare both bytes"
    );
}

#[test]
fn gray8_trns_payload_with_wrong_length_rejected() {
    let img = make(2, 1, PngPixelFormat::Gray8, vec![0, 1]);
    let png = encode_png_image(&img).expect("encode");
    // 1-byte payload — spec is fixed at 2 bytes regardless of bit depth.
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &[0x00]);
    let err = decode_png_to_rgba(&spliced).expect_err("must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("colour type 0") && msg.contains("2 bytes"),
        "{msg}"
    );
}

#[test]
fn gray_trns_sample_within_bit_depth_accepted() {
    // For colour type 0 at bit depth 16 the tRNS sample fits the full
    // 16-bit range — any u16 is valid. Spot-check that a maximum value
    // (0xffff) doesn't trip the bounds gate that catches sub-16-bit
    // overflows elsewhere.
    let img = make(2, 1, PngPixelFormat::Gray16Le, vec![0, 0, 0, 0]);
    let png = encode_png_image(&img).expect("encode");
    let trns_payload = [0xff, 0xff];
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &trns_payload);
    let _rgba = decode_png_to_rgba(&spliced).expect("16-bit gray accepts any u16 tRNS");
}

// ---- ct=2 (truecolor) ---------------------------------------------------

#[test]
fn rgb24_trns_marks_matching_pixels_transparent() {
    // 4x1 RGB. tRNS keys the second pixel exactly.
    let raw: Vec<u8> = vec![
        10, 20, 30, // p0
        40, 50, 60, // p1 — target
        70, 80, 90, // p2
        40, 50, 61, // p3 — one byte off, must stay opaque
    ];
    let img = make(4, 1, PngPixelFormat::Rgb24, raw.clone());
    let png = encode_png_image(&img).expect("encode");
    // tRNS payload (RGB triple, each 2 bytes BE at bit_depth 8 → high byte 0).
    let trns_payload = [0, 40, 0, 50, 0, 60];
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &trns_payload);

    let rgba = decode_png_to_rgba(&spliced).expect("decode");
    let alphas: Vec<u8> = (0..4).map(|i| rgba.data[i * 4 + 3]).collect();
    assert_eq!(alphas, vec![255, 0, 255, 255]);
    for (i, src) in raw.chunks_exact(3).enumerate() {
        assert_eq!(rgba.data[i * 4], src[0]);
        assert_eq!(rgba.data[i * 4 + 1], src[1]);
        assert_eq!(rgba.data[i * 4 + 2], src[2]);
    }
}

#[test]
fn rgb48le_trns_compares_full_16bit_per_channel() {
    // 3x1 RGB48. Pixel 0 = (0x0001, 0x0002, 0x0003); tRNS keys exactly
    // (0x0001, 0x0002, 0x0003). Pixel 1 = same high bytes but
    // off-by-one low byte → must stay opaque.
    let samples: [(u16, u16, u16); 3] = [
        (0x0001, 0x0002, 0x0003),
        (0x0101, 0x0202, 0x0303),
        (0x0001, 0x0002, 0x0003),
    ];
    let mut raw = Vec::with_capacity(18);
    for (r, g, b) in samples {
        raw.push((r & 0xff) as u8);
        raw.push((r >> 8) as u8);
        raw.push((g & 0xff) as u8);
        raw.push((g >> 8) as u8);
        raw.push((b & 0xff) as u8);
        raw.push((b >> 8) as u8);
    }
    let img = make(3, 1, PngPixelFormat::Rgb48Le, raw);
    let png = encode_png_image(&img).expect("encode");
    // tRNS payload: 2-byte BE per channel, R G B.
    let trns_payload = [0x00, 0x01, 0x00, 0x02, 0x00, 0x03];
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &trns_payload);

    let rgba = decode_png_to_rgba(&spliced).expect("decode");
    let alphas: Vec<u8> = (0..3).map(|i| rgba.data[i * 4 + 3]).collect();
    assert_eq!(alphas, vec![0, 255, 0]);
}

#[test]
fn rgb24_trns_payload_with_wrong_length_rejected() {
    let img = make(2, 1, PngPixelFormat::Rgb24, vec![1, 2, 3, 4, 5, 6]);
    let png = encode_png_image(&img).expect("encode");
    // Length 5 — spec is fixed at 6 bytes for ct=2.
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &[0, 0, 0, 0, 0]);
    let err = decode_png_to_rgba(&spliced).expect_err("must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("colour type 2") && msg.contains("6 bytes"),
        "{msg}"
    );
}

#[test]
fn rgb24_trns_sample_exceeds_bit_depth_rejected() {
    // Bit depth 8 → max sample 255. tRNS holds R=256 in 2 bytes.
    let img = make(1, 1, PngPixelFormat::Rgb24, vec![1, 2, 3]);
    let png = encode_png_image(&img).expect("encode");
    let trns_payload = [0x01, 0x00, 0x00, 0x02, 0x00, 0x03];
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &trns_payload);
    let err = decode_png_to_rgba(&spliced).expect_err("must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("bit depth 8") && msg.contains("exceeds"),
        "{msg}"
    );
}

// ---- ct=4 / ct=6 prohibition -------------------------------------------

#[test]
fn ya8_trns_is_prohibited() {
    let img = make(1, 1, PngPixelFormat::Ya8, vec![100, 50]);
    let png = encode_png_image(&img).expect("encode");
    // 2-byte payload — would be valid for ct=0 but is prohibited here.
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &[0, 100]);
    let err = decode_png_to_rgba(&spliced).expect_err("must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("prohibited") && msg.contains("colour type 4"),
        "{msg}"
    );
}

#[test]
fn rgba_trns_is_prohibited() {
    let img = make(1, 1, PngPixelFormat::Rgba, vec![10, 20, 30, 255]);
    let png = encode_png_image(&img).expect("encode");
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &[0, 10, 0, 20, 0, 30]);
    let err = decode_png_to_rgba(&spliced).expect_err("must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("prohibited") && msg.contains("colour type 6"),
        "{msg}"
    );
}

// ---- ct=3 length policing ---------------------------------------------

#[test]
fn pal8_trns_with_too_many_entries_rejected() {
    // 2-entry PLTE → tRNS may carry at most 2 alpha values.
    let palette: Vec<u8> = vec![0, 0, 0, 255, 255, 255];
    let img = PngImage {
        width: 2,
        height: 1,
        pixel_format: PngPixelFormat::Pal8,
        stride: 2,
        data: vec![0, 1],
        palette,
    };
    let png = encode_png_image(&img).expect("encode");
    // Splice an over-long tRNS (3 bytes for a 2-entry PLTE).
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &[0, 64, 128]);
    let err = decode_png_to_rgba(&spliced).expect_err("must reject");
    let msg = format!("{err}");
    assert!(msg.contains("exceed") && msg.contains("PLTE"), "{msg}");
}

#[test]
fn pal8_trns_shorter_than_plte_accepted_with_trailing_opaque() {
    // 3-entry PLTE + 2-byte tRNS → entry 2 stays opaque per spec.
    let palette: Vec<u8> = vec![10, 20, 30, 40, 50, 60, 70, 80, 90];
    let img = PngImage {
        width: 3,
        height: 1,
        pixel_format: PngPixelFormat::Pal8,
        stride: 3,
        data: vec![0, 1, 2],
        palette,
    };
    let png = encode_png_image(&img).expect("encode");
    let spliced = splice_chunk_before_idat(&png, b"tRNS", &[0x10, 0x80]);
    // The encoder will have already written its own tRNS (none — palette
    // has no alpha tail), so the splice introduces the first tRNS. Decode
    // and assert per-entry alpha.
    // Round-trip is via decode_png_to_rgba which walks the *splice*
    // stream's chunks, so the per-entry alpha lookup picks up our values.
    let rgba = decode_png_to_rgba(&spliced).expect("decode");
    assert_eq!(rgba.data[3], 0x10, "entry 0 alpha");
    assert_eq!(rgba.data[7], 0x80, "entry 1 alpha");
    assert_eq!(rgba.data[11], 255, "entry 2 implicit opaque");
}
