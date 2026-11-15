//! The PNG / APNG container carries what a file says about itself:
//! text chunks are the demuxer's string metadata and the muxer writes
//! `set_metadata` pairs back as text chunks; an animated file's
//! ancillary chunks survive `demux → mux`, and reach every decoded
//! frame.
#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use oxideav_core::{Frame, Packet, RuntimeContext, StreamInfo, VideoFrame};
use oxideav_png::{
    ApngFrameSpec, Bkgd, Cicp, Clli, EncodeOptions, Gama, Itxt, Mdcv, Metadata, Phys, PhysUnit,
    PngImage, PngMetadata, Sbit, Splt, SpltEntry, Text, Time, UnknownChunk, Ztxt, parse_metadata,
};

struct SharedWriter(Arc<Mutex<Cursor<Vec<u8>>>>);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

impl Seek for SharedWriter {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}

fn context() -> RuntimeContext {
    let mut ctx = RuntimeContext::new();
    oxideav_png::register(&mut ctx);
    ctx
}

fn demux(ctx: &RuntimeContext, file: &[u8]) -> (StreamInfo, Vec<(String, String)>, Vec<Packet>) {
    let mut dm = ctx
        .containers
        .open_demuxer("png", Box::new(Cursor::new(file.to_vec())), &ctx.codecs)
        .unwrap();
    let stream = dm.streams()[0].clone();
    let meta = dm.metadata().to_vec();
    let mut pkts = Vec::new();
    while let Ok(p) = dm.next_packet() {
        pkts.push(p);
    }
    (stream, meta, pkts)
}

fn mux(
    ctx: &RuntimeContext,
    stream: &StreamInfo,
    metadata: Option<&[(String, String)]>,
    packets: &[Packet],
) -> Vec<u8> {
    let shared = Arc::new(Mutex::new(Cursor::new(Vec::new())));
    let mut m = ctx
        .containers
        .open_muxer(
            "png",
            Box::new(SharedWriter(Arc::clone(&shared))),
            std::slice::from_ref(stream),
        )
        .unwrap();
    if let Some(md) = metadata {
        m.set_metadata(md).unwrap();
    }
    m.write_header().unwrap();
    for p in packets {
        m.write_packet(p).unwrap();
    }
    m.write_trailer().unwrap();
    drop(m);
    Arc::try_unwrap(shared)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap()
        .into_inner()
}

fn pair(k: &str, v: &str) -> (String, String) {
    (k.to_string(), v.to_string())
}

fn rgb(w: u32, h: u32, seed: u8) -> PngImage {
    PngImage::from_rgb8(
        w,
        h,
        (0..w * h * 3)
            .map(|i| (i as u8).wrapping_mul(7) ^ seed)
            .collect(),
    )
    .unwrap()
}

fn text_metadata() -> PngMetadata {
    PngMetadata::default()
        .with_texts(vec![
            Text::new("Title".into(), "A still".into()),
            Text::new("Camera".into(), "none".into()),
            Text::new("Comment".into(), "first".into()),
            Text::new("Comment".into(), "second".into()),
        ])
        .with_ztxts(vec![Ztxt::new("Description".into(), "d".repeat(2000))])
        .with_itxts(vec![
            Itxt::new("Author".into(), "\u{540d}\u{524d}".into()).with_language_tag("ja".into()),
        ])
}

fn full_metadata() -> PngMetadata {
    text_metadata()
        .with_cicp(Cicp::new(9, 16, 0, 1))
        .with_gama(Gama::new(45_455))
        .with_mdcv(Mdcv::new(
            [(35400, 14600), (8500, 39850), (6550, 2300)],
            (15635, 16450),
            10_000_000,
            50,
        ))
        .with_clli(Clli::new(10_000_000, 2_500_000))
        .with_sbit(Sbit::Rgb(5, 6, 5))
        .with_phys(Phys::new(2835, 2835, PhysUnit::Metre))
        .with_time(Time::new(2026, 11, 15, 1, 2, 3))
        .with_bkgd(Bkgd::Rgb(1, 2, 3))
        .with_splt(vec![Splt::new(
            "p".into(),
            8,
            vec![SpltEntry::new(1, 2, 3, 4, 5)],
        )])
        .with_unknowns(vec![
            UnknownChunk::new(*b"prVt", vec![1, 2, 3], false),
            UnknownChunk::new(*b"unSF", vec![4, 5], false),
            UnknownChunk::new(*b"afTr", vec![6], true),
        ])
}

