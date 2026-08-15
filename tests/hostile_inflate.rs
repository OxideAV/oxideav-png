//! Hostile-input coverage for every zlib-inflate site in the crate:
//! decompression bombs behind small chunks (W3C PNG3 §13.3 Security
//! considerations — "chunks can be extremely large"), oversized ICC
//! profiles, and the bomb-shaped IDAT / fdAT pixel streams whose
//! declared canvas implies a much smaller filtered stream.
//!
//! The library caps each inflate: IDAT / fdAT at the IHDR- / fcTL-
//! implied filtered-stream size (+1 so an overlong stream is
//! detectable), and the compressed metadata bodies (`zTXt` / `iTXt` /
//! `iCCP`) at [`MAX_INFLATED_METADATA_LEN`]. Every test here asserts
//! the hostile stream surfaces as an `Err` — never a crash, panic, or
//! unbounded allocation.

use compcol::zlib::{EncoderConfig, Zlib};
use oxideav_png::chunk::write_chunk;
use oxideav_png::metadata::{Iccp, Itxt, Ztxt};
use oxideav_png::{
    decode_apng, decode_png, encode_apng, encode_png_image, parse_metadata, Ihdr, PngImage,
    PngPixelFormat, MAX_INFLATED_METADATA_LEN,
};

/// Compress `data` into a zlib stream (level 1 — cheapest; these tests
/// only care that the stream *inflates* to the target size).
fn deflate(data: &[u8]) -> Vec<u8> {
    compcol::vec::compress_to_vec_with::<Zlib>(data, EncoderConfig { level: 1 })
        .expect("test-side zlib compression")
}

fn gray_1x1() -> PngImage {
    PngImage {
        width: 1,
        height: 1,
        pixel_format: PngPixelFormat::Gray8,
        stride: 1,
        data: vec![0x7F],
        palette: Vec::new(),
    }
}

/// Walk a PNG datastream's chunks, returning `(type, whole-chunk byte
/// range)` pairs (range covers length + type + data + CRC).
fn chunk_ranges(bytes: &[u8]) -> Vec<([u8; 4], std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let mut pos = 8; // signature
    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let ty = [
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ];
        let end = pos + 12 + len;
        out.push((ty, pos..end));
        pos = end;
    }
    out
}

// ---- metadata chunk bombs (cap = MAX_INFLATED_METADATA_LEN) ----------

/// A zlib body that inflates to one byte past the metadata bound.
fn metadata_bomb_body() -> Vec<u8> {
    deflate(&vec![b'A'; MAX_INFLATED_METADATA_LEN as usize + 1])
}

#[test]
fn ztxt_decompression_bomb_rejected() {
    // keyword + NUL + method 0 + bomb body.
    let mut payload = b"Comment\0\0".to_vec();
    payload.extend_from_slice(&metadata_bomb_body());
    let err = Ztxt::parse(&payload).expect_err("bomb must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("inflates past"),
        "want decompression-bomb error, got: {msg}"
    );
}

#[test]
fn iccp_decompression_bomb_rejected() {
    // profile name + NUL + method 0 + bomb body.
    let mut payload = b"bomb-profile\0\0".to_vec();
    payload.extend_from_slice(&metadata_bomb_body());
    let err = Iccp::parse(&payload).expect_err("bomb must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("inflates past"),
        "want oversized-profile error, got: {msg}"
    );
}

#[test]
fn itxt_compressed_decompression_bomb_rejected() {
    // keyword + NUL + flag 1 + method 0 + empty lang + NUL + empty
    // translated keyword + NUL + bomb body.
    let mut payload = b"Comment\0\x01\0\0\0".to_vec();
    payload.extend_from_slice(&metadata_bomb_body());
    let err = Itxt::parse(&payload).expect_err("bomb must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("inflates past"),
        "want decompression-bomb error, got: {msg}"
    );
}

