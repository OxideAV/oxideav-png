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

/// The schema the PNG encoder advertises. Exactly one field, `interlace`.
#[test]
fn schema_advertises_interlace() {
    let schema = <PngEncoderOptions as CodecOptionsStruct>::SCHEMA;
    assert_eq!(schema.len(), 1);
    assert_eq!(schema[0].name, "interlace");
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
