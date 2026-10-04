//! The image-crate contract (`IMAGE_CRATE_API.md`) pinned item by
//! item for `oxideav-png`: the root vocabulary, the image type and its
//! raw paths, limits, strictness, the lossless round-trip of planes +
//! colour + metadata, and the pre-contract wrappers.
//!
//! Framework-free: every test here builds with
//! `--no-default-features`.

use std::io::Cursor;
use std::time::Duration;

use oxideav_png::{
    decode, decode_all, decode_all_with, decode_from, decode_rgb8, decode_rgba8, decode_with,
    encode, encode_apng, encode_rgb8, encode_rgba8, encode_to, info, probe, ColorInfo, ColorRange,
    DecodeOptions, EncodeOptions, Error, Metadata, Palette, PixelFormat, Plane, PngError, PngImage,
    PngMetadata, PngPixelFormat, Trns, XMP_KEYWORD,
};

fn rgba_2x2() -> PngImage {
    PngImage::from_rgba8(
        2,
        2,
        vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
    )
}

#[test]
fn probe_is_a_signature_sniff() {
    assert!(!probe(b""));
    assert!(!probe(b"\x89PNG\r\n\x1a"));
    assert!(probe(b"\x89PNG\r\n\x1a\n"));
    assert!(probe(
        &encode(&rgba_2x2(), &EncodeOptions::default()).unwrap()
    ));
    assert!(!probe(b"RIFF\0\0\0\0WEBP"));
}

#[test]
fn info_reads_the_header_and_chunk_walk_only() {
    let img = PngImage::packed(3, 2, PixelFormat::Gray8, 3, vec![0, 1, 2, 3, 4, 5])
        .with_transparency(Trns::Grayscale(1))
        .with_color(ColorInfo::srgb())
        .with_metadata(
            Metadata::new()
                .with_exif(b"MM\0\x2a\0\0\0\x08".to_vec())
                .with_xmp(b"<x:xmpmeta/>".to_vec()),
        );
    let bytes = encode(&img, &EncodeOptions::default()).unwrap();
    let i = info(&bytes).unwrap();
    assert_eq!((i.width, i.height), (3, 2));
    assert_eq!(i.format, PixelFormat::Gray8);
    assert_eq!(i.frames, 1);
    assert!(i.has_alpha, "tRNS on a Gray8 source means alpha");
    assert_eq!(i.color, ColorInfo::srgb());
    assert!(!i.has_icc);
    assert!(i.has_exif);
    assert!(i.has_xmp);
    assert_eq!(i.bit_depth, 8);
    assert_eq!(i.colour_type, 0);
    assert!(!i.interlaced);

    // Truncating the IDAT still fails `info` (the chunk walk validates
    // framing + CRC), but a header-only probe never decodes pixels: a
    // hostile 60000x60000 IHDR is described, not allocated.
    let mut huge = bytes.clone();
    huge[16..20].copy_from_slice(&60_000u32.to_be_bytes());
    huge[20..24].copy_from_slice(&60_000u32.to_be_bytes());
    let crc = crc32(&huge[12..29]);
    huge[29..33].copy_from_slice(&crc.to_be_bytes());
    let i = info(&huge).unwrap();
    assert_eq!((i.width, i.height), (60_000, 60_000));
    assert!(matches!(decode(&huge), Err(PngError::LimitExceeded(_))));
}

#[test]
fn decode_returns_the_native_layout_with_colour_and_metadata() {
    let icc = vec![0u8; 128];
    let img = PngImage::packed(
        2,
        1,
        PixelFormat::Rgb48Le,
        12,
        vec![0x34, 0x12, 0x78, 0x56, 0xbc, 0x9a, 0, 0, 0, 0, 0xff, 0xff],
    )
    .with_metadata(Metadata::new().with_icc(icc.clone()).with_gamma(0.45455));
    let bytes = encode(&img, &EncodeOptions::default()).unwrap();
    let back = decode(&bytes).unwrap();
    assert_eq!(back.format(), PixelFormat::Rgb48Le);
    assert_eq!(back.width(), 2);
    assert_eq!(back.height(), 1);
    assert_eq!(back.as_bytes(), img.as_bytes());
    assert_eq!(back.metadata.icc, Some(icc));
    assert!((back.metadata.gamma.unwrap() - 0.45455).abs() < 1e-6);
    // An ICC profile governs: the code points stay unspecified.
    assert_eq!(back.color, ColorInfo::png_default());
    assert_eq!(back.palette, None);
    assert_eq!(back.transparency, None);
    assert_eq!(back.clone().into_raw(), img.as_bytes().unwrap().to_vec());
}

