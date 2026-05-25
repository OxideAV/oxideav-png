#![no_main]

//! Standalone encode → decode → re-encode roundtrip for both static
//! PNG and animated PNG (APNG) entry points.
//!
//! The companion `png_self_roundtrip.rs` target covers the framework
//! (`VideoFrame`-shaped) encode/decode pair via the `registry` feature.
//! This target instead drives the standalone `PngImage` / `ApngImage`
//! API directly so the no-`oxideav-core` build path also gets fuzz
//! coverage, and extends the roundtrip across multi-frame APNG output
//! (the framework target is single-image only).
//!
//! The contract under test, for both paths:
//!
//! 1. A `PngImage` built from fuzz input encodes without error
//!    (`encode_png_image` accepts any RGBA buffer whose `stride ==
//!    width * 4` and `data.len() == stride * height`).
//! 2. The encoded bytes round-trip through `decode_png` (or
//!    `decode_apng`) and yield equal dimensions + equal pixel data —
//!    `encode` is a left inverse of `decode` on encoder-emitted
//!    bitstreams.
//! 3. Re-encoding the decoded image is idempotent at the *image*
//!    level: `decode(encode(decode(encode(img)))) == decode(encode(img))`.
//!    (Byte-level idempotence isn't required — the encoder picks
//!    per-row filter heuristics that depend on neighbouring rows, so
//!    two encodes of bit-identical pixels may differ in their filter
//!    byte choices even when the decoded output matches.)
//!
//! For the APNG path the same contract holds per-frame: every
//! composited canvas in the re-decoded `ApngImage` must match the
//! corresponding canvas in the first decode.

use libfuzzer_sys::fuzz_target;
use oxideav_png::{
    decode_apng, decode_png, encode_apng, encode_png_image, PngImage, PngPixelFormat,
};

/// Cap on canvas dimensions for both targets. RGBA at 32×32 is 4 KiB
/// per frame, ×8 frames = 32 KiB worst-case, well within fuzz budget.
const MAX_DIM: u32 = 32;
const MAX_FRAMES: usize = 8;

fuzz_target!(|data: &[u8]| {
    // First byte: top nibble = mode bit (static vs APNG), low nibble
    // is reserved (consumed but ignored — frees future modes without
    // reshuffling the corpus).
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    if mode & 0x80 == 0 {
        try_static_roundtrip(rest);
    } else {
        try_apng_roundtrip(rest);
    }
});

/// Build one RGBA `PngImage` from fuzz bytes and round-trip it through
/// `encode_png_image` + `decode_png`. Asserts the decoded image
/// matches the encoder's input pixel-for-pixel, then re-encodes and
/// re-decodes to confirm the (image-level) idempotence property.
fn try_static_roundtrip(data: &[u8]) {
    let Some(img) = build_image(data) else {
        return;
    };

    let encoded = match encode_png_image(&img) {
        Ok(e) => e,
        // Encoder rejected a buffer the builder produced — that's a
        // bug in either side. Surface it as a panic so the fuzzer
        // records the input.
        Err(e) => panic!("encode_png_image rejected built image: {e}"),
    };
    let decoded = match decode_png(&encoded) {
        Ok(d) => d,
        Err(e) => panic!("decode_png rejected encoder output: {e}"),
    };
    assert_eq!(decoded.width, img.width, "static round-trip width");
    assert_eq!(decoded.height, img.height, "static round-trip height");
    assert_eq!(
        decoded.pixel_format, img.pixel_format,
        "static round-trip pixel_format"
    );
    assert_eq!(
        decoded.data, img.data,
        "static round-trip pixel data differs"
    );

    // Idempotence: re-encode + re-decode yields the same image.
    let encoded2 = match encode_png_image(&decoded) {
        Ok(e) => e,
        Err(e) => panic!("re-encode of decoded static image failed: {e}"),
    };
    let decoded2 = match decode_png(&encoded2) {
        Ok(d) => d,
        Err(e) => panic!("re-decode of static image failed: {e}"),
    };
    assert_eq!(
        decoded2.data, decoded.data,
        "encode/decode not idempotent (static)"
    );
}

