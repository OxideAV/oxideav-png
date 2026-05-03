#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_core::{PixelFormat, VideoFrame, VideoPlane};
use oxideav_png::{decode_png_to_frame, encode_single};

const MAX_WIDTH: usize = 64;
const MAX_PIXELS: usize = 2048;

fuzz_target!(|data: &[u8]| {
    let Some((width, height, rgba)) = image_from_fuzz_input(data) else {
        return;
    };

    let frame = rgba_video_frame(width, height, rgba);
    let encoded =
        encode_single(&frame, width, height, PixelFormat::Rgba, &[]).expect("PNG encode failed");
    let decoded = decode_png_to_frame(&encoded, None).expect("PNG decode failed");

    let plane = decoded.planes.first().expect("decoded PNG has no plane");
    assert_eq!(
        plane.stride,
        (width as usize) * 4,
        "decoded RGBA stride mismatch"
    );
    assert_eq!(
        plane.data.len(),
        (width as usize) * (height as usize) * 4,
        "decoded RGBA buffer size mismatch"
    );
    assert_eq!(
        plane.data.as_slice(),
        rgba,
        "decoded RGBA differs from input"
    );
});

fn image_from_fuzz_input(data: &[u8]) -> Option<(u32, u32, &[u8])> {
    let (&shape, rgba) = data.split_first()?;

    let pixel_count = (rgba.len() / 4).min(MAX_PIXELS);
    if pixel_count == 0 {
        return None;
    }

    let width = ((shape as usize) % MAX_WIDTH) + 1;
    let width = width.min(pixel_count);
    let height = pixel_count / width;
    let used_len = width * height * 4;
    let rgba = &rgba[..used_len];

    Some((width as u32, height as u32, rgba))
}

fn rgba_video_frame(width: u32, height: u32, rgba: &[u8]) -> VideoFrame {
    VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: (width as usize) * 4,
            data: rgba.to_vec(),
        }],
    }
}