#[test]
fn colour_precedence_follows_png3_table_1() {
    // cICP beats sRGB beats cHRM/gAMA.
    let base = rgba_2x2();
    let with = |meta: PngMetadata| {
        let bytes = encode(&base, &EncodeOptions::default().with_metadata(Some(meta))).unwrap();
        decode(&bytes).unwrap().color
    };
    assert_eq!(
        with(PngMetadata::default().with_cicp(Some(oxideav_png::Cicp::new(9, 16, 0, 1)))),
        ColorInfo::new(ColorRange::Full, 9, 16, 0)
    );
    assert_eq!(
        with(PngMetadata::default().with_cicp(Some(oxideav_png::Cicp::new(1, 1, 0, 0)))).range,
        ColorRange::Limited
    );
    assert_eq!(
        with(
            PngMetadata::default().with_srgb(Some(oxideav_png::Srgb::new(
                oxideav_png::RenderingIntent::Perceptual
            )))
        ),
        ColorInfo::srgb()
    );
    let chrm_only = with(
        PngMetadata::default()
            .with_chrm(Some(oxideav_png::Chrm::SRGB))
            .with_gama(Some(oxideav_png::Gama::SRGB)),
    );
    assert_eq!(chrm_only.primaries, ColorInfo::PRIMARIES_BT709);
    assert_eq!(chrm_only.transfer, ColorInfo::UNSPECIFIED);
    assert_eq!(
        decode(&encode(&base, &EncodeOptions::default()).unwrap())
            .unwrap()
            .color,
        ColorInfo::png_default()
    );
}

#[test]
fn raw_paths_are_tightly_packed_rgb8_and_rgba8() {
    // Every PNG layout through the one-call raw paths.
    let cases: Vec<(PngImage, Vec<u8>)> = vec![
        (
            PngImage::packed(2, 1, PixelFormat::Gray8, 2, vec![7, 9])
                .with_transparency(Trns::Grayscale(9)),
            vec![7, 7, 7, 255, 9, 9, 9, 0],
        ),
        (
            PngImage::packed(1, 1, PixelFormat::Gray16Le, 2, vec![0x34, 0x12]),
            vec![0x12, 0x12, 0x12, 255],
        ),
        (
            PngImage::from_rgb8(1, 1, vec![1, 2, 3]).with_transparency(Trns::Rgb(1, 2, 3)),
            vec![1, 2, 3, 0],
        ),
        (
            PngImage::packed(1, 1, PixelFormat::Rgb48Le, 6, vec![1, 2, 3, 4, 5, 6]),
            vec![2, 4, 6, 255],
        ),
        (
            PngImage::packed(2, 1, PixelFormat::Pal8, 2, vec![1, 0])
                .with_palette(Palette::from_rgb(&[9, 8, 7, 6, 5, 4], Some(&[33]))),
            vec![6, 5, 4, 255, 9, 8, 7, 33],
        ),
        (
            PngImage::packed(1, 1, PixelFormat::Ya8, 2, vec![50, 60]),
            vec![50, 50, 50, 60],
        ),
        (rgba_2x2(), rgba_2x2().into_raw()),
        (
            PngImage::packed(1, 1, PixelFormat::Rgba64Le, 8, vec![1, 2, 3, 4, 5, 6, 7, 8]),
            vec![2, 4, 6, 8],
        ),
    ];
    for (img, rgba) in cases {
        let bytes = encode(&img, &EncodeOptions::default()).unwrap();
        let a = decode_rgba8(&bytes).unwrap();
        assert_eq!(a.data, rgba, "{:?}", img.format());
        assert_eq!((a.width, a.height), (img.width(), img.height()));
        assert_eq!(a.stride(), img.width() as usize * 4);
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let r = decode_rgb8(&bytes).unwrap();
        assert_eq!(r.data, rgb, "{:?}", img.format());
        assert_eq!(r.as_bytes(), &rgb[..]);
        // The in-memory conversions agree with the raw paths.
        let back = decode(&bytes).unwrap();
        assert_eq!(back.to_rgba8(), rgba);
        assert_eq!(back.to_rgb8(), rgb);
    }
}

