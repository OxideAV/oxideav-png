//! Integration coverage for the §12.4 / §13.12 sample-depth scaling
//! module, driven through the real standalone encode -> decode path.
//!
//! Builds a genuine 16-bit PNG with the encoder, decodes it back to a
//! `PngImage`, and reduces it to 8 bits with the public `depth` helpers,
//! asserting the reduction matches the per-sample linear equation and
//! that the `sBIT`-aware path recovers the encoder's low-depth source
//! exactly.

use oxideav_png::depth::{
    max_sample, recover_sbit, rescale_16bit_to_8bit, rescale_16bit_to_8bit_via_sbit,
    rescale_sample, scale_up_bit_replication,
};
use oxideav_png::image::{PngImage, PngPixelFormat};
use oxideav_png::metadata::Sbit;
use oxideav_png::{decode_png, encode_png_image};

fn png16(format: PngPixelFormat, w: u32, h: u32, samples: &[u16]) -> PngImage {
    let mut data = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        data.extend_from_slice(&s.to_le_bytes());
    }
    let bpp = format.bytes_per_pixel();
    assert_eq!(data.len(), w as usize * h as usize * bpp);
    PngImage {
        width: w,
        height: h,
        pixel_format: format,
        stride: w as usize * bpp,
        data,
        palette: Vec::new(),
    }
}

fn gradient16(w: u32, h: u32, samples_per_px: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(w as usize * h as usize * samples_per_px);
    for y in 0..h as usize {
        for x in 0..w as usize {
            for c in 0..samples_per_px {
                // Spread across the full 16-bit range.
                let v = ((x * 977 + y * 613 + c * 20011) & 0xFFFF) as u16;
                out.push(v);
            }
        }
    }
    out
}

/// Encode a 16-bit PngImage, decode it back, and confirm the round-trip
/// is byte-exact — the precondition for asserting anything about the
/// rescale of the decoded buffer.
fn encode_decode(image: &PngImage) -> PngImage {
    let bytes = encode_png_image(image).expect("encode 16-bit PNG");
    let decoded = decode_png(&bytes).expect("decode 16-bit PNG");
    assert_eq!(decoded.pixel_format, image.pixel_format);
    assert_eq!(decoded.width, image.width);
    assert_eq!(decoded.height, image.height);
    // Decoder output is tightly packed; compare the live sample bytes.
    assert_eq!(decoded.data, image.data, "16-bit round-trip must be exact");
    decoded
}

#[test]
fn gray16_rescale_matches_linear_equation() {
    let (w, h) = (7u32, 5u32);
    let samples = gradient16(w, h, 1);
    let src = png16(PngPixelFormat::Gray16Le, w, h, &samples);
    let decoded = encode_decode(&src);

    let out = rescale_16bit_to_8bit(&decoded);
    assert_eq!(out.pixel_format, PngPixelFormat::Gray8);
    assert_eq!(out.stride, w as usize);
    assert_eq!(out.data.len(), (w * h) as usize);
    for (i, &s) in samples.iter().enumerate() {
        assert_eq!(
            out.data[i],
            rescale_sample(s, u16::MAX, 255) as u8,
            "px {i}"
        );
    }
}

#[test]
fn rgb48_rescale_matches_linear_equation() {
    let (w, h) = (6u32, 4u32);
    let samples = gradient16(w, h, 3);
    let src = png16(PngPixelFormat::Rgb48Le, w, h, &samples);
    let decoded = encode_decode(&src);

    let out = rescale_16bit_to_8bit(&decoded);
    assert_eq!(out.pixel_format, PngPixelFormat::Rgb24);
    assert_eq!(out.data.len(), (w * h * 3) as usize);
    for (i, &s) in samples.iter().enumerate() {
        assert_eq!(
            out.data[i],
            rescale_sample(s, u16::MAX, 255) as u8,
            "sample {i}"
        );
    }
}

#[test]
fn rgba64_rescale_reduces_every_channel_including_alpha() {
    let (w, h) = (5u32, 3u32);
    let samples = gradient16(w, h, 4);
    let src = png16(PngPixelFormat::Rgba64Le, w, h, &samples);
    let decoded = encode_decode(&src);

    let out = rescale_16bit_to_8bit(&decoded);
    assert_eq!(out.pixel_format, PngPixelFormat::Rgba);
    assert_eq!(out.data.len(), (w * h * 4) as usize);
    for (i, &s) in samples.iter().enumerate() {
        // Alpha (every 4th sample) is depth-reduced the same way — this
        // is a rescale, not gamma correction.
        assert_eq!(
            out.data[i],
            rescale_sample(s, u16::MAX, 255) as u8,
            "sample {i}"
        );
    }
}

#[test]
fn via_sbit_recovers_low_depth_source_through_the_real_codec() {
    // Author a 16-bit Gray image whose samples were bit-replicated up
    // from a 5-bit source, carrying an sBIT of 5. After a genuine
    // encode -> decode, the sBIT-aware rescale must reproduce the same
    // 8-bit value a direct 5-bit -> 8-bit scale of the source gives.
    let (w, h) = (8u32, 4u32);
    let source_5bit: Vec<u16> = (0..w * h).map(|i| (i as u16) & 0x1F).collect();
    let stored: Vec<u16> = source_5bit
        .iter()
        .map(|&v| scale_up_bit_replication(v, 5, 16))
        .collect();
    let src = png16(PngPixelFormat::Gray16Le, w, h, &stored);
    let decoded = encode_decode(&src);

    let via = rescale_16bit_to_8bit_via_sbit(&decoded, Sbit::Grayscale(5));
    assert_eq!(via.pixel_format, PngPixelFormat::Gray8);
    for (i, &v5) in source_5bit.iter().enumerate() {
        // Recover to 5 bits, then scale 5 -> 8.
        let recovered = recover_sbit(stored[i], 16, 5);
        assert_eq!(recovered, v5, "recovery px {i}");
        let expected = rescale_sample(v5, max_sample(5), 255) as u8;
        assert_eq!(via.data[i], expected, "sbit rescale px {i}");
    }
}

#[test]
fn eight_bit_decode_is_returned_unchanged() {
    // An 8-bit source never enters the 16->8 reduction path.
    let w = 4u32;
    let data: Vec<u8> = (0..(w * 3) as u8).collect();
    let src = PngImage {
        width: w,
        height: 1,
        pixel_format: PngPixelFormat::Rgb24,
        stride: (w * 3) as usize,
        data,
        palette: Vec::new(),
    };
    let bytes = encode_png_image(&src).expect("encode 8-bit");
    let decoded = decode_png(&bytes).expect("decode 8-bit");
    let out = rescale_16bit_to_8bit(&decoded);
    assert_eq!(out.pixel_format, PngPixelFormat::Rgb24);
    assert_eq!(out.data, decoded.data);
}
