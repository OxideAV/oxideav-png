//! Pins the exact bytes every encode entry point writes.
//!
//! Each case encodes a synthetic image (every layout, padded strides,
//! sub-byte depths, Adam7, every filter strategy, several DEFLATE
//! levels, a multi-segment pixel stream, the full ancillary chunk set,
//! the image-derived chunks, APNG) and compares the length and an
//! FNV-1a 64 hash of the file against a value recorded from the
//! encoder at v0.1.12. Changes to how the encoder moves bytes around
//! (borrowed planes, streamed metadata chunks, appending to a caller's
//! buffer) must leave every one of these files unchanged.
//!
//! On a mismatch the test prints the whole table of actual values, so a
//! deliberate output change re-records in one paste.

use oxideav_png::{
    encode, encode_all, encode_apng, encode_apng_frames_with_options, encode_apng_with_options,
    encode_rgb8, encode_rgba8, ApngBlend, ApngDisposal, ApngFrameSpec, Bkgd, Chrm, Cicp, Clli,
    ColorInfo, ColorRange, EncodeOptions, Exif, FilterStrategy, FilterType, Frame, Gama, Hist,
    Iccp, Itxt, Mdcv, Metadata, Palette, Phys, PhysUnit, PngImage, PngMetadata, PngPixelFormat,
    RenderingIntent, Sbit, Splt, SpltEntry, Srgb, Text, Time, Trns, UnknownChunk, Ztxt,
};

/// FNV-1a, 64-bit. Enough to tell two encoder outputs apart; not a
/// security hash.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Deterministic xorshift32 byte stream.
fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 11) as u8
        })
        .collect()
}

/// Gradient plus four bits of noise, `stride_pad` junk bytes after
/// every row. Sample values stay below `max + 1` so the sub-byte cases
/// can reuse it.
fn synth(w: u32, h: u32, pf: PngPixelFormat, stride_pad: usize, max: u8, seed: u32) -> PngImage {
    let bpp = pf.bytes_per_pixel();
    let row = w as usize * bpp;
    let stride = row + stride_pad;
    let n = noise(stride * h as usize, seed);
    let mut data = vec![0u8; stride * h as usize];
    for y in 0..h as usize {
        for x in 0..stride {
            let v = if x < row {
                let base = ((x / bpp) * 255 / w as usize) as u8 ^ (y as u8).wrapping_mul(3);
                base.wrapping_add(n[y * stride + x] & 0x0f)
            } else {
                // Padding: never part of the image, must not reach the wire.
                0xEE
            };
            data[y * stride + x] = if x < row && max < 255 {
                v % (max + 1)
            } else {
                v
            };
        }
    }
    PngImage::packed(w, h, pf, stride, data).unwrap()
}

fn palette(entries: usize, alpha: bool) -> Palette {
    let n = noise(entries * 4, 0x5A1E_77E5);
    Palette::new(
        (0..entries)
            .map(|i| {
                let a = if alpha && i % 3 == 0 {
                    n[i * 4 + 3]
                } else {
                    255
                };
                [n[i * 4], n[i * 4 + 1], n[i * 4 + 2], a]
            })
            .collect(),
    )
}

const LAYOUTS: [PngPixelFormat; 8] = [
    PngPixelFormat::Gray8,
    PngPixelFormat::Gray16Le,
    PngPixelFormat::Rgb24,
    PngPixelFormat::Rgb48Le,
    PngPixelFormat::Pal8,
    PngPixelFormat::Ya8,
    PngPixelFormat::Rgba,
    PngPixelFormat::Rgba64Le,
];

fn layout_image(pf: PngPixelFormat, stride_pad: usize, seed: u32) -> PngImage {
    let img = synth(61, 37, pf, stride_pad, 255, seed);
    if pf == PngPixelFormat::Pal8 {
        img.with_palette(palette(256, true))
    } else {
        img
    }
}