#[test]
fn encode_raw_paths_pick_rgb24_and_rgba() {
    let rgb = vec![1u8, 2, 3, 4, 5, 6];
    let bytes = encode_rgb8(2, 1, &rgb, &EncodeOptions::default().with_level(2)).unwrap();
    let back = decode(&bytes).unwrap();
    assert_eq!(back.format(), PixelFormat::Rgb24);
    assert_eq!(back.as_bytes(), Some(&rgb[..]));

    let rgba = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
    let bytes = encode_rgba8(2, 1, &rgba, &EncodeOptions::default()).unwrap();
    let back = decode(&bytes).unwrap();
    assert_eq!(back.format(), PixelFormat::Rgba);
    assert_eq!(back.as_bytes(), Some(&rgba[..]));

    // A short buffer is an error, never a panic.
    assert!(matches!(
        encode_rgba8(2, 1, &rgba[..7], &EncodeOptions::default()),
        Err(PngError::InvalidData(_))
    ));
}

#[test]
fn streaming_variants_match_the_byte_variants() {
    let img = rgba_2x2();
    let opts = EncodeOptions::default();
    let bytes = encode(&img, &opts).unwrap();
    let mut out = Vec::new();
    encode_to(&img, &opts, &mut out).unwrap();
    assert_eq!(out, bytes);
    let from = decode_from(Cursor::new(&bytes)).unwrap();
    assert_eq!(from, decode(&bytes).unwrap());
}

