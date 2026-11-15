//! Every ancillary chunk crosses the framework as a record: the
//! decoder hands the chunks the well-known kinds and the colour signal
//! do not cover as format-native `png-<chunk>` blobs (payload = the
//! chunk body as stored), the demuxer puts the same records on the
//! stream, and the encoder writes them back — dropping, never failing
//! on, a record that no longer fits the picture it is encoding.

#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{
    BlobKind, CodecId, CodecParameters, Frame, MetadataBlob, PixelFormat, RuntimeContext,
    VideoFrame, VideoPlane,
};
use oxideav_png::{
    Bkgd, Chrm, Cicp, Clli, ColorInfo, EncodeOptions, Gama, Hist, Itxt, Mdcv, Palette, Phys,
    PhysUnit, PngImage, PngMetadata, PngPixelFormat, RenderingIntent, Sbit, Splt, SpltEntry, Srgb,
    Text, Time, Trns, UnknownChunk, Ztxt, parse_metadata,
};

const W: u32 = 6;
const H: u32 = 4;

fn kind(name: &str) -> BlobKind {
    BlobKind::custom(name)
}

fn params(pix: PixelFormat) -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new("png"));
    p.width = Some(W);
    p.height = Some(H);
    p.pixel_format = Some(pix);
    p
}

/// Decode one PNG file through the framework decoder.
fn trait_decode(png: &[u8]) -> VideoFrame {
    let mut dec = oxideav_png::make_decoder(&params(PixelFormat::Rgba)).unwrap();
    let pkt = oxideav_core::Packet::new(0, oxideav_core::TimeBase::new(1, 100), png.to_vec());
    dec.send_packet(&pkt).unwrap();
    match dec.receive_frame().unwrap() {
        Frame::Video(v) => v,
        other => panic!("not a video frame: {other:?}"),
    }
}

/// Encode one frame through the framework encoder.
fn trait_encode(params: &CodecParameters, frame: VideoFrame) -> Vec<u8> {
    let mut enc = oxideav_png::make_encoder(params).unwrap();
    enc.send_frame(&Frame::Video(frame)).unwrap();
    enc.flush().unwrap();
    enc.receive_packet().unwrap().data
}

fn plane(bytes_per_pixel: usize) -> Vec<u8> {
    (0..W as usize * H as usize * bytes_per_pixel)
        .map(|i| (i * 11 % 251) as u8)
        .collect()
}

/// The records every layout can carry.
fn common_metadata() -> PngMetadata {
    PngMetadata::default()
        .with_phys(Phys::new(3780, 3780, PhysUnit::Metre))
        .with_time(Time::new(2026, 11, 15, 9, 30, 59))
        .with_mdcv(Mdcv::new(
            [(35400, 14600), (8500, 39850), (6550, 2300)],
            (15635, 16450),
            10_000_000,
            50,
        ))
        .with_clli(Clli::new(10_000_000, 2_500_000))
        .with_splt(vec![
            Splt::new(
                "eight".into(),
                8,
                vec![
                    SpltEntry::new(1, 2, 3, 255, 9),
                    SpltEntry::new(4, 5, 6, 0, 1),
                ],
            ),
            Splt::new(
                "sixteen".into(),
                16,
                vec![SpltEntry::new(65535, 2, 3, 4, 5)],
            ),
        ])
        .with_texts(vec![
            Text::new("Title".into(), "A picture".into()),
            Text::new("Comment".into(), "caf\u{e9}".into()),
        ])
        .with_ztxts(vec![Ztxt::new("Description".into(), "z".repeat(900))])
        .with_itxts(vec![
            Itxt::new("Author".into(), "\u{540d}\u{524d}".into())
                .with_language_tag("ja".into())
                .with_translated_keyword("\u{8457}\u{8005}".into()),
            Itxt::new("Source".into(), "s".repeat(700)).with_compressed(true),
        ])
        .with_unknowns(vec![
            UnknownChunk::new(*b"prVt", vec![1, 2, 3], false),
            UnknownChunk::new(*b"enDB", vec![9; 40], true),
            UnknownChunk::new(*b"afTr", vec![5; 4], true),
        ])
}

fn encode_file(image: &PngImage, meta: PngMetadata) -> Vec<u8> {
    oxideav_png::encode(image, &EncodeOptions::new().with_metadata(meta)).unwrap()
}