/// The full ancillary chunk set `EncodeOptions::metadata` can carry,
/// with payloads large enough that the variable-length chunks span
/// several compressor output buffers.
fn full_metadata() -> PngMetadata {
    let big_profile = noise(150_000, 0x1CC0_0001);
    let text: String = noise(90_000, 0x7E57_0001)
        .iter()
        .map(|&b| (b'a' + b % 26) as char)
        .collect();
    let utf8_text: String = text
        .chars()
        .map(|c| if c == 'q' { 'é' } else { c })
        .collect();
    let mut exif = vec![0x4D, 0x4D, 0x00, 0x2A];
    exif.extend_from_slice(&noise(70_000, 0xE71F_0001));
    PngMetadata::default()
        .with_sbit(Sbit::Rgb(5, 6, 5))
        .with_phys(Phys::new(2835, 2835, PhysUnit::Metre))
        .with_time(Time::new(2026, 10, 7, 12, 34, 56))
        .with_bkgd(Bkgd::Rgb(1, 2, 3))
        .with_trns(Trns::Rgb(4, 5, 6))
        .with_exif(Exif::new(exif))
        .with_iccp(Iccp::new("Pinned profile".to_string(), big_profile))
        .with_gama(Gama::new(45_455))
        .with_chrm(Chrm::SRGB)
        .with_mdcv(Mdcv::new(
            [(35_400, 14_600), (8_500, 39_850), (6_550, 2_300)],
            (15_635, 16_450),
            10_000_000,
            50,
        ))
        .with_clli(Clli::new(1000, 400))
        .with_splt(vec![Splt::new(
            "suggested".to_string(),
            8,
            (0..40u16)
                .map(|i| SpltEntry::new(i, 255 - i, i * 3, 255, 40 - i))
                .collect(),
        )])
        .with_texts(vec![
            Text::new("Title".to_string(), "Pinned output".to_string()),
            Text::new("Comment".to_string(), text.clone()),
        ])
        .with_ztxts(vec![Ztxt::new("Description".to_string(), text.clone())])
        .with_itxts(vec![
            Itxt::new("Author".to_string(), "Ünïcödé".to_string())
                .with_language_tag("de-DE".to_string())
                .with_translated_keyword("Autor".to_string()),
            Itxt::new("Notes".to_string(), utf8_text).with_compressed(true),
        ])
        .with_unknowns(vec![
            UnknownChunk::new(*b"prVt", noise(3000, 0x0B5E_0001), false),
            UnknownChunk::new(*b"laTe", noise(1200, 0x0B5E_0002), true),
        ])
}

