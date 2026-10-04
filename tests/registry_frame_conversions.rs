//! The framework adapter (`registry` feature): `PngImage` ⇄
//! `VideoFrame` conversions, and the trait-side `Decoder` / `Encoder`
//! as thin wrappers over the standalone functions.

#![cfg(feature = "registry")]

use oxideav_core::{
    CodecId, CodecParameters, ColorSignal, Frame, Packet, PixelFormat, TimeBase, VideoFrame,
};
use oxideav_png::{
    decode, encode, make_decoder, make_encoder, ColorInfo, ColorRange, EncodeOptions, Palette,
    PngImage, PngPixelFormat,
};

#[test]
fn image_to_frame_carries_plane_palette_and_colour_signal() {
    let img = PngImage::packed(2, 1, PngPixelFormat::Pal8, 2, vec![0, 1])
        .unwrap()
        .with_palette(Palette::from_rgb(&[1, 2, 3, 4, 5, 6], Some(&[7])))
        .with_color(ColorInfo::new(ColorRange::Full, 9, 16, 0));
    let frame = VideoFrame::from(&img);
    assert_eq!(frame.image_plane_count(), 1);
    assert_eq!(frame.image_planes()[0].data, vec![0, 1]);
    assert_eq!(frame.image_planes()[0].stride, 2);
    assert_eq!(frame.palette(), Some(&[1u8, 2, 3, 4, 5, 6][..]));
    let sig = frame.color_signal().expect("colour signal attached");
    assert_eq!(sig.primaries.0, 9);
    assert_eq!(sig.transfer.0, 16);
    assert_eq!(sig.matrix.0, 0);
    assert_eq!(sig.range, oxideav_core::ColorRange::Full);

    // The default colour description attaches nothing.
    let plain = VideoFrame::from(PngImage::from_rgba8(1, 1, vec![1, 2, 3, 4]).unwrap());
    assert_eq!(plain.planes.len(), 1);
    assert!(plain.color_signal().is_none());
    assert!(plain.palette().is_none());
}

#[test]
fn frame_to_image_reads_the_side_channels_back() {
    let mut params = CodecParameters::video(CodecId::new("png"));
    params.width = Some(2);
    params.height = Some(1);
    params.pixel_format = Some(PixelFormat::Pal8);
    let frame = VideoFrame {
        pts: None,
        planes: vec![oxideav_core::VideoPlane {
            stride: 2,
            data: vec![1, 0],
        }],
    }
    .with_palette(vec![9, 8, 7, 6, 5, 4])
    .with_color_signal(ColorSignal::srgb());
    let img = PngImage::from_video_frame(&frame, &params).unwrap();
    assert_eq!(img.format(), PngPixelFormat::Pal8);
    assert_eq!(img.as_bytes(), Some(&[1u8, 0][..]));
    assert_eq!(
        img.palette,
        Some(Palette::from_rgb(&[9, 8, 7, 6, 5, 4], None))
    );
    assert_eq!(img.color, ColorInfo::srgb());
    let via_try: PngImage = (&frame, &params).try_into().unwrap();
    assert_eq!(via_try, img);
}

#[test]
fn trait_decoder_and_encoder_are_thin_adapters_over_the_standalone_fns() {
    let img = PngImage::from_rgb8(2, 2, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])
        .unwrap()
        .with_color(ColorInfo::srgb());
    let bytes = encode(&img, &EncodeOptions::default()).unwrap();

    // Decoder: the same image the standalone path yields, as a frame.
    let params = CodecParameters::video(CodecId::new("png"));
    let mut dec = make_decoder(&params).unwrap();
    let mut pkt = Packet::new(0, TimeBase::new(1, 100), bytes.clone());
    pkt.pts = Some(7);
    dec.send_packet(&pkt).unwrap();
    let Frame::Video(vf) = dec.receive_frame().unwrap() else {
        panic!("video frame expected");
    };
    assert_eq!(vf.pts, Some(7));
    assert_eq!(vf.image_planes()[0].data, img.as_bytes().unwrap());
    assert_eq!(vf.color_signal(), Some(ColorSignal::srgb()));
    assert_eq!(decode(&bytes).unwrap(), img);

    // Encoder: a frame in, the standalone bytes out (the colour signal
    // on the frame becomes the sRGB chunk, exactly as `encode` writes).
    let mut eparams = CodecParameters::video(CodecId::new("png"));
    eparams.width = Some(2);
    eparams.height = Some(2);
    eparams.pixel_format = Some(PixelFormat::Rgb24);
    let mut enc = make_encoder(&eparams).unwrap();
    let frame = VideoFrame::from(&img);
    enc.send_frame(&Frame::Video(frame)).unwrap();
    enc.flush().unwrap();
    let out = enc.receive_packet().unwrap();
    assert_eq!(out.data, bytes);
}
