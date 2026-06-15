//! Options plumbing tests for the PNG encoder.
//!
//! The core options machinery has its own unit tests in
//! `oxideav_core::options::tests`; this file exercises the PNG-specific
//! typed struct and the factory path that parses
//! `CodecParameters::options` at init.

use oxideav_core::{
    parse_options, CodecId, CodecOptions, CodecOptionsStruct, CodecParameters, Error, PixelFormat,
};
use oxideav_png::PngEncoderOptions;

/// The schema the PNG encoder advertises: `interlace` (bool),
/// `bit_depth` (u32; 0 = native, 1/2/4 = sub-byte for Gray8 / Pal8,
/// 8 = no-op for those sources), `filter` (string;
/// `adaptive` / `none` / `sub` / `up` / `average` / `paeth`, default
/// `adaptive` per W3C PNG3 §12.7), and `compression_level` (u32;
/// 0 = default 6, 1..=9 = explicit DEFLATE level).
#[test]
fn schema_advertises_interlace_bit_depth_and_filter() {
    let schema = <PngEncoderOptions as CodecOptionsStruct>::SCHEMA;
    assert_eq!(schema.len(), 4);
    assert_eq!(schema[0].name, "interlace");
    assert_eq!(schema[1].name, "bit_depth");
    assert_eq!(schema[2].name, "filter");
    assert_eq!(schema[3].name, "compression_level");
}

/// `compression_level` threads through into
/// `PngEncoderOptions::compression_level: Option<u8>`. The `0` sentinel
/// maps to `None` (= encoder default 6); every other in-range value
/// becomes `Some(value)`. Range validation (1..=9) is deferred to
/// encode time, so a parse here accepts the raw integer.
#[test]
fn parse_from_bag_sets_compression_level() {
    for (raw, expected) in [
        ("0", None),
        ("1", Some(1u8)),
        ("6", Some(6)),
        ("9", Some(9)),
    ] {
        let opts = CodecOptions::new().set("compression_level", raw);
        let parsed = parse_options::<PngEncoderOptions>(&opts).expect("parse");
        assert_eq!(
            parsed.compression_level, expected,
            "compression_level = {raw}"
        );
    }
}

/// Every valid DEFLATE level (1..=9) plus the default (None) produces a
/// decodable PNG that round-trips to the same pixels; higher levels are
/// no larger than lower ones for compressible input, and an
/// out-of-range level is an encode error ahead of the wire.
#[test]
fn compression_level_roundtrips_and_validates_range() {
    use oxideav_png::image::{PngImage, PngPixelFormat};
    use oxideav_png::{decode_png_to_rgba, encode_png_image_with_options};

    // A 64x64 RGBA gradient — compressible enough that level matters.
    let (w, h) = (64u32, 64u32);
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&[(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8, 255]);
        }
    }
    let img = PngImage {
        width: w,
        height: h,
        stride: (w * 4) as usize,
        pixel_format: PngPixelFormat::Rgba,
        data,
        palette: Vec::new(),
    };

    let mut sizes = Vec::new();
    for level in 0u8..=9 {
        let opts = PngEncoderOptions {
            // 0 exercises the None default; 1..=9 the explicit levels.
            compression_level: if level == 0 { None } else { Some(level) },
            ..Default::default()
        };
        let bytes = encode_png_image_with_options(&img, &opts).expect("encode");
        let decoded = decode_png_to_rgba(&bytes).expect("decode");
        assert_eq!(decoded.width, w);
        assert_eq!(decoded.height, h);
        assert_eq!(decoded.data, img.data, "pixels differ at level {level}");
        sizes.push(bytes.len());
    }
    // Level 9 must not be larger than level 1 on compressible input.
    assert!(
        sizes[9] <= sizes[1],
        "level 9 ({}) larger than level 1 ({})",
        sizes[9],
        sizes[1]
    );

    // Out-of-range level is rejected before any bytes are emitted.
    for bad in [10u8, 11, 255] {
        let opts = PngEncoderOptions {
            compression_level: Some(bad),
            ..Default::default()
        };
        let err = encode_png_image_with_options(&img, &opts).unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("compression_level") && msg.contains(&bad.to_string()),
            "level {bad} should be rejected; got {msg}"
        );
    }
}

