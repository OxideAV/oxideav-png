//! End-to-end `bKGD` background compositing on the decode path
//! (W3C PNG3 §13.15 "Background color" / §13.16 "Alpha channel
//! processing" / §13.12 "Sample depth rescaling").
//!
//! `decode_over_background` decodes a PNG to RGBA, resolves the
//! background colour (caller override > `bKGD` chunk > medium-grey 153
//! default), and composites every pixel's straight alpha over it in
//! linear light: `out = α·foreground + (1−α)·background`. These tests
//! drive the whole chain through the real encode→decode path and assert
//! the §13.15 background-resolution precedence plus the §13.16 blend.

use oxideav_png::{
    decode_over_background, encode, srgb_from_linear, srgb_to_scaled_linear8, Bkgd, EncodeOptions,
    PngImage, PngMetadata, PngPixelFormat, RgbaImage, Trns, DEFAULT_BACKGROUND_GREY,
};

fn rgba_image(w: u32, h: u32, data: Vec<u8>) -> PngImage {
    PngImage::packed(w, h, PngPixelFormat::Rgba, w as usize * 4, data)
}

/// The §13.16 linear-light composite of `fg` over `bg` at 8-bit straight
/// alpha — the reference value `composite_over_background` must produce.
fn composite_ref(fg: u8, bg: u8, alpha: u8) -> u8 {
    let fl = srgb_to_scaled_linear8(fg) as u64;
    let bl = srgb_to_scaled_linear8(bg) as u64;
    let a = alpha as u64;
    let blended = ((fl * a + bl * (255 - a)) / 255) as u32;
    srgb_from_linear(blended)
}

/// No `bKGD` chunk and no override ⇒ the §13.15 medium-grey 153 default.
/// A fully transparent pixel must become that grey; an opaque pixel must
/// survive; every output pixel is opaque.
#[test]
fn default_grey_when_no_bkgd_and_no_override() {
    let img = rgba_image(
        2,
        1,
        vec![
            10, 20, 30, 255, // opaque
            99, 99, 99, 0, // fully transparent
        ],
    );
    let bytes = encode(&img, &EncodeOptions::default()).expect("encode");

    let out = decode_over_background(&bytes, None).expect("decode over bg");
    assert_eq!(out.data[0..4], [10, 20, 30, 255]);
    // Transparent pixel becomes the medium-grey default, fully opaque.
    assert_eq!(
        out.data[4..8],
        [
            DEFAULT_BACKGROUND_GREY[0],
            DEFAULT_BACKGROUND_GREY[1],
            DEFAULT_BACKGROUND_GREY[2],
            255
        ]
    );
}

/// A caller override beats both the chunk and the default (§13.15: a web
/// browser "should ignore the bKGD chunk … overriding bKGD with their
/// preferred background color").
#[test]
fn override_beats_bkgd_chunk() {
    let img = rgba_image(1, 1, vec![0, 0, 0, 0]); // one transparent pixel
    let opts = EncodeOptions::default().with_metadata(Some(
        PngMetadata::default().with_bkgd(Some(Bkgd::Rgb(10, 20, 30))),
    ));
    let bytes = encode(&img, &opts).expect("encode");

    let out = decode_over_background(&bytes, Some([200, 100, 50])).expect("decode");
    // The transparent pixel resolves to the override, not the bKGD chunk.
    assert_eq!(out.data[0..4], [200, 100, 50, 255]);
}

/// The `bKGD` chunk (RGB variant) drives the background when present and
/// no override is given. The half-alpha foreground over it must match the
/// §13.16 linear-light reference per channel.
#[test]
fn bkgd_rgb_chunk_composites_half_alpha() {
    let img = rgba_image(2, 1, vec![255, 255, 255, 128, 40, 60, 80, 255]);
    let opts = EncodeOptions::default().with_metadata(Some(
        PngMetadata::default().with_bkgd(Some(Bkgd::Rgb(0, 0, 0))),
    ));
    let bytes = encode(&img, &opts).expect("encode");

    let out = decode_over_background(&bytes, None).expect("decode");
    // Pixel 0: 50% white over black, per channel, in linear light.
    let expect0 = composite_ref(255, 0, 128);
    assert_eq!(out.data[0], expect0);
    assert_eq!(out.data[1], expect0);
    assert_eq!(out.data[2], expect0);
    assert_eq!(out.data[3], 255);
    // Pixel 1 is opaque ⇒ reproduced exactly.
    assert_eq!(out.data[4..8], [40, 60, 80, 255]);
}

/// Indexed image: a `tRNS` alpha table marks one palette entry fully
/// transparent, and a `bKGD` palette index names the background. The
/// transparent pixel must resolve to the §13.15 palette-looked-up colour.
#[test]
fn bkgd_palette_index_composites_transparent_entry() {
    // PLTE: idx0 red, idx1 green (the background), idx2 blue (transparent).
    let palette = vec![
        200, 0, 0, // idx0
        0, 180, 0, // idx1 (background)
        0, 0, 255, // idx2 (transparent)
    ];
    // tRNS: idx0 opaque, idx1 opaque, idx2 transparent.
    let trns = Trns::Palette(vec![255, 255, 0]);

    // Two-pixel indexed image: pixel0 = idx0 (opaque red), pixel1 = idx2
    // (transparent blue).
    let img = PngImage::packed(2, 1, PngPixelFormat::Pal8, 2, vec![0, 2])
        .with_palette(oxideav_png::Palette::from_rgb(&palette, None));
    let opts = EncodeOptions::default().with_metadata(Some(
        PngMetadata::default()
            .with_bkgd(Some(Bkgd::Palette(1)))
            .with_trns(Some(trns)),
    ));
    let bytes = encode(&img, &opts).expect("encode");

    let out = decode_over_background(&bytes, None).expect("decode");
    // Opaque red survives.
    assert_eq!(out.data[0..4], [200, 0, 0, 255]);
    // The transparent pixel composites fully onto the green background
    // colour (α = 0 ⇒ pure background), all opaque.
    assert_eq!(out.data[4..8], [0, 180, 0, 255]);
}

/// A fully opaque image is returned unchanged (every pixel already
/// α = 255) regardless of the background source.
#[test]
fn fully_opaque_image_is_unchanged() {
    let img = rgba_image(2, 1, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    let bytes = encode(&img, &EncodeOptions::default()).expect("encode");

    let out = decode_over_background(&bytes, Some([1, 2, 3])).expect("decode");
    assert_eq!(out.data, vec![10, 20, 30, 255, 40, 50, 60, 255]);
}

/// `decode_over_background` always yields a tightly-packed RGBA buffer
/// at the source dimensions with opaque alpha everywhere.
#[test]
fn output_is_opaque_packed_rgba() {
    let img = rgba_image(3, 2, vec![0; 3 * 2 * 4]); // all transparent black
    let bytes = encode(&img, &EncodeOptions::default()).expect("encode");

    let out: RgbaImage = decode_over_background(&bytes, None).expect("decode");
    assert_eq!(out.width, 3);
    assert_eq!(out.height, 2);
    assert_eq!(out.data.len(), 3 * 2 * 4);
    for px in out.data.chunks_exact(4) {
        assert_eq!(px[3], 255, "every pixel opaque after compositing");
        // All-transparent over default grey ⇒ pure grey.
        assert_eq!([px[0], px[1], px[2]], DEFAULT_BACKGROUND_GREY);
    }
}