/// Every case: a name and the file the encoder writes for it.
fn standalone_cases() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let d = EncodeOptions::default();

    for (i, &pf) in LAYOUTS.iter().enumerate() {
        for pad in [0usize, 3] {
            let img = layout_image(pf, pad, 0x1000 + i as u32);
            out.push((
                format!("layout {pf:?} pad {pad}"),
                encode(&img, &d).unwrap(),
            ));
        }
        let img = layout_image(pf, 2, 0x2000 + i as u32);
        out.push((
            format!("layout {pf:?} adam7"),
            encode(&img, &d.clone().with_interlace(true)).unwrap(),
        ));
    }

    for pf in [PngPixelFormat::Gray8, PngPixelFormat::Pal8] {
        for depth in [1u8, 2, 4] {
            let max = (1u8 << depth) - 1;
            let mut img = synth(45, 29, pf, 1, max, 0x3000 + depth as u32);
            if pf == PngPixelFormat::Pal8 {
                img = img.with_palette(palette(1 << depth, depth == 2));
            }
            for interlace in [false, true] {
                let opts = d.clone().with_bit_depth(depth).with_interlace(interlace);
                out.push((
                    format!("subbyte {pf:?} {depth}-bit interlace {interlace}"),
                    encode(&img, &opts).unwrap(),
                ));
            }
        }
    }

    let rgb = synth(64, 48, PngPixelFormat::Rgb24, 0, 255, 0x4000);
    for f in [
        FilterType::None,
        FilterType::Sub,
        FilterType::Up,
        FilterType::Average,
        FilterType::Paeth,
    ] {
        let opts = d.clone().with_filter_strategy(FilterStrategy::Fixed(f));
        out.push((format!("filter {f:?}"), encode(&rgb, &opts).unwrap()));
    }
    for interlace in [false, true] {
        let opts = d
            .clone()
            .with_filter_strategy(FilterStrategy::Brute)
            .with_interlace(interlace);
        out.push((
            format!("filter brute interlace {interlace}"),
            encode(&rgb, &opts).unwrap(),
        ));
    }
    let gray_sub = synth(33, 21, PngPixelFormat::Gray8, 0, 3, 0x4100);
    out.push((
        "filter brute 2-bit adam7".to_string(),
        encode(
            &gray_sub,
            &d.clone()
                .with_filter_strategy(FilterStrategy::Brute)
                .with_bit_depth(2)
                .with_interlace(true),
        )
        .unwrap(),
    ));
    for level in [1u8, 6, 9] {
        out.push((
            format!("level {level}"),
            encode(&rgb, &d.clone().with_level(level)).unwrap(),
        ));
    }

    // 800 x 600 RGB24 is 1.44 MB of filtered rows: two deflate
    // segments, so the custom zlib framing is on the wire.
    let large = synth(800, 600, PngPixelFormat::Rgb24, 0, 255, 0x5000);
    out.push((
        "multi-segment serial".to_string(),
        encode(&large, &d).unwrap(),
    ));
    out.push((
        "multi-segment 4 threads".to_string(),
        encode(&large, &d.clone().with_threads(4)).unwrap(),
    ));
    out.push((
        "multi-segment adam7".to_string(),
        encode(&large, &d.clone().with_interlace(true)).unwrap(),
    ));

    let raw_rgb = noise(30 * 20 * 3, 0x6000);
    out.push((
        "encode_rgb8".to_string(),
        encode_rgb8(30, 20, &raw_rgb, &d).unwrap(),
    ));
    let raw_rgba = noise(30 * 20 * 4 + 7, 0x6001);
    out.push((
        "encode_rgba8 long buffer".to_string(),
        encode_rgba8(30, 20, &raw_rgba, &d).unwrap(),
    ));

    // The full ancillary set from EncodeOptions::metadata.
    let rgb_small = synth(24, 16, PngPixelFormat::Rgb24, 0, 255, 0x7000);
    out.push((
        "metadata full set".to_string(),
        encode(&rgb_small, &d.clone().with_metadata(full_metadata())).unwrap(),
    ));
    let mut srgb_set = PngMetadata::default()
        .with_srgb(Srgb::new(RenderingIntent::RelativeColorimetric))
        .with_gama(Gama::SRGB)
        .with_chrm(Chrm::SRGB);
    srgb_set.sbit = Some(Sbit::Rgb(8, 8, 8));
    out.push((
        "metadata sRGB set".to_string(),
        encode(&rgb_small, &d.clone().with_metadata(srgb_set)).unwrap(),
    ));
    out.push((
        "metadata cICP".to_string(),
        encode(
            &rgb_small,
            &d.clone()
                .with_metadata(PngMetadata::default().with_cicp(Cicp::new(9, 16, 0, 1))),
        )
        .unwrap(),
    ));
    let pal = synth(20, 10, PngPixelFormat::Pal8, 0, 15, 0x7100).with_palette(palette(16, false));
    out.push((
        "metadata palette hist bkgd".to_string(),
        encode(
            &pal,
            &d.clone().with_metadata(
                PngMetadata::default()
                    .with_hist(Hist::new((0..16).collect()))
                    .with_bkgd(Bkgd::Palette(3))
                    .with_trns(Trns::Palette(vec![0, 128, 255])),
            ),
        )
        .unwrap(),
    ));
    out.push((
        "metadata empty".to_string(),
        encode(&rgb_small, &d.clone().with_metadata(PngMetadata::default())).unwrap(),
    ));

    // The chunks the image's own side fields imply.
    let derived = synth(24, 16, PngPixelFormat::Rgba, 0, 255, 0x7200).with_metadata(
        Metadata::new()
            .with_icc(noise(40_000, 0x1CC0_0002))
            .with_exif({
                let mut e = vec![0x49, 0x49, 0x2A, 0x00];
                e.extend_from_slice(&noise(5000, 0xE71F_0002));
                e
            })
            .with_xmp(b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">pinned</x:xmpmeta>".to_vec())
            .with_gamma(0.4545),
    );
    out.push(("image metadata".to_string(), encode(&derived, &d).unwrap()));
    out.push((
        "image metadata under options".to_string(),
        encode(
            &derived,
            &d.clone().with_metadata(
                PngMetadata::default()
                    .with_iccp(Iccp::new(
                        "Options win".to_string(),
                        noise(900, 0x1CC0_0003),
                    ))
                    .with_itxts(vec![Itxt::new("Extra".to_string(), "kept".to_string())]),
            ),
        )
        .unwrap(),
    ));
    out.push((
        "image color sRGB".to_string(),
        encode(&rgb_small.clone().with_color(ColorInfo::srgb()), &d).unwrap(),
    ));
    out.push((
        "image color cICP limited".to_string(),
        encode(
            &rgb_small.clone().with_color(
                ColorInfo::png_default()
                    .with_primaries(9)
                    .with_transfer(16)
                    .with_range(ColorRange::Limited),
            ),
            &d,
        )
        .unwrap(),
    ));
    out.push((
        "image transparency gray16".to_string(),
        encode(
            &synth(24, 16, PngPixelFormat::Gray16Le, 0, 255, 0x7300)
                .with_transparency(Trns::Grayscale(0x1234)),
            &d,
        )
        .unwrap(),
    ));

    // APNG entry points.
    let frames: Vec<PngImage> = (0..3)
        .map(|i| layout_image(PngPixelFormat::Rgba, 0, 0x8000 + i))
        .collect();
    out.push((
        "apng three frames".to_string(),
        encode_apng(&frames, 7, 2).unwrap(),
    ));
    // RGB24 frames: the full set's keyed `tRNS` is a truecolour key.
    let rgb_frames: Vec<PngImage> = (0..2)
        .map(|i| layout_image(PngPixelFormat::Rgb24, 0, 0x8050 + i))
        .collect();
    out.push((
        "apng adam7 with metadata".to_string(),
        encode_apng_with_options(
            &rgb_frames,
            4,
            0,
            &d.clone()
                .with_interlace(true)
                .with_metadata(full_metadata()),
        )
        .unwrap(),
    ));
    let pal_frames: Vec<PngImage> = (0..2)
        .map(|i| layout_image(PngPixelFormat::Pal8, 0, 0x8100 + i))
        .collect();
    out.push((
        "apng pal8".to_string(),
        encode_apng(&pal_frames, 10, 0).unwrap(),
    ));
    let canvas = layout_image(PngPixelFormat::Rgb24, 0, 0x8200);
    let specs = vec![
        ApngFrameSpec::new(synth(20, 10, PngPixelFormat::Rgb24, 0, 255, 0x8201))
            .with_x_offset(5)
            .with_y_offset(7)
            .with_delay_num(3)
            .with_delay_den(30)
            .with_dispose_op(ApngDisposal::Background)
            .with_blend_op(ApngBlend::Over),
        ApngFrameSpec::new(synth(61, 37, PngPixelFormat::Rgb24, 4, 255, 0x8202)),
    ];
    out.push((
        "apng regions with default image".to_string(),
        encode_apng_frames_with_options(61, 37, Some(&canvas), &specs, 1, &d).unwrap(),
    ));
    let all: Vec<Frame> = frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            Frame::new(
                f.clone(),
                Some(std::time::Duration::from_millis(40 + i as u64)),
            )
        })
        .collect();
    out.push(("encode_all".to_string(), encode_all(&all, &d).unwrap()));
    out.push((
        "encode_all single".to_string(),
        encode_all(&[Frame::new(frames[1].clone(), None)], &d).unwrap(),
    ));

    // zTXt text outside ASCII: the Latin-1 conversion path.
    out.push((
        "metadata zTXt non-ASCII".to_string(),
        encode(
            &rgb_small,
            &d.clone()
                .with_metadata(PngMetadata::default().with_ztxts(vec![Ztxt::new(
                    "Beschreibung".to_string(),
                    latin1_text(90_000, 0x7E57_0002),
                )])),
        )
        .unwrap(),
    ));

    out.extend(edge_cases());
    out
}

