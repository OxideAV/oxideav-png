//! Unrecognised-ancillary-chunk preservation for the PNG *editor*
//! round-trip (W3C PNG3 §14.2 "Behavior of PNG editors").
//!
//! The decoder captures any ancillary chunk type it does not parse into
//! [`PngMetadata::unknowns`], recording which side of the `IDAT` run it
//! was found on; the encoder replays each chunk on the same side. An
//! unrecognised *critical* chunk is a hard decode error ("PNG editors
//! shall terminate on encountering an unrecognized critical chunk
//! type"), and a chunk whose name carries a non-letter byte (§13.1
//! malformed) is dropped rather than re-emitted.

use oxideav_png::{
    decode_png, encode_apng, encode_png_image, encode_png_image_with_options, parse_apng,
    parse_metadata, PngEncoderOptions, PngImage, PngPixelFormat, UnknownChunk,
};

fn rgba_2x2() -> PngImage {
    PngImage {
        width: 2,
        height: 2,
        pixel_format: PngPixelFormat::Rgba,
        stride: 8,
        data: vec![
            255, 0, 0, 255, // (0,0)
            0, 255, 0, 255, // (1,0)
            0, 0, 255, 255, // (0,1)
            255, 255, 255, 255, // (1,1)
        ],
        palette: Vec::new(),
    }
}

/// Append a length|type|data|CRC chunk to `out` using the production
/// chunk writer so the CRC matches exactly what the walker expects.
fn push_chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    oxideav_png::chunk::write_chunk(out, ty, data);
}

/// Splice `chunks` into an encoded PNG immediately *before* the first
/// `IDAT` (i.e. after IHDR + any pre-IDAT chunks). Returns the new
/// stream.
fn splice_before_idat(bytes: &[u8], chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let idat = bytes
        .windows(4)
        .position(|w| w == b"IDAT")
        .expect("IDAT present");
    // The length prefix is the 4 bytes before the type code.
    let inject = idat - 4;
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..inject]);
    for (ty, data) in chunks {
        push_chunk(&mut out, ty, data);
    }
    out.extend_from_slice(&bytes[inject..]);
    out
}

/// Splice `chunks` in *after* the IDAT run, immediately before `IEND`.
fn splice_before_iend(bytes: &[u8], chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let iend = bytes
        .windows(4)
        .position(|w| w == b"IEND")
        .expect("IEND present");
    let inject = iend - 4;
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..inject]);
    for (ty, data) in chunks {
        push_chunk(&mut out, ty, data);
    }
    out.extend_from_slice(&bytes[inject..]);
    out
}

#[test]
fn unknown_ancillary_before_idat_is_captured() {
    let base = encode_png_image(&rgba_2x2()).expect("encode");
    // `prVt`: ancillary (lowercase 1st), private (lowercase 2nd),
    // reserved-clear (uppercase 3rd), safe-to-copy (lowercase 4th).
    let payload = b"hello-extension".to_vec();
    let tampered = splice_before_idat(&base, &[(b"prVt", &payload)]);

    let meta = parse_metadata(&tampered).expect("parse");
    assert_eq!(meta.unknowns.len(), 1, "one unknown ancillary captured");
    let u = &meta.unknowns[0];
    assert_eq!(&u.chunk_type, b"prVt");
    assert_eq!(u.data, payload);
    assert!(!u.after_idat, "spliced before IDAT");
    assert!(u.is_safe_to_copy(), "prVt fourth letter is lowercase");
    assert!(u.is_private(), "prVt second letter is lowercase");
}

#[test]
fn unknown_ancillary_after_idat_is_captured() {
    let base = encode_png_image(&rgba_2x2()).expect("encode");
    // `unSafe`-shaped: `prVT` is unsafe-to-copy (uppercase 4th letter).
    let payload = vec![1u8, 2, 3, 4];
    let tampered = splice_before_iend(&base, &[(b"prVT", &payload)]);

    let meta = parse_metadata(&tampered).expect("parse");
    assert_eq!(meta.unknowns.len(), 1);
    let u = &meta.unknowns[0];
    assert_eq!(&u.chunk_type, b"prVT");
    assert_eq!(u.data, payload);
    assert!(u.after_idat, "spliced after IDAT");
    assert!(!u.is_safe_to_copy(), "prVT fourth letter is uppercase");
}