fn blob_data(frame: &VideoFrame, name: &str) -> Vec<Vec<u8>> {
    frame
        .blobs()
        .into_iter()
        .filter(|b| b.is(&kind(name)))
        .map(|b| b.data)
        .collect()
}

/// `decode → encode` through the framework keeps every chunk of a file
/// with the full set, for each IHDR colour type.
#[test]
fn every_chunk_survives_decode_then_encode_on_every_colour_type() {
    let palette = Palette::new(vec![
        [0, 0, 0, 255],
        [255, 0, 0, 128],
        [0, 255, 0, 255],
        [0, 0, 255, 255],
    ]);
    let cases: Vec<(PngImage, PngMetadata, PixelFormat)> = vec![
        (
            PngImage::packed(W, H, PngPixelFormat::Gray8, W as usize, plane(1)).unwrap(),
            common_metadata()
                .with_sbit(Sbit::Grayscale(5))
                .with_bkgd(Bkgd::Grayscale(200))
                .with_trns(Trns::Grayscale(17))
                .with_gama(Gama::new(55_000))
                .with_chrm(Chrm::new(
                    (31270, 32900),
                    (68000, 32000),
                    (26500, 69000),
                    (15000, 6000),
                )),
            PixelFormat::Gray8,
        ),
        (
            PngImage::packed(W, H, PngPixelFormat::Rgb24, W as usize * 3, plane(3)).unwrap(),
            common_metadata()
                .with_sbit(Sbit::Rgb(5, 6, 5))
                .with_bkgd(Bkgd::Rgb(1, 2, 3))
                .with_trns(Trns::Rgb(10, 20, 30))
                .with_srgb(Srgb::new(RenderingIntent::Saturation))
                .with_gama(Gama::SRGB)
                .with_chrm(Chrm::SRGB),
            PixelFormat::Rgb24,
        ),
        (
            PngImage::packed(W, H, PngPixelFormat::Rgb48Le, W as usize * 6, plane(6)).unwrap(),
            common_metadata()
                .with_sbit(Sbit::Rgb(12, 12, 12))
                .with_bkgd(Bkgd::Rgb(60000, 2, 3))
                .with_trns(Trns::Rgb(1000, 2000, 65535))
                .with_cicp(Cicp::new(9, 16, 0, 1)),
            PixelFormat::Rgb48Le,
        ),
        (
            PngImage::packed(
                W,
                H,
                PngPixelFormat::Pal8,
                W as usize,
                (0..W * H).map(|i| (i % 4) as u8).collect(),
            )
            .unwrap()
            .with_palette(palette),
            common_metadata()
                .with_sbit(Sbit::Rgb(8, 7, 6))
                .with_bkgd(Bkgd::Palette(3))
                .with_hist(Hist::new(vec![6, 6, 6, 6]))
                .with_gama(Gama::new(45_455)),
            PixelFormat::Pal8,
        ),
        (
            PngImage::packed(W, H, PngPixelFormat::Ya8, W as usize * 2, plane(2)).unwrap(),
            common_metadata()
                .with_sbit(Sbit::GrayscaleAlpha(8, 4))
                .with_bkgd(Bkgd::Grayscale(7)),
            PixelFormat::Ya8,
        ),
        (
            PngImage::packed(W, H, PngPixelFormat::Rgba64Le, W as usize * 8, plane(8)).unwrap(),
            common_metadata()
                .with_sbit(Sbit::Rgba(10, 10, 10, 16))
                .with_bkgd(Bkgd::Rgb(1, 2, 65535)),
            PixelFormat::Rgba64Le,
        ),
    ];
    for (image, meta, pix) in cases {
        let file = encode_file(&image, meta);
        let before = parse_metadata(&file).unwrap();
        let frame = trait_decode(&file);
        let out = trait_encode(&params(pix), frame);
        let after = parse_metadata(&out).unwrap();
        // The one planned difference: an unsafe-to-copy unknown chunk
        // (`enDB`: upper-case fourth letter) is not written back.
        let mut expected = before.clone();
        expected.unknowns.retain(|u| u.is_safe_to_copy());
        assert_eq!(expected.unknowns.len(), 2, "{pix:?}");
        assert!(expected.unknowns[1].after_idat);
        // A palette `tRNS` comes back through the palette-alpha record.
        if pix == PixelFormat::Pal8 {
            assert!(matches!(after.trns, Some(Trns::Palette(_))));
            assert_eq!(after.trns, before.trns);
        }
        // sRGB files keep their intent; the cICP file keeps its points.
        assert_eq!(after.srgb, before.srgb, "{pix:?}");
        assert_eq!(after.cicp, before.cicp, "{pix:?}");
        assert_eq!(after, expected, "{pix:?}");
        // And the pixels.
        assert_eq!(
            oxideav_png::decode(&out).unwrap(),
            oxideav_png::decode(&file).unwrap(),
            "{pix:?}"
        );
    }
}

