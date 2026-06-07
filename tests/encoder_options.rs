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
/// 8 = no-op for those sources), and `filter` (string;
/// `adaptive` / `none` / `sub` / `up` / `average` / `paeth`, default
/// `adaptive` per W3C PNG3 §12.7).
#[test]
fn schema_advertises_interlace_bit_depth_and_filter() {
    let schema = <PngEncoderOptions as CodecOptionsStruct>::SCHEMA;
    assert_eq!(schema.len(), 3);
    assert_eq!(schema[0].name, "interlace");
    assert_eq!(schema[1].name, "bit_depth");
    assert_eq!(schema[2].name, "filter");
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
