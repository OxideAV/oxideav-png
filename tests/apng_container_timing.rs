//! APNG timing through the framework container: the muxer writes each
//! packet's `duration` as the exact reduced `fcTL` fraction of the
//! stream's `time_base`, and the demuxer reports every frame's
//! `duration` exactly in its own tick — so `demux(mux(frames))` gives
//! the input delays back for a 1/1000 stream, a 1/100 stream and a
//! frame-rate tick, and the pixels survive untouched.
#![cfg(feature = "registry")]

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxideav_core::{
    CodecId, CodecParameters, Frame, Packet, PixelFormat, RuntimeContext, StreamInfo, TimeBase,
    VideoFrame, VideoPlane,
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

const W: u32 = 6;
const H: u32 = 4;

fn rgba(i: u8) -> Vec<u8> {
    (0..H)
        .flat_map(|y| {
            (0..W).flat_map(move |x| {
                [
                    (x * 40).wrapping_add(i.wrapping_mul(60) as u32) as u8,
                    (y * 60) as u8,
                    ((x + y) * 17) as u8,
                    if (x + y) % 3 == 0 {
                        255
                    } else {
                        (x * 40 + y * 17) as u8
                    },
                ]
            })
        })
        .collect()
}

fn params() -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new("png"));
    p.width = Some(W);
    p.height = Some(H);
    p.pixel_format = Some(PixelFormat::Rgba);
    p
}

/// One still PNG (the framework encoder over one frame).
fn still(ctx: &RuntimeContext, pixels: Vec<u8>) -> Vec<u8> {
    let mut enc = ctx.codecs.first_encoder(&params()).unwrap();
    enc.send_frame(&Frame::Video(VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: W as usize * 4,
            data: pixels,
        }],
    }))
    .unwrap();
    enc.flush().unwrap();
    enc.receive_packet().unwrap().data
}

fn ticks_to_duration(ticks: i64, tb: TimeBase) -> Duration {
    let ns = (ticks as i128) * 1_000_000_000 * (tb.0.num as i128) / (tb.0.den as i128);
    Duration::from_nanos(u64::try_from(ns).unwrap())
}

/// Mux `durations` (ticks of `tb`) as an APNG, demux it, and return the
/// demuxer's `(time_base, durations)` plus the decoded frames.
fn round_trip(
    ctx: &RuntimeContext,
    tb: TimeBase,
    durations: &[i64],
) -> (Vec<u8>, TimeBase, Vec<i64>, Vec<Vec<u8>>) {
    let stream = StreamInfo {
        index: 0,
        time_base: tb,
        duration: Some(durations.iter().sum()),
        start_time: Some(0),
        params: params(),
    };
    let shared = Arc::new(Mutex::new(Cursor::new(Vec::new())));
    let mut muxer = ctx
        .containers
        .open_muxer(
            "png",
            Box::new(SharedWriter(Arc::clone(&shared))),
            std::slice::from_ref(&stream),
        )
        .unwrap();
    muxer.write_header().unwrap();
    let mut pts = 0;
    for (i, d) in durations.iter().enumerate() {
        let mut pkt = Packet::new(0, tb, still(ctx, rgba(i as u8)));
        pkt.pts = Some(pts);
        pkt.dts = Some(pts);
        pkt.duration = Some(*d);
        pkt.flags.keyframe = true;
        muxer.write_packet(&pkt).unwrap();
        pts += d;
    }
    muxer.write_trailer().unwrap();
    drop(muxer);
    let bytes = Arc::try_unwrap(shared)
        .unwrap()
        .into_inner()
        .unwrap()
        .into_inner();

    let mut dm = ctx
        .containers
        .open_demuxer("png", Box::new(Cursor::new(bytes.clone())), &ctx.codecs)
        .unwrap();
    let info = dm.streams()[0].clone();
    let mut dec = ctx.codecs.first_decoder(&info.params).unwrap();
    let mut out_durations = Vec::new();
    let mut frames = Vec::new();
    let mut expect_pts = 0;
    while let Ok(p) = dm.next_packet() {
        assert_eq!(p.pts, Some(expect_pts), "pts cumulative");
        let d = p.duration.expect("duration");
        expect_pts += d;
        out_durations.push(d);
        dec.send_packet(&p).unwrap();
        let Frame::Video(v) = dec.receive_frame().unwrap() else {
            panic!("video frame");
        };
        frames.push(v.image_planes()[0].data.clone());
    }
    assert_eq!(info.duration, Some(out_durations.iter().sum::<i64>()));
    (bytes, info.time_base, out_durations, frames)
}

