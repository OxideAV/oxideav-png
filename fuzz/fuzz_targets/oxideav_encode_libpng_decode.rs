#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_core::{PixelFormat, VideoFrame, VideoPlane};
use oxideav_png::encode_single;
use oxideav_png_fuzz::libpng;

const MAX_WIDTH: usize = 64;
const MAX_PIXELS: usize = 2048;

fuzz_target!(|data: &[u8]| {
    // Skip silently if libpng isn't installed on this host.
    if !libpng::available() {
        return;
    }

    let Some((width, height, rgba)) = image_from_fuzz_input(data) else {
        return;
    };

    let frame = rgba_video_frame(width, height, rgba);
    let encoded = encode_single(&frame, width, height, PixelFormat::Rgba, &[])
        .expect("oxideav-png encoding failed");
    let decoded = libpng::decode_to_rgba(&encoded).expect("libpng decoding failed");

    assert_eq!(decoded.width, width);
    assert_eq!(decoded.height, height);
    // libpng's simplified-read API treats output RGBA as associated
    // alpha, so for transparent pixels it may zero RGB during
    // composition. Match the webp cross-decode harnesses and only
    // enforce alpha-equality for fully-transparent pixels.
    assert_rgba_allow_transparent_rgb_differences(rgba, &decoded.rgba);
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

fn assert_rgba_allow_transparent_rgb_differences(expected: &[u8], actual: &[u8]) {
    assert_eq!(actual.len(), expected.len(), "decoded RGBA length mismatch");

    for (pixel_index, (expected, actual)) in expected
        .chunks_exact(4)
        .zip(actual.chunks_exact(4))
        .enumerate()
    {
        if expected[3] == 0 {
            assert_eq!(
                actual[3], 0,
                "decoded alpha differs for transparent pixel {pixel_index}"
            );
        } else {
            assert_eq!(
                actual, expected,
                "decoded RGBA differs at pixel {pixel_index}"
            );
        }
    }
}
