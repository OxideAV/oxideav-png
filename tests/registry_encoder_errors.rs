//! What the framework `Encoder` does with a frame it cannot encode.
//!
//! The encoder compresses each frame in `send_frame`, so that call is
//! where a bad frame is first seen and it returns the error. The file
//! the frame belonged to is then failed, as it was when frames were
//! buffered until `flush`: later frames of that file are dropped,
//! `flush` returns the same error, no packet is emitted, and no frame
//! is renumbered into the gap. Frames sent after that `flush` start a
//! new file.

#![cfg(feature = "registry")]

use oxideav_core::{
    CodecId, CodecOptions, CodecParameters, Frame, PixelFormat, VideoFrame, VideoPlane,
};
use oxideav_png::{encode, EncodeOptions, PngImage, PngPixelFormat};

const W: u32 = 33;
const H: u32 = 17;

fn params() -> CodecParameters {
    let mut p = CodecParameters::video(CodecId::new("png"));
    p.width = Some(W);
    p.height = Some(H);
    p.pixel_format = Some(PixelFormat::Rgba);
    p
}

/// A valid RGBA frame whose pixels depend on `seed`.
fn frame(seed: u8, pts: i64) -> VideoFrame {
    let data: Vec<u8> = (0..W * H * 4)
        .map(|i| (i as u8).wrapping_mul(seed | 1).wrapping_add(seed))
        .collect();
    VideoFrame {
        pts: Some(pts),
        planes: vec![VideoPlane {
            stride: W as usize * 4,
            data,
        }],
    }
}

/// A frame whose plane is far shorter than 33 x 17 RGBA.
fn short_frame() -> VideoFrame {
    VideoFrame {
        pts: Some(99),
        planes: vec![VideoPlane {
            stride: W as usize * 4,
            data: vec![0; 10],
        }],
    }
}

/// The error the short frame's plane gets, as the encoder reports it.
fn short_frame_error() -> String {
    let e = PngImage::packed(W, H, PngPixelFormat::Rgba, W as usize * 4, vec![0; 10]).unwrap_err();
    oxideav_core::Error::from(e).to_string()
}

enum Op {
    Send(VideoFrame),
    Flush,
    Recv,
}

/// Run `ops` against a fresh encoder and describe each result.
fn trace(params: &CodecParameters, ops: Vec<Op>) -> Vec<String> {
    let mut enc = oxideav_png::make_encoder(params).unwrap();
    ops.into_iter()
        .map(|op| match op {
            Op::Send(f) => match enc.send_frame(&Frame::Video(f)) {
                Ok(()) => "send ok".to_string(),
                Err(e) => format!("send ERR {e}"),
            },
            Op::Flush => match enc.flush() {
                Ok(()) => "flush ok".to_string(),
                Err(e) => format!("flush ERR {e}"),
            },
            Op::Recv => match enc.receive_packet() {
                Ok(p) => format!("recv pts={:?} {} bytes", p.pts, p.data.len()),
                Err(e) => format!("recv ERR {e}"),
            },
        })
        .collect()
}

fn lines(expected: &[&str]) -> Vec<String> {
    expected.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_bad_middle_frame_fails_the_whole_file() {
    let err = short_frame_error();
    let got = trace(
        &params(),
        vec![
            Op::Send(frame(1, 10)),
            Op::Send(short_frame()),
            Op::Send(frame(3, 12)),
            Op::Flush,
            Op::Recv,
            Op::Recv,
        ],
    );
    assert_eq!(
        got,
        lines(&[
            "send ok",
            &format!("send ERR {err}"),
            "send ok",
            &format!("flush ERR {err}"),
            "recv ERR end of stream",
            "recv ERR end of stream",
        ])
    );
}

#[test]
fn a_bad_first_frame_fails_the_whole_file() {
    let err = short_frame_error();
    let got = trace(
        &params(),
        vec![
            Op::Send(short_frame()),
            Op::Send(frame(2, 11)),
            Op::Send(frame(3, 12)),
            Op::Flush,
            Op::Recv,
            Op::Recv,
        ],
    );
    assert_eq!(
        got,
        lines(&[
            &format!("send ERR {err}"),
            "send ok",
            "send ok",
            &format!("flush ERR {err}"),
            "recv ERR end of stream",
            "recv ERR end of stream",
        ])
    );
}

#[test]
fn a_bad_option_fails_the_whole_file() {
    let mut p = params();
    p.options = CodecOptions::new().set("bit_depth", "2");
    // The option is rejected for any RGBA image, as `encode` rejects it.
    let img = PngImage::from_rgba8(W, H, frame(1, 10).planes[0].data.clone()).unwrap();
    let err = oxideav_core::Error::from(
        encode(&img, &EncodeOptions::default().with_bit_depth(2)).unwrap_err(),
    )
    .to_string();
    let got = trace(
        &p,
        vec![
            Op::Send(frame(1, 10)),
            Op::Send(frame(2, 11)),
            Op::Flush,
            Op::Recv,
            Op::Recv,
        ],
    );
    assert_eq!(
        got,
        lines(&[
            &format!("send ERR {err}"),
            "send ok",
            &format!("flush ERR {err}"),
            "recv ERR end of stream",
            "recv ERR end of stream",
        ])
    );
}

/// After the failed `flush`, the next frames make a new file.
#[test]
fn frames_after_a_failed_flush_start_a_new_file() {
    let good = frame(5, 20);
    let still = oxideav_png::encode_single(&good, W, H, PixelFormat::Rgba, &[]).unwrap();
    let got = trace(
        &params(),
        vec![
            Op::Send(frame(1, 10)),
            Op::Send(short_frame()),
            Op::Flush,
            Op::Send(good),
            Op::Flush,
            Op::Recv,
            Op::Recv,
        ],
    );
    let err = short_frame_error();
    assert_eq!(
        got,
        lines(&[
            "send ok",
            &format!("send ERR {err}"),
            &format!("flush ERR {err}"),
            "send ok",
            "flush ok",
            &format!("recv pts=Some(20) {} bytes", still.len()),
            "recv ERR end of stream",
        ])
    );
}

/// Frames sent after a `flush` start a new file without another
/// `flush`; when that file fails, `receive_packet` reports it after
/// the packets already queued.
#[test]
fn a_failure_after_flush_is_reported_by_receive_packet() {
    let first = frame(1, 10);
    let still = oxideav_png::encode_single(&first, W, H, PixelFormat::Rgba, &[]).unwrap();
    let err = short_frame_error();
    let got = trace(
        &params(),
        vec![
            Op::Send(first),
            Op::Flush,
            Op::Send(short_frame()),
            Op::Recv,
            Op::Recv,
            Op::Recv,
        ],
    );
    assert_eq!(
        got,
        lines(&[
            "send ok",
            "flush ok",
            &format!("send ERR {err}"),
            &format!("recv pts=Some(10) {} bytes", still.len()),
            &format!("recv ERR {err}"),
            "recv ERR end of stream",
        ])
    );
}