/// `filter` accepts every §12.7 filter type by name (case-insensitive)
/// plus the explicit `adaptive` / empty string defaults; an unknown
/// value is rejected with an error message that names the offending
/// token so the caller can see what they typed.
#[test]
fn parse_from_bag_sets_filter_strategy() {
    use oxideav_png::{FilterStrategy, FilterType};
    for (raw, expected) in [
        ("", FilterStrategy::Adaptive),
        ("adaptive", FilterStrategy::Adaptive),
        ("Adaptive", FilterStrategy::Adaptive),
        ("none", FilterStrategy::Fixed(FilterType::None)),
        ("NONE", FilterStrategy::Fixed(FilterType::None)),
        ("sub", FilterStrategy::Fixed(FilterType::Sub)),
        ("up", FilterStrategy::Fixed(FilterType::Up)),
        ("average", FilterStrategy::Fixed(FilterType::Average)),
        ("paeth", FilterStrategy::Fixed(FilterType::Paeth)),
        ("Paeth", FilterStrategy::Fixed(FilterType::Paeth)),
    ] {
        let opts = CodecOptions::new().set("filter", raw);
        let parsed = parse_options::<PngEncoderOptions>(&opts).expect("parse");
        assert_eq!(parsed.filter_strategy, expected, "filter = {raw:?}");
    }
}

#[test]
fn filter_strategy_default_when_unset() {
    use oxideav_png::FilterStrategy;
    let opts = CodecOptions::new();
    let parsed = parse_options::<PngEncoderOptions>(&opts).expect("parse");
    assert_eq!(parsed.filter_strategy, FilterStrategy::Adaptive);
}

#[test]
fn unknown_filter_value_rejected() {
    let opts = CodecOptions::new().set("filter", "median");
    let err = parse_options::<PngEncoderOptions>(&opts).unwrap_err();
    assert!(
        matches!(err, Error::InvalidData(ref s) if s.contains("median")),
        "got {err:?}"
    );
}

/// `bit_depth` accepts a u32 and threads through into
/// `PngEncoderOptions::bit_depth: Option<u8>`. The `0` sentinel maps
/// to `None` (= leave native), every other in-range value becomes
/// `Some(value)`.
#[test]
fn parse_from_bag_sets_bit_depth() {
    for (raw, expected) in [
        ("0", None),
        ("1", Some(1u8)),
        ("2", Some(2)),
        ("4", Some(4)),
        ("8", Some(8)),
    ] {
        let opts = CodecOptions::new().set("bit_depth", raw);
        let parsed = parse_options::<PngEncoderOptions>(&opts).expect("parse");
        assert_eq!(parsed.bit_depth, expected, "bit_depth = {raw}");
    }
}

#[test]
fn parse_from_bag_sets_interlace() {
    let opts = CodecOptions::new().set("interlace", "true");
    let parsed = parse_options::<PngEncoderOptions>(&opts).expect("parse");
    assert!(parsed.interlace);
}

#[test]
fn parse_default_when_empty() {
    let opts = CodecOptions::new();
    let parsed = parse_options::<PngEncoderOptions>(&opts).expect("parse");
    assert!(!parsed.interlace);
}

#[test]
fn unknown_key_rejected() {
    let opts = CodecOptions::new().set("not_a_real_option", "1");
    let err = parse_options::<PngEncoderOptions>(&opts).unwrap_err();
    assert!(
        matches!(err, Error::InvalidData(ref s) if s.contains("not_a_real_option")),
        "got {err:?}"
    );
}

#[test]
fn bad_value_type_rejected() {
    let opts = CodecOptions::new().set("interlace", "sometimes");
    let err = parse_options::<PngEncoderOptions>(&opts).unwrap_err();
    assert!(
        matches!(err, Error::InvalidData(ref s) if s.contains("expects bool")),
        "got {err:?}"
    );
}

/// The factory must reject bad options at init time — no frame has to
/// be sent to trigger the error.
#[test]
fn make_encoder_fails_on_bad_option() {
    let mut params = CodecParameters::video(CodecId::new("png"));
    params.width = Some(8);
    params.height = Some(8);
    params.pixel_format = Some(PixelFormat::Rgba);
    params.options = CodecOptions::new().set("nope", "x");

    let err = match oxideav_png::encoder::make_encoder(&params) {
        Err(e) => e,
        Ok(_) => panic!("expected factory to reject unknown option"),
    };
    assert!(matches!(err, Error::InvalidData(ref s) if s.contains("nope")));
}

#[test]
fn make_encoder_accepts_default_options() {
    let mut params = CodecParameters::video(CodecId::new("png"));
    params.width = Some(8);
    params.height = Some(8);
    params.pixel_format = Some(PixelFormat::Rgba);
    // No options set → factory succeeds.
    assert!(oxideav_png::encoder::make_encoder(&params).is_ok());
}

/// Builder pattern with `set()` is the ergonomic way to attach one
/// option in one line.
#[test]
fn make_encoder_accepts_interlace_true() {
    let mut params = CodecParameters::video(CodecId::new("png"));
    params.width = Some(8);
    params.height = Some(8);
    params.pixel_format = Some(PixelFormat::Rgba);
    params.options = CodecOptions::new().set("interlace", "true");
    assert!(oxideav_png::encoder::make_encoder(&params).is_ok());
}
