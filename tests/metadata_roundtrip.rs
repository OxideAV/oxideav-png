//! sBIT / pHYs / tIME round-trip through the standalone encode / decode
//! API. Verifies the encoder emits each chunk at a spec-compliant
//! position and the decoder reads them back byte-for-byte.

use oxideav_png::{
    decode_png, encode_png_image, encode_png_image_with_options, parse_metadata, Bkgd, Exif, Hist,
    Phys, PhysUnit, PngEncoderOptions, PngImage, PngMetadata, PngPixelFormat, RenderingIntent,
    Sbit, Srgb, Time,
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

/// 4×1 Pal8 image with a 4-entry palette (red, green, blue, white). Used
/// by the bKGD-palette and hIST tests; both need a PLTE to exist on the
/// encoded stream.
fn pal8_4x1() -> PngImage {
    let palette: Vec<u8> = vec![
        255, 0, 0, // entry 0 = red
        0, 255, 0, // entry 1 = green
        0, 0, 255, // entry 2 = blue
        255, 255, 255, // entry 3 = white
    ];
    PngImage {
        width: 4,
        height: 1,
        pixel_format: PngPixelFormat::Pal8,
        stride: 4,
        data: vec![0, 1, 2, 3],
        palette,
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
        // bKGD on colour type 6 ⇒ RGB triple; hIST not meaningful here
        // (no PLTE on RGBA).
        bkgd: Some(Bkgd::Rgb(255, 255, 255)),
        hist: None,
        exif: None,
        srgb: Some(Srgb {
            rendering_intent: RenderingIntent::Saturation,
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
fn bkgd_rgb_8bit_roundtrip() {
    // Colour type 6 (Rgba) sample image; bKGD payload is 6 bytes BE.
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            bkgd: Some(Bkgd::Rgb(0x80, 0x40, 0x10)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.bkgd, Some(Bkgd::Rgb(0x80, 0x40, 0x10)));
    // Image must still decode bit-exactly.
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn bkgd_grayscale_roundtrip() {
    // Colour type 0 (Gray8): bKGD is one 2-byte BE u16, MSB = 0 for 8-bit.
    let img = gray8_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            bkgd: Some(Bkgd::Grayscale(123)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.bkgd, Some(Bkgd::Grayscale(123)));
}

#[test]
fn bkgd_palette_index_roundtrip() {
    // Colour type 3: bKGD payload is a single u8 palette index.
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            bkgd: Some(Bkgd::Palette(2)), // blue, the 3rd entry
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.bkgd, Some(Bkgd::Palette(2)));
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn hist_matches_palette_entry_count() {
    // 4-entry palette ⇒ hIST carries exactly 4 frequencies.
    let img = pal8_4x1();
    let h = Hist {
        frequencies: vec![1, 1, 1, 1],
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            hist: Some(h.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.hist, Some(h));
}

#[test]
fn chunk_ordering_bkgd_and_hist_follow_plte_precede_idat() {
    // PNG3 §5.6 Table 1: bKGD / hIST go after PLTE and before IDAT. We
    // verify the byte positions explicitly.
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            bkgd: Some(Bkgd::Palette(0)),
            hist: Some(Hist {
                frequencies: vec![10, 5, 2, 1],
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| bytes.windows(4).position(|w| w == tag);
    let plte_pos = pos(b"PLTE").expect("PLTE");
    let bkgd_pos = pos(b"bKGD").expect("bKGD");
    let hist_pos = pos(b"hIST").expect("hIST");
    let idat_pos = pos(b"IDAT").expect("IDAT");
    assert!(plte_pos < bkgd_pos, "PLTE before bKGD");
    assert!(plte_pos < hist_pos, "PLTE before hIST");
    assert!(bkgd_pos < idat_pos, "bKGD before IDAT");
    assert!(hist_pos < idat_pos, "hIST before IDAT");
}

#[test]
fn parse_metadata_rejects_hist_without_plte() {
    // Hand-craft an hIST on top of an RGBA stream (no PLTE). PNG3
    // §11.3.4.2: "A histogram chunk can appear only when a PLTE chunk
    // appears."
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    // Inject hIST after IHDR (IHDR ends at offset 8+25=33).
    let inject_pos = 8 + 25;
    let hist_data = [0x00, 0x05]; // any 2 bytes
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"hIST", &hist_data);
    let mut tampered = Vec::with_capacity(bytes.len() + single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("hIST without PLTE must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("requires a PLTE"),
        "expected hIST-needs-PLTE error, got {msg}"
    );
}

#[test]
fn parse_metadata_rejects_bkgd_palette_index_out_of_range() {
    // Pal8 image has 4 entries; bKGD index 7 is out of range.
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            bkgd: Some(Bkgd::Palette(7)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let err = parse_metadata(&bytes).expect_err("oob palette index must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("out of range"),
        "expected oob-index error, got {msg}"
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

/// A small but realistic Exif/TIFF blob: little-endian header, one IFD
/// offset, zero entries, no next-IFD. We don't interpret it — the codec
/// treats `eXIf` as opaque — but it exercises a non-trivial body.
fn sample_exif_le() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00]); // "II", 42 LE
    v.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset = 8
    v.extend_from_slice(&0u16.to_le_bytes()); // 0 directory entries
    v.extend_from_slice(&0u32.to_le_bytes()); // next-IFD offset = 0 (none)
    v
}

#[test]
fn exif_roundtrip() {
    let img = rgba_2x2();
    let exif = Exif {
        data: sample_exif_le(),
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            exif: Some(exif.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.exif, Some(exif));
    // Pixels must still decode bit-exactly.
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn exif_big_endian_roundtrip() {
    let img = gray8_2x2();
    let mut data = vec![0x4D, 0x4D, 0x00, 0x2A]; // "MM", 42 BE
    data.extend_from_slice(&8u32.to_be_bytes());
    data.extend_from_slice(&0u16.to_be_bytes());
    data.extend_from_slice(&0u32.to_be_bytes());
    let exif = Exif { data };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            exif: Some(exif.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.exif, Some(exif));
}

#[test]
fn chunk_ordering_exif_precedes_idat() {
    // PNG3 §5.6 Table 1: eXIf is "Before IDAT". Verify the byte position.
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            exif: Some(Exif {
                data: sample_exif_le(),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let exif_pos = bytes
        .windows(4)
        .position(|w| w == b"eXIf")
        .expect("eXIf chunk type tag present");
    let idat_pos = bytes
        .windows(4)
        .position(|w| w == b"IDAT")
        .expect("IDAT chunk type tag present");
    assert!(exif_pos < idat_pos, "eXIf must precede IDAT");
}

#[test]
fn parse_metadata_detects_duplicate_exif() {
    // Hand-craft a PNG with two eXIf chunks; the dedup check must fire.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25; // after IHDR
    let exif_data = sample_exif_le();
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"eXIf", &exif_data);
    let mut tampered = Vec::with_capacity(bytes.len() + 2 * single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("two eXIf chunks must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("duplicate eXIf"),
        "expected duplicate-eXIf error, got {msg}"
    );
}

#[test]
fn parse_metadata_rejects_exif_with_bad_tiff_header() {
    // PNG3 §11.3.4.5.2: only "II"/42 and "MM"/42 byte-order headers are
    // valid. A blob with a bogus header must be rejected on decode.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let bad = [0x00, 0x01, 0x02, 0x03, 0x04]; // not a TIFF header
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"eXIf", &bad);
    let mut tampered = Vec::with_capacity(bytes.len() + single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("bad eXIf header must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("TIFF byte-order header"),
        "expected bad-TIFF-header error, got {msg}"
    );
}

#[test]
fn srgb_roundtrip() {
    // PNG3 §11.3.2.5: one-byte rendering intent. Round-trip the
    // "Relative colorimetric" intent through encode/decode.
    let img = rgba_2x2();
    let srgb = Srgb {
        rendering_intent: RenderingIntent::RelativeColorimetric,
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            srgb: Some(srgb),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.srgb, Some(srgb));
    // Pixels must still decode bit-exactly.
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn chunk_ordering_srgb_precedes_plte_and_idat() {
    // PNG3 §5.6 Table 1: sRGB is "Before PLTE and IDAT". Verify the byte
    // positions against a palette image so PLTE is actually present.
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            srgb: Some(Srgb {
                rendering_intent: RenderingIntent::Perceptual,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| bytes.windows(4).position(|w| w == tag);
    let srgb_pos = pos(b"sRGB").expect("sRGB");
    let plte_pos = pos(b"PLTE").expect("PLTE");
    let idat_pos = pos(b"IDAT").expect("IDAT");
    assert!(srgb_pos < plte_pos, "sRGB must precede PLTE");
    assert!(srgb_pos < idat_pos, "sRGB must precede IDAT");
}

#[test]
fn parse_metadata_detects_duplicate_srgb() {
    // Hand-craft a PNG with two sRGB chunks; the dedup check must fire.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25; // after IHDR
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"sRGB", &[0u8]);
    let mut tampered = Vec::with_capacity(bytes.len() + 2 * single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("two sRGB chunks must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("duplicate sRGB"),
        "expected duplicate-sRGB error, got {msg}"
    );
}

#[test]
fn parse_metadata_rejects_reserved_srgb_intent() {
    // PNG3 §11.3.2.5 Table 16 defines only intents 0..=3; a chunk
    // carrying 4 must be rejected on decode.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"sRGB", &[4u8]);
    let mut tampered = Vec::with_capacity(bytes.len() + single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("reserved sRGB intent must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("rendering intent"),
        "expected reserved-intent error, got {msg}"
    );
}