/// The payload of each record is the chunk body as stored.
#[test]
fn decoder_attaches_one_record_per_chunk_with_the_stored_body() {
    let image = PngImage::packed(W, H, PngPixelFormat::Rgb24, W as usize * 3, plane(3)).unwrap();
    let meta = common_metadata()
        .with_sbit(Sbit::Rgb(5, 6, 5))
        .with_bkgd(Bkgd::Rgb(1, 2, 3))
        .with_trns(Trns::Rgb(10, 20, 30))
        .with_gama(Gama::new(50_000));
    let file = encode_file(&image, meta.clone());
    let frame = trait_decode(&file);
    assert_eq!(blob_data(&frame, "png-sbit"), vec![vec![5, 6, 5]]);
    assert_eq!(
        blob_data(&frame, "png-phys"),
        vec![meta.phys.unwrap().to_bytes().to_vec()]
    );
    assert_eq!(
        blob_data(&frame, "png-time"),
        vec![vec![0x07, 0xEA, 11, 15, 9, 30, 59]]
    );
    assert_eq!(blob_data(&frame, "png-bkgd"), vec![vec![0, 1, 0, 2, 0, 3]]);
    assert_eq!(
        blob_data(&frame, "png-trns"),
        vec![vec![0, 10, 0, 20, 0, 30]]
    );
    assert_eq!(
        blob_data(&frame, "png-gama"),
        vec![50_000u32.to_be_bytes().to_vec()]
    );
    assert_eq!(blob_data(&frame, "png-mdcv").len(), 1);
    assert_eq!(blob_data(&frame, "png-mdcv")[0].len(), 24);
    assert_eq!(blob_data(&frame, "png-clli")[0].len(), 8);
    assert_eq!(blob_data(&frame, "png-splt").len(), 2);
    assert_eq!(
        blob_data(&frame, "png-text"),
        vec![b"Title\0A picture".to_vec(), b"Comment\0caf\xE9".to_vec()]
    );
    assert_eq!(blob_data(&frame, "png-ztxt").len(), 1);
    assert!(blob_data(&frame, "png-ztxt")[0].starts_with(b"Description\0\0"));
    assert_eq!(blob_data(&frame, "png-itxt").len(), 2);
    assert_eq!(
        blob_data(&frame, "png-chunk"),
        vec![b"prVt\x01\x02\x03".to_vec()]
    );
    let mut after = b"enDB".to_vec();
    after.extend_from_slice(&[9; 40]);
    assert_eq!(
        blob_data(&frame, "png-chunk-after-idat"),
        vec![after, b"afTr\x05\x05\x05\x05".to_vec()]
    );
    // Every kind is spelled by the framework's `<producer>-<chunk>` rule.
    for b in frame.blobs() {
        assert!(b.kind.follows_naming_rule(), "{}", b.kind);
        if !b.kind.is_well_known() {
            assert_eq!(b.kind.producer(), Some("png"), "{}", b.kind);
        }
    }
    // `metadata_blobs` builds the same records from a `PngMetadata`.
    let built = oxideav_png::metadata_blobs(&parse_metadata(&file).unwrap()).unwrap();
    let mut from_frame: Vec<MetadataBlob> = frame
        .blobs()
        .into_iter()
        .filter(|b| !b.kind.is_well_known())
        .collect();
    let mut built_sorted = built.clone();
    // zTXt / compressed iTXt bodies are re-deflated by the builder, at
    // the level the encoder wrote them with, so they compare equal too.
    from_frame.sort_by(|a, b| (a.kind.as_str(), &a.data).cmp(&(b.kind.as_str(), &b.data)));
    built_sorted.sort_by(|a, b| (a.kind.as_str(), &a.data).cmp(&(b.kind.as_str(), &b.data)));
    assert_eq!(from_frame, built_sorted);
}

