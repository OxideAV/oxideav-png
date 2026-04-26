//! For every supported (colour_type, bit_depth) combination, build a
//! synthetic frame, encode to a PNG, decode it back, and assert the
//! resulting bytes are identical to what we started with.
//!
//! This proves the encoder + decoder are inverses and that per-row filters
//! + CRC + deflate round-trip cleanly.

use oxideav_core::{CodecId, CodecParameters, Frame, PixelFormat, VideoFrame, VideoPlane};

fn gradient(w: usize, h: usize, bpp: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * bpp];
    for y in 0..h {
        for x in 0..w {
            for c in 0..bpp {
                let v = ((x + y) * (c + 1) * 7) as u8;
                out[(y * w + x) * bpp + c] = v;
            }
        }
    }
    out
}

fn make_frame(w: u32, h: u32, fmt: PixelFormat, bpp: usize, palette: Option<&[u8]>) -> VideoFrame {
    let data = if fmt == PixelFormat::Pal8 {
        // Pal8: byte = palette index. Cycle through palette entries.
        let n = palette.map(|p| p.len() / 3).unwrap_or(16);
        let mut d = vec![0u8; w as usize * h as usize];
        for (i, v) in d.iter_mut().enumerate() {
            *v = (i % n) as u8;
        }
        d
    } else {
        gradient(w as usize, h as usize, bpp)
    };
    VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: w as usize * bpp,
            data,
        }],
    }
}

fn roundtrip_check(w: u32, h: u32, fmt: PixelFormat, bpp: usize, palette: Option<Vec<u8>>) {
    roundtrip_check_inner(w, h, fmt, bpp, palette, false);
}

fn roundtrip_check_inner(
    w: u32,
    h: u32,
    fmt: PixelFormat,
    bpp: usize,
    palette: Option<Vec<u8>>,
    interlace: bool,
) {
    let frame = make_frame(w, h, fmt, bpp, palette.as_deref());

    let mut params = CodecParameters::video(CodecId::new("png"));
    params.width = Some(w);
    params.height = Some(h);
    params.pixel_format = Some(fmt);
    if let Some(p) = &palette {
        params.extradata = p.clone();
    }
    if interlace {
        params.options = params.options.set("interlace", "true");
    }

    let mut enc = oxideav_png::encoder::make_encoder(&params).expect("make encoder");
    enc.send_frame(&Frame::Video(frame.clone())).expect("send");
    enc.flush().expect("flush");
    let pkt = enc.receive_packet().expect("recv");

    // Decode the produced PNG. Stream-level metadata (format, width,
    // height) is now reported via the IHDR-derived CodecParameters
    // contract, not on the frame.
    let vf = oxideav_png::decoder::decode_png_to_frame(&pkt.data, Some(0)).expect("decode");

    assert_eq!(
        vf.planes[0].data, frame.planes[0].data,
        "roundtrip byte mismatch for {fmt:?} {w}x{h} interlace={interlace}"
    );
}

#[test]
fn roundtrip_gray8() {
    roundtrip_check(16, 8, PixelFormat::Gray8, 1, None);
}

#[test]
fn roundtrip_gray16le() {
    roundtrip_check(16, 8, PixelFormat::Gray16Le, 2, None);
}

#[test]
fn roundtrip_rgb24() {
    roundtrip_check(16, 8, PixelFormat::Rgb24, 3, None);
}

#[test]
fn roundtrip_rgb48le() {
    roundtrip_check(16, 8, PixelFormat::Rgb48Le, 6, None);
}

#[test]
fn roundtrip_rgba() {
    roundtrip_check(16, 8, PixelFormat::Rgba, 4, None);
}

#[test]
fn roundtrip_rgba64le() {
    roundtrip_check(16, 8, PixelFormat::Rgba64Le, 8, None);
}

#[test]
fn roundtrip_ya8() {
    roundtrip_check(16, 8, PixelFormat::Ya8, 2, None);
}

#[test]
fn roundtrip_pal8() {
    // 4-entry palette.
    let palette: Vec<u8> = vec![
        0u8, 0, 0, // black
        255, 0, 0, // red
        0, 255, 0, // green
        0, 0, 255, // blue
    ];
    roundtrip_check(16, 8, PixelFormat::Pal8, 1, Some(palette));
}

// ---- Adam7 interlaced round-trips --------------------------------------
//
// Each test encodes with `options = { "interlace": "true" }`, which flips
// `IHDR.interlace` to 1 and routes compression through the seven-pass
// packer. The decoder's existing Adam7 path must reconstruct the image
// pixel-identical.

#[test]
fn roundtrip_rgba_interlaced() {
    roundtrip_check_inner(16, 8, PixelFormat::Rgba, 4, None, true);
}

#[test]
fn roundtrip_rgb24_interlaced() {
    roundtrip_check_inner(16, 8, PixelFormat::Rgb24, 3, None, true);
}

#[test]
fn roundtrip_gray8_interlaced() {
    roundtrip_check_inner(16, 8, PixelFormat::Gray8, 1, None, true);
}

#[test]
fn roundtrip_rgba64le_interlaced() {
    // 16-bit-per-channel: exercises bytes_per_pixel = 8 in the pass packer.
    roundtrip_check_inner(16, 8, PixelFormat::Rgba64Le, 8, None, true);
}

// Non-power-of-8 dimensions exercise the edge cases of Adam7 pass
// dimension math (img_w % 8 != 0 means passes 1/2/4/6 all see odd widths).
#[test]
fn roundtrip_rgba_interlaced_odd_dims() {
    roundtrip_check_inner(13, 11, PixelFormat::Rgba, 4, None, true);
}

// Tiny images where some passes have zero rows/cols — the packer must
// skip them cleanly.
#[test]
fn roundtrip_rgba_interlaced_3x3() {
    roundtrip_check_inner(3, 3, PixelFormat::Rgba, 4, None, true);
}

// Confirm interlace also flows through via the typed entry point (no
// CodecOptions bag involved) and the output really sets IHDR.interlace = 1.
#[test]
fn encode_single_with_options_sets_interlace_flag() {
    let frame = make_frame(8, 8, PixelFormat::Rgba, 4, None);
    let bytes = oxideav_png::encode_single_with_options(
        &frame,
        8,
        8,
        PixelFormat::Rgba,
        &[],
        &oxideav_png::PngEncoderOptions { interlace: true },
    )
    .expect("encode");
    // IHDR body starts at offset 8 (magic) + 8 (chunk length+type) = 16.
    // Interlace byte is the 13th (last) byte of IHDR data → offset 16 + 12 = 28.
    assert_eq!(bytes[28], 1, "interlace byte should be 1 for Adam7 encode");

    // And decoding reproduces the same pixels.
    let vf = oxideav_png::decoder::decode_png_to_frame(&bytes, Some(0)).expect("decode");
    assert_eq!(vf.planes[0].data, frame.planes[0].data);
}
