//! `encode_into` appends the file to a caller's buffer.

use oxideav_png::{
    encode, encode_into, EncodeOptions, Iccp, PngImage, PngMetadata, PngPixelFormat,
};

fn image() -> PngImage {
    let data: Vec<u8> = (0..64 * 32 * 3).map(|i| (i * 7 % 251) as u8).collect();
    PngImage::from_rgb8(64, 32, data).unwrap()
}

fn opts_with_profile() -> EncodeOptions {
    let profile: Vec<u8> = (0..20_000u32).map(|i| (i * 31 % 253) as u8).collect();
    EncodeOptions::default()
        .with_metadata(PngMetadata::default().with_iccp(Iccp::new("p".to_string(), profile)))
}

#[test]
fn encode_into_appends_the_same_bytes_as_encode() {
    let img = image();
    let opts = opts_with_profile();
    let whole = encode(&img, &opts).unwrap();

    let mut buf = b"prefix".to_vec();
    encode_into(&img, &opts, &mut buf).unwrap();
    assert_eq!(&buf[..6], b"prefix");
    assert_eq!(&buf[6..], &whole[..]);

    // A second file appends after the first.
    encode_into(&img, &opts, &mut buf).unwrap();
    assert_eq!(&buf[6 + whole.len()..], &whole[..]);
}

/// A caller that reserved room for the whole file keeps its buffer:
/// no regrowth, no move.
#[test]
fn encode_into_uses_the_room_the_caller_reserved() {
    let img = image();
    let opts = opts_with_profile();
    let len = encode(&img, &opts).unwrap().len();
    // At least the encoder's own estimate, so it does not reserve more.
    let room = len.max(1024 + 64 * 32 * 3 / 3) + 100;
    let mut buf = Vec::with_capacity(1 + room);
    buf.extend_from_slice(b"x");
    let (ptr, cap) = (buf.as_ptr(), buf.capacity());
    encode_into(&img, &opts, &mut buf).unwrap();
    assert_eq!(buf.len(), 1 + len);
    assert_eq!(buf.capacity(), cap, "the reserved buffer was not regrown");
    assert_eq!(buf.as_ptr(), ptr, "the reserved buffer was not moved");
}

#[test]
fn encode_into_leaves_the_buffer_unchanged_on_error() {
    // A sub-byte sample out of range fails in the pixel stream, after
    // the signature, IHDR and the iCCP chunk are already written.
    let mut data = vec![1u8; 16 * 4];
    data[37] = 9;
    let img = PngImage::packed(16, 4, PngPixelFormat::Gray8, 16, data).unwrap();
    let opts = opts_with_profile().with_bit_depth(2);
    let mut buf = b"keep".to_vec();
    let err = encode_into(&img, &opts, &mut buf).unwrap_err();
    assert!(err.to_string().contains("exceeds"), "got {err}");
    assert_eq!(buf, b"keep");

    // And a bad option fails before anything is written.
    let bad = EncodeOptions::default().with_compression_level(12);
    assert!(encode_into(&image(), &bad, &mut buf).is_err());
    assert_eq!(buf, b"keep");
}
