//! Pins for the W3C PNG3 §4.3 "Color Chunk Priority" rules (Table 1:
//! cICP = 1 > iCCP = 2 > sRGB = 3 > cHRM/gAMA = 4 — "the chunk with
//! the lowest Priority number should take precedence and any
//! higher-numbered chunk types should be ignored") and the §11.3.2.5
//! Table 17 rule that an sRGB chunk may only ride alongside the
//! sRGB-equivalent gAMA / cHRM values ("Only the following values
//! shall be used").

use oxideav_png::{
    decode_png, encode_png_image_with_options, parse_metadata, Chrm, Cicp, ColourSource, Gama,
    Iccp, PngEncoderOptions, PngImage, PngMetadata, PngPixelFormat, RenderingIntent, Srgb,
};

fn rgba_2x2() -> PngImage {
    PngImage {
        width: 2,
        height: 2,
        pixel_format: PngPixelFormat::Rgba,
        stride: 8,
        data: vec![0x55; 16],
        palette: Vec::new(),
    }
}

fn cicp() -> Cicp {
    Cicp {
        color_primaries: 1,
        transfer_function: 13,
        matrix_coefficients: 0,
        video_full_range_flag: 1,
    }
}

fn iccp() -> Iccp {
    Iccp {
        name: "test-profile".into(),
        profile: vec![0u8; 128],
    }
}

fn srgb() -> Srgb {
    Srgb {
        rendering_intent: RenderingIntent::Perceptual,
    }
}

// ---- §4.3 Table 1 resolution ----------------------------------------

#[test]
fn no_colour_chunks_resolves_to_none() {
    assert_eq!(PngMetadata::default().colour_source(), None);
}

#[test]
fn each_single_chunk_resolves_to_itself() {
    let m = PngMetadata {
        cicp: Some(cicp()),
        ..Default::default()
    };
    assert_eq!(m.colour_source(), Some(ColourSource::Cicp));

    let m = PngMetadata {
        iccp: Some(iccp()),
        ..Default::default()
    };
    assert_eq!(m.colour_source(), Some(ColourSource::Iccp));

    let m = PngMetadata {
        srgb: Some(srgb()),
        ..Default::default()
    };
    assert_eq!(m.colour_source(), Some(ColourSource::Srgb));

    // §4.3 puts cHRM and gAMA on ONE shared priority row — either one
    // alone (or both together) selects the rank-4 description.
    let m = PngMetadata {
        gama: Some(Gama::SRGB),
        ..Default::default()
    };
    assert_eq!(m.colour_source(), Some(ColourSource::GamaChrm));

    let m = PngMetadata {
        chrm: Some(Chrm::SRGB),
        ..Default::default()
    };
    assert_eq!(m.colour_source(), Some(ColourSource::GamaChrm));
}

#[test]
fn lowest_priority_number_wins_pairwise() {
    // cICP (1) beats every other signal.
    let m = PngMetadata {
        cicp: Some(cicp()),
        iccp: Some(iccp()),
        srgb: Some(srgb()),
        gama: Some(Gama::SRGB),
        chrm: Some(Chrm::SRGB),
        ..Default::default()
    };
    assert_eq!(m.colour_source(), Some(ColourSource::Cicp));

    // iCCP (2) beats sRGB (3) and gAMA/cHRM (4). §11.3.2.3: the chunk
    // "is ignored unless it is the highest-precedence color chunk".
    let m = PngMetadata {
        iccp: Some(iccp()),
        srgb: Some(srgb()),
        gama: Some(Gama::SRGB),
        ..Default::default()
    };
    assert_eq!(m.colour_source(), Some(ColourSource::Iccp));

    // sRGB (3) beats gAMA/cHRM (4) — the Table 17 companion chunks are
    // compatibility fallbacks for sRGB-unaware decoders, not the signal.
    let m = PngMetadata {
        srgb: Some(srgb()),
        gama: Some(Gama::SRGB),
        chrm: Some(Chrm::SRGB),
        ..Default::default()
    };
    assert_eq!(m.colour_source(), Some(ColourSource::Srgb));
}

