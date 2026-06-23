//! sBIT / pHYs / tIME round-trip through the standalone encode / decode
//! API. Verifies the encoder emits each chunk at a spec-compliant
//! position and the decoder reads them back byte-for-byte.

use oxideav_png::{
    decode_png, encode_png_image, encode_png_image_with_options, parse_metadata, Bkgd, Chrm, Cicp,
    Clli, Exif, Gama, Hist, Iccp, Itxt, Mdcv, Phys, PhysUnit, PngEncoderOptions, PngImage,
    PngMetadata, PngPixelFormat, RenderingIntent, Sbit, Splt, SpltEntry, Srgb, Text, Time, Trns,
    Ztxt,
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
        // tRNS is prohibited on ct=6 (full alpha channel present per
        // RFC 2083 §4.2.9 final paragraph). The "all chunks" fixture
        // omits it for that reason; a dedicated ct=0/ct=2 round-trip
        // test below covers the on-wire path.
        trns: None,
        exif: None,
        srgb: Some(Srgb {
            rendering_intent: RenderingIntent::Saturation,
        }),
        cicp: Some(Cicp {
            // §11.3.2.6 Example 4 — Display P3 full-range.
            color_primaries: 12,
            transfer_function: 13,
            matrix_coefficients: 0,
            video_full_range_flag: 1,
        }),
        iccp: None,
        gama: Some(Gama {
            // RFC 2083 §4.2.3: γ 1/2.2 ≈ 0.45455 ⇒ 45455.
            gamma_times_100000: 45_455,
        }),
        chrm: Some(Chrm {
            // sRGB / Rec.709 primaries + D65 white point (× 100000).
            white_point_x: 31_270,
            white_point_y: 32_900,
            red_x: 64_000,
            red_y: 33_000,
            green_x: 30_000,
            green_y: 60_000,
            blue_x: 15_000,
            blue_y: 6_000,
        }),
        mdcv: None,
        clli: None,
        splt: Vec::new(),
        texts: Vec::new(),
        ztxts: Vec::new(),
        itxts: Vec::new(),
        unknowns: Vec::new(),
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

#[test]
fn cicp_roundtrip() {
    // PNG3 §11.3.2.6 Example 4 — Display P3 (full-range): 0C 0D 00 01.
    let img = rgba_2x2();
    let cicp = Cicp {
        color_primaries: 12,
        transfer_function: 13,
        matrix_coefficients: 0,
        video_full_range_flag: 1,
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            cicp: Some(cicp),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.cicp, Some(cicp));
    // The pixel plane must still decode bit-exactly with the colour
    // metadata attached.
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn chunk_ordering_cicp_precedes_plte_and_idat() {
    // PNG3 §5.6 Table 1: cICP is "Before PLTE and IDAT". Use a palette
    // image so PLTE is actually present in the output stream.
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            cicp: Some(Cicp {
                color_primaries: 1,
                transfer_function: 1,
                matrix_coefficients: 0,
                video_full_range_flag: 0,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| bytes.windows(4).position(|w| w == tag);
    let cicp_pos = pos(b"cICP").expect("cICP");
    let plte_pos = pos(b"PLTE").expect("PLTE");
    let idat_pos = pos(b"IDAT").expect("IDAT");
    assert!(cicp_pos < plte_pos, "cICP must precede PLTE");
    assert!(cicp_pos < idat_pos, "cICP must precede IDAT");
}

#[test]
fn chunk_ordering_cicp_precedes_other_colour_chunks() {
    // PNG3 §4.3 Table 1 makes cICP the highest-precedence colour chunk.
    // When both cICP and sRGB are emitted, cICP must come first so that
    // a viewer that walks the file in order picks the higher-precedence
    // signal up first.
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            cicp: Some(Cicp {
                color_primaries: 9,
                transfer_function: 16,
                matrix_coefficients: 0,
                video_full_range_flag: 1,
            }),
            srgb: Some(Srgb {
                rendering_intent: RenderingIntent::Perceptual,
            }),
            sbit: Some(Sbit::Rgba(8, 8, 8, 8)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| bytes.windows(4).position(|w| w == tag);
    let cicp_pos = pos(b"cICP").expect("cICP");
    let sbit_pos = pos(b"sBIT").expect("sBIT");
    let srgb_pos = pos(b"sRGB").expect("sRGB");
    assert!(cicp_pos < sbit_pos, "cICP must come before sBIT");
    assert!(cicp_pos < srgb_pos, "cICP must come before sRGB");
}

#[test]
fn parse_metadata_detects_duplicate_cicp() {
    // §5.6 Table 1: cICP is "Multiple allowed: No". Two consecutive cICP
    // chunks must be rejected by the dedup check.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25; // immediately after IHDR.
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"cICP", &[1u8, 13, 0, 1]);
    let mut tampered = Vec::with_capacity(bytes.len() + 2 * single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("two cICP chunks must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("duplicate cICP"),
        "expected duplicate-cICP error, got {msg}"
    );
}

#[test]
fn parse_metadata_rejects_nonzero_cicp_matrix_coefficients() {
    // §11.3.2.6 pins matrix_coefficients at 0 for PNG; anything else
    // must be rejected on decode.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut single = Vec::new();
    // color_primaries=1, transfer=13, matrix=1 (BT.709) — illegal for PNG.
    oxideav_png::chunk::write_chunk(&mut single, b"cICP", &[1u8, 13, 1, 1]);
    let mut tampered = Vec::with_capacity(bytes.len() + single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("non-zero matrix_coefficients must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("matrix_coefficients"),
        "expected matrix_coefficients error, got {msg}"
    );
}

// ---- sPLT (W3C PNG3 §11.3.4.4) ----------------------------------------

#[test]
fn splt_single_8bit_roundtrip() {
    // One 8-bit suggested palette survives encode → decode intact.
    let img = rgba_2x2();
    let splt = Splt {
        name: "web safe".to_string(),
        sample_depth: 8,
        entries: vec![
            SpltEntry {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255,
                frequency: 50000,
            },
            SpltEntry {
                red: 0,
                green: 255,
                blue: 0,
                alpha: 128,
                frequency: 10000,
            },
        ],
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            splt: vec![splt.clone()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    // Image data still decodes.
    let decoded = decode_png(&bytes).expect("decode");
    assert_eq!(decoded.width, 2);
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.splt.len(), 1);
    assert_eq!(meta.splt[0], splt);
}

#[test]
fn splt_16bit_roundtrip() {
    let img = rgba_2x2();
    let splt = Splt {
        name: "deep".to_string(),
        sample_depth: 16,
        entries: vec![SpltEntry {
            red: 0xFFFE,
            green: 0x8001,
            blue: 0x0123,
            alpha: 0xFFFF,
            frequency: 0xCAFE,
        }],
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            splt: vec![splt.clone()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.splt, vec![splt]);
}

#[test]
fn splt_multiple_palettes_roundtrip_in_order() {
    // PNG permits multiple sPLT chunks (distinct names); the Vec order is
    // preserved across the round-trip.
    let img = rgba_2x2();
    let a = Splt {
        name: "first".to_string(),
        sample_depth: 8,
        entries: vec![SpltEntry {
            red: 1,
            green: 2,
            blue: 3,
            alpha: 4,
            frequency: 9,
        }],
    };
    let b = Splt {
        name: "second".to_string(),
        sample_depth: 16,
        entries: vec![SpltEntry {
            red: 0x1111,
            green: 0x2222,
            blue: 0x3333,
            alpha: 0x4444,
            frequency: 0x5555,
        }],
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            splt: vec![a.clone(), b.clone()],
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.splt, vec![a, b]);
}

#[test]
fn chunk_ordering_splt_precedes_idat() {
    // PNG3 §5.6 Table 7: sPLT is "Before IDAT".
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            splt: vec![Splt {
                name: "p".to_string(),
                sample_depth: 8,
                entries: vec![],
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let splt_pos = bytes
        .windows(4)
        .position(|w| w == b"sPLT")
        .expect("sPLT chunk type tag present");
    let idat_pos = bytes
        .windows(4)
        .position(|w| w == b"IDAT")
        .expect("IDAT chunk type tag present");
    assert!(splt_pos < idat_pos, "sPLT must precede IDAT");
}

#[test]
fn parse_metadata_rejects_duplicate_splt_palette_name() {
    // Two sPLT chunks with the SAME palette name are illegal (§11.3.4.4
    // final paragraph: "each shall have a different palette name").
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25; // after IHDR.
    let payload = Splt {
        name: "same".to_string(),
        sample_depth: 8,
        entries: vec![],
    }
    .to_bytes()
    .expect("splt payload");
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"sPLT", &payload);
    let mut tampered = Vec::with_capacity(bytes.len() + 2 * single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("duplicate sPLT name must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("duplicate palette name"),
        "expected duplicate-palette-name error, got {msg}"
    );
}

#[test]
fn parse_metadata_allows_two_splt_with_distinct_names() {
    // Distinct names ⇒ both are accepted and surfaced.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mk = |name: &str| {
        let payload = Splt {
            name: name.to_string(),
            sample_depth: 8,
            entries: vec![],
        }
        .to_bytes()
        .unwrap();
        let mut c = Vec::new();
        oxideav_png::chunk::write_chunk(&mut c, b"sPLT", &payload);
        c
    };
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&mk("alpha"));
    tampered.extend_from_slice(&mk("beta"));
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let meta = parse_metadata(&tampered).expect("two distinct sPLT names accepted");
    assert_eq!(meta.splt.len(), 2);
    assert_eq!(meta.splt[0].name, "alpha");
    assert_eq!(meta.splt[1].name, "beta");
}

#[test]
fn text_single_roundtrip_through_encoder() {
    // tEXt encode → decode end-to-end via parse_metadata. The
    // standalone encoder must emit the chunk before IDAT, and
    // parse_metadata must surface it in `texts`.
    let img = rgba_2x2();
    let mut opts = PngEncoderOptions::default();
    let mut meta = PngMetadata::default();
    meta.texts.push(Text {
        keyword: "Title".to_string(),
        text: "Sunset over the cape".to_string(),
    });
    opts.metadata = Some(meta);
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.texts.len(), 1);
    assert_eq!(parsed.texts[0].keyword, "Title");
    assert_eq!(parsed.texts[0].text, "Sunset over the cape");
}

#[test]
fn text_multiple_with_same_keyword_roundtrip() {
    // RFC 2083 §4.2.7: "more than one with the same keyword is
    // permissible." Drive two `Comment` entries through encode +
    // decode and confirm both survive in order — `tEXt` is the lone
    // metadata chunk where keyword duplicates are explicitly OK.
    let img = rgba_2x2();
    let mut opts = PngEncoderOptions::default();
    let mut meta = PngMetadata::default();
    meta.texts.push(Text {
        keyword: "Comment".to_string(),
        text: "first comment".to_string(),
    });
    meta.texts.push(Text {
        keyword: "Comment".to_string(),
        text: "second comment".to_string(),
    });
    meta.texts.push(Text {
        keyword: "Author".to_string(),
        text: "anon".to_string(),
    });
    opts.metadata = Some(meta);
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.texts.len(), 3);
    assert_eq!(parsed.texts[0].keyword, "Comment");
    assert_eq!(parsed.texts[0].text, "first comment");
    assert_eq!(parsed.texts[1].keyword, "Comment");
    assert_eq!(parsed.texts[1].text, "second comment");
    assert_eq!(parsed.texts[2].keyword, "Author");
    assert_eq!(parsed.texts[2].text, "anon");
}

#[test]
fn text_with_latin1_high_byte_roundtrips() {
    // Cover the 0xA1..=0xFF Latin-1 band on both keyword and text.
    // 'é' (U+00E9) sits at byte 0xE9, the keyword still uses only
    // bytes valid per the printable-Latin-1 predicate.
    let img = rgba_2x2();
    let mut opts = PngEncoderOptions::default();
    let mut meta = PngMetadata::default();
    meta.texts.push(Text {
        keyword: "Author".to_string(),
        text: "Renée Lévesque".to_string(),
    });
    opts.metadata = Some(meta);
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.texts.len(), 1);
    assert_eq!(parsed.texts[0].text, "Renée Lévesque");
}

#[test]
fn text_chunk_precedes_idat_in_output() {
    // §5.6: ancillary chunks like `tEXt` must precede `IDAT`. Walk
    // the chunk stream and confirm the `tEXt` we emitted is found
    // before the first `IDAT`. (Same shape as
    // `chunk_ordering_sbit_precedes_idat`.)
    let img = rgba_2x2();
    let mut opts = PngEncoderOptions::default();
    let mut meta = PngMetadata::default();
    meta.texts.push(Text {
        keyword: "Software".to_string(),
        text: "oxideav-png".to_string(),
    });
    opts.metadata = Some(meta);
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");

    // PNG signature is 8 bytes; chunks follow as len|type|data|crc.
    let mut cur = 8usize;
    let mut text_pos = None;
    let mut idat_pos = None;
    while cur + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[cur], bytes[cur + 1], bytes[cur + 2], bytes[cur + 3]])
            as usize;
        let ty = &bytes[cur + 4..cur + 8];
        if ty == b"tEXt" {
            text_pos = Some(cur);
        }
        if ty == b"IDAT" && idat_pos.is_none() {
            idat_pos = Some(cur);
        }
        cur += 12 + len; // 4 len + 4 type + len data + 4 crc
    }
    let tp = text_pos.expect("tEXt chunk must be present in the output");
    let ip = idat_pos.expect("IDAT chunk must be present in the output");
    assert!(tp < ip, "tEXt at {tp} must precede IDAT at {ip}");
}

#[test]
fn text_empty_string_roundtrips_through_encoder() {
    // §4.2.7: text may be zero bytes. Drive a keyword-only `tEXt`
    // through encode + decode.
    let img = rgba_2x2();
    let mut opts = PngEncoderOptions::default();
    let mut meta = PngMetadata::default();
    meta.texts.push(Text {
        keyword: "Comment".to_string(),
        text: String::new(),
    });
    opts.metadata = Some(meta);
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.texts.len(), 1);
    assert_eq!(parsed.texts[0].keyword, "Comment");
    assert!(parsed.texts[0].text.is_empty());
}

#[test]
fn parse_metadata_rejects_text_with_invalid_keyword() {
    // Inject a raw `tEXt` whose keyword carries a leading space; the
    // parser must reject the chunk per §4.2.7's keyword rules even
    // though the encoder would have rejected the value first.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    // Walk to just after IHDR (8-byte signature + 25-byte IHDR chunk).
    let inject_pos = 8 + 25;
    let mut chunk = Vec::new();
    // Build a raw payload: " Title\0body" (leading space → invalid).
    let mut payload = Vec::new();
    payload.extend_from_slice(b" Title");
    payload.push(0);
    payload.extend_from_slice(b"body");
    oxideav_png::chunk::write_chunk(&mut chunk, b"tEXt", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn parse_metadata_rejects_text_with_nul_in_text_string() {
    // Inject a raw `tEXt` whose text portion contains a NUL — the
    // chunk parser walks to the first NUL for the keyword/text split,
    // then must reject any further NUL inside the remaining text bytes
    // per §4.2.7's "Neither the keyword nor the text string can
    // contain a null character."
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut chunk = Vec::new();
    let mut payload = Vec::new();
    payload.extend_from_slice(b"Keyword");
    payload.push(0);
    payload.extend_from_slice(b"first\0second");
    oxideav_png::chunk::write_chunk(&mut chunk, b"tEXt", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn parse_metadata_rejects_text_without_separator() {
    // A `tEXt` payload with no NUL at all cannot be split into
    // keyword + text; parse must fail.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut chunk = Vec::new();
    let payload = b"NoSeparatorAnywhere";
    oxideav_png::chunk::write_chunk(&mut chunk, b"tEXt", payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

// ---- gAMA (RFC 2083 §4.2.3 / W3C PNG3 §11.3.2.2) ----------------------

#[test]
fn gama_roundtrip() {
    // RFC 2083 §4.2.3: a gamma of 0.45 is stored as 45000. Round-trip it
    // through encode/decode and confirm the pixels still decode exactly.
    let img = rgba_2x2();
    let gama = Gama {
        gamma_times_100000: 45_000,
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            gama: Some(gama),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.gama, Some(gama));
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn chunk_ordering_gama_precedes_plte_and_idat() {
    // PNG3 §5.6 Table 1: gAMA is "Before PLTE and IDAT". Use a palette
    // image so PLTE is present in the stream.
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            gama: Some(Gama {
                gamma_times_100000: 100_000,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| bytes.windows(4).position(|w| w == tag);
    let gama_pos = pos(b"gAMA").expect("gAMA");
    let plte_pos = pos(b"PLTE").expect("PLTE");
    let idat_pos = pos(b"IDAT").expect("IDAT");
    assert!(gama_pos < plte_pos, "gAMA must precede PLTE");
    assert!(gama_pos < idat_pos, "gAMA must precede IDAT");
}

#[test]
fn parse_metadata_detects_duplicate_gama() {
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25; // after IHDR
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"gAMA", &45_000u32.to_be_bytes());
    let mut tampered = Vec::with_capacity(bytes.len() + 2 * single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("two gAMA chunks must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("duplicate gAMA"),
        "expected duplicate-gAMA error, got {msg}"
    );
}

#[test]
fn parse_metadata_rejects_gama_wrong_length() {
    // gAMA is exactly four bytes; a 3-byte payload must be rejected.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"gAMA", &[0u8, 0, 0]);
    let mut tampered = Vec::with_capacity(bytes.len() + single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("3-byte gAMA must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("gAMA"),
        "expected gAMA length error, got {msg}"
    );
}

// ---- cHRM (RFC 2083 §4.2.2 / W3C PNG3 §11.3.2.1) ----------------------

fn srgb_chrm() -> Chrm {
    // Canonical sRGB / Rec.709 primaries + D65 white point (× 100000),
    // per RFC 2083 §4.2.2's example storage convention.
    Chrm {
        white_point_x: 31_270,
        white_point_y: 32_900,
        red_x: 64_000,
        red_y: 33_000,
        green_x: 30_000,
        green_y: 60_000,
        blue_x: 15_000,
        blue_y: 6_000,
    }
}

#[test]
fn chrm_roundtrip() {
    let img = rgba_2x2();
    let chrm = srgb_chrm();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            chrm: Some(chrm),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.chrm, Some(chrm));
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn chunk_ordering_chrm_precedes_plte_and_idat() {
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            chrm: Some(srgb_chrm()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| bytes.windows(4).position(|w| w == tag);
    let chrm_pos = pos(b"cHRM").expect("cHRM");
    let plte_pos = pos(b"PLTE").expect("PLTE");
    let idat_pos = pos(b"IDAT").expect("IDAT");
    assert!(chrm_pos < plte_pos, "cHRM must precede PLTE");
    assert!(chrm_pos < idat_pos, "cHRM must precede IDAT");
}

#[test]
fn chunk_ordering_colour_chunks_follow_priority_table() {
    // PNG3 §4.3 "Color Chunk Priority": cICP (1) > sRGB (3) > cHRM/gAMA
    // (4). Emit all four and confirm the byte positions honour the
    // precedence ordering, with gAMA ahead of cHRM by our deterministic
    // convention.
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            cicp: Some(Cicp {
                color_primaries: 9,
                transfer_function: 16,
                matrix_coefficients: 0,
                video_full_range_flag: 1,
            }),
            srgb: Some(Srgb {
                rendering_intent: RenderingIntent::Perceptual,
            }),
            gama: Some(Gama {
                gamma_times_100000: 45_455,
            }),
            chrm: Some(srgb_chrm()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| {
        bytes
            .windows(4)
            .position(|w| w == tag)
            .expect("chunk present")
    };
    let cicp = pos(b"cICP");
    let srgb = pos(b"sRGB");
    let gama = pos(b"gAMA");
    let chrm = pos(b"cHRM");
    let idat = pos(b"IDAT");
    assert!(cicp < srgb, "cICP (priority 1) before sRGB (3)");
    assert!(srgb < gama, "sRGB (priority 3) before gAMA (4)");
    assert!(gama < chrm, "gAMA before cHRM (deterministic convention)");
    assert!(chrm < idat, "all colour chunks precede IDAT");
}

#[test]
fn parse_metadata_detects_duplicate_chrm() {
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"cHRM", &srgb_chrm().to_bytes());
    let mut tampered = Vec::with_capacity(bytes.len() + 2 * single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("two cHRM chunks must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("duplicate cHRM"),
        "expected duplicate-cHRM error, got {msg}"
    );
}

#[test]
fn parse_metadata_rejects_chrm_wrong_length() {
    // cHRM is exactly 32 bytes; a 31-byte payload must be rejected.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"cHRM", &[0u8; 31]);
    let mut tampered = Vec::with_capacity(bytes.len() + single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("31-byte cHRM must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("cHRM"),
        "expected cHRM length error, got {msg}"
    );
}

#[test]
fn gama_chrm_combined_with_other_metadata_roundtrip() {
    // Full colour-management stack plus structural metadata in one image
    // exercises the encoder's chunk-bucketing across both before-PLTE and
    // before-IDAT positions.
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            gama: Some(Gama {
                gamma_times_100000: 45_455,
            }),
            chrm: Some(srgb_chrm()),
            sbit: Some(Sbit::Rgb(8, 8, 8)),
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
    assert_eq!(meta.gama.unwrap().gamma_times_100000, 45_455);
    assert_eq!(meta.chrm, Some(srgb_chrm()));
    assert_eq!(meta.sbit, Some(Sbit::Rgb(8, 8, 8)));
    assert!(meta.phys.is_some());
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

// ---- zTXt (RFC 2083 §4.2.10 / W3C PNG3 §11.3.3.3) ---------------------

#[test]
fn ztxt_single_roundtrip_through_encoder() {
    // RFC 2083 §4.2.10: zTXt is tEXt with a zlib-compressed body. Drive
    // encode → parse_metadata and confirm the decompressed text matches.
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.ztxts.push(Ztxt {
        keyword: "Description".to_string(),
        text: "A compressed annotation that survives the codec.".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.ztxts.len(), 1);
    assert_eq!(parsed.ztxts[0].keyword, "Description");
    assert_eq!(
        parsed.ztxts[0].text,
        "A compressed annotation that survives the codec."
    );
    // Pixel data must still decode (zTXt is ancillary; no impact on
    // IDAT).
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn ztxt_multiple_with_same_keyword_roundtrip() {
    // RFC 2083 §4.2.10 ¶6: "Any number of zTXt and tEXt chunks can
    // appear in the same file" — and the no-uniqueness-check rule of
    // tEXt extends to zTXt. Drive two `Description` entries through
    // encode + decode and confirm both survive in order.
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.ztxts.push(Ztxt {
        keyword: "Description".to_string(),
        text: "first description".to_string(),
    });
    meta.ztxts.push(Ztxt {
        keyword: "Description".to_string(),
        text: "second description".to_string(),
    });
    meta.ztxts.push(Ztxt {
        keyword: "Copyright".to_string(),
        text: "© anon 2026".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.ztxts.len(), 3);
    assert_eq!(parsed.ztxts[0].keyword, "Description");
    assert_eq!(parsed.ztxts[0].text, "first description");
    assert_eq!(parsed.ztxts[1].keyword, "Description");
    assert_eq!(parsed.ztxts[1].text, "second description");
    assert_eq!(parsed.ztxts[2].keyword, "Copyright");
    assert_eq!(parsed.ztxts[2].text, "© anon 2026");
}

#[test]
fn ztxt_chunk_precedes_idat_in_output() {
    // §5.6 Table 1: `zTXt` shares the "Before IDAT, no ordering
    // constraint" bucket with `tEXt`. Walk the chunk stream and
    // confirm the emitted `zTXt` is found before the first `IDAT`.
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.ztxts.push(Ztxt {
        keyword: "Software".to_string(),
        text: "oxideav-png".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");

    let mut cur = 8usize;
    let mut ztxt_pos = None;
    let mut idat_pos = None;
    while cur + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[cur], bytes[cur + 1], bytes[cur + 2], bytes[cur + 3]])
            as usize;
        let ty = &bytes[cur + 4..cur + 8];
        if ty == b"zTXt" {
            ztxt_pos = Some(cur);
        }
        if ty == b"IDAT" && idat_pos.is_none() {
            idat_pos = Some(cur);
        }
        cur += 12 + len;
    }
    let zp = ztxt_pos.expect("zTXt chunk must be present in the output");
    let ip = idat_pos.expect("IDAT chunk must be present in the output");
    assert!(zp < ip, "zTXt at {zp} must precede IDAT at {ip}");
}

#[test]
fn ztxt_emitted_after_text_in_chunk_stream() {
    // Encoder convention: emit plain tEXt before compressed zTXt so a
    // streaming reader sees the cheap-to-display annotations first.
    // The spec leaves the ordering free (both share "Ordering: None"
    // in §5.6 Table 1); this asserts the project's documented choice.
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.texts.push(Text {
        keyword: "Title".to_string(),
        text: "Plain".to_string(),
    });
    meta.ztxts.push(Ztxt {
        keyword: "Description".to_string(),
        text: "Compressed".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| bytes.windows(4).position(|w| w == tag);
    let text_pos = pos(b"tEXt").expect("tEXt");
    let ztxt_pos = pos(b"zTXt").expect("zTXt");
    assert!(
        text_pos < ztxt_pos,
        "tEXt at {text_pos} should precede zTXt at {ztxt_pos}"
    );
}

#[test]
fn ztxt_compresses_large_repetitive_text() {
    // The whole point of zTXt: a 4 KB run of one character must occupy
    // far fewer bytes on the wire than the literal tEXt encoding would.
    let img = rgba_2x2();
    let payload = "A".repeat(4096);
    let mut meta = PngMetadata::default();
    meta.ztxts.push(Ztxt {
        keyword: "Bulk".to_string(),
        text: payload.clone(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let zpos = bytes
        .windows(4)
        .position(|w| w == b"zTXt")
        .expect("zTXt chunk present");
    let chunk_len = u32::from_be_bytes([
        bytes[zpos - 4],
        bytes[zpos - 3],
        bytes[zpos - 2],
        bytes[zpos - 1],
    ]) as usize;
    // 4096-byte plaintext should compress to well under 200 bytes
    // (deflate level 6 produces a few dozen bytes for runs).
    assert!(
        chunk_len < 200,
        "zTXt body for 4096 identical chars should be <200 B (got {chunk_len})"
    );
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.ztxts.len(), 1);
    assert_eq!(parsed.ztxts[0].text, payload);
}

#[test]
fn parse_metadata_rejects_ztxt_with_unknown_compression_method() {
    // §4.2.10: "The only value presently defined for [compression
    // method] is 0". Inject a hand-built zTXt with method byte = 1 and
    // confirm parse_metadata rejects it.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25; // after PNG signature + IHDR.
    let mut payload = Vec::new();
    payload.extend_from_slice(b"Description");
    payload.push(0); // NUL separator.
    payload.push(1); // bogus compression method.
    payload.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"zTXt", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn parse_metadata_rejects_ztxt_with_corrupted_zlib_body() {
    // Method byte = 0 (deflate) but the compressed body is garbage;
    // inflate must fail and the parse must surface InvalidData rather
    // than panicking.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"Description");
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"zTXt", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn parse_metadata_rejects_ztxt_with_invalid_keyword() {
    // Inject a raw `zTXt` whose keyword carries a leading space; the
    // parser must reject the chunk per §4.2.7's keyword rules (shared
    // with §4.2.10) even though the encoder would have rejected it
    // first.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    // Build a payload: " Title\0<method><zlib>" (leading space → invalid)
    let mut payload = Vec::new();
    payload.extend_from_slice(b" Title");
    payload.push(0);
    payload.push(0);
    // Valid empty-payload zlib stream (deflate of zero bytes).
    payload.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"zTXt", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn ztxt_coexists_with_text_in_one_file() {
    // Drive both tEXt and zTXt through the codec in the same PNG and
    // confirm both vectors survive independently with file order
    // preserved within each vector.
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.texts.push(Text {
        keyword: "Title".to_string(),
        text: "Plain title".to_string(),
    });
    meta.texts.push(Text {
        keyword: "Author".to_string(),
        text: "anon".to_string(),
    });
    meta.ztxts.push(Ztxt {
        keyword: "Description".to_string(),
        text: "Long compressed description".to_string(),
    });
    meta.ztxts.push(Ztxt {
        keyword: "Comment".to_string(),
        text: "Another compressed annotation".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.texts.len(), 2);
    assert_eq!(parsed.texts[0].keyword, "Title");
    assert_eq!(parsed.texts[1].keyword, "Author");
    assert_eq!(parsed.ztxts.len(), 2);
    assert_eq!(parsed.ztxts[0].keyword, "Description");
    assert_eq!(parsed.ztxts[1].keyword, "Comment");
}

// ---- iCCP (W3C PNG3 §11.3.2.3) ----------------------------------------

#[test]
fn iccp_roundtrip_through_encoder() {
    // §11.3.2.3: iCCP is an embedded ICC profile blob, zlib-compressed
    // on the wire. Drive a populated iccp through the codec and confirm
    // both name and inflated profile bytes survive.
    let img = rgba_2x2();
    // 256-byte synthesised profile blob — opaque to the codec.
    let profile: Vec<u8> = (0..=255u8).collect();
    let meta = PngMetadata {
        iccp: Some(Iccp {
            name: "Synthetic Profile".to_string(),
            profile: profile.clone(),
        }),
        ..Default::default()
    };
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    let back = parsed.iccp.expect("iCCP must round-trip");
    assert_eq!(back.name, "Synthetic Profile");
    assert_eq!(back.profile, profile);
    // Pixel data still decodes (iCCP is ancillary).
    decode_png(&bytes).expect("decode");
}

#[test]
fn iccp_duplicate_rejected() {
    // §5.6 Table 1: iCCP is "Multiple OK? No". Inject two iCCP chunks
    // and confirm parse_metadata rejects.
    let img = rgba_2x2();
    let meta = PngMetadata {
        iccp: Some(Iccp {
            name: "First".to_string(),
            profile: vec![1, 2, 3],
        }),
        ..Default::default()
    };
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    // Inject a second iCCP after the first (8 PNG sig + 25 IHDR + we
    // know the first iCCP follows immediately).
    let inject_pos = 8 + 25;
    let dup = Iccp {
        name: "Second".to_string(),
        profile: vec![4, 5, 6],
    };
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"iCCP", &dup.to_bytes().unwrap());
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn iccp_chunk_precedes_plte_and_idat_in_output() {
    // §5.6 Table 1: iCCP must appear before PLTE and IDAT. Walk the
    // emitted chunk stream and verify the iCCP position.
    let img = rgba_2x2();
    let meta = PngMetadata {
        iccp: Some(Iccp {
            name: "P".to_string(),
            profile: vec![0xAA; 32],
        }),
        ..Default::default()
    };
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let mut pos = 8usize;
    let mut iccp_pos = None;
    let mut idat_pos = None;
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let ty = &bytes[pos + 4..pos + 8];
        if ty == b"iCCP" && iccp_pos.is_none() {
            iccp_pos = Some(pos);
        }
        if ty == b"IDAT" && idat_pos.is_none() {
            idat_pos = Some(pos);
        }
        if ty == b"IEND" {
            break;
        }
        pos += 8 + len + 4;
    }
    let ip = iccp_pos.expect("iCCP chunk must be present");
    let dp = idat_pos.expect("IDAT chunk must be present");
    assert!(ip < dp, "iCCP at {ip} must precede IDAT at {dp}");
}

#[test]
fn parse_metadata_rejects_iccp_with_unknown_compression_method() {
    // §11.3.2.3: only method 0 (deflate) is defined; non-zero must be
    // rejected.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"BadProfile");
    payload.push(0); // NUL after name
    payload.push(1); // bogus method
    payload.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"iCCP", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn parse_metadata_rejects_iccp_with_corrupted_zlib_body() {
    // Method = 0 but body is garbage; inflate must fail without panic.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"BadProfile");
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(&[0xFF; 6]);
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"iCCP", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn parse_metadata_rejects_iccp_with_invalid_name() {
    // Leading space → name predicate rejects.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut payload = Vec::new();
    payload.extend_from_slice(b" Leading");
    payload.push(0);
    payload.push(0);
    payload.extend_from_slice(&[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"iCCP", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

// ---- iTXt (W3C PNG3 §11.3.3.4) ----------------------------------------

#[test]
fn itxt_uncompressed_roundtrip_through_encoder() {
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.itxts.push(Itxt {
        keyword: "Title".to_string(),
        compressed: false,
        language_tag: "en-US".to_string(),
        translated_keyword: String::new(),
        text: "An internationalised plain title.".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.itxts.len(), 1);
    let back = &parsed.itxts[0];
    assert_eq!(back.keyword, "Title");
    assert!(!back.compressed);
    assert_eq!(back.language_tag, "en-US");
    assert_eq!(back.translated_keyword, "");
    assert_eq!(back.text, "An internationalised plain title.");
    decode_png(&bytes).expect("decode");
}

#[test]
fn itxt_compressed_roundtrip_through_encoder() {
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.itxts.push(Itxt {
        keyword: "Description".to_string(),
        compressed: true,
        language_tag: "ja".to_string(),
        translated_keyword: "説明".to_string(),
        text: "圧縮された日本語の説明テキスト".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.itxts.len(), 1);
    let back = &parsed.itxts[0];
    assert!(back.compressed);
    assert_eq!(back.language_tag, "ja");
    assert_eq!(back.translated_keyword, "説明");
    assert_eq!(back.text, "圧縮された日本語の説明テキスト");
}

#[test]
fn itxt_multiple_with_same_keyword_roundtrip() {
    // §11.3.3.4 inherits §4.2.7 ¶3 — keyword uniqueness is not required.
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.itxts.push(Itxt {
        keyword: "Description".to_string(),
        compressed: false,
        language_tag: "en".to_string(),
        translated_keyword: String::new(),
        text: "First description".to_string(),
    });
    meta.itxts.push(Itxt {
        keyword: "Description".to_string(),
        compressed: false,
        language_tag: "fr".to_string(),
        translated_keyword: String::new(),
        text: "Première description".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.itxts.len(), 2);
    assert_eq!(parsed.itxts[0].keyword, "Description");
    assert_eq!(parsed.itxts[0].language_tag, "en");
    assert_eq!(parsed.itxts[1].keyword, "Description");
    assert_eq!(parsed.itxts[1].language_tag, "fr");
    assert_eq!(parsed.itxts[1].text, "Première description");
}

#[test]
fn itxt_chunk_precedes_idat_in_output() {
    // §5.6 Table 1: iTXt shares the "Before IDAT, no ordering
    // constraint" bucket with tEXt / zTXt.
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.itxts.push(Itxt {
        keyword: "Title".to_string(),
        compressed: false,
        language_tag: String::new(),
        translated_keyword: String::new(),
        text: "T".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let mut pos = 8usize;
    let mut itxt_pos = None;
    let mut idat_pos = None;
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let ty = &bytes[pos + 4..pos + 8];
        if ty == b"iTXt" && itxt_pos.is_none() {
            itxt_pos = Some(pos);
        }
        if ty == b"IDAT" && idat_pos.is_none() {
            idat_pos = Some(pos);
        }
        if ty == b"IEND" {
            break;
        }
        pos += 8 + len + 4;
    }
    let ip = itxt_pos.expect("iTXt chunk must be present");
    let dp = idat_pos.expect("IDAT chunk must be present");
    assert!(ip < dp, "iTXt at {ip} must precede IDAT at {dp}");
}

#[test]
fn itxt_coexists_with_text_and_ztxt() {
    // The three text-chunk types must independently round-trip when
    // mixed in one file.
    let img = rgba_2x2();
    let mut meta = PngMetadata::default();
    meta.texts.push(Text {
        keyword: "Title".to_string(),
        text: "plain".to_string(),
    });
    meta.ztxts.push(Ztxt {
        keyword: "Description".to_string(),
        text: "zlib compressed".to_string(),
    });
    meta.itxts.push(Itxt {
        keyword: "Comment".to_string(),
        compressed: true,
        language_tag: "en".to_string(),
        translated_keyword: "Comment".to_string(),
        text: "international UTF-8 comment".to_string(),
    });
    let opts = PngEncoderOptions {
        metadata: Some(meta),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let parsed = parse_metadata(&bytes).expect("parse_metadata");
    assert_eq!(parsed.texts.len(), 1);
    assert_eq!(parsed.ztxts.len(), 1);
    assert_eq!(parsed.itxts.len(), 1);
    assert_eq!(parsed.texts[0].text, "plain");
    assert_eq!(parsed.ztxts[0].text, "zlib compressed");
    assert_eq!(parsed.itxts[0].text, "international UTF-8 comment");
    assert_eq!(parsed.itxts[0].translated_keyword, "Comment");
}

#[test]
fn parse_metadata_rejects_itxt_with_unknown_compression_flag() {
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"Title");
    payload.push(0); // NUL after keyword
    payload.push(2); // bogus flag
    payload.push(0); // method
    payload.push(0); // NUL after lang
    payload.push(0); // NUL after translated kw
    payload.extend_from_slice(b"text");
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"iTXt", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn parse_metadata_rejects_itxt_with_invalid_utf8_text() {
    // Inject a raw iTXt whose text is byte 0xFF — invalid UTF-8 start.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut payload = Vec::new();
    payload.extend_from_slice(b"Title");
    payload.push(0);
    payload.push(0); // flag = 0 (uncompressed)
    payload.push(0);
    payload.push(0); // NUL after lang
    payload.push(0); // NUL after translated kw
    payload.push(0xFF); // invalid UTF-8 lead byte
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"iTXt", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

#[test]
fn parse_metadata_rejects_itxt_with_invalid_keyword() {
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut payload = Vec::new();
    payload.extend_from_slice(b" Leading"); // leading space → invalid keyword
    payload.push(0);
    payload.push(0);
    payload.push(0);
    payload.push(0);
    payload.push(0);
    // empty text
    let mut chunk = Vec::new();
    oxideav_png::chunk::write_chunk(&mut chunk, b"iTXt", &payload);
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&chunk);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    assert!(parse_metadata(&tampered).is_err());
}

// ---- mDCV + cLLI (W3C PNG3 §11.3.2.7 / §11.3.2.8) HDR static metadata --

fn bt2100_mdcv() -> Mdcv {
    // PNG3 §11.3.2.7 Examples 5-8 — BT.2100 HDR mastering display.
    Mdcv {
        primary_r_x: 35400,
        primary_r_y: 14600,
        primary_g_x: 8500,
        primary_g_y: 39850,
        primary_b_x: 6550,
        primary_b_y: 2300,
        white_point_x: 15635,
        white_point_y: 16450,
        max_luminance: 40_000_000, // 4000 cd/m²
        min_luminance: 5,          // 0.0005 cd/m²
    }
}

fn hdr10_clli() -> Clli {
    // PNG3 §11.3.2.8 Examples 13-14 — MaxCLL 1000, MaxFALL 250 cd/m².
    Clli {
        max_content_light_level: 10_000_000,
        max_frame_average_light_level: 2_500_000,
    }
}

#[test]
fn mdcv_roundtrip() {
    let img = rgba_2x2();
    let mdcv = bt2100_mdcv();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            mdcv: Some(mdcv),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.mdcv, Some(mdcv));
    // The pixel plane must still decode bit-exactly.
    let back = decode_png(&bytes).expect("decode pixels");
    assert_eq!(back.data, img.data);
}

#[test]
fn clli_roundtrip() {
    let img = rgba_2x2();
    let clli = hdr10_clli();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            clli: Some(clli),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.clli, Some(clli));
}

#[test]
fn mdcv_and_clli_together_with_cicp_hdr10() {
    // The §11.3.2.7 note "a cICP chunk must accompany the use of mDCV in
    // order to establish the basic characteristics of the image content"
    // is content-level guidance, not a parse-time `shall`; but in
    // practice the HDR10 profile pairs cICP (BT.2100 + PQ + full-range)
    // with mDCV + cLLI. Verify the three round-trip together.
    let img = rgba_2x2();
    let cicp = Cicp {
        color_primaries: 9,       // BT.2020 / BT.2100
        transfer_function: 16,    // SMPTE 2084 / PQ
        matrix_coefficients: 0,   // RGB (pinned by PNG3 §11.3.2.6)
        video_full_range_flag: 1, // full range
    };
    let mdcv = bt2100_mdcv();
    let clli = hdr10_clli();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            cicp: Some(cicp),
            mdcv: Some(mdcv),
            clli: Some(clli),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.cicp, Some(cicp));
    assert_eq!(meta.mdcv, Some(mdcv));
    assert_eq!(meta.clli, Some(clli));
}

#[test]
fn mdcv_payload_matches_spec_examples() {
    // PNG3 §11.3.2.7 Examples 5/7/8 — verify the encoded mDCV payload
    // matches the spec's tabulated hex bytes exactly.
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            mdcv: Some(bt2100_mdcv()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let chunk_pos = bytes
        .windows(4)
        .position(|w| w == b"mDCV")
        .expect("mDCV present");
    // 24-byte payload follows the 4-byte type tag.
    let payload_start = chunk_pos + 4;
    let payload = &bytes[payload_start..payload_start + 24];
    // Example 5 row 1 stored hex: { 8A 48, 39 08 } for primary R.
    assert_eq!(&payload[..4], &[0x8A, 0x48, 0x39, 0x08]);
    // Example 7 stored hex: 02 62 5A 00 for max luminance.
    assert_eq!(&payload[16..20], &[0x02, 0x62, 0x5A, 0x00]);
    // Example 8 stored hex: 00 00 00 05 for min luminance.
    assert_eq!(&payload[20..24], &[0x00, 0x00, 0x00, 0x05]);
}

#[test]
fn clli_payload_matches_spec_examples() {
    // PNG3 §11.3.2.8 Examples 13/14 — MaxCLL 1000 cd/m² (00 98 96 80),
    // MaxFALL 250 cd/m² (00 26 25 A0).
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            clli: Some(hdr10_clli()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let chunk_pos = bytes
        .windows(4)
        .position(|w| w == b"cLLI")
        .expect("cLLI present");
    let payload_start = chunk_pos + 4;
    let payload = &bytes[payload_start..payload_start + 8];
    assert_eq!(payload, &[0x00, 0x98, 0x96, 0x80, 0x00, 0x26, 0x25, 0xA0]);
}

#[test]
fn chunk_ordering_mdcv_and_clli_precede_plte_and_idat() {
    // §11.3.2.7: "The mDCV chunk MUST come before the PLTE and IDAT
    // chunks." §5.6 Table 1 says the same for cLLI.
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            mdcv: Some(bt2100_mdcv()),
            clli: Some(hdr10_clli()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| bytes.windows(4).position(|w| w == tag);
    let mdcv_pos = pos(b"mDCV").expect("mDCV");
    let clli_pos = pos(b"cLLI").expect("cLLI");
    let plte_pos = pos(b"PLTE").expect("PLTE");
    let idat_pos = pos(b"IDAT").expect("IDAT");
    assert!(mdcv_pos < plte_pos, "mDCV must precede PLTE");
    assert!(mdcv_pos < idat_pos, "mDCV must precede IDAT");
    assert!(clli_pos < plte_pos, "cLLI must precede PLTE");
    assert!(clli_pos < idat_pos, "cLLI must precede IDAT");
}

#[test]
fn chunk_ordering_mdcv_clli_trail_basic_colour_chunks() {
    // §4.3-ranked colour chunks (cICP/sRGB/sBIT/gAMA/cHRM) describe the
    // colour space the samples live in; mDCV + cLLI add HDR
    // tone-mapping metadata supplementing it. The encoder emits the
    // ranked chunks first so a viewer walking the file in order picks
    // up the basic colour signal before the HDR hints.
    let img = rgba_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            cicp: Some(Cicp {
                color_primaries: 9,
                transfer_function: 16,
                matrix_coefficients: 0,
                video_full_range_flag: 1,
            }),
            mdcv: Some(bt2100_mdcv()),
            clli: Some(hdr10_clli()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let pos = |tag: &[u8; 4]| bytes.windows(4).position(|w| w == tag);
    let cicp_pos = pos(b"cICP").expect("cICP");
    let mdcv_pos = pos(b"mDCV").expect("mDCV");
    let clli_pos = pos(b"cLLI").expect("cLLI");
    assert!(cicp_pos < mdcv_pos, "cICP precedes mDCV");
    assert!(cicp_pos < clli_pos, "cICP precedes cLLI");
    assert!(mdcv_pos < clli_pos, "mDCV precedes cLLI");
}

#[test]
fn parse_metadata_detects_duplicate_mdcv() {
    // §5.6 Table 1 marks mDCV "Multiple OK? No" — two consecutive
    // chunks must be rejected.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25; // immediately after IHDR.
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"mDCV", &bt2100_mdcv().to_bytes());
    let mut tampered = Vec::with_capacity(bytes.len() + 2 * single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("two mDCV chunks must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("duplicate mDCV"),
        "expected duplicate-mDCV error, got {msg}"
    );
}

#[test]
fn parse_metadata_detects_duplicate_clli() {
    // §5.6 Table 1 marks cLLI "Multiple OK? No" — two consecutive
    // chunks must be rejected.
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut single = Vec::new();
    oxideav_png::chunk::write_chunk(&mut single, b"cLLI", &hdr10_clli().to_bytes());
    let mut tampered = Vec::with_capacity(bytes.len() + 2 * single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("two cLLI chunks must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("duplicate cLLI"),
        "expected duplicate-cLLI error, got {msg}"
    );
}

#[test]
fn parse_metadata_rejects_mdcv_with_wrong_length() {
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut single = Vec::new();
    // 23 bytes — short by one.
    oxideav_png::chunk::write_chunk(&mut single, b"mDCV", &[0u8; 23]);
    let mut tampered = Vec::with_capacity(bytes.len() + single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("wrong-length mDCV must fail");
    let msg = format!("{err:?}");
    assert!(msg.contains("mDCV"), "expected mDCV error, got {msg}");
}

#[test]
fn parse_metadata_rejects_clli_with_wrong_length() {
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    let inject_pos = 8 + 25;
    let mut single = Vec::new();
    // 9 bytes — long by one.
    oxideav_png::chunk::write_chunk(&mut single, b"cLLI", &[0u8; 9]);
    let mut tampered = Vec::with_capacity(bytes.len() + single.len());
    tampered.extend_from_slice(&bytes[..inject_pos]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[inject_pos..]);
    let err = parse_metadata(&tampered).expect_err("wrong-length cLLI must fail");
    let msg = format!("{err:?}");
    assert!(msg.contains("cLLI"), "expected cLLI error, got {msg}");
}

#[test]
fn clli_zero_unknown_sentinel_roundtrips() {
    // §11.3.2.8: "A value of zero for either MaxCLL or MaxFALL means
    // that the value is unknown or not currently calculable." We
    // preserve the raw integer rather than rejecting it — a live APNG
    // encoder that does not yet know its peak values can emit a
    // placeholder cLLI and rewrite the value when the stream ends.
    let img = rgba_2x2();
    let clli = Clli {
        max_content_light_level: 0,
        max_frame_average_light_level: 0,
    };
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            clli: Some(clli),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.clli, Some(clli));
}

// ---- tRNS round-trip (RFC 2083 §4.2.9 / W3C PNG3 §11.3.1.1) ------------

/// Helper: build a 4×1 `Rgb24` image with the four canonical primaries
/// (red, green, blue, white). Stride matches `3 * width` since the data
/// is tightly packed.
fn rgb24_4x1() -> PngImage {
    PngImage {
        width: 4,
        height: 1,
        pixel_format: PngPixelFormat::Rgb24,
        stride: 12,
        data: vec![
            255, 0, 0, // red
            0, 255, 0, // green
            0, 0, 255, // blue
            255, 255, 255, // white
        ],
        palette: Vec::new(),
    }
}

/// Helper: build a 2×1 `Gray16Le` image. `Gray16Le` per [`PngPixelFormat`]
/// stores each sample as two little-endian bytes; the encoder swaps them
/// to big-endian when packing the IDAT.
fn gray16le_2x1() -> PngImage {
    PngImage {
        width: 2,
        height: 1,
        pixel_format: PngPixelFormat::Gray16Le,
        stride: 4,
        data: vec![0x00, 0x10, 0xFF, 0x80],
        palette: Vec::new(),
    }
}

#[test]
fn trns_gray_keyed_sample_roundtrips_through_encoder() {
    // ct=0, 8-bit: encode a grayscale image with a keyed-sample tRNS,
    // re-parse, expect the same Trns::Grayscale(v) back.
    let img = gray8_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            trns: Some(Trns::Grayscale(64)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.trns, Some(Trns::Grayscale(64)));
    // The decoder must still round-trip the pixels.
    let img_back = decode_png(&bytes).expect("decode");
    assert_eq!(img_back.pixel_format, PngPixelFormat::Gray8);
    assert_eq!(img_back.data, img.data);
}

#[test]
fn trns_rgb_keyed_sample_roundtrips_through_encoder() {
    // ct=2, 8-bit: a keyed RGB triple round-trips byte-for-byte.
    let img = rgb24_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            trns: Some(Trns::Rgb(255, 0, 0)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.trns, Some(Trns::Rgb(255, 0, 0)));
    let img_back = decode_png(&bytes).expect("decode");
    assert_eq!(img_back.pixel_format, PngPixelFormat::Rgb24);
    assert_eq!(img_back.data, img.data);
}

#[test]
fn trns_gray16_keyed_sample_preserves_both_bytes() {
    // §4.2.9 note: "if the grayscale level 0x0001 is specified to be
    // transparent, it would be incorrect to compare only the high-order
    // byte and decide that 0x0002 is also transparent." The encoder must
    // store both bytes; the parse path returns them unchanged.
    let img = gray16le_2x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            trns: Some(Trns::Grayscale(0x0001)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.trns, Some(Trns::Grayscale(0x0001)));
}

#[test]
fn trns_palette_table_routed_via_metadata_round_trips() {
    // ct=3: the long-standing path is image.palette = `PLTE || tRNS`,
    // but the new metadata.trns field also accepts the alpha table. A
    // Pal8 image with an EMPTY palette tail (no per-entry alpha) plus
    // a metadata.trns table must produce the same on-wire result as
    // the legacy path.
    let mut img = pal8_4x1();
    // Strip any per-entry alpha tail by leaving palette at the bare 4×3
    // RGB entries; pal8_4x1 already does that.
    assert_eq!(img.palette.len(), 12);
    let alphas = vec![0, 128, 255, 200];
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            trns: Some(Trns::Palette(alphas.clone())),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.trns, Some(Trns::Palette(alphas.clone())));

    // Legacy path: append the alpha tail to image.palette directly and
    // emit without metadata.trns. The on-wire tRNS payload must match.
    img.palette.extend_from_slice(&alphas);
    let opts_legacy = PngEncoderOptions::default();
    let bytes_legacy = encode_png_image_with_options(&img, &opts_legacy).expect("encode legacy");
    let meta_legacy = parse_metadata(&bytes_legacy).expect("parse legacy");
    assert_eq!(meta_legacy.trns, Some(Trns::Palette(alphas)));
}

#[test]
fn trns_two_sources_reject_to_avoid_duplicate_chunk() {
    // PNG3 §5.6 Table 1 marks tRNS "Multiple OK? No". If the caller
    // supplies BOTH image.palette's tRNS tail AND metadata.trns the
    // encoder must refuse to emit, since each path independently writes
    // a tRNS chunk and the file would end up non-conforming.
    let mut img = pal8_4x1();
    img.palette.extend_from_slice(&[0, 0]); // palette tail = 2-byte tRNS
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            trns: Some(Trns::Palette(vec![0, 0, 0, 0])),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = encode_png_image_with_options(&img, &opts).expect_err("must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("tRNS") || msg.contains("trns"),
        "expected tRNS-conflict error, got {msg}"
    );
}

#[test]
fn trns_variant_mismatching_colour_type_rejected_by_encoder() {
    // Asking the encoder to emit a Trns::Rgb on a Gray8 image must fail
    // — the resulting on-wire chunk would not parse.
    let img = gray8_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            trns: Some(Trns::Rgb(0, 0, 0)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = encode_png_image_with_options(&img, &opts).expect_err("must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("colour type") || msg.contains("variant"),
        "expected variant/colour-type mismatch error, got {msg}"
    );
}

#[test]
fn trns_gray_sample_beyond_bit_depth_rejected_by_encoder() {
    // 8-bit grayscale ⇒ key sample cap is 255. Asking for 256 is
    // structurally impossible (u16 fits but the IHDR width says no).
    let img = gray8_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            trns: Some(Trns::Grayscale(256)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = encode_png_image_with_options(&img, &opts).expect_err("must fail");
    let msg = format!("{err:?}");
    assert!(msg.contains("256") || msg.contains("exceeds"), "got {msg}");
}

#[test]
fn trns_chunk_lands_after_plte_in_encoded_stream() {
    // §5.6 ordering: tRNS comes "After PLTE; before IDAT". A Pal8 image
    // with both PLTE and tRNS in the same metadata round-trip must have
    // PLTE before tRNS before IDAT in the resulting wire bytes.
    let img = pal8_4x1();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            trns: Some(Trns::Palette(vec![0, 0])),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    let plte_pos = bytes.windows(4).position(|w| w == b"PLTE").expect("PLTE");
    let trns_pos = bytes.windows(4).position(|w| w == b"tRNS").expect("tRNS");
    let idat_pos = bytes.windows(4).position(|w| w == b"IDAT").expect("IDAT");
    assert!(plte_pos < trns_pos, "PLTE must precede tRNS");
    assert!(trns_pos < idat_pos, "tRNS must precede IDAT");
}

#[test]
fn trns_decoder_rejects_duplicate_chunk() {
    // §5.6 Table 1: tRNS is "Multiple OK? No". An attacker-crafted file
    // with two tRNS chunks must be rejected by parse_metadata.
    let img = gray8_2x2();
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            trns: Some(Trns::Grayscale(64)),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
    // Splice a second tRNS chunk in just after the legitimate one.
    let trns_pos = bytes.windows(4).position(|w| w == b"tRNS").expect("tRNS");
    // tRNS chunk: 4-byte length + 4-byte type + 2-byte payload + 4-byte CRC = 14 bytes total.
    // Chunk header starts 4 bytes before the type marker.
    let chunk_start = trns_pos - 4;
    let chunk_len = 12 + 2; // header(4)+type(4)+payload(2)+CRC(4) = 14
    let single = bytes[chunk_start..chunk_start + chunk_len].to_vec();
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..chunk_start + chunk_len]);
    tampered.extend_from_slice(&single);
    tampered.extend_from_slice(&bytes[chunk_start + chunk_len..]);
    let err = parse_metadata(&tampered).expect_err("duplicate tRNS must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("tRNS") || msg.contains("duplicate"),
        "got {msg}"
    );
}

#[test]
fn trns_decoder_rejects_chunk_on_ct6_input() {
    // RFC 2083 §4.2.9 final paragraph: tRNS is "prohibited" on ct=4/ct=6.
    // Splice a synthetic tRNS into an RGBA stream and confirm
    // parse_metadata rejects it (rather than silently round-tripping).
    let img = rgba_2x2();
    let bytes = encode_png_image(&img).expect("encode");
    // Build a minimal tRNS chunk (6 bytes of zero payload) including
    // its CRC and inject it right before IDAT.
    let idat_pos = bytes.windows(4).position(|w| w == b"IDAT").expect("IDAT");
    let chunk_start = idat_pos - 4;
    let mut payload = Vec::new();
    payload.extend_from_slice(&6u32.to_be_bytes());
    payload.extend_from_slice(b"tRNS");
    payload.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    // Compute CRC over type+data per §5.5 (use a tiny inline impl so the
    // test stays self-contained).
    let crc = simple_crc32(&payload[4..]);
    payload.extend_from_slice(&crc.to_be_bytes());
    let mut tampered = Vec::new();
    tampered.extend_from_slice(&bytes[..chunk_start]);
    tampered.extend_from_slice(&payload);
    tampered.extend_from_slice(&bytes[chunk_start..]);
    let err = parse_metadata(&tampered).expect_err("tRNS on ct=6 must fail");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("tRNS") || msg.contains("prohibited"),
        "got {msg}"
    );
}

/// Self-contained PNG-spec CRC-32 (§5.5) for the splice tests above.
/// Polynomial 0xEDB88320, init 0xFFFFFFFF, post-invert. Per the
/// RFC 2083 Annex D pseudocode, which is the canonical reference
/// for the chunk-CRC computation.
fn simple_crc32(buf: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, item) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *item = c;
    }
    let mut c: u32 = 0xFFFF_FFFF;
    for &b in buf {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}
