//! `encode_plane` encodes a borrowed plane in any layout without first
//! building a `PngImage`, and writes the file `encode` writes for the
//! same plane.

use oxideav_png::{
    decode_rgba8, encode, encode_plane, EncodeOptions, Palette, PngImage, PngPixelFormat,
};

/// A four-entry palette, one entry translucent.
fn palette() -> Palette {
    Palette::new(vec![
        [255, 0, 0, 255],
        [0, 255, 0, 255],
        [0, 0, 255, 128],
        [9, 8, 7, 255],
    ])
}

#[test]
fn encode_plane_writes_the_file_encode_writes_for_the_same_plane() {
    let opts = EncodeOptions::default().with_level(4);
    for (format, pad) in [
        (PngPixelFormat::Rgb24, 0usize),
        (PngPixelFormat::Rgba, 5),
        (PngPixelFormat::Gray16Le, 3),
        (PngPixelFormat::Ya8, 1),
        (PngPixelFormat::Pal8, 2),
    ] {
        let (w, h) = (23u32, 9u32);
        let stride = w as usize * format.bytes_per_pixel() + pad;
        // The last row is unpadded: the shortest buffer `packed` accepts.
        let len = stride * (h as usize - 1) + w as usize * format.bytes_per_pixel();
        let data: Vec<u8> = (0..len).map(|i| (i * 13 % 241) as u8 % 4).collect();
        let pal = (format == PngPixelFormat::Pal8).then(palette);
        let image = PngImage::packed(w, h, format, stride, data.clone())
            .unwrap()
            .with_palette(pal.clone());
        assert_eq!(
            encode_plane(w, h, format, stride, &data, pal.as_ref(), &opts).unwrap(),
            encode(&image, &opts).unwrap(),
            "{format:?} stride {stride}"
        );
    }
}

#[test]
fn a_pal8_plane_keeps_its_palette() {
    let (w, h) = (8u32, 4u32);
    let data: Vec<u8> = (0..w * h).map(|i| (i % 4) as u8).collect();
    let png = encode_plane(
        w,
        h,
        PngPixelFormat::Pal8,
        w as usize,
        &data,
        Some(&palette()),
        &EncodeOptions::default(),
    )
    .unwrap();
    let rgba = decode_rgba8(&png).expect("every index has a colour");
    let expected: Vec<u8> = data
        .iter()
        .flat_map(|&i| palette().get(i).unwrap())
        .collect();
    assert_eq!(rgba.data, expected);
}

#[test]
fn a_pal8_plane_without_a_palette_is_rejected() {
    let data = vec![0u8, 1, 2, 3];
    for pal in [None, Some(Palette::new(Vec::new()))] {
        let err = encode_plane(
            4,
            1,
            PngPixelFormat::Pal8,
            4,
            &data,
            pal.as_ref(),
            &EncodeOptions::default(),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid data: PNG encoder: a Pal8 plane needs a palette with at least one entry"
        );
    }
}

#[test]
fn encode_plane_rejects_what_packed_rejects() {
    let opts = EncodeOptions::default();
    let cases: [(u32, u32, usize, usize); 3] = [
        (0, 4, 12, 48), // zero width
        (4, 4, 11, 48), // stride shorter than the 12-byte row
        (4, 4, 12, 47), // buffer one byte short
    ];
    for (w, h, stride, len) in cases {
        let data = vec![0u8; len];
        let plane = encode_plane(w, h, PngPixelFormat::Rgb24, stride, &data, None, &opts)
            .unwrap_err()
            .to_string();
        let packed = PngImage::packed(w, h, PngPixelFormat::Rgb24, stride, data)
            .unwrap_err()
            .to_string();
        assert_eq!(plane, packed, "{w}x{h} stride {stride} len {len}");
    }
}