#[test]
fn durations_round_trip_exactly_for_millisecond_and_centisecond_streams() {
    let ctx = context();
    let cases: &[(TimeBase, &[i64])] = &[
        (TimeBase::new(1, 1000), &[100, 200, 50]),
        (TimeBase::new(1, 100), &[10, 20, 5]),
        (TimeBase::new(1, 1000), &[33, 67, 1]),
        (TimeBase::new(1, 30), &[1, 2, 3]),
        (TimeBase::new(1001, 30000), &[1, 1, 2]),
    ];
    for (tb, durations) in cases {
        let (bytes, out_tb, out, frames) = round_trip(&ctx, *tb, durations);
        let want: Vec<Duration> = durations
            .iter()
            .map(|d| ticks_to_duration(*d, *tb))
            .collect();
        let got: Vec<Duration> = out.iter().map(|d| ticks_to_duration(*d, out_tb)).collect();
        assert_eq!(
            got, want,
            "{tb:?} {durations:?}: delays exact (demux tick {out_tb:?})"
        );
        assert_eq!(frames.len(), durations.len());
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(*f, rgba(i as u8), "{tb:?}: frame {i} pixels");
        }
        // A `1/den` stream's own tick survives: same time base, same
        // ticks (a `num/den` stream demuxes at `1/den`, still exact).
        if tb.0.num == 1 {
            assert_eq!(out_tb, *tb, "{tb:?}: demux tick");
            assert_eq!(out, *durations, "{tb:?}: ticks");
        } else {
            assert_eq!(out_tb, TimeBase::new(1, tb.0.den), "{tb:?}: demux tick");
        }
        // The file carries the exact fractions.
        let apng = oxideav_png::decoder::parse_apng(&bytes).unwrap();
        for (frame, d) in apng.frames.iter().zip(durations.iter()) {
            let f = &frame.fctl;
            let secs = (f.delay_num as i128) * (tb.0.den as i128);
            let want_secs = (*d as i128) * (tb.0.num as i128) * (f.delay_den as i128);
            assert_eq!(
                secs, want_secs,
                "{tb:?}: fcTL {}/{} vs {d} ticks",
                f.delay_num, f.delay_den
            );
        }
    }
    // The spec'd case by name: 100 / 200 / 50 ms in a 1/1000 stream.
    let (bytes, out_tb, out, _) = round_trip(&ctx, TimeBase::new(1, 1000), &[100, 200, 50]);
    let apng = oxideav_png::decoder::parse_apng(&bytes).unwrap();
    let fr: Vec<(u16, u16)> = apng
        .frames
        .iter()
        .map(|f| (f.fctl.delay_num, f.fctl.delay_den))
        .collect();
    assert_eq!(fr, [(100, 1000), (200, 1000), (50, 1000)]);
    assert_eq!(out_tb, TimeBase::new(1, 1000));
    assert_eq!(out, [100, 200, 50]);
}

/// A centisecond file (every `delay_den = 100`, as most producers write)
/// keeps the historical `1/100` demux tick; `duration_micros` follows
/// the real time base; mixed denominators demux at their lcm, exactly.
#[test]
fn centisecond_files_keep_the_1_100_tick_and_mixed_denominators_their_lcm() {
    let ctx = context();
    let (bytes, tb, out, _) = round_trip(&ctx, TimeBase::new(1, 100), &[10, 25, 5]);
    assert_eq!(tb, TimeBase::new(1, 100));
    assert_eq!(out, [10, 25, 5]);
    let dm = ctx
        .containers
        .open_demuxer("png", Box::new(Cursor::new(bytes)), &ctx.codecs)
        .unwrap();
    assert_eq!(dm.duration_micros(), Some(400_000));
    // A file whose fcTLs mix denominators (1/10 s, 1/4 s, 7/1000 s):
    // tick = 1/1000, every delay exact.
    let apng = oxideav_png::encoder::encode_apng_frames(
        W,
        H,
        None,
        &[spec(0, 1, 10), spec(1, 1, 4), spec(2, 7, 1000)],
        0,
    )
    .unwrap();
    let mut dm = ctx
        .containers
        .open_demuxer("png", Box::new(Cursor::new(apng)), &ctx.codecs)
        .unwrap();
    assert_eq!(dm.streams()[0].time_base, TimeBase::new(1, 1000));
    let d: Vec<i64> = std::iter::from_fn(|| dm.next_packet().ok())
        .map(|p| p.duration.unwrap())
        .collect();
    assert_eq!(d, [100, 250, 7]);
}

fn spec(i: u8, delay_num: u16, delay_den: u16) -> oxideav_png::encoder::ApngFrameSpec {
    let mut f = oxideav_png::encoder::ApngFrameSpec::new(
        oxideav_png::PngImage::from_rgba8(W, H, rgba(i)).unwrap(),
    );
    f.delay_num = delay_num;
    f.delay_den = delay_den;
    f
}
