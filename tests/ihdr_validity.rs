//! IHDR field-validity gate (W3C PNG3 §11.2.1 "IHDR Image header").
//!
//! The decoder rejects a structurally-malformed IHDR at the wire-decode
//! boundary rather than letting a degenerate value (zero dimension, a
//! colour-type/bit-depth pairing outside §11.2.1 Table 12, or an unknown
//! compression / filter / interlace method) flow downstream and surface
//! as a confusing late error — or, for a zero dimension, as a silently
//! empty decode.
//!
//! Each case builds a *valid* PNG with the public encoder, patches the
//! 13-byte IHDR payload in place, recomputes the IHDR chunk CRC so the
//! framing stays intact, and asserts the decoder reports `InvalidData`.
//! Patching a valid stream isolates the IHDR gate: the only thing wrong
//! with the stream under test is the field we deliberately corrupted.

use oxideav_core::{CodecId, CodecParameters, Frame, PixelFormat, VideoFrame, VideoPlane};

/// Standard PNG CRC-32 (W3C PNG3 §13.2, polynomial 0xEDB88320, computed
/// over the chunk type + data). Inlined here so the test depends only on
/// the public crate surface, not an internal helper.
fn png_crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// Encode a minimal valid 2x2 Gray8 PNG.
fn valid_png() -> Vec<u8> {
    let mut params = CodecParameters::video(CodecId::new("png"));
    params.width = Some(2);
    params.height = Some(2);
    params.pixel_format = Some(PixelFormat::Gray8);
    let mut enc = oxideav_png::encoder::make_encoder(&params).unwrap();
    enc.send_frame(&Frame::Video(VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: 2,
            data: vec![10u8, 20, 30, 40],
        }],
    }))
    .unwrap();
    enc.flush().unwrap();
    enc.receive_packet().unwrap().data
}

/// IHDR layout in a freshly-encoded PNG:
/// 8 (signature) + 4 (length) + 4 (type "IHDR") + 13 (data) + 4 (CRC).
/// The 13-byte payload starts at offset 16; its CRC is at offset 29.
const IHDR_DATA_OFF: usize = 16;
const IHDR_TYPE_OFF: usize = 12; // "IHDR" type tag, start of CRC coverage
const IHDR_CRC_OFF: usize = 29;

/// Apply `mutate` to the 13-byte IHDR payload, then fix the IHDR CRC so
/// the stream framing is intact and only the field value is at fault.
fn corrupt_ihdr(mut bytes: Vec<u8>, mutate: impl FnOnce(&mut [u8])) -> Vec<u8> {
    mutate(&mut bytes[IHDR_DATA_OFF..IHDR_DATA_OFF + 13]);
    let crc = png_crc32(&bytes[IHDR_TYPE_OFF..IHDR_DATA_OFF + 13]);
    bytes[IHDR_CRC_OFF..IHDR_CRC_OFF + 4].copy_from_slice(&crc.to_be_bytes());
    bytes
}

fn assert_rejected(bytes: &[u8], needle: &str) {
    let err = oxideav_png::decode_png(bytes)
        .err()
        .unwrap_or_else(|| panic!("expected decode error for {needle}, got Ok"));
    let msg = format!("{err}");
    assert!(
        msg.contains(needle),
        "expected error mentioning {needle:?}, got: {msg}"
    );
    // The mutated stream is well-framed; the failure must be classified as
    // invalid *data*, not an unsupported feature.
    assert!(
        matches!(err, oxideav_png::PngError::InvalidData(_)),
        "expected InvalidData for {needle}, got: {err:?}"
    );
}

#[test]
fn unmodified_stream_still_decodes() {
    // Sanity: the offsets and CRC helper produce a stream that decodes,
    // so a rejection in the other cases is the mutation, not the harness.
    let bytes = corrupt_ihdr(valid_png(), |_| {});
    assert!(oxideav_png::decode_png(&bytes).is_ok());
}

#[test]
fn zero_width_is_rejected() {
    // §11.2.1: "Zero is an invalid value." Width occupies bytes 0..4.
    let bytes = corrupt_ihdr(valid_png(), |d| {
        d[0..4].copy_from_slice(&0u32.to_be_bytes())
    });
    assert_rejected(&bytes, "dimension");
}

#[test]
fn zero_height_is_rejected() {
    // Height occupies bytes 4..8.
    let bytes = corrupt_ihdr(valid_png(), |d| {
        d[4..8].copy_from_slice(&0u32.to_be_bytes())
    });
    assert_rejected(&bytes, "dimension");
}

#[test]
fn one_bit_truecolor_is_rejected() {
    // §11.2.1 Table 12: truecolor (colour type 2) allows only 8 / 16.
    // Byte 8 = bit depth, byte 9 = colour type.
    let bytes = corrupt_ihdr(valid_png(), |d| {
        d[8] = 1; // bit depth 1
        d[9] = 2; // truecolor
    });
    assert_rejected(&bytes, "Table 12");
}

#[test]
fn sixteen_bit_indexed_is_rejected() {
    // Table 12: indexed (colour type 3) tops out at bit depth 8.
    let bytes = corrupt_ihdr(valid_png(), |d| {
        d[8] = 16;
        d[9] = 3;
    });
    assert_rejected(&bytes, "Table 12");
}

#[test]
fn bit_depth_zero_is_rejected() {
    // 0 is not a member of any Table 12 row.
    let bytes = corrupt_ihdr(valid_png(), |d| d[8] = 0);
    assert_rejected(&bytes, "Table 12");
}

#[test]
fn non_power_of_two_bit_depth_is_rejected() {
    // 3 is not a Table 12 depth for any colour type.
    let bytes = corrupt_ihdr(valid_png(), |d| d[8] = 3);
    assert_rejected(&bytes, "Table 12");
}

#[test]
fn invented_colour_type_is_rejected() {
    // §6.1 / Table 9 defines only colour types 0, 2, 3, 4, 6. An out-of-range
    // colour-type byte is caught by `colour_type_typed` ("bad colour type")
    // inside the combination check; either way the verdict is InvalidData.
    for ct in [1u8, 5, 7, 8, 255] {
        let bytes = corrupt_ihdr(valid_png(), |d| {
            d[8] = 8;
            d[9] = ct;
        });
        assert_rejected(&bytes, "colour type");
    }
}

#[test]
fn unknown_compression_method_is_rejected() {
    // §11.2.1: "Only compression method 0 ... is defined." Byte 10.
    let bytes = corrupt_ihdr(valid_png(), |d| d[10] = 1);
    assert_rejected(&bytes, "compression method");
}

#[test]
fn unknown_filter_method_is_rejected() {
    // §11.2.1: "Only filter method 0 is defined." Byte 11.
    let bytes = corrupt_ihdr(valid_png(), |d| d[11] = 1);
    assert_rejected(&bytes, "filter method");
}

#[test]
fn unknown_interlace_method_is_rejected() {
    // §11.2.1 defines interlace methods 0 (none) and 1 (Adam7). Byte 12.
    let bytes = corrupt_ihdr(valid_png(), |d| d[12] = 2);
    assert_rejected(&bytes, "interlace method");
}

#[test]
fn parse_metadata_also_gates_ihdr() {
    // The gate lives in `Ihdr::parse`, so `parse_metadata` (which never
    // touches the pixel pipeline) rejects the same malformed IHDR.
    let bytes = corrupt_ihdr(valid_png(), |d| {
        d[0..4].copy_from_slice(&0u32.to_be_bytes())
    });
    let err = oxideav_png::parse_metadata(&bytes).unwrap_err();
    assert!(matches!(err, oxideav_png::PngError::InvalidData(_)));
}