/// The sizes around compcol's 16 KiB block and its 32 / 64 KiB window
/// buffers, where how the compressor's input arrives can change its
/// output.
const EDGE_SIZES: [usize; 9] = [
    16_383, 16_384, 16_385, 32_767, 32_768, 32_769, 65_535, 65_536, 65_537,
];

/// Bytes with both literals and matches for the compressor: a slow
/// ramp plus three bits of noise.
fn ramp(len: usize, seed: u32) -> Vec<u8> {
    noise(len, seed)
        .iter()
        .enumerate()
        .map(|(i, n)| ((i * 7 / 5) as u8).wrapping_add(n & 7))
        .collect()
}

/// `len` Latin-1 characters, some of them outside ASCII.
fn latin1_text(len: usize, seed: u32) -> String {
    ramp(len, seed)
        .iter()
        .map(|&b| match b {
            0x20..=0x7E | 0xA0..=0xFF => b as char,
            _ => (b'a' + b % 26) as char,
        })
        .collect()
}

/// Text whose UTF-8 encoding is exactly `len` bytes: the Latin-1
/// characters of [`latin1_text`] (one or two bytes each in UTF-8), cut
/// at `len` bytes and padded with ASCII when a two-byte character would
/// cross the end.
fn utf8_text(len: usize, seed: u32) -> String {
    let mut text = String::with_capacity(len);
    for c in latin1_text(len, seed).chars() {
        if text.len() + c.len_utf8() > len {
            break;
        }
        text.push(c);
    }
    while text.len() < len {
        text.push('x');
    }
    text
}

/// Gray8 images whose filtered stream (one filter byte per row) is
/// exactly each edge size, and metadata bodies that reach the
/// compressor at exactly each edge size: an iCCP profile of `n` bytes,
/// a zTXt text of `n` Latin-1 characters (one byte each on the wire)
/// and an iTXt text of `n` UTF-8 bytes.
fn edge_cases() -> Vec<(String, Vec<u8>)> {
    let d = EncodeOptions::default();
    let tiny = synth(4, 4, PngPixelFormat::Gray8, 0, 255, 0xED00);
    // (width + 1) * height = the edge size.
    let geometry: [(u32, u32); 9] = [
        (126, 129),
        (127, 128),
        (112, 145),
        (150, 217),
        (127, 256),
        (98, 331),
        (256, 255),
        (255, 256),
        (65_536, 1),
    ];
    let mut out = Vec::new();
    for (&n, &(w, h)) in EDGE_SIZES.iter().zip(geometry.iter()) {
        assert_eq!((w as usize + 1) * h as usize, n);
        let img = synth(w, h, PngPixelFormat::Gray8, 0, 255, 0xED01 + n as u32);
        out.push((format!("edge IDAT {n}"), encode(&img, &d).unwrap()));

        let itxt = utf8_text(n, 0xED04 + n as u32);
        assert_eq!(itxt.len(), n, "the iTXt text is {n} UTF-8 bytes");
        assert!(
            !itxt.is_ascii(),
            "the iTXt text carries two-byte characters"
        );
        let meta = PngMetadata::default()
            .with_iccp(Iccp::new("Edge".to_string(), ramp(n, 0xED02 + n as u32)))
            .with_ztxts(vec![Ztxt::new(
                "Edge".to_string(),
                latin1_text(n, 0xED03 + n as u32),
            )])
            .with_itxts(vec![
                Itxt::new("Edge".to_string(), itxt).with_compressed(true)
            ]);
        out.push((
            format!("edge iCCP zTXt iTXt {n}"),
            encode(&tiny, &d.clone().with_metadata(meta)).unwrap(),
        ));
    }
    out
}

