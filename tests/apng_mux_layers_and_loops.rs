//! The PNG muxer writes what the stream says: the `acTL` `num_plays`
//! from the stream's `loop_count` option (W3C PNG3 §11.3.6.1: the number
//! of plays, `0` = indefinitely) or a `("loop_count", n)` pair handed to
//! `Muxer::set_metadata`, and — for the demuxer's frame packets — every
//! frame's `fcTL` region offset, `dispose_op` and `blend_op`, so
//! `mux(demux(file))` renders the same pictures as the file.
#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use oxideav_core::{
    CodecId, CodecParameters, Frame, Packet, PixelFormat, RuntimeContext, StreamInfo, TimeBase,
    VideoFrame, VideoPlane,
};
use oxideav_png::apng::{Blend, Disposal};
use oxideav_png::chunk::write_chunk;
use oxideav_png::decoder::parse_apng;

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

fn zlib(raw: &[u8]) -> Vec<u8> {
    compcol::vec::compress_to_vec_with::<compcol::zlib::Zlib>(
        raw,
        compcol::zlib::EncoderConfig { level: 6 },
    )
    .unwrap()
}

/// RGBA8 frame data: `w × h` pixels of `px`, filter None.
fn frame_z(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
    let mut row = vec![0u8];
    row.extend(px.repeat(w as usize));
    zlib(&row.repeat(h as usize))
}

/// `(x, y, w, h, dispose, blend, pixel)` per frame.
type Spec = (u32, u32, u32, u32, u8, u8, [u8; 4]);

/// An 8×6 RGBA APNG of `frames`, `plays` plays. With `hidden_default`
/// the IDAT is a default image outside the animation and every frame
/// is an `fdAT` frame; otherwise frame 0 (which must cover the canvas)
/// is the default image.
fn apng(frames: &[Spec], plays: u32, hidden_default: bool) -> Vec<u8> {
    let mut out = vec![137, 80, 78, 71, 13, 10, 26, 10];
    let mut ihdr = 8u32.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&6u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    write_chunk(&mut out, b"IHDR", &ihdr);
    let mut actl = (frames.len() as u32).to_be_bytes().to_vec();
    actl.extend_from_slice(&plays.to_be_bytes());
    write_chunk(&mut out, b"acTL", &actl);
    if hidden_default {
        write_chunk(&mut out, b"IDAT", &frame_z(8, 6, [9, 9, 9, 255]));
    }
    let mut seq = 0u32;
    for (i, &(x, y, w, h, d, b, px)) in frames.iter().enumerate() {
        let mut f = seq.to_be_bytes().to_vec();
        for v in [w, h, x, y] {
            f.extend_from_slice(&v.to_be_bytes());
        }
        f.extend_from_slice(&3u16.to_be_bytes());
        f.extend_from_slice(&100u16.to_be_bytes());
        f.extend_from_slice(&[d, b]);
        write_chunk(&mut out, b"fcTL", &f);
        seq += 1;
        if i == 0 && !hidden_default {
            write_chunk(&mut out, b"IDAT", &frame_z(w, h, px));
        } else {
            let mut fd = seq.to_be_bytes().to_vec();
            fd.extend_from_slice(&frame_z(w, h, px));
            write_chunk(&mut out, b"fdAT", &fd);
            seq += 1;
        }
    }
    write_chunk(&mut out, b"IEND", &[]);
    out
}

const LAYERS: &[Spec] = &[
    (0, 0, 8, 6, 0, 0, [0, 0, 255, 255]),
    (2, 1, 3, 2, 2, 1, [255, 0, 0, 128]),
    (4, 3, 4, 3, 1, 0, [0, 255, 0, 0]),
    (1, 0, 2, 6, 0, 1, [255, 255, 0, 200]),
];

/// Demux `file` into its stream, its metadata and its packets.
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