#[test]
fn priority_numbers_match_table_1() {
    assert_eq!(ColourSource::Cicp.priority(), 1);
    assert_eq!(ColourSource::Iccp.priority(), 2);
    assert_eq!(ColourSource::Srgb.priority(), 3);
    assert_eq!(ColourSource::GamaChrm.priority(), 4);
}

#[test]
fn resolution_survives_a_real_roundtrip() {
    // Encode a file carrying iCCP + sRGB + gAMA (legal: §11.3.2.3 says
    // at-most-one-embedded-profile is a `should`, and the §4.3 rule
    // exists precisely because several signals may coexist), re-parse
    // it, and confirm the resolver picks iCCP.
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            iccp: Some(iccp()),
            srgb: Some(srgb()),
            gama: Some(Gama::SRGB),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&rgba_2x2(), &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.colour_source(), Some(ColourSource::Iccp));
    // All three chunks still round-trip — precedence is a read-side
    // resolution, not a discard.
    assert!(meta.iccp.is_some() && meta.srgb.is_some() && meta.gama.is_some());
    decode_png(&bytes).expect("pixels still decode");
}

// ---- §11.3.2.5 Table 17: sRGB companion-value gate -------------------

#[test]
fn srgb_with_matching_companions_encodes() {
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            srgb: Some(srgb()),
            gama: Some(Gama::SRGB),
            chrm: Some(Chrm::SRGB),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&rgba_2x2(), &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.gama, Some(Gama::SRGB));
    assert_eq!(meta.chrm, Some(Chrm::SRGB));
}

#[test]
fn srgb_with_contradicting_gama_rejected_on_encode() {
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            srgb: Some(srgb()),
            gama: Some(Gama {
                gamma_times_100000: 100_000, // linear — contradicts sRGB
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = encode_png_image_with_options(&rgba_2x2(), &opts)
        .expect_err("contradicting gAMA next to sRGB must be rejected");
    assert!(format!("{err}").contains("Table 17"));
}

#[test]
fn srgb_with_contradicting_chrm_rejected_on_encode() {
    let mut chrm = Chrm::SRGB;
    chrm.red_x = 70_800; // BT.2020 red — contradicts sRGB primaries
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            srgb: Some(srgb()),
            chrm: Some(chrm),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = encode_png_image_with_options(&rgba_2x2(), &opts)
        .expect_err("contradicting cHRM next to sRGB must be rejected");
    assert!(format!("{err}").contains("Table 17"));
}

#[test]
fn non_srgb_gama_without_srgb_chunk_still_encodes() {
    // The Table 17 gate is scoped to sRGB-bearing streams only — a
    // plain gAMA-described colour space may use any value.
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            gama: Some(Gama {
                gamma_times_100000: 100_000,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&rgba_2x2(), &opts).expect("encode");
    let meta = parse_metadata(&bytes).expect("parse");
    assert_eq!(meta.gama.unwrap().gamma_times_100000, 100_000);
}

#[test]
fn table_17_constants_match_spec_values() {
    // W3C PNG3 §11.3.2.5 Table 17, byte-for-byte.
    assert_eq!(Gama::SRGB.gamma_times_100000, 45_455);
    assert_eq!(Chrm::SRGB.white_point_x, 31_270);
    assert_eq!(Chrm::SRGB.white_point_y, 32_900);
    assert_eq!(Chrm::SRGB.red_x, 64_000);
    assert_eq!(Chrm::SRGB.red_y, 33_000);
    assert_eq!(Chrm::SRGB.green_x, 30_000);
    assert_eq!(Chrm::SRGB.green_y, 60_000);
    assert_eq!(Chrm::SRGB.blue_x, 15_000);
    assert_eq!(Chrm::SRGB.blue_y, 6_000);
}