/// `(name, length, fnv1a64)` recorded from the v0.1.12 encoder.
const STANDALONE: &[(&str, usize, u64)] = &[
    ("layout Gray8 pad 0", 1666, 0x7ee802afff19099f),
    ("layout Gray8 pad 3", 1659, 0x26b5336f32a1fca2),
    ("layout Gray8 adam7", 1885, 0xb3710da5e869ecc9),
    ("layout Gray16Le pad 0", 3236, 0xcaf4caef40d7b24a),
    ("layout Gray16Le pad 3", 3225, 0xef88415c26e7f7d3),
    ("layout Gray16Le adam7", 3615, 0xa02b3fdf44ea47fc),
    ("layout Rgb24 pad 0", 4825, 0xe2dbbe7a50d238b3),
    ("layout Rgb24 pad 3", 4815, 0x3ef7a080e65afef0),
    ("layout Rgb24 adam7", 5378, 0x55ed1f1c8683bf1b),
    ("layout Rgb48Le pad 0", 9653, 0xf53c433e9d9bcdb3),
    ("layout Rgb48Le pad 3", 9670, 0xe656f5f12b4d26ab),
    ("layout Rgb48Le adam7", 10671, 0x70a58d7c263912f3),
    ("layout Pal8 pad 0", 2695, 0x1a85a30d930e905c),
    ("layout Pal8 pad 3", 2700, 0x40ee6cb73bb24d0e),
    ("layout Pal8 adam7", 2917, 0x8aa78a3f9d463a19),
    ("layout Ya8 pad 0", 3224, 0xc14bdb6559055836),
    ("layout Ya8 pad 3", 3226, 0xca13445ca5ed64a0),
    ("layout Ya8 adam7", 3610, 0x307f00579401dd5b),
    ("layout Rgba pad 0", 6419, 0x47321359eabbd6ef),
    ("layout Rgba pad 3", 6397, 0xdd4889535ddde3e5),
    ("layout Rgba adam7", 7128, 0xb4c0181923db2279),
    ("layout Rgba64Le pad 0", 12928, 0xe50f58276de19aa8),
    ("layout Rgba64Le pad 3", 12924, 0x2457d3a2088dcab9),
    ("layout Rgba64Le adam7", 14192, 0x82abeb44049cab13),
    (
        "subbyte Gray8 1-bit interlace false",
        271,
        0x5db3a5c604233f85,
    ),
    (
        "subbyte Gray8 1-bit interlace true",
        306,
        0x676479db11df856f,
    ),
    (
        "subbyte Gray8 2-bit interlace false",
        445,
        0xebadec8faec1a169,
    ),
    (
        "subbyte Gray8 2-bit interlace true",
        476,
        0x45eb188fb4ea2cfb,
    ),
    (
        "subbyte Gray8 4-bit interlace false",
        764,
        0x1c41cdec06768344,
    ),
    (
        "subbyte Gray8 4-bit interlace true",
        791,
        0x66f82a8dc3fcaeba,
    ),
    (
        "subbyte Pal8 1-bit interlace false",
        289,
        0xd1e590aee8d7c53c,
    ),
    ("subbyte Pal8 1-bit interlace true", 324, 0x66bdfe5b128f94a8),
    (
        "subbyte Pal8 2-bit interlace false",
        485,
        0x4ba522c199bdb6a0,
    ),
    ("subbyte Pal8 2-bit interlace true", 516, 0x2c1c41c0bfdba464),
    (
        "subbyte Pal8 4-bit interlace false",
        824,
        0xe24bd14c7c8e6a49,
    ),
    ("subbyte Pal8 4-bit interlace true", 851, 0x8fc26a11945232a5),
    ("filter None", 9273, 0xd9106b3adc87556c),
    ("filter Sub", 7316, 0x0d81466cef4cb383),
    ("filter Up", 7214, 0x1029017bd4b539eb),
    ("filter Average", 7473, 0x5b23fa68da19d9b7),
    ("filter Paeth", 6625, 0x31e35b69f8ebab75),
    ("filter brute interlace false", 6508, 0xcadea8cb4673d6f1),
    ("filter brute interlace true", 7191, 0xce506a0107a1a135),
    ("filter brute 2-bit adam7", 298, 0xb8b6c01e898b82ec),
    ("level 1", 6508, 0xb24b7d36e6c1c739),
    ("level 6", 6509, 0x2d8ca479be667189),
    ("level 9", 6509, 0xc000273d9d151c28),
    ("multi-segment serial", 981851, 0x7fbae03f935416f3),
    ("multi-segment 4 threads", 981851, 0x7fbae03f935416f3),
    ("multi-segment adam7", 1006197, 0xedead227fd1fad8e),
    ("encode_rgb8", 1888, 0xcbe0d06a534e5e11),
    ("encode_rgba8 long buffer", 2488, 0xd0c541db1e3b6562),
    ("metadata full set", 434679, 0xebf2aa0b9ec13750),
    ("metadata sRGB set", 1001, 0x24096ac1a501ab5b),
    ("metadata cICP", 929, 0xed9cb438fd3f83bc),
    ("metadata palette hist bkgd", 337, 0x842f68e9cc8ab9bb),
    ("metadata empty", 913, 0xd2186f81c9ad7cac),
    ("image metadata", 46349, 0xc1f2a0960ef16e41),
    ("image metadata under options", 7265, 0x423403df5d2cb9e1),
    ("image color sRGB", 926, 0x26568ccbe3191c18),
    ("image color cICP limited", 929, 0x94a35a69b1bbf1a3),
    ("image transparency gray16", 654, 0xad7ea8c8c489b309),
    ("apng three frames", 19260, 0x03da58d3cb2aa69d),
    ("apng adam7 with metadata", 444575, 0x3d0235e61c137e43),
    ("apng pal8", 4417, 0xdea3fddb8ed1a2ad),
    ("apng regions with default image", 10139, 0x061c7bf2493aa2ce),
    ("encode_all", 19260, 0x9e21db827306ad38),
    ("encode_all single", 6404, 0x13c070961f75f681),
    ("metadata zTXt non-ASCII", 67577, 0xb1c129c31f022277),
    ("edge IDAT 16383", 11519, 0xeb4d4c88821266ae),
    ("edge iCCP zTXt iTXt 16383", 39068, 0x3e50b538352b442a),
    ("edge IDAT 16384", 11508, 0x0416aaac06fe1d14),
    ("edge iCCP zTXt iTXt 16384", 39121, 0x57da9479c4ffe339),
    ("edge IDAT 16385", 11537, 0xdafb88b3c593f4ab),
    ("edge iCCP zTXt iTXt 16385", 39103, 0xf1b79e4c2cd8e05a),
    ("edge IDAT 32767", 23077, 0xcd2e2d8e528b2a51),
    ("edge iCCP zTXt iTXt 32767", 73739, 0x407a681997cd88fa),
    ("edge IDAT 32768", 23132, 0x2f69261d87cffe92),
    ("edge iCCP zTXt iTXt 32768", 73837, 0xe9aec30e66f3a172),
    ("edge IDAT 32769", 23246, 0xeced709a7923651e),
    ("edge iCCP zTXt iTXt 32769", 73770, 0x5349f985a3062b19),
    ("edge IDAT 65535", 45611, 0x20b2a80b931ce121),
    ("edge iCCP zTXt iTXt 65535", 140827, 0x2203f77479abeba4),
    ("edge IDAT 65536", 45648, 0xbceaa66761cc17f0),
    ("edge iCCP zTXt iTXt 65536", 140814, 0x2965aa9740062742),
    ("edge IDAT 65537", 42212, 0x8d9a0b7b8096a339),
    ("edge iCCP zTXt iTXt 65537", 140816, 0xabcdb32aede0861c),
];