#[test]
fn demuxer_reports_text_chunks_as_pairs_in_file_order() {
    let ctx = context();
    let file = oxideav_png::encode(
        &rgb(5, 4, 0).with_metadata(Metadata::new().with_xmp(b"<x:xmpmeta/>".to_vec())),
        &EncodeOptions::new().with_metadata(text_metadata()),
    )
    .unwrap();
    let (_, meta, _) = demux(&ctx, &file);
    assert_eq!(
        meta,
        vec![
            pair("title", "A still"),
            pair("Camera", "none"),
            pair("comment", "first"),
            pair("comment", "second"),
            pair("description", &"d".repeat(2000)),
            pair("artist", "\u{540d}\u{524d}"),
        ],
        "the XMP packet is a blob, not a pair"
    );
}

#[test]
fn remux_with_the_demuxed_pairs_is_byte_identical() {
    let ctx = context();
    let file = oxideav_png::encode(
        &rgb(5, 4, 1),
        &EncodeOptions::new().with_metadata(full_metadata()),
    )
    .unwrap();
    let (stream, meta, pkts) = demux(&ctx, &file);
    assert_eq!(mux(&ctx, &stream, Some(&meta), &pkts), file);
    assert_eq!(mux(&ctx, &stream, None, &pkts), file);
}

#[test]
fn set_metadata_pairs_become_text_chunks() {
    let ctx = context();
    let file = oxideav_png::encode(&rgb(5, 4, 2), &EncodeOptions::new()).unwrap();
    let (stream, _, pkts) = demux(&ctx, &file);
    let long = "long ".repeat(400);
    let pairs = vec![
        pair("title", "Plain"),
        pair("artist", "Zo\u{eb}"),
        pair("description", &long),
        pair("comment", "\u{1f600} not Latin-1"),
        pair("My Key", "kept under its own keyword"),
        pair("two  spaces", "no keyword: dropped"),
        pair("date", "nul\0inside: dropped"),
    ];
    let out = mux(&ctx, &stream, Some(&pairs), &pkts);
    let m = parse_metadata(&out).unwrap();
    assert_eq!(
        m.texts,
        vec![
            Text::new("Title".into(), "Plain".into()),
            Text::new("Author".into(), "Zo\u{eb}".into()),
            Text::new("My Key".into(), "kept under its own keyword".into()),
        ]
    );
    assert_eq!(m.ztxts, vec![Ztxt::new("Description".into(), long.clone())]);
    assert_eq!(m.itxts.len(), 1);
    assert_eq!(m.itxts[0].keyword, "Comment");
    assert_eq!(m.itxts[0].text, "\u{1f600} not Latin-1");
    // Pixels untouched, and the pairs read back.
    assert_eq!(
        oxideav_png::decode(&out).unwrap().as_bytes(),
        oxideav_png::decode(&file).unwrap().as_bytes()
    );
    let (_, back, _) = demux(&ctx, &out);
    assert_eq!(
        back,
        vec![
            pair("title", "Plain"),
            pair("artist", "Zo\u{eb}"),
            pair("description", &long),
            pair("comment", "\u{1f600} not Latin-1"),
            pair("My Key", "kept under its own keyword"),
        ],
        "in the order handed in"
    );
}

#[test]
fn a_named_key_is_decided_by_the_pairs_and_other_keys_are_left_alone() {
    let ctx = context();
    let file = oxideav_png::encode(
        &rgb(5, 4, 3),
        &EncodeOptions::new().with_metadata(text_metadata()),
    )
    .unwrap();
    let (stream, _, pkts) = demux(&ctx, &file);
    // Change the title, keep one of the two comments, add a key.
    let pairs = vec![
        pair("title", "Renamed"),
        pair("comment", "second"),
        pair("source", "a test"),
    ];
    let out = mux(&ctx, &stream, Some(&pairs), &pkts);
    let (_, back, _) = demux(&ctx, &out);
    assert_eq!(
        back,
        vec![
            pair("Camera", "none"),
            pair("comment", "second"),
            pair("description", &"d".repeat(2000)),
            pair("artist", "\u{540d}\u{524d}"),
            pair("title", "Renamed"),
            pair("source", "a test"),
        ],
        "kept chunks stay in place, new ones go ahead of the image data"
    );
    // The untouched international chunk keeps its language tag.
    let m = parse_metadata(&out).unwrap();
    assert_eq!(m.itxts[0].language_tag, "ja");
}

fn animation(meta: PngMetadata) -> Vec<u8> {
    let frames = vec![
        ApngFrameSpec::full_canvas(rgb(6, 5, 10), 5),
        ApngFrameSpec::new(rgb(3, 2, 20))
            .with_x_offset(2)
            .with_y_offset(1)
            .with_delay_num(7)
            .with_delay_den(100),
        ApngFrameSpec::full_canvas(rgb(6, 5, 30), 9),
    ];
    oxideav_png::encode_apng_frames_with_options(
        6,
        5,
        None,
        &frames,
        3,
        &EncodeOptions::new().with_metadata(meta),
    )
    .unwrap()
}