#[test]
fn lossless_round_trip_of_planes_colour_metadata_palette_and_transparency() {
    let imgs = vec![
        rgba_2x2().with_color(ColorInfo::srgb()).with_metadata(
            Metadata::new()
                .with_exif(b"II\x2a\0\x08\0\0\0".to_vec())
                .with_xmp(b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>".to_vec())
                .with_gamma(0.45455),
        ),
        PngImage::packed(2, 1, PixelFormat::Pal8, 2, vec![0, 1])
            .with_palette(Palette::from_rgb(&[1, 2, 3, 4, 5, 6], Some(&[0, 128])))
            .with_color(ColorInfo::new(ColorRange::Full, 9, 16, 0)),
        PngImage::packed(2, 1, PixelFormat::Gray8, 2, vec![0, 1])
            .with_transparency(Trns::Grayscale(1)),
        PngImage::packed(1, 1, PixelFormat::Rgb48Le, 6, vec![1, 2, 3, 4, 5, 6])
            .with_transparency(Trns::Rgb(0x0201, 0x0403, 0x0605))
            .with_metadata(Metadata::new().with_icc(vec![1, 2, 3, 4])),
    ];
    for img in imgs {
        let bytes = encode(&img, &EncodeOptions::default()).unwrap();
        let back = decode(&bytes).unwrap();
        assert_eq!(back, img, "{:?}", img.format());
        // and again, idempotently
        assert_eq!(encode(&back, &EncodeOptions::default()).unwrap(), bytes);
    }
}

#[test]
fn xmp_rides_the_adobe_itxt_keyword() {
    let img = rgba_2x2().with_metadata(Metadata::new().with_xmp(b"<x/>".to_vec()));
    let bytes = encode(&img, &EncodeOptions::default()).unwrap();
    let meta = oxideav_png::parse_metadata(&bytes).unwrap();
    assert_eq!(meta.itxts.len(), 1);
    assert_eq!(meta.itxts[0].keyword, XMP_KEYWORD);
    assert!(!meta.itxts[0].compressed);
    assert_eq!(meta.itxts[0].text, "<x/>");
}

#[test]
fn encode_refuses_what_png_cannot_carry() {
    // A non-identity matrix cannot be signalled (cICP matrix shall be 0).
    let img = rgba_2x2().with_color(ColorInfo::new(ColorRange::Full, 1, 1, 1));
    assert!(matches!(
        encode(&img, &EncodeOptions::default()),
        Err(Error::Unsupported(_))
    ));
    // A palette longer than PLTE allows.
    let img = PngImage::packed(1, 1, PixelFormat::Pal8, 1, vec![0])
        .with_palette(Palette::new(vec![[0, 0, 0, 255]; 257]));
    assert!(encode(&img, &EncodeOptions::default()).is_err());
    // Two tRNS sources.
    let img = PngImage::packed(1, 1, PixelFormat::Pal8, 1, vec![0])
        .with_palette(Palette::new(vec![[0, 0, 0, 7]]))
        .with_transparency(Trns::Palette(vec![1]));
    assert!(encode(&img, &EncodeOptions::default()).is_err());
}

/// A luma-only image is matrix-invariant (neutral chroma reconstructs
/// R = G = B = Y under every H.273 matrix), so a gray layout tagged
/// with a YUV matrix — a monochrome HEVC still decoded as BT.601 —
/// still encodes, with cICP carrying matrix 0.
#[test]
fn gray_layouts_accept_a_yuv_matrix_and_write_identity() {
    for fmt in [PixelFormat::Gray8, PixelFormat::Ya8, PixelFormat::Gray16Le] {
        let bpp = fmt.bytes_per_pixel();
        let img = PngImage::packed(2, 1, fmt, 2 * bpp, vec![7; 2 * bpp])
            .with_color(ColorInfo::new(ColorRange::Full, 1, 13, 6));
        let bytes =
            encode(&img, &EncodeOptions::default()).unwrap_or_else(|e| panic!("{fmt:?}: {e}"));
        let meta = oxideav_png::parse_metadata(&bytes).unwrap();
        let cicp = meta.cicp.expect("cICP written");
        assert_eq!(cicp.matrix_coefficients, 0, "{fmt:?}");
        assert_eq!(
            decode(&bytes).unwrap().as_bytes(),
            img.as_bytes(),
            "{fmt:?}"
        );
    }
}

#[test]
fn decode_options_limits_fire_before_allocation() {
    let bytes = encode(&rgba_2x2(), &EncodeOptions::default()).unwrap();
    let too_narrow = DecodeOptions::default().with_max_width(1u32);
    assert!(matches!(
        decode_with(&bytes, &too_narrow),
        Err(PngError::LimitExceeded(_))
    ));
    let too_many = DecodeOptions::default().with_max_pixels(3u64);
    assert!(matches!(
        decode_with(&bytes, &too_many),
        Err(PngError::LimitExceeded(_))
    ));
    let too_big = DecodeOptions::default().with_max_bytes(15u64);
    assert!(matches!(
        decode_with(&bytes, &too_big),
        Err(PngError::LimitExceeded(_))
    ));
    let fits = DecodeOptions::default()
        .with_max_bytes(16u64)
        .with_max_pixels(4u64);
    assert!(decode_with(&bytes, &fits).is_ok());
    assert!(decode_with(&bytes, &DecodeOptions::default().unlimited()).is_ok());
    // decode_all honours the same limits.
    assert!(matches!(
        decode_all_with(&bytes, &too_narrow),
        Err(PngError::LimitExceeded(_))
    ));
}

#[test]
fn strict_mode_rejects_what_lenient_mode_drops() {
    let bytes = encode(&rgba_2x2(), &EncodeOptions::default()).unwrap();
    // Splice a 3-byte (malformed) gAMA right after IHDR.
    let mut spliced = Vec::new();
    spliced.extend_from_slice(&bytes[..33]);
    push_chunk(&mut spliced, b"gAMA", &[1, 2, 3]);
    spliced.extend_from_slice(&bytes[33..]);
    let lenient = decode(&spliced).unwrap();
    assert_eq!(lenient.metadata.gamma, None);
    assert_eq!(lenient.as_bytes(), rgba_2x2().as_bytes());
    assert!(matches!(
        decode_with(&spliced, &DecodeOptions::default().with_strict(true)),
        Err(PngError::InvalidData(_))
    ));
    // A gAMA placed after IDAT is an ordering violation strict mode
    // catches and lenient mode tolerates.
    let mut late = Vec::new();
    late.extend_from_slice(&bytes[..bytes.len() - 12]);
    push_chunk(&mut late, b"gAMA", &45455u32.to_be_bytes());
    late.extend_from_slice(&bytes[bytes.len() - 12..]);
    assert!((decode(&late).unwrap().metadata.gamma.unwrap() - 0.45455).abs() < 1e-6);
    assert!(decode_with(&late, &DecodeOptions::default().with_strict(true)).is_err());
}

#[test]
fn decode_all_yields_one_frame_for_stills_and_composited_apng_frames() {
    let still = encode(&rgba_2x2(), &EncodeOptions::default()).unwrap();
    let frames = decode_all(&still).unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].delay, None);
    assert_eq!(frames[0].image, rgba_2x2());

    let f0 = rgba_2x2();
    let f1 = PngImage::from_rgba8(2, 2, vec![9; 16]);
    let apng = encode_apng(&[f0.clone(), f1.clone()], 25, 0).unwrap();
    let i = info(&apng).unwrap();
    assert_eq!(i.frames, 2);
    assert_eq!(i.num_plays, 0);
    let frames = decode_all(&apng).unwrap();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].delay, Some(Duration::from_millis(250)));
    assert_eq!(frames[0].image.as_bytes(), f0.as_bytes());
    assert_eq!(frames[1].image.as_bytes(), f1.as_bytes());
    // `decode` on an APNG is its default image.
    assert_eq!(decode(&apng).unwrap().as_bytes(), f0.as_bytes());
}

