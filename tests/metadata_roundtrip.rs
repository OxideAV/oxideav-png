//! sBIT / pHYs / tIME round-trip through the standalone encode / decode
//! API. Verifies the encoder emits each chunk at a spec-compliant
//! position and the decoder reads them back byte-for-byte.

use oxideav_png::{
    decode_png, encode_png_image, encode_png_image_with_options, parse_metadata, Phys, PhysUnit,
    PngEncoderOptions, PngImage, PngMetadata, PngPixelFormat, Sbit, Time,
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

fn gray8_2x2() -> PngImage {
    PngImage {
        width: 2,
        height: 2,
        pixel_format: PngPixelFormat::Gray8,
        stride: 2,
        data: vec![0, 64, 128, 255],
        palette: Vec::new(),
    }
}

#[test]
fn no_metadata_in_default_encode() {
    // The baseline encoder (no options.metadata) must not emit any of
    // these chunks — the round-trip then reports `PngMetadata::default`.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert!(meta.is_empty());
    assert!(meta.sbit.is_none());
    assert!(meta.phys.is_none());
    assert!(meta.time.is_none());
}

#[test]
fn sbit_rgba_8bit_roundtrip() {
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            sbit: Some(Sbit::Rgba(8, 8, 8, 8)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.sbit, Some(Sbit::Rgba(8, 8, 8, 8)));
    // The image itself must still decode bit-exactly.
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn sbit_grayscale_roundtrip() {
    let img = gray8_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            sbit: Some(Sbit::Grayscale(6)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.sbit, Some(Sbit::Grayscale(6)));
}

#[test]
fn phys_72_dpi_roundtrip() {
    let img = rgba_2x2();
    // 2835 px/m == 72.009 DPI — the classic "72 DPI" image. Spec says
    // one inch is exactly 0.0254 metres, so the back-conversion lands a
    // hair off 72.
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            phys: Some(Phys {
                pixels_per_unit_x: 2835,
                pixels_per_unit_y: 2835,
                unit: PhysUnit::Metre,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    let p = meta.phys.expect("pHYs");
    assert_eq!(p.pixels_per_unit_x, 2835);
    assert_eq!(p.pixels_per_unit_y, 2835);
    assert_eq!(p.unit, PhysUnit::Metre);
    let (dx, dy) = p.dpi().expect("metre unit → dpi");
    assert!((dx - 72.009).abs() < 0.01);
    assert!((dy - 72.009).abs() < 0.01);
}

#[test]
fn phys_aspect_only_roundtrip() {
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            phys: Some(Phys {
                pixels_per_unit_x: 4,
                pixels_per_unit_y: 3,
                unit: PhysUnit::Unknown,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    let p = meta.phys.expect("pHYs");
    assert_eq!(p.unit, PhysUnit::Unknown);
    // Unknown unit → aspect ratio only, no absolute DPI.
    assert!(p.dpi().is_none());
}

#[test]
fn time_roundtrip() {
    let img = rgba_2x2();
    let t = Time {
        year: 2026,
        month: 5,
        day: 20,
        hour: 14,
        minute: 30,
        second: 45,
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            time: Some(t),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.time, Some(t));
}

#[test]
fn all_three_chunks_roundtrip() {
    let img = rgba_2x2();
    let meta_in = PngMetadata {
        sbit: Some(Sbit::Rgba(8, 8, 8, 8)),
        phys: Some(Phys {
            pixels_per_unit_x: 11811,
            pixels_per_unit_y: 11811,
            unit: PhysUnit::Metre,
        }),
        time: Some(Time {
            year: 2026,
            month: 5,
            day: 20,
            hour: 0,
            minute: 0,
            second: 60, // RFC 2083 §4.2.8: 60 is the leap-second sentinel.
        }),
    };
    let opts = PngEncoderOptions {
        metadata: Some(meta_in.clone()),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta, meta_in);
}

#[test]
fn chunk_ordering_sbit_precedes_idat() {
    // sBIT must come before PLTE and IDAT (RFC 2083 §4.3). Verify it
    // appears in the byte stream strictly before the first IDAT.
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            sbit: Some(Sbit::Rgba(8, 8, 8, 8)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let sbit_pos = bytes
        .windows(4)
        .position(|w| w == b"sBIT")
        .expect("sBIT chunk type tag present");
    let idat_pos = bytes
        .windows(4)
        .position(|w| w == b"IDAT")
        .expect("IDAT chunk type tag present");
    assert!(sbit_pos < idat_pos, "sBIT must precede IDAT");
}

#[test]
fn chunk_ordering_phys_precedes_idat() {
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            phys: Some(Phys {
                pixels_per_unit_x: 100,
                pixels_per_unit_y: 100,
                unit: PhysUnit::Metre,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let phys_pos = bytes
        .windows(4)
        .position(|w| w == b"pHYs")
        .expect("pHYs chunk type tag present");
    let idat_pos = bytes
        .windows(4)
        .position(|w| w == b"IDAT")
        .expect("IDAT chunk type tag present");
    assert!(phys_pos < idat_pos, "pHYs must precede IDAT");
}

#[test]
fn parse_metadata_detects_duplicate_phys() {
    // Hand-craft a PNG with two pHYs chunks to verify the dedup check
    // fires. We start with a valid encoded PNG (no metadata) and splice
    // two pHYs chunks in.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");

    // Find IHDR end and inject two pHYs chunks immediately after.
    // IHDR is the first chunk after the 8-byte magic: length=13 means
    // total chunk size = 4(len) + 4(type) + 13(data) + 4(crc) = 25.
    let inject_pos = 8 + 25;
    let phys = Phys {
        pixels_per_unit_x: 100,
        pixels_per_unit_y: 100,
        unit: PhysUnit::Metre,
    };
    let mut tampered = Vec::with_capacity(bytes.len() + 2 * (4 + 4 + 9 + 4));
    tampered.extend_from_slice(&bytes[..inject_pos]);

    let phys_data = phys.to_bytes();
    for _ in 0..2 {
        // length (9)
        tampered.extend_from_slice(&9u32.to_be_bytes());
        // type
        tampered.extend_from_slice(b"pHYs");
        // data
        tampered.extend_from_slice(&phys_data);
        // CRC = crc32(type || data) — recompute exactly the way the
        // encoder does (we use the public chunk writer here to dodge
        // re-implementing it).
        let mut single = Vec::new();
        oxideav_png::chunk::write_chunk(&mut single, b"pHYs", &phys_data);
        // single is the full (len|type|data|crc); copy just the 4-byte
        // tail (the CRC) into the tampered stream.
        tampered.extend_from_slice(&single[single.len() - 4..]);
    }
    tampered.extend_from_slice(&bytes[inject_pos..]);

    let err = parse_metadata(&tampered).expect_err("two pHYs chunks must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("duplicate pHYs"),
        "expected duplicate-pHYs error, got {msg}"
    );
}

#[test]
fn parse_metadata_validates_sbit_against_ihdr() {
    // Forge an sBIT chunk for an RGBA image whose first byte is 0.
    // §4.2.6: "Each depth specified in sBIT must be greater than zero".
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;

    let bad_sbit = vec![0u8, 8, 8, 8];
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"sBIT", &bad_sbit);

    let mut tampered = Vec::with_capacity(bytes.len() + single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);

    let err = parse_metadata(&tampered).expect_err("sBIT(0,…) must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("significant-bit count"),
        "expected sBIT bounds error, got {msg}"
    );
}