/// The XMP packet and the two blob-kind chunks are not doubled as
/// format-native records.
#[test]
fn well_known_kinds_are_not_repeated_as_native_records() {
    let image = PngImage::from_rgb8(W, H, plane(3))
        .unwrap()
        .with_metadata(
            oxideav_png::Metadata::new()
                .with_icc(vec![7u8; 300])
                .with_exif(b"MM\0*\0\0\0\x08\0\0\0\0".to_vec())
                .with_xmp(b"<x:xmpmeta/>".to_vec()),
        )
        .with_color(ColorInfo::png_default());
    let file = oxideav_png::encode(&image, &EncodeOptions::new()).unwrap();
    let frame = trait_decode(&file);
    let kinds: Vec<String> = frame
        .blobs()
        .iter()
        .map(|b| b.kind.as_str().to_string())
        .collect();
    assert_eq!(kinds, ["icc", "exif", "xmp"]);
    let out = trait_encode(&params(PixelFormat::Rgb24), frame);
    let meta = parse_metadata(&out).unwrap();
    assert_eq!(meta.itxts.len(), 1, "one XMP iTXt, not two");
    assert!(meta.iccp.is_some() && meta.exif.is_some());
}

/// Records that do not fit the output picture are dropped; the encode
/// succeeds.
#[test]
fn records_that_no_longer_fit_the_output_are_dropped() {
    // Records of an RGB file …
    let image = PngImage::packed(W, H, PngPixelFormat::Rgb24, W as usize * 3, plane(3)).unwrap();
    let meta = PngMetadata::default()
        .with_sbit(Sbit::Rgb(5, 6, 5))
        .with_bkgd(Bkgd::Rgb(1, 2, 3))
        .with_trns(Trns::Rgb(10, 20, 30))
        .with_phys(Phys::new(1, 2, PhysUnit::Unknown))
        .with_srgb(Srgb::new(RenderingIntent::AbsoluteColorimetric))
        .with_gama(Gama::SRGB);
    let blobs = trait_decode(&encode_file(&image, meta)).blobs();
    // … on a grey + alpha frame with no colour signal.
    let frame = VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: W as usize * 2,
            data: plane(2),
        }],
    }
    .with_blobs(blobs.clone());
    let out = trait_encode(&params(PixelFormat::Ya8), frame);
    let after = parse_metadata(&out).unwrap();
    assert_eq!(
        after.sbit, None,
        "three-channel sBIT on a two-channel image"
    );
    assert_eq!(after.bkgd, None, "RGB bKGD on a greyscale image");
    assert_eq!(
        after.trns, None,
        "tRNS is prohibited beside an alpha channel"
    );
    assert_eq!(after.srgb, None, "no sRGB signal on the frame");
    assert_eq!(after.phys, Some(Phys::new(1, 2, PhysUnit::Unknown)));
    assert_eq!(after.gama, Some(Gama::SRGB), "gAMA stands alone");

    // A 16-bit record on an 8-bit image: out-of-range samples.
    let frame = VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: W as usize * 3,
            data: plane(3),
        }],
    }
    .with_blob(MetadataBlob::new(kind("png-bkgd"), vec![1, 0, 0, 0, 0, 0]))
    .with_blob(MetadataBlob::new(kind("png-sbit"), vec![16, 16, 16]))
    .with_blob(MetadataBlob::new(kind("png-time"), vec![0; 7]))
    .with_blob(MetadataBlob::new(kind("png-phys"), vec![1, 2, 3]))
    .with_blob(MetadataBlob::new(kind("png-hist"), vec![0, 1]))
    .with_blob(MetadataBlob::new(
        kind("png-text"),
        b"no separator".to_vec(),
    ))
    .with_blob(MetadataBlob::new(
        kind("png-ztxt"),
        b"k\0\0not zlib".to_vec(),
    ))
    .with_blob(MetadataBlob::new(kind("png-chunk"), b"IDAT".to_vec()))
    .with_blob(MetadataBlob::new(kind("png-chunk"), b"tEXtk\0v".to_vec()))
    .with_blob(MetadataBlob::new(kind("png-chunk"), b"ab".to_vec()))
    .with_blob(MetadataBlob::new(kind("png-chunk"), b"a1cd".to_vec()));
    let out = trait_encode(&params(PixelFormat::Rgb24), frame);
    assert!(parse_metadata(&out).unwrap().is_empty());
    oxideav_png::decode(&out).unwrap();
}