/// Mux `packets` on `stream`, `metadata` handed to `set_metadata` first.
fn mux(
    ctx: &RuntimeContext,
    stream: &StreamInfo,
    metadata: Option<&[(String, String)]>,
    packets: &[Packet],
) -> oxideav_core::Result<Vec<u8>> {
    let shared = Arc::new(Mutex::new(Cursor::new(Vec::new())));
    let mut m = ctx.containers.open_muxer(
        "png",
        Box::new(SharedWriter(Arc::clone(&shared))),
        std::slice::from_ref(stream),
    )?;
    if let Some(md) = metadata {
        m.set_metadata(md)?;
    }
    m.write_header()?;
    for p in packets {
        m.write_packet(p)?;
    }
    m.write_trailer()?;
    drop(m);
    Ok(Arc::try_unwrap(shared)
        .unwrap()
        .into_inner()
        .unwrap()
        .into_inner())
}

fn decoded(file: &[u8]) -> Vec<Vec<u8>> {
    oxideav_png::decode_apng(file)
        .unwrap()
        .frames
        .iter()
        .map(|f| f.image.as_bytes().unwrap().to_vec())
        .collect()
}

#[test]
fn remux_keeps_frame_offsets_dispose_and_blend() {
    let ctx = context();
    let file = apng(LAYERS, 0, false);
    let (stream, _, pkts) = demux(&ctx, &file);
    let out = mux(&ctx, &stream, None, &pkts).unwrap();
    let info = parse_apng(&out).unwrap();
    assert_eq!((info.ihdr.width, info.ihdr.height), (8, 6));
    assert_eq!(info.frames.len(), LAYERS.len());
    for (f, &(x, y, w, h, d, b, _)) in info.frames.iter().zip(LAYERS) {
        let fc = &f.fctl;
        assert_eq!(
            (fc.x_offset, fc.y_offset, fc.width, fc.height),
            (x, y, w, h)
        );
        assert_eq!(fc.dispose_op as u8, d);
        assert_eq!(fc.blend_op as u8, b);
        assert_eq!((fc.delay_num, fc.delay_den), (3, 100));
    }
    assert_eq!(decoded(&out), decoded(&file));
    // Before: every frame was rewritten to 0, 0 / NONE / SOURCE.
    assert!(info.frames[1].fctl.dispose_op == Disposal::Previous);
    assert!(info.frames[3].fctl.blend_op == Blend::Over);
}

#[test]
fn remux_of_a_hidden_default_image_animation_writes_a_hidden_default() {
    let ctx = context();
    let frames: &[Spec] = &[
        (1, 1, 3, 3, 0, 0, [10, 20, 30, 255]),
        (4, 2, 4, 4, 0, 1, [200, 0, 0, 100]),
    ];
    let file = apng(frames, 2, true);
    let (stream, meta, pkts) = demux(&ctx, &file);
    assert_eq!(pkts.len(), 2);
    let out = mux(&ctx, &stream, Some(&meta), &pkts).unwrap();
    let info = parse_apng(&out).unwrap();
    // The frame-0 region is not the canvas, so it cannot be the default
    // image: a default image outside the animation precedes it.
    assert!(!info.first_frame_is_default);
    assert_eq!(info.frames.len(), 2);
    assert_eq!(info.actl.num_plays, 2);
    assert_eq!(decoded(&out), decoded(&file));
}

#[test]
fn stream_loop_count_option_sets_num_plays() {
    let ctx = context();
    let file = apng(LAYERS, 0, false);
    let (mut stream, meta, pkts) = demux(&ctx, &file);
    assert_eq!(meta, vec![("loop_count".to_string(), "0".to_string())]);
    for plays in [0u32, 1, 3, 65_536] {
        stream.params.options = stream
            .params
            .options
            .clone()
            .set("loop_count", plays.to_string());
        let out = mux(&ctx, &stream, None, &pkts).unwrap();
        assert_eq!(parse_apng(&out).unwrap().actl.num_plays, plays);
        // …and the demuxer reports it back.
        let (_, meta, _) = demux(&ctx, &out);
        assert_eq!(meta, vec![("loop_count".to_string(), plays.to_string())]);
    }
}