fn check(cases: Vec<(String, Vec<u8>)>, expected: &[(&str, usize, u64)]) {
    let actual: Vec<(String, usize, u64)> = cases
        .iter()
        .map(|(n, b)| (n.clone(), b.len(), fnv1a64(b)))
        .collect();
    let matches = actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(a, e)| a.0 == e.0 && a.1 == e.1 && a.2 == e.2);
    if !matches {
        let mut table = String::new();
        for (n, len, h) in &actual {
            table.push_str(&format!("    ({n:?}, {len}, 0x{h:016x}),\n"));
        }
        for (a, e) in actual.iter().zip(expected) {
            if a.0 != e.0 || a.1 != e.1 || a.2 != e.2 {
                eprintln!(
                    "mismatch: {:?} is {} bytes 0x{:016x}, pinned {:?} {} bytes 0x{:016x}",
                    a.0, a.1, a.2, e.0, e.1, e.2
                );
            }
        }
        panic!(
            "encoder output differs from the pinned files ({} actual cases, {} pinned); \
             actual table:\n{table}",
            actual.len(),
            expected.len()
        );
    }
}

#[test]
fn standalone_entry_points_write_the_pinned_bytes() {
    check(standalone_cases(), STANDALONE);
}

#[cfg(feature = "registry")]
mod registry {
    use super::*;
    use oxideav_core::{
        CodecId, CodecParameters, ColorPrimaries, ColorSignal, ExecutionContext,
        Frame as CoreFrame, MatrixCoefficients, PixelFormat, Rational, TransferCharacteristics,
        VideoFrame, VideoPlane,
    };

    fn frame(img: &PngImage, pts: i64) -> VideoFrame {
        let mut f = VideoFrame::from(img);
        f.pts = Some(pts);
        f
    }