#[test]
fn hostile_inputs_never_panic() {
    let bytes = encode(&rgba_2x2(), &EncodeOptions::default()).unwrap();
    for cut in 0..bytes.len() {
        let _ = probe(&bytes[..cut]);
        let _ = info(&bytes[..cut]);
        let _ = decode(&bytes[..cut]);
        let _ = decode_rgba8(&bytes[..cut]);
        let _ = decode_all(&bytes[..cut]);
    }
    for i in 0..bytes.len() {
        let mut flipped = bytes.clone();
        flipped[i] ^= 0x5a;
        let _ = info(&flipped);
        let _ = decode(&flipped);
        let _ = decode_all(&flipped);
    }
}

#[test]
fn image_constructors_and_accessors() {
    let img = PngImage::new(
        2,
        1,
        PixelFormat::Rgb24,
        vec![Plane::new(8, vec![1, 2, 3, 4, 5, 6, 0, 0])],
    );
    assert_eq!(img.stride(), 8);
    assert_eq!(img.to_rgb8(), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(img.to_rgba8(), vec![1, 2, 3, 255, 4, 5, 6, 255]);
    assert!(!img.has_alpha());
    assert!(img.with_transparency(Trns::Rgb(0, 0, 0)).has_alpha());
    let bytes = encode(
        &PngImage::new(
            2,
            1,
            PixelFormat::Rgb24,
            vec![Plane::new(8, vec![1, 2, 3, 4, 5, 6, 0, 0])],
        ),
        &EncodeOptions::default(),
    )
    .unwrap();
    assert_eq!(
        decode(&bytes).unwrap().as_bytes(),
        Some(&[1u8, 2, 3, 4, 5, 6][..])
    );
    // The contract aliases.
    let _: PixelFormat = PngPixelFormat::Rgba;
    let e: Error = PngError::invalid("x");
    assert_eq!(e.to_string(), "invalid data: x");
    assert!(DecodeOptions::new().max_bytes.is_some());
}

#[test]
#[allow(deprecated)]
fn pre_contract_wrappers_still_work_and_agree() {
    use oxideav_png::{
        decode_png, decode_png_over_background, decode_png_to_rgba, encode_png_image,
        encode_png_image_with_options, PngEncoderOptions, RgbaBitmap,
    };
    let img = PngImage::packed(2, 1, PixelFormat::Pal8, 2, vec![0, 1])
        .with_palette(Palette::from_rgb(&[1, 2, 3, 4, 5, 6], Some(&[0])));
    let bytes = encode_png_image(&img).unwrap();
    assert_eq!(bytes, encode(&img, &EncodeOptions::default()).unwrap());
    assert_eq!(
        encode_png_image_with_options(&img, &PngEncoderOptions::default().with_level(9)).unwrap(),
        encode(&img, &EncodeOptions::default().with_level(9)).unwrap()
    );
    assert_eq!(decode_png(&bytes).unwrap(), decode(&bytes).unwrap());
    let a: RgbaBitmap = decode_png_to_rgba(&bytes).unwrap();
    assert_eq!(a, decode_rgba8(&bytes).unwrap());
    assert_eq!(
        decode_png_over_background(&bytes, Some([0, 0, 0])).unwrap(),
        oxideav_png::decode_over_background(&bytes, Some([0, 0, 0])).unwrap()
    );
}

// ---- helpers ---------------------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn push_chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}
