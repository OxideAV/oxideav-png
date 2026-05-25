#![no_main]

//! Standalone encode → decode → re-encode round-trip for both static PNG
//! (`encode_png_image` / `decode_png`) and animated PNG (`encode_apng` /
//! `decode_apng`). Asserts the decoder is a right inverse of the encoder
//! on encoder-emitted bitstreams (per-frame pixel-byte equality), then
//! re-encodes + re-decodes to confirm image-level idempotence.
//!
//! Mirrors the `gif::roundtrip` / `flac::roundtrip` shape and covers the
//! no-`oxideav-core` standalone build path (the existing
//! `png_self_roundtrip` target exercises the framework `VideoFrame`
//! surface — this one stays inside the `image::PngImage` API).
//!
//! Pixel-format restriction: the harness forces opaque alpha
//! (α = 0xFF) on every pixel of every Rgba / Ya8 / Rgba64Le frame so
//! the encoder's per-row filter heuristic always emits a deterministic
//! filter chain and the decoder returns the exact bytes the encoder
//! consumed. Without this clamp, the same encoder→decode cycle is
//! still loss-free for non-palette formats, but indexed / tRNS paths
//! make the comparison harness needlessly fragile.

use libfuzzer_sys::fuzz_target;
use oxideav_png::{
    decode_apng, decode_png, encode_apng, encode_png_image, PngImage, PngPixelFormat,
};

/// Maximum per-frame dimension. Static + animated round-trip cost is
/// quadratic in `width * height`, so cap both small.
const MAX_DIM: u32 = 24;
const MIN_DIM: u32 = 1;
/// Frame-count cap for the APNG arm.
const MAX_FRAMES: usize = 6;

fuzz_target!(|data: &[u8]| {
    let Some(plan) = Plan::from_fuzz_input(data) else {
        return;
    };
    match plan {
        Plan::Static {
            width,
            height,
            format,
            seed,
        } => static_roundtrip(width, height, format, seed),
        Plan::Apng {
            width,
            height,
            format,
            seed,
            num_frames,
            delay_cs,
            num_plays,
        } => apng_roundtrip(width, height, format, seed, num_frames, delay_cs, num_plays),
    }
});

fn static_roundtrip(width: u32, height: u32, format: PngPixelFormat, seed: u8) {
    let original = solid_frame(width, height, format, seed);
    let encoded = match encode_png_image(&original) {
        Ok(b) => b,
        // Encoder rejecting a self-constructed input is itself a bug —
        // but the encoder also rejects width=0 / height=0 across the
        // surface, and we already bounded those out at plan time.
        // Surface anything else as a panic so the fuzzer bins it.
        Err(e) => panic!("encode_png_image rejected synthetic frame: {e}"),
    };
    let decoded = match decode_png(&encoded) {
        Ok(img) => img,
        Err(e) => panic!("decode_png failed on encoder output: {e}"),
    };
    assert_image_eq(&original, &decoded, "static encode→decode");

    // Re-encode the decoded image and decode that — the encoder is
    // deterministic on a clean frame, so the bytes must match.
    let re_encoded =
        encode_png_image(&decoded).expect("encode_png_image rejected its own decode output");
    assert_eq!(
        encoded, re_encoded,
        "static encode→decode→encode is not byte-identical"
    );
    let re_decoded =
        decode_png(&re_encoded).expect("decode_png failed on second-cycle encoder output");
    assert_image_eq(&original, &re_decoded, "static encode→decode→encode→decode");
}