    fn params(w: u32, h: u32, pf: PixelFormat) -> CodecParameters {
        let mut p = CodecParameters::video(CodecId::new("png"));
        p.width = Some(w);
        p.height = Some(h);
        p.pixel_format = Some(pf);
        p
    }

    /// Drive the framework encoder: send `frames`, flush, drain.
    fn trait_encode(params: &CodecParameters, frames: &[VideoFrame]) -> Vec<Vec<u8>> {
        trait_encode_threaded(params, frames, 1)
    }

    /// [`trait_encode`] under an execution context of `threads`.
    fn trait_encode_threaded(
        params: &CodecParameters,
        frames: &[VideoFrame],
        threads: usize,
    ) -> Vec<Vec<u8>> {
        let mut enc = oxideav_png::make_encoder(params).unwrap();
        enc.set_execution_context(&ExecutionContext::with_threads(threads));
        for f in frames {
            enc.send_frame(&CoreFrame::Video(f.clone())).unwrap();
        }
        enc.flush().unwrap();
        let mut out = Vec::new();
        while let Ok(p) = enc.receive_packet() {
            out.push(p.data);
        }
        out
    }

    fn registry_cases() -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();

        let rgba = layout_image(PngPixelFormat::Rgba, 5, 0x9000);
        let rgba_frame = frame(&rgba, 3);
        out.push((
            "encode_single rgba padded".to_string(),
            oxideav_png::encode_single(&rgba_frame, 61, 37, PixelFormat::Rgba, &[]).unwrap(),
        ));
        out.push((
            "encode_single_with_options adam7".to_string(),
            oxideav_png::encode_single_with_options(
                &rgba_frame,
                61,
                37,
                PixelFormat::Rgba,
                &[],
                &EncodeOptions::default().with_interlace(true),
            )
            .unwrap(),
        ));
        let pal = synth(16, 8, PngPixelFormat::Pal8, 0, 5, 0x9100);
        let pal_frame = VideoFrame {
            pts: None,
            planes: vec![VideoPlane {
                stride: pal.stride(),
                data: pal.as_bytes().unwrap().to_vec(),
            }],
        };
        let legacy_blob: Vec<u8> = (0..30).map(|i| (i * 7) as u8).collect();
        out.push((
            "encode_single pal8 legacy palette".to_string(),
            oxideav_png::encode_single(&pal_frame, 16, 8, PixelFormat::Pal8, &legacy_blob).unwrap(),
        ));

        // The framework Encoder trait.
        let p = params(61, 37, PixelFormat::Rgba);
        for (i, data) in trait_encode(&p, std::slice::from_ref(&rgba_frame))
            .into_iter()
            .enumerate()
        {
            out.push((format!("trait still packet {i}"), data));
        }
        let anim: Vec<VideoFrame> = (0..3)
            .map(|i| {
                frame(
                    &layout_image(PngPixelFormat::Rgba, i as usize, 0x9200 + i),
                    i as i64,
                )
            })
            .collect();
        for (i, data) in trait_encode(&p, &anim).into_iter().enumerate() {
            out.push((format!("trait three frames packet {i}"), data));
        }
        let mut hinted = p.clone();
        hinted.frame_rate = Some(Rational::new(25, 1));
        for (i, data) in trait_encode(&hinted, std::slice::from_ref(&rgba_frame))
            .into_iter()
            .enumerate()
        {
            out.push((format!("trait frame-rate hint packet {i}"), data));
        }
        let mut adam7 = p.clone();
        adam7.options = oxideav_core::CodecOptions::new().set("interlace", "true");
        for (i, data) in trait_encode(&adam7, &anim[..2]).into_iter().enumerate() {
            out.push((format!("trait adam7 two frames packet {i}"), data));
        }
        let sub = synth(16, 8, PngPixelFormat::Gray8, 0, 3, 0x9300);
        let mut sub_params = params(16, 8, PixelFormat::Gray8);
        sub_params.options = oxideav_core::CodecOptions::new().set("bit_depth", "2");
        for (i, data) in trait_encode(&sub_params, &[frame(&sub, 0), frame(&sub, 1)])
            .into_iter()
            .enumerate()
        {
            out.push((format!("trait 2-bit two frames packet {i}"), data));
        }
        let pal_side = pal_frame
            .clone()
            .with_palette((0..18).map(|i| (i * 11) as u8).collect())
            .with_color_signal(ColorSignal::srgb());
        let mut pal_params = params(16, 8, PixelFormat::Pal8);
        pal_params.extradata = legacy_blob;
        for (i, data) in trait_encode(&pal_params, &[pal_side])
            .into_iter()
            .enumerate()
        {
            out.push((format!("trait pal8 side channels packet {i}"), data));
        }
        let rgb48 = layout_image(PngPixelFormat::Rgb48Le, 0, 0x9400);
        for (i, data) in trait_encode(&params(61, 37, PixelFormat::Rgb48Le), &[frame(&rgb48, 9)])
            .into_iter()
            .enumerate()
        {
            out.push((format!("trait rgb48 packet {i}"), data));
        }

