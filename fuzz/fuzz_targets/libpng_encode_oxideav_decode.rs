#![no_main]

use libfuzzer_sys::fuzz_target;
use oxideav_png::decode_png_to_frame;
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

    let encoded = libpng::encode_rgba(rgba, width, height).expect("libpng RGBA encoding failed");

    let decoded = decode_png_to_frame(&encoded, None).expect("oxideav-png decoding failed");

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
    // libpng's simplified-write API treats the input alpha as
    // associated (premultiplied); for transparent pixels it may
    // therefore zero out the RGB before storing. PNG colour type 6
    // itself stores unassociated alpha, so what comes back through
    // our decoder is whatever libpng chose to write — match the
    // webp cross-decode harnesses and only enforce alpha-equality
    // for fully-transparent pixels.
    assert_rgba_allow_transparent_rgb_differences(rgba, &plane.data);
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