fn apng_roundtrip(
    width: u32,
    height: u32,
    format: PngPixelFormat,
    seed: u8,
    num_frames: usize,
    delay_cs: u16,
    num_plays: u32,
) {
    let frames: Vec<PngImage> = (0..num_frames)
        .map(|i| solid_frame(width, height, format, seed.wrapping_add(i as u8)))
        .collect();
    let encoded = match encode_apng(&frames, delay_cs, num_plays) {
        Ok(b) => b,
        Err(e) => panic!("encode_apng rejected synthetic frame chain: {e}"),
    };
    let decoded = match decode_apng(&encoded) {
        Ok(a) => a,
        Err(e) => panic!("decode_apng failed on encoder output: {e}"),
    };
    assert_eq!(
        decoded.frames.len(),
        num_frames,
        "decoded APNG frame count {} != input {}",
        decoded.frames.len(),
        num_frames,
    );
    assert_eq!(decoded.width, width, "decoded APNG width drift");
    assert_eq!(decoded.height, height, "decoded APNG height drift");
    assert_eq!(
        decoded.pixel_format, format,
        "decoded APNG pixel-format drift"
    );
    for (i, (got, want)) in decoded.frames.iter().zip(frames.iter()).enumerate() {
        assert_image_eq(want, &got.image, &format!("apng frame {i} encode→decode"));
    }

    // Re-encode the decoded frame chain. Idempotence is at the *image*
    // level, not the bitstream level — re-encoding may produce a different
    // byte stream than `encoded` because the standalone APNG encoder
    // chooses filter / window parameters from the input pixels, which
    // happen to match here. Assert image equality on the second round.
    let re_encoded_frames: Vec<PngImage> = decoded.frames.iter().map(|f| f.image.clone()).collect();
    let re_encoded = encode_apng(&re_encoded_frames, delay_cs, num_plays)
        .expect("encode_apng rejected its own decode output");
    let re_decoded =
        decode_apng(&re_encoded).expect("decode_apng failed on second-cycle encoder output");
    assert_eq!(
        re_decoded.frames.len(),
        num_frames,
        "second-cycle APNG frame count drift"
    );
    for (i, (got, want)) in re_decoded.frames.iter().zip(frames.iter()).enumerate() {
        assert_image_eq(
            want,
            &got.image,
            &format!("apng frame {i} second-cycle encode→decode"),
        );
    }
}

/// Compare two [`PngImage`]s for pixel-byte equality.
fn assert_image_eq(expected: &PngImage, actual: &PngImage, ctx: &str) {
    assert_eq!(
        actual.width, expected.width,
        "{ctx}: width drift {} != {}",
        actual.width, expected.width,
    );
    assert_eq!(
        actual.height, expected.height,
        "{ctx}: height drift {} != {}",
        actual.height, expected.height,
    );
    assert_eq!(
        actual.pixel_format, expected.pixel_format,
        "{ctx}: pixel-format drift {:?} != {:?}",
        actual.pixel_format, expected.pixel_format,
    );
    // Decoder output is always tightly packed; encoder input may have
    // stride > width*bpp. Compare the visible pixels row by row.
    let bpp = expected.bytes_per_pixel();
    let row_len = expected.width as usize * bpp;
    for row in 0..expected.height as usize {
        let e_start = row * expected.stride;
        let a_start = row * actual.stride;
        assert_eq!(
            &actual.data[a_start..a_start + row_len],
            &expected.data[e_start..e_start + row_len],
            "{ctx}: pixel drift on row {row}",
        );
    }
}

/// Choose a pixel format from a fuzz-derived byte. Restricted to the
/// formats whose round-trip is unambiguous without metadata (palette is
/// excluded — its round-trip needs PLTE bytes the standalone encoder
/// derives heuristically).
fn pixel_format_for(sel: u8) -> PngPixelFormat {
    match sel % 7 {
        0 => PngPixelFormat::Gray8,
        1 => PngPixelFormat::Gray16Le,
        2 => PngPixelFormat::Rgb24,
        3 => PngPixelFormat::Rgb48Le,
        4 => PngPixelFormat::Ya8,
        5 => PngPixelFormat::Rgba,
        _ => PngPixelFormat::Rgba64Le,
    }
}