#[test]
fn animated_remux_keeps_every_ancillary_chunk() {
    let ctx = context();
    let file = animation(full_metadata());
    let before = parse_metadata(&file).unwrap();
    assert!(before.cicp.is_some() && before.unknowns.len() == 3);
    let (stream, meta, pkts) = demux(&ctx, &file);
    assert_eq!(pkts.len(), 3);
    let out = mux(&ctx, &stream, Some(&meta), &pkts);
    assert_eq!(parse_metadata(&out).unwrap(), before);
    assert_eq!(
        oxideav_png::parse_chunk_order(&out).unwrap(),
        oxideav_png::parse_chunk_order(&file).unwrap(),
        "nothing moved"
    );
    let (a, b) = (
        oxideav_png::decode_apng(&out).unwrap(),
        oxideav_png::decode_apng(&file).unwrap(),
    );
    assert_eq!(a.num_plays, 3);
    assert_eq!(a.frames.len(), b.frames.len());
    for (x, y) in a.frames.iter().zip(&b.frames) {
        assert_eq!(x.image, y.image);
        assert_eq!(x.delay_cs, y.delay_cs);
    }
    // And a second generation is stable byte for byte.
    let (stream2, meta2, pkts2) = demux(&ctx, &out);
    assert_eq!(mux(&ctx, &stream2, Some(&meta2), &pkts2), out);
}

#[test]
fn every_decoded_animation_frame_carries_the_file_records() {
    let ctx = context();
    let image0 = rgb(6, 5, 10).with_metadata(
        Metadata::new()
            .with_icc(vec![3u8; 200])
            .with_exif(b"II*\0\x08\0\0\0\0\0\0\0".to_vec()),
    );
    let frames = vec![
        ApngFrameSpec::full_canvas(image0, 5),
        ApngFrameSpec::full_canvas(rgb(6, 5, 20), 5),
        ApngFrameSpec::full_canvas(rgb(6, 5, 30), 5),
    ];
    let meta = PngMetadata::default()
        .with_phys(Phys::new(10, 20, PhysUnit::Unknown))
        .with_texts(vec![Text::new("Title".into(), "anim".into())]);
    let file = oxideav_png::encode_apng_frames_with_options(
        6,
        5,
        None,
        &frames,
        0,
        &EncodeOptions::new().with_metadata(meta),
    )
    .unwrap();
    let (stream, _, pkts) = demux(&ctx, &file);
    let mut dec = oxideav_png::make_decoder(&stream.params).unwrap();
    let mut seen: Vec<VideoFrame> = Vec::new();
    for p in &pkts {
        dec.send_packet(p).unwrap();
        match dec.receive_frame().unwrap() {
            Frame::Video(v) => seen.push(v),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(seen.len(), 3);
    let kinds = |f: &VideoFrame| -> Vec<String> {
        f.blobs()
            .iter()
            .map(|b| b.kind.as_str().to_string())
            .collect()
    };
    assert_eq!(kinds(&seen[0]), ["icc", "exif", "png-phys", "png-text"]);
    for f in &seen[1..] {
        assert_eq!(f.blobs(), seen[0].blobs());
        assert_eq!(f.color_signal(), seen[0].color_signal());
    }
}

/// `demux → decode → encode → mux` with the demuxed pairs handed to the
/// muxer: the text the encoder already wrote from the frame records is
/// not doubled.
#[test]
fn transcode_does_not_double_the_text() {
    let ctx = context();
    let file = oxideav_png::encode(
        &rgb(5, 4, 4),
        &EncodeOptions::new().with_metadata(full_metadata()),
    )
    .unwrap();
    let (stream, meta, pkts) = demux(&ctx, &file);
    let mut dec = oxideav_png::make_decoder(&stream.params).unwrap();
    dec.send_packet(&pkts[0]).unwrap();
    let frame = dec.receive_frame().unwrap();
    let mut enc = oxideav_png::make_encoder(&stream.params).unwrap();
    enc.send_frame(&frame).unwrap();
    enc.flush().unwrap();
    let pkt = enc.receive_packet().unwrap();
    let out = mux(&ctx, &stream, Some(&meta), &[pkt]);
    let (before, after) = (
        parse_metadata(&file).unwrap(),
        parse_metadata(&out).unwrap(),
    );
    assert_eq!(after.texts, before.texts);
    assert_eq!(after.ztxts, before.ztxts);
    assert_eq!(after.itxts, before.itxts);
    assert_eq!(after.phys, before.phys);
    assert_eq!(after.cicp, before.cicp);
    // `unSF` is unsafe to copy and the pixels were re-encoded.
    assert_eq!(after.unknowns.len(), 2);
}
