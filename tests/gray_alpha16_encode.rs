//! `PngPixelFormat::Ya16Le` (16-bit gray + alpha, little-endian per
//! sample) encodes as colour type 4 at bit depth 16 (W3C PNG3 §11.2.1
//! IHDR: colour type 4 allows bit depths 8 and 16; §7.1: each pixel is
//! a gray sample then an alpha sample, two bytes each, most significant
//! byte first) instead of having to be widened to 16-bit RGBA.

use oxideav_png::{EncodeOptions, PngImage, PngPixelFormat};

fn ya16(w: u32, h: u32) -> (Vec<u8>, Vec<[u16; 2]>) {
    let mut bytes = Vec::new();
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let g = (x * 4099 + y * 257) as u16;
            let a = (65535 - x * 1000 - y * 3) as u16;
            bytes.extend_from_slice(&g.to_le_bytes());
            bytes.extend_from_slice(&a.to_le_bytes());
            px.push([g, a]);
        }
    }
    (bytes, px)
}

#[test]
fn ya16_writes_colour_type_4_depth_16() {
    let (w, h) = (7u32, 5u32);
    let (bytes, px) = ya16(w, h);
    let img = PngImage::packed(w, h, PngPixelFormat::Ya16Le, w as usize * 4, bytes).unwrap();
    let file = oxideav_png::encode(&img, &EncodeOptions::default()).unwrap();
    // IHDR: bit depth 16, colour type 4.
    assert_eq!(&file[12..16], b"IHDR");
    assert_eq!((file[24], file[25]), (16, 4));
    // The decoder widens colour type 4 / 16-bit to Rgba64Le (gray
    // replicated): every sample survives exactly.
    let back = oxideav_png::decode(&file).unwrap();
    assert_eq!(back.format, PngPixelFormat::Rgba64Le);
    let d = back.as_bytes().unwrap();
    for (i, [g, a]) in px.iter().enumerate() {
        let s = |c: usize| u16::from_le_bytes([d[i * 8 + 2 * c], d[i * 8 + 2 * c + 1]]);
        assert_eq!([s(0), s(1), s(2), s(3)], [*g, *g, *g, *a], "pixel {i}");
    }
    // A quarter smaller raw stream than the RGBA64 detour it replaces.
    let wide = oxideav_png::encode(&back, &EncodeOptions::default()).unwrap();
    assert_eq!((wide[24], wide[25]), (16, 6));
}

#[test]
fn ya16_layout_facts() {
    let f = PngPixelFormat::Ya16Le;
    assert_eq!(f.bytes_per_pixel(), 4);
    assert!(f.has_alpha());
    assert_eq!(f.bits_per_sample(), 16);
}

#[test]
fn ya16_to_rgba8_takes_the_high_bytes() {
    let img = PngImage::packed(
        1,
        1,
        PngPixelFormat::Ya16Le,
        4,
        vec![0x34, 0x12, 0x78, 0x56],
    )
    .unwrap();
    assert_eq!(img.to_rgba8(), [0x12, 0x12, 0x12, 0x56]);
}

#[cfg(feature = "registry")]
#[test]
fn ya16_through_the_framework_encoder() {
    use oxideav_core::{
        CodecId, CodecParameters, Frame, PixelFormat, RuntimeContext, VideoFrame, VideoPlane,
    };
    let mut ctx = RuntimeContext::new();
    oxideav_png::register(&mut ctx);
    let (w, h) = (3u32, 2u32);
    let (bytes, _) = ya16(w, h);
    let mut p = CodecParameters::video(CodecId::new("png"));
    p.width = Some(w);
    p.height = Some(h);
    p.pixel_format = Some(PixelFormat::Ya16Le);
    let mut enc = ctx.codecs.first_encoder(&p).unwrap();
    enc.send_frame(&Frame::Video(VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: w as usize * 4,
            data: bytes,
        }],
    }))
    .unwrap();
    enc.flush().unwrap();
    let pkt = enc.receive_packet().unwrap();
    assert_eq!((pkt.data[24], pkt.data[25]), (16, 4));
}
