//! W3C PNG3 §11.3.2.3: the embedded profile "shall be an RGB color
//! space for color images (color types 2, 3, and 6), or a greyscale
//! color space for greyscale images (color types 0 and 4)". The data
//! colour space is the four-byte signature at offset 16 of the ICC
//! header (ICC.1 §7.2.6), readable when the bytes carry a header at all
//! (128 bytes, `acsp` at offset 36).

use oxideav_png::{
    DecodeOptions, EncodeOptions, Iccp, Metadata, Palette, PngImage, PngMetadata, PngPixelFormat,
    UnknownChunk, decode_with, encode, parse_metadata,
};

/// A 128-byte ICC header with the given data colour space.
fn profile(space: &[u8; 4]) -> Vec<u8> {
    let mut p = vec![0u8; 128];
    p[0..4].copy_from_slice(&128u32.to_be_bytes());
    p[12..16].copy_from_slice(b"mntr");
    p[16..20].copy_from_slice(space);
    p[36..40].copy_from_slice(b"acsp");
    p
}

fn image(pf: PngPixelFormat) -> PngImage {
    let bpp = pf.bytes_per_pixel();
    let mut img = PngImage::packed(3, 2, pf, 3 * bpp, vec![1u8; 6 * bpp]).unwrap();
    if pf == PngPixelFormat::Pal8 {
        img = img.with_palette(Palette::new(vec![[0, 0, 0, 255], [9, 9, 9, 255]]));
    }
    img
}

#[test]
fn data_colour_space_is_read_from_a_real_header_only() {
    let rgb = Iccp::new("p".into(), profile(b"RGB "));
    assert_eq!(rgb.data_colour_space(), Some(*b"RGB "));
    assert_eq!(rgb.conforms_to_colour_type(2), Some(true));
    assert_eq!(rgb.conforms_to_colour_type(3), Some(true));
    assert_eq!(rgb.conforms_to_colour_type(6), Some(true));
    assert_eq!(rgb.conforms_to_colour_type(0), Some(false));
    assert_eq!(rgb.conforms_to_colour_type(4), Some(false));
    assert_eq!(rgb.conforms_to_colour_type(5), None);
    let gray = Iccp::new("p".into(), profile(b"GRAY"));
    assert_eq!(gray.conforms_to_colour_type(0), Some(true));
    assert_eq!(gray.conforms_to_colour_type(4), Some(true));
    assert_eq!(gray.conforms_to_colour_type(2), Some(false));
    assert_eq!(
        Iccp::new("p".into(), profile(b"CMYK")).conforms_to_colour_type(2),
        Some(false)
    );
    // No header: too short, or no file signature.
    let mut unsigned = profile(b"RGB ");
    unsigned[36] = b'x';
    for bytes in [vec![], vec![7u8; 127], unsigned] {
        let p = Iccp::new("p".into(), bytes);
        assert_eq!(p.data_colour_space(), None);
        assert_eq!(p.conforms_to_colour_type(2), None);
    }
}

#[test]
fn strict_decode_rejects_a_mismatched_profile_and_lenient_carries_it() {
    let strict = DecodeOptions::new().with_strict(true);
    let lenient = DecodeOptions::new();
    let cases = [
        (PngPixelFormat::Gray8, b"GRAY", b"RGB "),
        (PngPixelFormat::Ya8, b"GRAY", b"RGB "),
        (PngPixelFormat::Gray16Le, b"GRAY", b"CMYK"),
        (PngPixelFormat::Rgb24, b"RGB ", b"GRAY"),
        (PngPixelFormat::Rgba, b"RGB ", b"CMYK"),
        (PngPixelFormat::Rgb48Le, b"RGB ", b"Lab "),
        (PngPixelFormat::Pal8, b"RGB ", b"GRAY"),
    ];
    for (pf, good, bad) in cases {
        let with = |space: &[u8; 4]| {
            let img = image(pf).with_metadata(Metadata::new().with_icc(profile(space)));
            encode(&img, &EncodeOptions::new()).unwrap()
        };
        let ok = with(good);
        assert_eq!(
            decode_with(&ok, &strict).unwrap().metadata.icc,
            Some(profile(good)),
            "{pf:?}"
        );
        let mismatched = with(bad);
        let err = decode_with(&mismatched, &strict).unwrap_err();
        assert!(err.to_string().contains("iCCP"), "{pf:?}: {err}");
        // Lenient: the pixels decode and the profile is carried.
        let img = decode_with(&mismatched, &lenient).unwrap();
        assert_eq!(img.metadata.icc, Some(profile(bad)), "{pf:?}");
        // The record API reports it either way.
        let iccp = parse_metadata(&mismatched).unwrap().iccp.unwrap();
        let ct = match pf {
            PngPixelFormat::Gray8 | PngPixelFormat::Gray16Le => 0,
            PngPixelFormat::Ya8 => 4,
            PngPixelFormat::Pal8 => 3,
            PngPixelFormat::Rgba => 6,
            _ => 2,
        };
        assert_eq!(iccp.conforms_to_colour_type(ct), Some(false), "{pf:?}");
    }
    // A blob without an ICC header is not judged, strict or not.
    let img = image(PngPixelFormat::Gray8).with_metadata(Metadata::new().with_icc(vec![5u8; 64]));
    let file = encode(&img, &EncodeOptions::new()).unwrap();
    assert!(decode_with(&file, &strict).is_ok());
}

#[test]
fn without_unsafe_to_copy_keeps_the_safe_chunks() {
    let meta = PngMetadata::default().with_unknowns(vec![
        UnknownChunk::new(*b"saFe", vec![1], false),
        UnknownChunk::new(*b"unSF", vec![2], false),
        UnknownChunk::new(*b"afTr", vec![3], true),
        UnknownChunk::new(*b"alSO", vec![4], true),
    ]);
    let kept = meta.without_unsafe_to_copy();
    let names: Vec<[u8; 4]> = kept.unknowns.iter().map(|u| u.chunk_type).collect();
    assert_eq!(names, [*b"saFe", *b"afTr"]);
    assert!(kept.unknowns[1].after_idat);
    // The edited-pixels re-encode: decode, change, write the survivors.
    let img = image(PngPixelFormat::Rgb24);
    let file = encode(&img, &EncodeOptions::new().with_metadata(kept.clone())).unwrap();
    assert_eq!(parse_metadata(&file).unwrap().unknowns, kept.unknowns);
}
