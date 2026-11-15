#![no_main]

//! Framework path under hostile input: demux → (remux) and demux →
//! decode → encode → mux, with the demuxer's string metadata handed to
//! the muxer.
//!
//! The input is read as a PNG chunk stream (an optional signature is
//! skipped) and every chunk's length is clamped to the bytes present
//! and its CRC rewritten, so mutations land in chunk types, orders and
//! bodies instead of dying at the CRC gate. Whatever the demuxer
//! accepts must then hold these properties:
//!
//! * nothing panics, on any path;
//! * a remux the muxer accepts still demuxes, to the same packet count;
//! * a frame the decoder produced is one the encoder accepts, and the
//!   file the encoder writes from the frame's records — the blob kinds,
//!   the colour signal, the palette — satisfies the strict metadata
//!   parser (every chunk it wrote is well-formed, single where it must
//!   be, and in a legal place) and decodes back to the frame's pixels;
//! * that file, muxed with the demuxed text pairs, is still strictly
//!   parseable and its pixels are unchanged.

use std::io::{Cursor, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use libfuzzer_sys::fuzz_target;
use oxideav_core::{Frame, Packet, RuntimeContext, StreamInfo};
use oxideav_png::chunk::write_chunk;

const MAGIC: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
/// Canvas area above which the transcode half is skipped (the encoder
/// is not the interesting part at size).
const MAX_PIXELS: u64 = 1 << 14;
const MAX_PACKETS: usize = 16;
/// Canvas area above which the remux half is skipped.
const MAX_REMUX_PIXELS: u64 = 1 << 22;

struct SharedWriter(Arc<Mutex<Cursor<Vec<u8>>>>);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for SharedWriter {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.lock().unwrap().seek(pos)
    }
}

/// The input as a file whose chunk framing is sound.
fn repair(data: &[u8]) -> Vec<u8> {
    let mut rest = data.strip_prefix(&MAGIC).unwrap_or(data);
    let mut out = MAGIC.to_vec();
    while rest.len() >= 8 {
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let ty = [rest[4], rest[5], rest[6], rest[7]];
        let body = &rest[8..];
        let len = len.min(body.len());
        write_chunk(&mut out, &ty, &body[..len]);
        // Skip the body and, when present, the 4 CRC bytes.
        rest = &body[len..];
        rest = rest.get(4..).unwrap_or(&[]);
    }
    out
}

fn demux(ctx: &RuntimeContext, file: &[u8]) -> Option<(StreamInfo, Vec<(String, String)>, Vec<Packet>)> {
    let mut dm = ctx
        .containers
        .open_demuxer("png", Box::new(Cursor::new(file.to_vec())), &ctx.codecs)
        .ok()?;
    let stream = dm.streams().first()?.clone();
    let meta = dm.metadata().to_vec();
    let mut packets = Vec::new();
    while packets.len() < MAX_PACKETS {
        match dm.next_packet() {
            Ok(p) => packets.push(p),
            Err(_) => break,
        }
    }
    Some((stream, meta, packets))
}

fn mux(
    ctx: &RuntimeContext,
    stream: &StreamInfo,
    meta: &[(String, String)],
    packets: &[Packet],
) -> Option<Vec<u8>> {
    let shared = Arc::new(Mutex::new(Cursor::new(Vec::new())));
    let mut m = ctx
        .containers
        .open_muxer(
            "png",
            Box::new(SharedWriter(Arc::clone(&shared))),
            std::slice::from_ref(stream),
        )
        .ok()?;
    m.set_metadata(meta).ok()?;
    m.write_header().ok()?;
    for p in packets {
        m.write_packet(p).ok()?;
    }
    m.write_trailer().ok()?;
    drop(m);
    let bytes = shared.lock().unwrap().get_ref().clone();
    Some(bytes)
}

fuzz_target!(|data: &[u8]| {
    let file = repair(data);
    let mut ctx = RuntimeContext::new();
    oxideav_png::register(&mut ctx);
    let Some((stream, meta, packets)) = demux(&ctx, &file) else {
        return;
    };
    if packets.is_empty() {
        return;
    }

    let (w, h) = (
        stream.params.width.unwrap_or(0) as u64,
        stream.params.height.unwrap_or(0) as u64,
    );
    // Remux. A hidden default image costs time in proportion to the
    // canvas the header declares, so huge canvases are left out.
    if w * h <= MAX_REMUX_PIXELS
        && packets.len() < MAX_PACKETS
        && let Some(out) = mux(&ctx, &stream, &meta, &packets)
    {
        let (_, _, again) = demux(&ctx, &out).expect("a muxed file demuxes");
        assert_eq!(again.len(), packets.len(), "remux changed the packet count");
    }

    // Transcode.
    if w * h == 0 || w * h > MAX_PIXELS {
        return;
    }
    let Ok(mut dec) = oxideav_png::make_decoder(&stream.params) else {
        return;
    };
    if dec.send_packet(&packets[0]).is_err() {
        return;
    }
    let Ok(Frame::Video(frame)) = dec.receive_frame() else {
        return;
    };
    let mut enc = oxideav_png::make_encoder(&stream.params).expect("encoder for a demuxed stream");
    enc.send_frame(&Frame::Video(frame.clone()))
        .expect("a decoded frame encodes");
    enc.flush().expect("flush");
    let packet = enc.receive_packet().expect("one packet");
    oxideav_png::parse_metadata(&packet.data).expect("the encoder wrote a strictly valid file");
    let back = oxideav_png::decode_png_to_frame(&packet.data, None).expect("it decodes");
    assert_eq!(
        back.planes[0].data, frame.planes[0].data,
        "transcode changed the pixels"
    );
    let out = mux(&ctx, &stream, &meta, std::slice::from_ref(&packet))
        .expect("the encoder's packet muxes");
    oxideav_png::parse_metadata(&out).expect("muxed with text pairs: still strictly valid");
    let muxed = oxideav_png::decode_png_to_frame(&out, None).expect("muxed file decodes");
    assert_eq!(muxed.planes[0].data, frame.planes[0].data);
});
