//! End-to-end sRGB linear-light conversion: encode an RGBA PNG carrying
//! an `sRGB` chunk, decode it, confirm the sRGB marker survives the
//! round-trip via `parse_metadata`, and composite the decoded image over
//! an opaque background in linear light (W3C PNG3 §13).
//!
//! The point of these tests is the *behavioural* difference between
//! compositing in sRGB (gamma) space and in linear light: a 50%-alpha
//! white pixel over black is ~188 sRGB in linear light, but only ~128 if
//! you (wrongly) average the gamma-encoded bytes. The §13 path is the
//! linear one, and that is what `composite_over_background` does.

use oxideav_png::{
    composite_over_background, decode_png_to_rgba, encode_png_image_with_options, linearize_rgba,
    parse_metadata, srgb_from_linear, srgb_to_linear8, srgb_to_scaled_linear8, PngEncoderOptions,
    PngImage, PngMetadata, PngPixelFormat, RenderingIntent, RgbaBitmap, Srgb,
};

fn rgba_image(w: u32, h: u32, data: Vec<u8>) -> PngImage {
    PngImage {
        width: w,
        height: h,
        pixel_format: PngPixelFormat::Rgba,
        stride: w as usize * 4,
        data,
        palette: Vec::new(),
    }
}

/// Encode an RGBA image with an `sRGB` chunk (Perceptual intent), then
/// decode it back and confirm `parse_metadata` still reports the sRGB
/// marker. The colour space tag must survive a full encode→decode trip
/// so a downstream viewer knows to apply the sRGB transfer function.
#[test]
fn srgb_chunk_survives_roundtrip() {
    let img = rgba_image(2, 1, vec![10, 20, 30, 255, 200, 150, 100, 255]);
    let opts = PngEncoderOptions {
        metadata: Some(PngMetadata {
            srgb: Some(Srgb {
                rendering_intent: RenderingIntent::Perceptual,
            }),
            ..PngMetadata::default()
        }),
        ..PngEncoderOptions::default()
    };
    let bytes = encode_png_image_with_options(&img, &opts).expect("encode");

    let meta = parse_metadata(&bytes).expect("parse_metadata");
    let srgb = meta.srgb.expect("sRGB chunk present after roundtrip");
    assert_eq!(srgb.rendering_intent, RenderingIntent::Perceptual);

    // Pixels themselves come back unchanged (the codec leaves wire samples
    // verbatim; the sRGB transfer is a viewer-side opt-in transform).
    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    assert_eq!(rgba.data[0..4], [10, 20, 30, 255]);
    assert_eq!(rgba.data[4..8], [200, 150, 100, 255]);
}

/// Decode an sRGB PNG, then linearize: each colour channel must equal the
/// table EOTF value and alpha must pass through unchanged.
#[test]
fn decoded_srgb_linearizes_per_channel() {
    let img = rgba_image(2, 1, vec![0, 128, 255, 64, 255, 0, 64, 200]);
    let bytes = encode_png_image_with_options(
        &img,
        &PngEncoderOptions {
            metadata: Some(PngMetadata {
                srgb: Some(Srgb {
                    rendering_intent: RenderingIntent::RelativeColorimetric,
                }),
                ..PngMetadata::default()
            }),
            ..PngEncoderOptions::default()
        },
    )
    .expect("encode");

    let rgba = decode_png_to_rgba(&bytes).expect("decode");
    let lin = linearize_rgba(&rgba);
    // Pixel 0: linear R/G/B from the EOTF, alpha straight through.
    assert_eq!(lin[0], srgb_to_linear8(0));
    assert_eq!(lin[1], srgb_to_linear8(128));
    assert_eq!(lin[2], srgb_to_linear8(255));
    assert_eq!(lin[3], 64);
    // Pixel 1.
    assert_eq!(lin[4], srgb_to_linear8(255));
    assert_eq!(lin[7], 200);
}

/// Full §13 path: composite a semi-transparent decoded sRGB image over an
/// opaque background in linear light. The half-alpha white-over-black
/// pixel must land in the linear-light band (~188), provably above the
/// gamma-space average (128).
#[test]
fn composite_decoded_image_over_background_linear() {
    // Three pixels: opaque red, 50% white, fully transparent.
    let img = rgba_image(
        3,
        1,
        vec![
            220, 30, 30, 255, // opaque red
            255, 255, 255, 128, // 50% white
            7, 7, 7, 0, // transparent
        ],
    );
    let bytes = encode_png_image_with_options(
        &img,
        &PngEncoderOptions {
            metadata: Some(PngMetadata {
                srgb: Some(Srgb {
                    rendering_intent: RenderingIntent::Perceptual,
                }),
                ..PngMetadata::default()
            }),
            ..PngEncoderOptions::default()
        },
    )
    .expect("encode");

    let decoded = decode_png_to_rgba(&bytes).expect("decode");
    let mut canvas = RgbaBitmap {
        width: decoded.width,
        height: decoded.height,
        data: decoded.data.clone(),
    };
    composite_over_background(&mut canvas, [0, 0, 0]);

    // Opaque red reproduces exactly; transparent pixel becomes the
    // background (black); every output pixel is opaque.
    assert_eq!(canvas.data[0..4], [220, 30, 30, 255]);
    let half = canvas.data[4];
    assert!(
        (180..195).contains(&half),
        "50% white over black in linear light should be ~188, got {half}"
    );
    assert_eq!(canvas.data[5], half);
    assert_eq!(canvas.data[6], half);
    assert_eq!(canvas.data[7], 255);
    assert_eq!(canvas.data[8..12], [0, 0, 0, 255]);
}

/// The base/delta inverse tables are an exact inverse of the EOTF table
/// for every 8-bit value — the property a viewer relies on when it
/// linearizes, operates, and re-encodes without drift.
#[test]
fn linear_roundtrip_is_lossless_for_every_byte() {
    for s in 0u16..=255 {
        let s = s as u8;
        assert_eq!(srgb_from_linear(srgb_to_scaled_linear8(s)), s);
    }
}