#[test]
fn unknown_chunks_round_trip_on_correct_side_of_idat() {
    let base = encode_png_image(&rgba_2x2()).expect("encode");
    let before = b"prVt".to_owned();
    let after = b"prVT".to_owned();
    let before_payload = b"BEFORE".to_vec();
    let after_payload = b"AFTER".to_vec();
    let tampered = splice_before_idat(&base, &[(&before, &before_payload)]);
    let tampered = splice_before_iend(&tampered, &[(&after, &after_payload)]);

    let meta = parse_metadata(&tampered).expect("parse");
    assert_eq!(meta.unknowns.len(), 2);

    // Re-encode carrying the captured unknowns forward.
    let opts = PngEncoderOptions {
        metadata: Some(meta.clone()),
        ..Default::default()
    };
    let reencoded = encode_png_image_with_options(&rgba_2x2(), &opts).expect("re-encode");

    // The re-decoded metadata must match exactly (same types, payloads,
    // and IDAT sides).
    let meta2 = parse_metadata(&reencoded).expect("re-parse");
    assert_eq!(meta2.unknowns, meta.unknowns, "unknowns survive round-trip");

    // Positional check: `prVt` precedes IDAT, `prVT` follows it.
    let pos = |tag: &[u8; 4]| reencoded.windows(4).position(|w| w == tag);
    let idat = pos(b"IDAT").expect("IDAT");
    let p_before = pos(&before).expect("prVt present");
    let p_after = pos(&after).expect("prVT present");
    assert!(p_before < idat, "safe-to-copy chunk stays before IDAT");
    assert!(p_after > idat, "unsafe-to-copy chunk stays after IDAT");

    // The pixel payload is unaffected by the spliced chunks.
    let img = decode_png(&reencoded).expect("decode");
    assert_eq!(img.width, 2);
    assert_eq!(img.height, 2);
}

#[test]
fn unrecognised_critical_chunk_is_a_decode_error() {
    let base = encode_png_image(&rgba_2x2()).expect("encode");
    // `PrIv`: ancillary bit CLEAR (uppercase 1st letter `P`) → a
    // critical chunk the codec does not recognise. §14.2 mandates
    // termination.
    let tampered = splice_before_idat(&base, &[(b"PrIv", b"x")]);
    let err = parse_metadata(&tampered).expect_err("unknown critical must error");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("critical"),
        "expected an unrecognised-critical-chunk error, got {msg}"
    );
}

#[test]
fn decode_png_rejects_unrecognised_critical_chunk() {
    // §5.4 / §13.1: the pixel-decode path must also refuse an unknown
    // critical chunk rather than silently produce a possibly-wrong
    // image. `PrIv` has the ancillary bit clear (uppercase `P`).
    let base = encode_png_image(&rgba_2x2()).expect("encode");
    let tampered = splice_before_idat(&base, &[(b"PrIv", b"x")]);
    let err = decode_png(&tampered).expect_err("decode must refuse unknown critical");
    assert!(format!("{err:?}").contains("critical"));
}

#[test]
fn decode_png_ignores_unknown_ancillary_chunk() {
    // An unrecognised *ancillary* chunk is skipped by the pixel-decode
    // path (it carries no image-data dependency the decoder must
    // honour); only `parse_metadata` captures it.
    let base = encode_png_image(&rgba_2x2()).expect("encode");
    let tampered = splice_before_idat(&base, &[(b"prVt", b"ignored")]);
    let img = decode_png(&tampered).expect("decode tolerates unknown ancillary");
    assert_eq!((img.width, img.height), (2, 2));
}

#[test]
fn malformed_name_chunk_is_dropped_not_captured() {
    let base = encode_png_image(&rgba_2x2()).expect("encode");
    // A name with a digit byte (`pH1s`) is §13.1-malformed. It has the
    // ancillary bit set (lowercase 'p') so it is not a critical-chunk
    // error, but it is not a conformant extension either — drop it
    // rather than propagate the malformation.
    let tampered = splice_before_idat(&base, &[(b"pH1s", b"junk")]);
    let meta = parse_metadata(&tampered).expect("parse (malformed name tolerated)");
    assert!(
        meta.unknowns.is_empty(),
        "a non-letter chunk name is dropped, not captured"
    );
}

#[test]
fn file_order_of_multiple_unknowns_is_preserved() {
    let base = encode_png_image(&rgba_2x2()).expect("encode");
    let tampered = splice_before_idat(
        &base,
        &[(b"prVt", b"one"), (b"qrVt", b"two"), (b"srVt", b"three")],
    );
    let meta = parse_metadata(&tampered).expect("parse");
    let order: Vec<[u8; 4]> = meta.unknowns.iter().map(|u| u.chunk_type).collect();
    assert_eq!(order, vec![*b"prVt", *b"qrVt", *b"srVt"]);

    // The decoder field constructs the expected struct shape.
    assert_eq!(
        meta.unknowns[1],
        UnknownChunk {
            chunk_type: *b"qrVt",
            data: b"two".to_vec(),
            after_idat: false,
        }
    );
}

#[test]
fn parse_apng_rejects_unrecognised_critical_chunk() {
    // The APNG decode path applies the same §5.4 / §14.2 unknown-critical
    // gate as `decode_png`. acTL / fcTL / fdAT are ancillary, so the
    // critical allow-set is the four core chunks; `PrIv` is rejected.
    let frames = [rgba_2x2(), rgba_2x2()];
    let base = encode_apng(&frames, 10, 0).expect("encode apng");
    let tampered = splice_before_idat(&base, &[(b"PrIv", b"x")]);
    let err = parse_apng(&tampered).expect_err("apng decode must refuse unknown critical");
    assert!(format!("{err:?}").contains("critical"));
}

#[test]
fn no_unknowns_when_stream_has_none() {
    let base = encode_png_image(&rgba_2x2()).expect("encode");
    let meta = parse_metadata(&base).expect("parse");
    assert!(meta.unknowns.is_empty());
    assert!(meta.is_empty(), "a bare image has no metadata at all");
}
