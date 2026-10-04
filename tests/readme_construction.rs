//! The README's "Constructing options and records" example, compiled
//! and run so the documented construction path stays truthful: every
//! `#[non_exhaustive]` public struct is reachable through `Default` +
//! `with_*` or `new(...)` + `with_*`, and the result encodes and
//! round-trips.

use oxideav_png::{
    decode, encode, parse_metadata, ApngFrameSpec, EncodeOptions, FilterStrategy, FilterType, Gama,
    Ihdr, Itxt, PngImage, PngMetadata, PngPixelFormat, Text,
};

#[test]
fn readme_construction_example_encodes_and_round_trips() {
    let image =
        PngImage::packed(2, 1, PngPixelFormat::Rgb24, 6, vec![255, 0, 0, 0, 255, 0]).unwrap();
    let meta = PngMetadata::default()
        .with_gama(Gama::new(45_455))
        .with_texts(vec![Text::new("Software".into(), "oxideav".into())]);
    let opts = EncodeOptions::default()
        .with_compression_level(4)
        .with_filter_strategy(FilterStrategy::Fixed(FilterType::Paeth))
        .with_metadata(meta.clone());
    let png = encode(&image, &opts).expect("encode");
    assert_eq!(decode(&png).unwrap().planes[0].data, image.planes[0].data);
    assert_eq!(parse_metadata(&png).unwrap(), meta);

    // `Option` setters accept both the value and `None`.
    let cleared = opts.clone().with_bit_depth(4).with_bit_depth(None);
    assert_eq!(cleared.bit_depth, None);
    assert_eq!(opts.compression_level, Some(4));

    // Wire records and frame specs through their constructors.
    let ihdr = Ihdr::new(2, 1, 8, 2).with_interlace(1);
    assert_eq!((ihdr.compression, ihdr.filter, ihdr.interlace), (0, 0, 1));
    let spec = ApngFrameSpec::new(image.clone())
        .with_x_offset(3)
        .with_delay_num(1)
        .with_delay_den(30);
    assert_eq!(
        (spec.x_offset, spec.y_offset, spec.delay_num, spec.delay_den),
        (3, 0, 1, 30)
    );
    let itxt = Itxt::new("Comment".into(), "héllo".into()).with_language_tag("fr".into());
    assert!(!itxt.compressed);
    assert_eq!(itxt.language_tag, "fr");
}