/// Build a small APNG (1..=8 RGBA frames) from fuzz bytes and
/// round-trip it through `encode_apng` + `decode_apng`. Asserts every
/// composited canvas matches its source frame pixel-for-pixel.
fn try_apng_roundtrip(data: &[u8]) {
    let (&n_byte, rest) = match data.split_first() {
        Some(s) => s,
        None => return,
    };
    let n_frames = ((n_byte as usize) % MAX_FRAMES) + 1;

    let Some(first) = build_image(rest) else {
        return;
    };

    // Build N frames sharing the same dimensions/format. Vary content
    // by XORing in the frame index so successive frames differ.
    let mut frames: Vec<PngImage> = Vec::with_capacity(n_frames);
    for i in 0..n_frames {
        let mut pixels = first.data.clone();
        for (j, p) in pixels.iter_mut().enumerate() {
            *p ^= (i as u8).wrapping_mul((j % 7) as u8 + 1);
        }
        // Force alpha = 0xFF so blending in decode never zeroes channels
        // — we want exact byte equality, which requires opaque pixels
        // because the encoder always emits dispose=None / blend=Source
        // and the first-frame is the default image (no compositing).
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 0xFF;
        }
        frames.push(PngImage {
            width: first.width,
            height: first.height,
            pixel_format: PngPixelFormat::Rgba,
            stride: (first.width as usize) * 4,
            data: pixels,
            palette: Vec::new(),
        });
    }

    let encoded = match encode_apng(&frames, 10, 0) {
        Ok(e) => e,
        Err(e) => panic!("encode_apng rejected built frames: {e}"),
    };
    let decoded = match decode_apng(&encoded) {
        Ok(d) => d,
        Err(e) => panic!("decode_apng rejected encoder output: {e}"),
    };
    assert_eq!(decoded.frames.len(), frames.len(), "APNG frame count");
    assert_eq!(decoded.width, frames[0].width, "APNG canvas width");
    assert_eq!(decoded.height, frames[0].height, "APNG canvas height");

    for (i, (orig, got)) in frames.iter().zip(decoded.frames.iter()).enumerate() {
        assert_eq!(
            got.image.data, orig.data,
            "APNG frame {i} pixel data differs after round-trip"
        );
    }

    // Idempotence: re-encode the decoded composited frames and verify
    // a second round-trip produces the same canvases.
    let reframes: Vec<PngImage> = decoded.frames.iter().map(|f| f.image.clone()).collect();
    let encoded2 = match encode_apng(&reframes, 10, 0) {
        Ok(e) => e,
        Err(e) => panic!("re-encode of decoded APNG failed: {e}"),
    };
    let decoded2 = match decode_apng(&encoded2) {
        Ok(d) => d,
        Err(e) => panic!("re-decode of APNG failed: {e}"),
    };
    for (i, (orig, got)) in decoded
        .frames
        .iter()
        .zip(decoded2.frames.iter())
        .enumerate()
    {
        assert_eq!(
            got.image.data, orig.image.data,
            "APNG frame {i} not idempotent across encode/decode"
        );
    }
}

/// Derive one `PngImage` (RGBA) from fuzz input.
/// Layout: `[shape_byte][pixel_bytes...]`. `shape_byte` low nibble →
/// width in 1..=MAX_DIM; the remaining length determines height.
fn build_image(data: &[u8]) -> Option<PngImage> {
    let (&shape, rest) = data.split_first()?;
    let w = ((shape & 0x0F) as u32 % MAX_DIM) + 1;
    if rest.is_empty() {
        return None;
    }

    let max_pixels = (MAX_DIM as usize) * (MAX_DIM as usize);
    let avail_pixels = (rest.len() / 4).min(max_pixels);
    if avail_pixels == 0 {
        return None;
    }
    let w_usize = (w as usize).min(avail_pixels);
    let h = (avail_pixels / w_usize) as u32;
    if h == 0 {
        return None;
    }
    let used_bytes = (w_usize) * (h as usize) * 4;
    let mut pixels = rest[..used_bytes].to_vec();
    // Same opaque-alpha trick as in `try_apng_roundtrip` so a plain
    // encode/decode round-trip is bit-exact (the encoder doesn't apply
    // any blending to RGBA, but tRNS / palette resolution is not in
    // play here so straight equality is the right check).
    for px in pixels.chunks_exact_mut(4) {
        px[3] = 0xFF;
    }
    Some(PngImage {
        width: w_usize as u32,
        height: h,
        pixel_format: PngPixelFormat::Rgba,
        stride: w_usize * 4,
        data: pixels,
        palette: Vec::new(),
    })
}