        // A second file after the first was drained: frames sent after
        // `flush` start a new packet.
        let mut enc = oxideav_png::make_encoder(&p).unwrap();
        enc.send_frame(&CoreFrame::Video(anim[0].clone())).unwrap();
        enc.flush().unwrap();
        out.push((
            "trait reuse first".to_string(),
            enc.receive_packet().unwrap().data,
        ));
        enc.send_frame(&CoreFrame::Video(anim[1].clone())).unwrap();
        enc.send_frame(&CoreFrame::Video(anim[2].clone())).unwrap();
        out.push((
            "trait reuse second".to_string(),
            enc.receive_packet().unwrap().data,
        ));

        // A Pal8 animation: the palette side-channel of frame 0 is the
        // file's PLTE / tRNS.
        let pal_anim: Vec<VideoFrame> = (0..3)
            .map(|i| {
                let img = synth(16, 8, PngPixelFormat::Pal8, 0, 17, 0x9500 + i);
                VideoFrame {
                    pts: Some(i as i64),
                    planes: vec![VideoPlane {
                        stride: img.stride(),
                        data: img.as_bytes().unwrap().to_vec(),
                    }],
                }
                .with_palette((0..18 * 3).map(|c| (c * 5 + i as usize) as u8).collect())
            })
            .collect();
        for (i, data) in trait_encode(&params(16, 8, PixelFormat::Pal8), &pal_anim)
            .into_iter()
            .enumerate()
        {
            out.push((format!("trait pal8 three frames packet {i}"), data));
        }

        // A colour-carrying animation: frame 0's colour signal becomes
        // the file's cICP chunk.
        let pq = ColorSignal::new(
            oxideav_core::ColorRange::Full,
            ColorPrimaries(9),
            TransferCharacteristics(16),
            MatrixCoefficients(0),
        );
        let colour_anim: Vec<VideoFrame> = (0..2)
            .map(|i| {
                frame(
                    &layout_image(PngPixelFormat::Rgb24, 0, 0x9600 + i),
                    i as i64,
                )
                .with_color_signal(pq)
            })
            .collect();
        for (i, data) in trait_encode(&params(61, 37, PixelFormat::Rgb24), &colour_anim)
            .into_iter()
            .enumerate()
        {
            out.push((format!("trait colour two frames packet {i}"), data));
        }

        // Four threads over frames large enough for several deflate
        // segments: the bytes must not depend on the budget.
        let big: Vec<VideoFrame> = (0..2)
            .map(|i| {
                frame(
                    &synth(800, 600, PngPixelFormat::Rgb24, 0, 255, 0x9700 + i),
                    i as i64,
                )
            })
            .collect();
        let big_params = params(800, 600, PixelFormat::Rgb24);
        for (i, data) in trait_encode_threaded(&big_params, &big, 4)
            .into_iter()
            .enumerate()
        {
            out.push((format!("trait 4 threads two frames packet {i}"), data));
        }
        for (i, data) in trait_encode_threaded(&big_params, &big[..1], 4)
            .into_iter()
            .enumerate()
        {
            out.push((format!("trait 4 threads still packet {i}"), data));
        }
        out
    }

    /// `(name, length, fnv1a64)` recorded from the v0.1.12 encoder.
    const REGISTRY: &[(&str, usize, u64)] = &[
        ("encode_single rgba padded", 6405, 0x33344947de0f718d),
        ("encode_single_with_options adam7", 7145, 0xdec43ce5fd865927),
        ("encode_single pal8 legacy palette", 185, 0x0cbe620f185b4e43),
        ("trait still packet 0", 6405, 0x33344947de0f718d),
        ("trait three frames packet 0", 19284, 0x91cd994bbbe36bc0),
        ("trait frame-rate hint packet 0", 6463, 0x9be186bfb82e747f),
        ("trait adam7 two frames packet 0", 14296, 0x55ec3c5e4643c35f),
        ("trait 2-bit two frames packet 0", 269, 0xa60ae894cd4c2e6c),
        ("trait pal8 side channels packet 0", 180, 0x06be9123de660370),
        ("trait rgb48 packet 0", 9660, 0x7ee297c6b81e7b52),
        ("trait reuse first", 6426, 0xef056a35d1b267db),
        ("trait reuse second", 12861, 0x152f86fb2c46c7cf),
        ("trait pal8 three frames packet 0", 609, 0x12db520d76d1e31b),
        ("trait colour two frames packet 0", 9687, 0x24a46e1a16e5d9fb),
        (
            "trait 4 threads two frames packet 0",
            1963263,
            0xba289c209ac060fc,
        ),
        ("trait 4 threads still packet 0", 981604, 0x1d7e87d9c72b0793),
    ];

    #[test]
    fn registry_entry_points_write_the_pinned_bytes() {
        check(registry_cases(), REGISTRY);
    }
}