#[test]
fn set_metadata_loop_count_sets_num_plays_and_the_option_wins() {
    let ctx = context();
    let file = apng(LAYERS, 5, false);
    let (stream, meta, pkts) = demux(&ctx, &file);
    // A plain remux keeps the source's plays through the metadata pair.
    let out = mux(&ctx, &stream, Some(&meta), &pkts).unwrap();
    assert_eq!(parse_apng(&out).unwrap().actl.num_plays, 5);
    // Without it the assembled animation loops forever.
    let out = mux(&ctx, &stream, None, &pkts).unwrap();
    assert_eq!(parse_apng(&out).unwrap().actl.num_plays, 0);
    // The stream option is the caller's word and wins.
    let mut s7 = stream.clone();
    s7.params.options = s7.params.options.clone().set("loop_count", "7");
    let out = mux(&ctx, &s7, Some(&meta), &pkts).unwrap();
    assert_eq!(parse_apng(&out).unwrap().actl.num_plays, 7);
    let m = ctx
        .containers
        .open_muxer(
            "png",
            Box::new(SharedWriter(Arc::new(Mutex::new(Cursor::new(Vec::new()))))),
            std::slice::from_ref(&stream),
        )
        .unwrap();
    assert!(m.carries_metadata_key("loop_count"));
    assert!(!m.carries_metadata_key("title"));
}

#[test]
fn bad_loop_count_is_rejected() {
    let ctx = context();
    let file = apng(LAYERS, 0, false);
    let (mut stream, _, pkts) = demux(&ctx, &file);
    stream.params.options = stream.params.options.clone().set("loop_count", "-1");
    assert!(mux(&ctx, &stream, None, &pkts).is_err());
    let (stream, _, _) = demux(&ctx, &file);
    assert!(
        mux(
            &ctx,
            &stream,
            Some(&[("loop_count".to_string(), "forever".to_string())]),
            &pkts
        )
        .is_err()
    );
}

fn encoder_params(loop_count: Option<&str>) -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new("png"));
    p.width = Some(4);
    p.height = Some(2);
    p.pixel_format = Some(PixelFormat::Rgba);
    if let Some(v) = loop_count {
        p.options = p.options.clone().set("loop_count", v);
    }
    p
}

/// Encode two RGBA frames through the registry encoder: one APNG packet.
fn encode_two(ctx: &RuntimeContext, params: &CodecParameters) -> Packet {
    let mut enc = ctx.codecs.first_encoder(params).unwrap();
    for i in 0..2u8 {
        let f = VideoFrame {
            pts: Some(i64::from(i) * 10),
            planes: vec![VideoPlane {
                stride: 16,
                data: vec![i * 100; 32],
            }],
        };
        enc.send_frame(&Frame::Video(f)).unwrap();
    }
    enc.flush().unwrap();
    enc.receive_packet().unwrap()
}

#[test]
fn encoder_loop_count_option_writes_num_plays() {
    let ctx = context();
    let pkt = encode_two(&ctx, &encoder_params(None));
    assert_eq!(parse_apng(&pkt.data).unwrap().actl.num_plays, 0);
    let pkt = encode_two(&ctx, &encoder_params(Some("4")));
    assert_eq!(parse_apng(&pkt.data).unwrap().actl.num_plays, 4);
}

/// The gateway's save path: the encoder folds the frames into one APNG
/// packet and the muxer stream carries `loop_count`. The muxer used to
/// write the packet as it was (`num_plays` 0); it now sets the plays.
#[test]
fn muxer_sets_num_plays_on_a_whole_apng_packet() {
    let ctx = context();
    let pkt = encode_two(&ctx, &encoder_params(None));
    let mut params = encoder_params(None);
    params.options = params.options.clone().set("loop_count", "2");
    let stream = StreamInfo {
        index: 0,
        time_base: TimeBase::new(1, 100),
        duration: Some(20),
        start_time: Some(0),
        params,
    };
    let out = mux(&ctx, &stream, None, std::slice::from_ref(&pkt)).unwrap();
    let info = parse_apng(&out).unwrap();
    assert_eq!(info.actl.num_plays, 2);
    assert_eq!(info.frames.len(), 2);
    assert_eq!(decoded(&out), decoded(&pkt.data));
    // Without the option the packet is written untouched.
    let stream = StreamInfo {
        params: encoder_params(None),
        ..stream
    };
    let out = mux(&ctx, &stream, None, std::slice::from_ref(&pkt)).unwrap();
    assert_eq!(out, pkt.data);
}