/// A frame's colour signal decides the colour chunk; the `gAMA` /
/// `cHRM` records ride along only where the staged text allows them.
#[test]
fn colour_records_follow_the_frame_colour_signal() {
    let rgb = || VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: W as usize * 3,
            data: plane(3),
        }],
    };
    let srgb_signal = oxideav_png::to_color_signal(&ColorInfo::srgb());
    let gama = |v: u32| MetadataBlob::new(kind("png-gama"), v.to_be_bytes().to_vec());

    // sRGB signal + a contradicting gAMA record: sRGB wins, gAMA goes.
    let frame = rgb()
        .with_color_signal(srgb_signal)
        .with_blob(gama(100_000))
        .with_blob(MetadataBlob::new(kind("png-srgb"), vec![1]));
    let m = parse_metadata(&trait_encode(&params(PixelFormat::Rgb24), frame)).unwrap();
    assert_eq!(
        m.srgb,
        Some(Srgb::new(RenderingIntent::RelativeColorimetric))
    );
    assert_eq!(m.gama, None);

    // sRGB signal + the Table 17 companions: all three are written.
    let frame = rgb()
        .with_color_signal(srgb_signal)
        .with_blob(gama(45_455))
        .with_blob(MetadataBlob::new(
            kind("png-chrm"),
            Chrm::SRGB.to_bytes().to_vec(),
        ));
    let m = parse_metadata(&trait_encode(&params(PixelFormat::Rgb24), frame)).unwrap();
    assert_eq!(m.srgb, Some(Srgb::new(RenderingIntent::Perceptual)));
    assert_eq!((m.gama, m.chrm), (Some(Gama::SRGB), Some(Chrm::SRGB)));

    // A BT.2020 PQ signal: cICP, with the records as its fallback.
    let pq =
        oxideav_png::to_color_signal(&ColorInfo::png_default().with_primaries(9).with_transfer(16));
    let frame = rgb()
        .with_color_signal(pq)
        .with_blob(gama(100_000))
        .with_blob(MetadataBlob::new(kind("png-srgb"), vec![0]));
    let m = parse_metadata(&trait_encode(&params(PixelFormat::Rgb24), frame)).unwrap();
    assert_eq!(m.cicp, Some(Cicp::new(9, 16, 0, 1)));
    assert_eq!(m.srgb, None, "an sRGB record does not override the signal");
    assert_eq!(m.gama, Some(Gama::new(100_000)));
}

/// A file described by `gAMA` + `cHRM` alone decodes with the primaries
/// its `cHRM` names; re-encoding the frame must not add a `cICP` (which
/// would outrank the pair and say nothing about the transfer).
#[test]
fn a_gama_chrm_file_is_not_given_a_cicp() {
    let image = PngImage::packed(W, H, PngPixelFormat::Rgb24, W as usize * 3, plane(3)).unwrap();
    for chrm in [
        Chrm::SRGB,
        // BT.2020 primaries, D65.
        Chrm::new(
            (31270, 32900),
            (70800, 29200),
            (17000, 79700),
            (13100, 4600),
        ),
        // Primaries no code point names.
        Chrm::new(
            (31270, 32900),
            (68000, 32000),
            (26500, 69000),
            (15000, 6000),
        ),
    ] {
        let meta = PngMetadata::default()
            .with_gama(Gama::new(45_455))
            .with_chrm(chrm);
        let file = encode_file(&image, meta.clone());
        let out = trait_encode(&params(PixelFormat::Rgb24), trait_decode(&file));
        assert_eq!(parse_metadata(&out).unwrap(), meta);
    }
    // A signal that says more than the record (a transfer) is written.
    let frame = trait_decode(&encode_file(
        &image,
        PngMetadata::default().with_chrm(Chrm::SRGB),
    ))
    .with_color_signal(oxideav_png::to_color_signal(
        &ColorInfo::png_default().with_primaries(1).with_transfer(16),
    ));
    let m = parse_metadata(&trait_encode(&params(PixelFormat::Rgb24), frame)).unwrap();
    assert_eq!(m.cicp, Some(Cicp::new(1, 16, 0, 1)));
    assert_eq!(m.chrm, Some(Chrm::SRGB));
}

