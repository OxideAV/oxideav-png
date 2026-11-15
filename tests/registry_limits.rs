//! The registry decoder holds every size a stream declares to its
//! limits — the crate's defaults tightened by `CodecParameters::limits`
//! — before allocating: a still's header, and the animation canvas the
//! stream parameters name (which a frame packet's own header does not).
#![cfg(feature = "registry")]

use oxideav_core::{
    CodecId, CodecParameters, DecoderLimits, Error, Frame, Packet, PixelFormat, TimeBase,
};
use oxideav_png::chunk::write_chunk;
use oxideav_png::{EncodeOptions, PngImage};

fn params(w: u32, h: u32) -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new("png"));
    p.width = Some(w);
    p.height = Some(h);
    p.pixel_format = Some(PixelFormat::Rgba);
    p
}

fn still(w: u32, h: u32) -> Vec<u8> {
    let image = PngImage::from_rgba8(w, h, vec![7u8; (w * h * 4) as usize]).unwrap();
    oxideav_png::encode(&image, &EncodeOptions::new()).unwrap()
}

/// A frame packet as the demuxer makes them: a `w × h` still carrying
/// an `fcTL` for that region at (0, 0), sequence number 0.
fn frame_packet(w: u32, h: u32) -> Packet {
    let mut fctl = 0u32.to_be_bytes().to_vec();
    for v in [w, h, 0, 0] {
        fctl.extend_from_slice(&v.to_be_bytes());
    }
    fctl.extend_from_slice(&[0, 1, 0, 10, 0, 0]);
    let mut chunk = Vec::new();
    write_chunk(&mut chunk, b"fcTL", &fctl);
    let mut data = still(w, h);
    data.splice(33..33, chunk);
    Packet::new(0, TimeBase::new(1, 100), data)
}

fn decode(params: &CodecParameters, packet: &Packet) -> oxideav_core::Result<Frame> {
    let mut dec = oxideav_png::make_decoder(params)?;
    dec.send_packet(packet)?;
    dec.receive_frame()
}

#[test]
fn a_small_frame_on_an_oversized_canvas_is_refused_before_allocation() {
    // 60000 × 60000 RGBA: a 14 GiB canvas named by the stream, for a
    // 4 × 4 frame.
    let err = decode(&params(60_000, 60_000), &frame_packet(4, 4)).unwrap_err();
    assert!(matches!(err, Error::ResourceExhausted(_)), "{err:?}");
    // The same frame on a canvas within the limits renders.
    match decode(&params(64, 48), &frame_packet(4, 4)).unwrap() {
        Frame::Video(v) => assert_eq!(v.planes[0].data.len(), 64 * 48 * 4),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stream_limits_tighten_the_decoder() {
    let packet = Packet::new(0, TimeBase::new(1, 100), still(20, 20));
    assert!(decode(&params(20, 20), &packet).is_ok());

    let mut tight = params(20, 20);
    tight.limits = DecoderLimits::default().with_max_pixels_per_frame(399);
    let err = decode(&tight, &packet).unwrap_err();
    assert!(matches!(err, Error::ResourceExhausted(_)), "{err:?}");

    let mut bytes = params(20, 20);
    bytes.limits = DecoderLimits::default().with_max_alloc_bytes_per_frame(20 * 20 * 4 - 1);
    assert!(matches!(
        decode(&bytes, &packet).unwrap_err(),
        Error::ResourceExhausted(_)
    ));
    bytes.limits = DecoderLimits::default().with_max_alloc_bytes_per_frame(20 * 20 * 4);
    assert!(decode(&bytes, &packet).is_ok());

    // The canvas of an animation is held to the same caps.
    let mut canvas = params(64, 48);
    canvas.limits = DecoderLimits::default().with_max_pixels_per_frame(64 * 48 - 1);
    assert!(matches!(
        decode(&canvas, &frame_packet(4, 4)).unwrap_err(),
        Error::ResourceExhausted(_)
    ));
}