#[test]
fn ztxt_bomb_spliced_into_file_fails_parse_metadata() {
    // The full-file path must reject the same bomb (the per-chunk parse
    // routines are what parse_metadata dispatches into).
    let bytes = encode_png_image(&gray_1x1()).expect("encode");
    let mut payload = b"Comment\0\0".to_vec();
    payload.extend_from_slice(&metadata_bomb_body());
    let mut chunk = Vec::new();
    write_chunk(&mut chunk, b"zTXt", &payload);
    // Splice ahead of IEND.
    let iend = chunk_ranges(&bytes)
        .into_iter()
        .find(|(ty, _)| ty == b"IEND")
        .expect("IEND")
        .1;
    let mut tampered = bytes[..iend.start].to_vec();
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[iend.start..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn large_but_legitimate_compressed_text_still_parses() {
    // Just *under* the bound must keep working — the cap is a bomb
    // defence, not a functional limit on real annotations.
    let text = "x".repeat(4 * 1024 * 1024); // 4 MiB, well under 64 MiB
    let z = Ztxt {
        keyword: "Comment".into(),
        text: text.clone(),
    };
    let wire = z.to_bytes().expect("encode");
    let back = Ztxt::parse(&wire).expect("parse");
    assert_eq!(back.text.len(), text.len());
}

// ---- encoder-side mirror of the bound --------------------------------

#[test]
fn oversized_ztxt_text_rejected_on_encode() {
    let z = Ztxt {
        keyword: "Comment".into(),
        text: "y".repeat(MAX_INFLATED_METADATA_LEN as usize + 1),
    };
    let err = z.to_bytes().expect_err("over-bound text must be rejected");
    assert!(format!("{err}").contains("exceeds"));
}

#[test]
fn oversized_iccp_profile_rejected_on_encode() {
    let p = Iccp {
        name: "big".into(),
        profile: vec![0u8; MAX_INFLATED_METADATA_LEN as usize + 1],
    };
    let err = p
        .to_bytes()
        .expect_err("over-bound profile must be rejected");
    assert!(format!("{err}").contains("exceeds"));
}

#[test]
fn oversized_itxt_text_rejected_on_encode() {
    let t = Itxt {
        keyword: "Comment".into(),
        compressed: true,
        language_tag: String::new(),
        translated_keyword: String::new(),
        text: "z".repeat(MAX_INFLATED_METADATA_LEN as usize + 1),
    };
    let err = t.to_bytes().expect_err("over-bound text must be rejected");
    assert!(format!("{err}").contains("exceeds"));
}

// ---- IDAT bomb behind a tiny declared canvas -------------------------

#[test]
fn idat_bomb_behind_1x1_ihdr_rejected() {
    // A 1x1 Gray8 image implies a 2-byte filtered stream (1 filter-type
    // byte + 1 sample byte). Hand the decoder an IDAT that inflates to
    // 64 KiB instead: the capped inflate must cut it off at
    // expected + 1 = 3 bytes and error, without materialising the rest.
    let good = encode_png_image(&gray_1x1()).expect("encode");
    let bomb_idat = deflate(&vec![0u8; 64 * 1024]);
    let mut tampered = good[..8].to_vec();
    for (ty, range) in chunk_ranges(&good) {
        if &ty == b"IDAT" {
            let mut chunk = Vec::new();
            write_chunk(&mut chunk, b"IDAT", &bomb_idat);
            tampered.extend_from_slice(&chunk);
        } else {
            tampered.extend_from_slice(&good[range]);
        }
    }
    let err = decode_png(&tampered).expect_err("bomb IDAT must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("filtered-stream size"),
        "want IHDR-implied-size error, got: {msg}"
    );
}

#[test]
fn idat_short_stream_still_reports_length_mismatch() {
    // The cap must not swallow the pre-existing too-short diagnostics:
    // a stream inflating *under* the expected size is a plain length
    // mismatch, not a bomb.
    let good = encode_png_image(&gray_1x1()).expect("encode");
    let short_idat = deflate(&[0u8; 1]); // 1 byte < expected 2
    let mut tampered = good[..8].to_vec();
    for (ty, range) in chunk_ranges(&good) {
        if &ty == b"IDAT" {
            let mut chunk = Vec::new();
            write_chunk(&mut chunk, b"IDAT", &short_idat);
            tampered.extend_from_slice(&chunk);
        } else {
            tampered.extend_from_slice(&good[range]);
        }
    }
    assert!(decode_png(&tampered).is_err());
}

// ---- fdAT bomb behind a small fcTL region ----------------------------

#[test]
fn fdat_bomb_behind_small_fctl_rejected() {
    // Build a valid 2-frame 2x2 APNG, then swell the second frame's
    // fdAT into a stream that inflates far past the fcTL-implied
    // filtered size. The per-frame capped inflate must reject it.
    let f = PngImage {
        width: 2,
        height: 2,
        pixel_format: PngPixelFormat::Rgba,
        stride: 8,
        data: vec![0x40; 16],
        palette: Vec::new(),
    };
    let good = encode_apng(&[f.clone(), f], 10, 0).expect("encode apng");
    let bomb = deflate(&vec![0u8; 64 * 1024]);
    let mut tampered = good[..8].to_vec();
    for (ty, range) in chunk_ranges(&good) {
        if &ty == b"fdAT" {
            // fdAT payload = 4-byte sequence number + compressed data.
            let seq = &good[range.start + 8..range.start + 12];
            let mut payload = seq.to_vec();
            payload.extend_from_slice(&bomb);
            let mut chunk = Vec::new();
            write_chunk(&mut chunk, b"fdAT", &payload);
            tampered.extend_from_slice(&chunk);
        } else {
            tampered.extend_from_slice(&good[range]);
        }
    }
    let err = decode_apng(&tampered).expect_err("bomb fdAT must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("filtered-stream size"),
        "want fcTL-implied-size error, got: {msg}"
    );
}

// ---- expected_filtered_len pins --------------------------------------

#[test]
fn expected_filtered_len_matches_hand_computed_shapes() {
    // Non-interlaced 1x1 Gray8: 1 row x (1 filter byte + 1 sample).
    let ihdr = Ihdr {
        width: 1,
        height: 1,
        bit_depth: 8,
        colour_type: 0,
        compression: 0,
        filter: 0,
        interlace: 0,
    };
    assert_eq!(ihdr.expected_filtered_len().unwrap(), 2);

    // Non-interlaced 3x2 RGBA16: 2 rows x (1 + 3*8).
    let ihdr = Ihdr {
        width: 3,
        height: 2,
        bit_depth: 16,
        colour_type: 6,
        compression: 0,
        filter: 0,
        interlace: 0,
    };
    assert_eq!(ihdr.expected_filtered_len().unwrap(), 2 * (1 + 24));

    // Adam7 7x7 Gray8 — hand-summed per-pass (W3C PNG3 §8.1: each pass
    // serialized as a complete image of its own dimensions):
    //   p1 1x1:2  p2 1x1:2  p3 2x1:3  p4 2x2:6  p5 4x2:10
    //   p6 3x4:16 p7 7x3:24  = 63.
    let ihdr = Ihdr {
        width: 7,
        height: 7,
        bit_depth: 8,
        colour_type: 0,
        compression: 0,
        filter: 0,
        interlace: 1,
    };
    assert_eq!(ihdr.expected_filtered_len().unwrap(), 63);

    // Adam7 sub-8x8 shapes exercise the empty-pass arms: a 1x1
    // interlaced image has pixels only in pass 1.
    let ihdr = Ihdr {
        width: 1,
        height: 1,
        bit_depth: 8,
        colour_type: 0,
        compression: 0,
        filter: 0,
        interlace: 1,
    };
    assert_eq!(ihdr.expected_filtered_len().unwrap(), 2);
}

#[test]
fn expected_filtered_len_saturates_on_max_dimensions() {
    // 2^31-1 x 2^31-1 RGBA16 overflows u64 in the naive multiply; the
    // helper must saturate, not wrap (a wrapped small value would
    // wrongly cap a legitimate inflate).
    let ihdr = Ihdr {
        width: u32::MAX >> 1,
        height: u32::MAX >> 1,
        bit_depth: 16,
        colour_type: 6,
        compression: 0,
        filter: 0,
        interlace: 0,
    };
    let v = ihdr.expected_filtered_len().unwrap();
    assert!(v >= (u32::MAX >> 1) as u64 * 24);
}

// ---- iTXt hostile bodies beyond the bomb -----------------------------

#[test]
fn itxt_compressed_body_with_invalid_utf8_rejected() {
    // The *compressed* arm must apply the same rfc3629 validation as
    // the uncompressed arm: 0xFF can never appear in UTF-8.
    let mut payload = b"Comment\0\x01\0\0\0".to_vec();
    payload.extend_from_slice(&deflate(&[0xFF, 0xFE, 0x80]));
    let err = Itxt::parse(&payload).expect_err("bad UTF-8 must be rejected");
    assert!(format!("{err}").contains("UTF-8"));
}

#[test]
fn itxt_compressed_body_with_nul_rejected() {
    // §11.3.3.4: the text "shall not contain a zero byte" — including
    // when it arrives compressed.
    let mut payload = b"Comment\0\x01\0\0\0".to_vec();
    payload.extend_from_slice(&deflate(b"embedded\0nul"));
    let err = Itxt::parse(&payload).expect_err("NUL in text must be rejected");
    assert!(format!("{err}").contains("NUL"));
}

#[test]
fn itxt_translated_keyword_with_invalid_utf8_rejected() {
    // keyword + NUL + flag 0 + method 0 + lang "en" + NUL + bad-UTF-8
    // translated keyword + NUL + text.
    let mut payload = b"Comment\0\0\0en\0".to_vec();
    payload.extend_from_slice(&[0xC3, 0x28]); // invalid 2-byte sequence
    payload.push(0);
    payload.extend_from_slice(b"body");
    let err = Itxt::parse(&payload).expect_err("bad UTF-8 translated keyword");
    assert!(format!("{err}").contains("UTF-8"));
}

#[test]
fn itxt_non_ascii_language_tag_rejected() {
    // BCP47 tags are ASCII; a high byte in the tag is malformed.
    let mut payload = b"Comment\0\0\0".to_vec();
    payload.push(0xE9); // 'e-acute' in Latin-1 — not ASCII
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(b"body");
    let err = Itxt::parse(&payload).expect_err("non-ASCII language tag");
    assert!(format!("{err}").contains("ASCII"));
}

#[test]
fn iccp_truncated_after_name_rejected() {
    // Name + NUL but no compression-method byte.
    assert!(Iccp::parse(b"profile\0").is_err());
    // No NUL at all.
    assert!(Iccp::parse(b"profile-name-without-nul").is_err());
    // Method byte present but truncated zlib body (empty is a corrupt
    // stream, not an empty profile — zlib needs its 2-byte header).
    assert!(Iccp::parse(b"profile\0\0").is_err());
}