/// A frame's records of a kind replace the stream's of that kind; the
/// stream fills the kinds the frame leaves out.
#[test]
fn frame_records_take_the_place_of_stream_records_per_kind() {
    let text = |s: &str| MetadataBlob::new(kind("png-text"), s.as_bytes().to_vec());
    let phys = |x: u32| {
        MetadataBlob::new(
            kind("png-phys"),
            Phys::new(x, x, PhysUnit::Metre).to_bytes().to_vec(),
        )
    };
    let p = params(PixelFormat::Rgb24)
        .with_blob(text("Title\0stream"))
        .with_blob(text("Comment\0stream"))
        .with_blob(phys(100));
    let frame = VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: W as usize * 3,
            data: plane(3),
        }],
    };
    let m = parse_metadata(&trait_encode(&p, frame.clone())).unwrap();
    assert_eq!(m.texts.len(), 2);
    assert_eq!(m.phys, Some(Phys::new(100, 100, PhysUnit::Metre)));
    let m = parse_metadata(&trait_encode(&p, frame.with_blob(text("Title\0frame")))).unwrap();
    assert_eq!(m.texts, vec![Text::new("Title".into(), "frame".into())]);
    assert_eq!(m.phys, Some(Phys::new(100, 100, PhysUnit::Metre)));
}

/// The demuxer puts the file's records on the stream, and the codec
/// advertises every kind it embeds.
#[test]
fn demuxer_stream_and_capabilities_carry_the_native_kinds() {
    let image = PngImage::packed(W, H, PngPixelFormat::Rgb24, W as usize * 3, plane(3)).unwrap();
    let file = encode_file(&image, common_metadata().with_sbit(Sbit::Rgb(5, 6, 5)));
    let mut ctx = RuntimeContext::new();
    oxideav_png::register(&mut ctx);
    let demux = ctx
        .containers
        .open_demuxer("png", Box::new(Cursor::new(file.clone())), &ctx.codecs)
        .unwrap();
    let stream_kinds: Vec<String> = demux.streams()[0]
        .params
        .blobs()
        .iter()
        .map(|b| b.kind.as_str().to_string())
        .collect();
    let frame_kinds: Vec<String> = trait_decode(&file)
        .blobs()
        .iter()
        .map(|b| b.kind.as_str().to_string())
        .collect();
    assert_eq!(stream_kinds, frame_kinds);
    assert!(stream_kinds.contains(&"png-sbit".to_string()));
    assert!(stream_kinds.contains(&"png-chunk-after-idat".to_string()));

    let id = CodecId::new("png");
    let kinds: Vec<BlobKind> = ctx
        .codecs
        .all_implementations()
        .filter(|(i, _)| **i == id)
        .flat_map(|(_, imp)| imp.caps.embedded_blob_kinds.clone())
        .collect();
    for k in frame_kinds {
        assert!(kinds.contains(&kind(&k)), "{k} not advertised");
    }
}

/// `From<PngImage>` has no chunk list: it carries what the image
/// holds, and `from_video_frame` reads it back.
#[test]
fn image_conversions_carry_gamma_and_keyed_transparency() {
    let image = PngImage::packed(W, H, PngPixelFormat::Rgb48Le, W as usize * 6, plane(6))
        .unwrap()
        .with_transparency(Trns::Rgb(1, 2, 65535))
        .with_metadata(oxideav_png::Metadata::new().with_gamma(0.5));
    let frame: VideoFrame = (&image).into();
    assert_eq!(
        blob_data(&frame, "png-trns"),
        vec![vec![0, 1, 0, 2, 255, 255]]
    );
    assert_eq!(blob_data(&frame, "png-gama").len(), 1);
    let back = PngImage::from_video_frame(&frame, &{
        let mut p = params(PixelFormat::Rgb48Le);
        p.pixel_format = Some(PixelFormat::Rgb48Le);
        p
    })
    .unwrap();
    assert_eq!(back, image);
}