/// Build a deterministic single-colour frame. All-same pixels give the
/// encoder's filter heuristic an unambiguous answer (Sub or None always
/// wins), so the encode→decode→encode arm produces byte-identical
/// output. Alpha is forced opaque (α = 0xFF for 8-bit, 0xFFFF for 16-bit)
/// so RGBA / Ya8 paths stay in the no-tRNS regime.
fn solid_frame(width: u32, height: u32, format: PngPixelFormat, seed: u8) -> PngImage {
    let bpp = format.bytes_per_pixel();
    let stride = width as usize * bpp;
    let mut data = Vec::with_capacity(stride * height as usize);
    let pixel: Vec<u8> = match format {
        PngPixelFormat::Gray8 => vec![seed],
        PngPixelFormat::Gray16Le => {
            let v = (seed as u16) | ((seed.wrapping_mul(3) as u16) << 8);
            vec![(v & 0xFF) as u8, (v >> 8) as u8]
        }
        PngPixelFormat::Rgb24 => vec![seed, seed.wrapping_mul(3), seed.wrapping_mul(7)],
        PngPixelFormat::Rgb48Le => {
            let r = (seed as u16) | ((seed.wrapping_mul(3) as u16) << 8);
            let g = (seed.wrapping_mul(5) as u16) | ((seed.wrapping_mul(11) as u16) << 8);
            let b = (seed.wrapping_mul(7) as u16) | ((seed.wrapping_mul(13) as u16) << 8);
            vec![
                (r & 0xFF) as u8,
                (r >> 8) as u8,
                (g & 0xFF) as u8,
                (g >> 8) as u8,
                (b & 0xFF) as u8,
                (b >> 8) as u8,
            ]
        }
        PngPixelFormat::Ya8 => vec![seed, 0xFF],
        PngPixelFormat::Rgba => vec![seed, seed.wrapping_mul(3), seed.wrapping_mul(7), 0xFF],
        PngPixelFormat::Rgba64Le => vec![
            seed,
            seed.wrapping_mul(3),
            seed.wrapping_mul(5),
            seed.wrapping_mul(7),
            seed.wrapping_mul(11),
            seed.wrapping_mul(13),
            0xFF,
            0xFF,
        ],
        // We exclude Pal8 from `pixel_format_for`, so this arm is dead;
        // a fall-through to an empty pixel is fine if the enum ever
        // grows another variant.
        PngPixelFormat::Pal8 => vec![seed],
    };
    for _ in 0..(width as usize * height as usize) {
        data.extend_from_slice(&pixel);
    }
    PngImage {
        width,
        height,
        pixel_format: format,
        stride,
        data,
        palette: Vec::new(),
    }
}

enum Plan {
    Static {
        width: u32,
        height: u32,
        format: PngPixelFormat,
        seed: u8,
    },
    Apng {
        width: u32,
        height: u32,
        format: PngPixelFormat,
        seed: u8,
        num_frames: usize,
        delay_cs: u16,
        num_plays: u32,
    },
}

impl Plan {
    fn from_fuzz_input(data: &[u8]) -> Option<Self> {
        // Layout (static):
        //   [0] mode (low bit: 0 → static, 1 → apng)
        //   [1] width sel
        //   [2] height sel
        //   [3] format sel
        //   [4] seed
        //   apng adds:
        //     [5] num_frames sel (1..=MAX_FRAMES)
        //     [6..8] delay_cs (le u16)
        //     [8] num_plays low byte
        if data.len() < 5 {
            return None;
        }
        let width = MIN_DIM + (data[1] as u32 % (MAX_DIM - MIN_DIM + 1));
        let height = MIN_DIM + (data[2] as u32 % (MAX_DIM - MIN_DIM + 1));
        let format = pixel_format_for(data[3]);
        let seed = data[4];

        if data[0] & 1 == 0 {
            Some(Plan::Static {
                width,
                height,
                format,
                seed,
            })
        } else {
            if data.len() < 9 {
                return None;
            }
            let num_frames = ((data[5] as usize) % MAX_FRAMES) + 1;
            let delay_cs = u16::from_le_bytes([data[6], data[7]]) % 200;
            let num_plays = data[8] as u32;
            Some(Plan::Apng {
                width,
                height,
                format,
                seed,
                num_frames,
                delay_cs,
                num_plays,
            })
        }
    }
}
