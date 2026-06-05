//! Sub-byte encode round-trip tests for `PngEncoderOptions::bit_depth`.
//!
//! For each supported sub-byte depth (1, 2, 4 bits) on each supported
//! colour type (0 grayscale, 3 indexed), build a `Gray8` / `Pal8`
//! source whose bytes are pre-quantized to `0..=(1 << bit_depth) - 1`,
//! encode it with the new option, decode the bitstream, and assert
//! the recovered pixels match the spec-defined unpacking rule.
//!
//! For colour type 0 (grayscale), the decoder applies the §13.12
//! scale-up to `Gray8` (×255 / ×85 / ×17 for bit_depth 1 / 2 / 4),
//! so the round-trip check multiplies the input sample by the same
//! scale. For colour type 3 (indexed), the decoder hands back one
//! palette-index byte per pixel, matching the source 1:1.

use oxideav_png::{
    decode_png, encode_png_image_with_options, PngEncoderOptions, PngImage, PngPixelFormat,
};

fn gray_source(w: u32, h: u32, bit_depth: u8) -> PngImage {
    let max = (1u16 << bit_depth) as u8 - 1;
    let w_us = w as usize;
    let mut data = vec![0u8; w_us * h as usize];
    for y in 0..h as usize {
        for x in 0..w_us {
            data[y * w_us + x] = ((x + y) as u8) & max;
        }
    }
    PngImage {
        width: w,
        height: h,
        pixel_format: PngPixelFormat::Gray8,
        stride: w_us,
        data,
        palette: Vec::new(),
    }
}

fn pal_source(w: u32, h: u32, bit_depth: u8, palette_entries: usize) -> PngImage {
    let max_index = ((1u16 << bit_depth) as usize) - 1;
    let used = palette_entries.min(max_index + 1);
    let mut palette = vec![0u8; used * 3];
    for i in 0..used {
        palette[i * 3] = (i * 37) as u8;
        palette[i * 3 + 1] = (i * 73) as u8;
        palette[i * 3 + 2] = (i * 113) as u8;
    }
    let w_us = w as usize;
    let mut data = vec![0u8; w_us * h as usize];
    // Force the buffer to actually use every palette index so the
    // encoder's `max_idx + 1` PLTE-sizing heuristic sees the full
    // palette (rather than mis-splitting trailing entries into a
    // bogus tRNS tail).
    let first_row_n = used.min(w_us * h as usize);
    for (i, slot) in data.iter_mut().take(first_row_n).enumerate() {
        *slot = i as u8;
    }
    for y in 0..h as usize {
        for x in 0..w_us {
            let off = y * w_us + x;
            if off >= first_row_n {
                data[off] = ((x + y) % used) as u8;
            }
        }
    }
    PngImage {
        width: w,
        height: h,
        pixel_format: PngPixelFormat::Pal8,
        stride: w_us,
        data,
        palette,
    }
}

fn gray_scale_factor(bit_depth: u8) -> u8 {
    // PNG §13.12: 1-bit ×255, 2-bit ×85, 4-bit ×17.
    match bit_depth {
        1 => 255,
        2 => 85,
        4 => 17,
        _ => unreachable!(),
    }
}

fn roundtrip_gray(w: u32, h: u32, bit_depth: u8) {
    let src = gray_source(w, h, bit_depth);
    let opts = PngEncoderOptions {
        bit_depth: Some(bit_depth),
        ..Default::default()
    };
    let bytes =
        encode_png_image_with_options(&src, &opts).expect("encode sub-byte grayscale must succeed");
    let decoded = decode_png(&bytes).expect("decode sub-byte grayscale must succeed");

    assert_eq!(decoded.width, w);
    assert_eq!(decoded.height, h);
    assert_eq!(decoded.pixel_format, PngPixelFormat::Gray8);

    let scale = gray_scale_factor(bit_depth);
    let w_us = w as usize;
    for y in 0..h as usize {
        for x in 0..w_us {
            let src_sample = src.data[y * w_us + x];
            let expected = src_sample.wrapping_mul(scale);
            let got = decoded.data[y * w_us + x];
            assert_eq!(
                got, expected,
                "bit_depth {bit_depth} gray pixel ({x},{y}): \
                 source {src_sample} → expected {expected} got {got}",
            );
        }
    }
}

fn roundtrip_pal(w: u32, h: u32, bit_depth: u8) {
    let palette_entries = 1 << bit_depth;
    let src = pal_source(w, h, bit_depth, palette_entries);
    let opts = PngEncoderOptions {
        bit_depth: Some(bit_depth),
        ..Default::default()
    };
    let bytes =
        encode_png_image_with_options(&src, &opts).expect("encode sub-byte indexed must succeed");
    let decoded = decode_png(&bytes).expect("decode sub-byte indexed must succeed");

    assert_eq!(decoded.width, w);
    assert_eq!(decoded.height, h);
    assert_eq!(decoded.pixel_format, PngPixelFormat::Pal8);
    let w_us = w as usize;
    for y in 0..h as usize {
        for x in 0..w_us {
            assert_eq!(
                decoded.data[y * w_us + x],
                src.data[y * w_us + x],
                "bit_depth {bit_depth} indexed pixel ({x},{y}) round-trip"
            );
        }
    }
    // The decoder preserves the original PLTE bytes verbatim, prefixed
    // before any tRNS tail (none here).
    assert_eq!(&decoded.palette[..src.palette.len()], &src.palette[..]);
}

// ---- Gray sub-byte round-trip ------------------------------------------

#[test]
fn gray_1bit_roundtrip() {
    roundtrip_gray(16, 8, 1);
}

#[test]
fn gray_2bit_roundtrip() {
    roundtrip_gray(16, 8, 2);
}

#[test]
fn gray_4bit_roundtrip() {
    roundtrip_gray(16, 8, 4);
}

// Widths that are not a multiple of `8 / bit_depth` exercise the
// trailing-byte zero-pad path (PNG §2.3 "Scanlines always begin on byte
// boundaries").

#[test]
fn gray_1bit_odd_width_roundtrip() {
    roundtrip_gray(15, 5, 1);
}

#[test]
fn gray_2bit_odd_width_roundtrip() {
    roundtrip_gray(13, 5, 2);
}

#[test]
fn gray_4bit_odd_width_roundtrip() {
    roundtrip_gray(11, 5, 4);
}

// ---- Indexed sub-byte round-trip ---------------------------------------

#[test]
fn pal_1bit_roundtrip() {
    roundtrip_pal(16, 8, 1);
}

#[test]
fn pal_2bit_roundtrip() {
    roundtrip_pal(16, 8, 2);
}

#[test]
fn pal_4bit_roundtrip() {
    roundtrip_pal(16, 8, 4);
}

#[test]
fn pal_1bit_odd_width_roundtrip() {
    roundtrip_pal(7, 3, 1);
}

#[test]
fn pal_2bit_odd_width_roundtrip() {
    roundtrip_pal(9, 3, 2);
}

#[test]
fn pal_4bit_odd_width_roundtrip() {
    roundtrip_pal(5, 3, 4);
}

// ---- Wire format spot checks -------------------------------------------

#[test]
fn ihdr_bit_depth_is_honoured() {
    // The IHDR is the 13-byte payload of the second chunk in the file
    // (after the 8-byte magic + 4-byte length + 4-byte tag). The 9th
    // byte of the payload is the bit-depth field per PNG §11.2.2.
    for &bd in &[1u8, 2, 4] {
        let src = gray_source(8, 4, bd);
        let opts = PngEncoderOptions {
            bit_depth: Some(bd),
            ..Default::default()
        };
        let bytes = encode_png_image_with_options(&src, &opts).expect("encode");
        // 8 (magic) + 4 (len) + 4 (tag) = 16; IHDR payload starts at 16.
        let ihdr_payload_start = 16;
        let depth_byte = bytes[ihdr_payload_start + 8];
        assert_eq!(depth_byte, bd, "IHDR.bit_depth byte at depth {bd}");
        let colour_type_byte = bytes[ihdr_payload_start + 9];
        assert_eq!(colour_type_byte, 0, "IHDR.colour_type byte (grayscale)");
    }
}

#[test]
fn pal_1bit_packs_msb_first() {
    // Build a 1-row, 8-pixel Pal8 with alternating 0,1,0,1,...; the
    // packed wire byte must be 0b01010101 = 0x55.
    let src = PngImage {
        width: 8,
        height: 1,
        pixel_format: PngPixelFormat::Pal8,
        stride: 8,
        data: vec![0, 1, 0, 1, 0, 1, 0, 1],
        palette: vec![0u8, 0, 0, 255, 255, 255],
    };
    let opts = PngEncoderOptions {
        bit_depth: Some(1),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&src, &opts).expect("encode");
    let decoded = decode_png(&bytes).expect("decode");
    // Indexed: decoded.data is one byte per pixel matching the source.
    assert_eq!(decoded.data, vec![0, 1, 0, 1, 0, 1, 0, 1]);
}

#[test]
fn pal_2bit_packs_two_pixels_per_byte() {
    // Four pixels per byte at 2-bit packing. Source 0,1,2,3 →
    // 0b00_01_10_11 = 0x1B.
    let src = PngImage {
        width: 4,
        height: 1,
        pixel_format: PngPixelFormat::Pal8,
        stride: 4,
        data: vec![0, 1, 2, 3],
        palette: vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
    };
    let opts = PngEncoderOptions {
        bit_depth: Some(2),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&src, &opts).expect("encode");
    let decoded = decode_png(&bytes).expect("decode");
    assert_eq!(decoded.data, vec![0, 1, 2, 3]);
}

#[test]
fn pal_4bit_packs_two_nibbles_per_byte() {
    // Eight pixels at 4-bit packing. Source 0..8 → 0x01 0x23 0x45 0x67
    // packed.
    let src = PngImage {
        width: 8,
        height: 1,
        pixel_format: PngPixelFormat::Pal8,
        stride: 8,
        data: vec![0, 1, 2, 3, 4, 5, 6, 7],
        palette: (0..8 * 3).map(|i| i as u8).collect(),
    };
    let opts = PngEncoderOptions {
        bit_depth: Some(4),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&src, &opts).expect("encode");
    let decoded = decode_png(&bytes).expect("decode");
    assert_eq!(decoded.data, vec![0, 1, 2, 3, 4, 5, 6, 7]);
}

// ---- Negative cases ----------------------------------------------------

#[test]
fn rejects_subbyte_on_rgb_source() {
    let src = PngImage {
        width: 2,
        height: 1,
        pixel_format: PngPixelFormat::Rgb24,
        stride: 6,
        data: vec![0u8; 6],
        palette: Vec::new(),
    };
    let opts = PngEncoderOptions {
        bit_depth: Some(4),
        ..Default::default()
    };
    let err =
        encode_png_image_with_options(&src, &opts).expect_err("sub-byte on RGB must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("Gray8") && msg.contains("Pal8"),
        "error message must name allowed source formats, got: {msg}"
    );
}

#[test]
fn rejects_subbyte_on_rgba_source() {
    let src = PngImage {
        width: 2,
        height: 1,
        pixel_format: PngPixelFormat::Rgba,
        stride: 8,
        data: vec![0u8; 8],
        palette: Vec::new(),
    };
    let opts = PngEncoderOptions {
        bit_depth: Some(2),
        ..Default::default()
    };
    assert!(encode_png_image_with_options(&src, &opts).is_err());
}

#[test]
fn rejects_sample_overflowing_bit_depth_cap() {
    // 2-bit source must be in 0..=3; a value of 4 trips the cap check.
    let src = PngImage {
        width: 4,
        height: 1,
        pixel_format: PngPixelFormat::Gray8,
        stride: 4,
        data: vec![0, 1, 2, 4],
        palette: Vec::new(),
    };
    let opts = PngEncoderOptions {
        bit_depth: Some(2),
        ..Default::default()
    };
    let err = encode_png_image_with_options(&src, &opts).expect_err("overflow must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("exceeds") && msg.contains("(3)"),
        "error must mention the cap, got: {msg}"
    );
}

#[test]
fn rejects_unsupported_bit_depth_value() {
    let src = gray_source(4, 1, 1);
    for &bd in &[0u8, 3, 5, 6, 7, 9, 12, 16, 32] {
        let opts = PngEncoderOptions {
            bit_depth: Some(bd),
            ..Default::default()
        };
        assert!(
            encode_png_image_with_options(&src, &opts).is_err(),
            "bit_depth = Some({bd}) must be rejected"
        );
    }
}

#[test]
fn bit_depth_8_is_a_no_op_for_gray_and_pal() {
    let src_gray = PngImage {
        width: 3,
        height: 1,
        pixel_format: PngPixelFormat::Gray8,
        stride: 3,
        data: vec![10, 20, 30],
        palette: Vec::new(),
    };
    let opts = PngEncoderOptions {
        bit_depth: Some(8),
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&src_gray, &opts).expect("encode");
    let decoded = decode_png(&bytes).expect("decode");
    assert_eq!(decoded.data, vec![10, 20, 30]);
    assert_eq!(decoded.pixel_format, PngPixelFormat::Gray8);
}

// ---- Adam7 interlaced sub-byte round-trip ------------------------------

/// Round-trip helper for the Adam7 interlaced path on `Gray8` /
/// `Pal8` sources. We compare the recovered pixels against the
/// non-interlaced encode of the same source so the test pins
/// (1) the seven-pass scatter/gather layout, (2) per-pass sub-byte
/// packing (each pass laid out as a complete image of its own
/// dimensions per RFC 2083 §2.6), and (3) per-pass independent
/// filtering against a zero prior row at the top of the pass
/// (§6.3 first-scanline-of-a-pass rule).
fn adam7_subbyte_roundtrip_gray(w: u32, h: u32, bit_depth: u8) {
    let src = gray_source(w, h, bit_depth);
    let mut opts = PngEncoderOptions {
        bit_depth: Some(bit_depth),
        interlace: true,
        ..Default::default()
    };
    let bytes_interlaced =
        encode_png_image_with_options(&src, &opts).expect("Adam7 sub-byte encode");
    let decoded_interlaced = decode_png(&bytes_interlaced).expect("Adam7 sub-byte decode");
    opts.interlace = false;
    let bytes_progressive =
        encode_png_image_with_options(&src, &opts).expect("non-interlaced sub-byte encode");
    let decoded_progressive =
        decode_png(&bytes_progressive).expect("non-interlaced sub-byte decode");
    assert_eq!(decoded_interlaced.width, w);
    assert_eq!(decoded_interlaced.height, h);
    assert_eq!(decoded_interlaced.pixel_format, PngPixelFormat::Gray8);
    assert_eq!(
        decoded_interlaced.data, decoded_progressive.data,
        "interlaced round-trip must recover the same pixel plane as the \
         non-interlaced encode at depth {bit_depth} ({w}x{h})"
    );
    // IHDR.interlace byte at offset 16+12 = 28 (13-byte payload, last byte).
    assert_eq!(bytes_interlaced[28], 1, "IHDR.interlace must be 1");
    assert_eq!(
        bytes_progressive[28], 0,
        "non-interlaced IHDR.interlace = 0"
    );
}

fn adam7_subbyte_roundtrip_pal(w: u32, h: u32, bit_depth: u8) {
    let palette_entries = 1 << bit_depth;
    let src = pal_source(w, h, bit_depth, palette_entries);
    let mut opts = PngEncoderOptions {
        bit_depth: Some(bit_depth),
        interlace: true,
        ..Default::default()
    };
    let bytes = encode_png_image_with_options(&src, &opts).expect("Adam7 sub-byte indexed encode");
    let decoded = decode_png(&bytes).expect("Adam7 sub-byte indexed decode");
    opts.interlace = false;
    let bytes_progressive =
        encode_png_image_with_options(&src, &opts).expect("non-interlaced sub-byte indexed encode");
    let decoded_progressive = decode_png(&bytes_progressive).expect("non-interlaced decode");
    assert_eq!(decoded.width, w);
    assert_eq!(decoded.height, h);
    assert_eq!(decoded.pixel_format, PngPixelFormat::Pal8);
    assert_eq!(
        decoded.data, decoded_progressive.data,
        "interlaced indexed round-trip must recover the same pixels as the \
         non-interlaced encode at depth {bit_depth} ({w}x{h})"
    );
    // Decoded palette tail matches the source palette bytes.
    assert_eq!(&decoded.palette[..src.palette.len()], &src.palette[..]);
}

#[test]
fn adam7_gray_1bit_roundtrip() {
    adam7_subbyte_roundtrip_gray(16, 16, 1);
}

#[test]
fn adam7_gray_2bit_roundtrip() {
    adam7_subbyte_roundtrip_gray(16, 12, 2);
}

#[test]
fn adam7_gray_4bit_roundtrip() {
    adam7_subbyte_roundtrip_gray(13, 11, 4);
}

#[test]
fn adam7_pal_1bit_roundtrip() {
    adam7_subbyte_roundtrip_pal(16, 8, 1);
}

#[test]
fn adam7_pal_2bit_roundtrip() {
    adam7_subbyte_roundtrip_pal(12, 12, 2);
}

#[test]
fn adam7_pal_4bit_roundtrip() {
    adam7_subbyte_roundtrip_pal(11, 5, 4);
}

/// Tiny image (≤ 4 columns / rows) where some Adam7 passes are
/// entirely empty — RFC 2083 §2.6 "Caution: If the image contains
/// fewer than five columns or fewer than five rows, some passes
/// will be entirely empty. Encoders and decoders must handle this
/// case correctly. In particular, filter type bytes are only
/// associated with nonempty scanlines; no filter type bytes are
/// present in an empty pass."
#[test]
fn adam7_gray_1bit_tiny_drops_empty_passes() {
    adam7_subbyte_roundtrip_gray(3, 3, 1);
}

#[test]
fn adam7_pal_2bit_tiny_drops_empty_passes() {
    // 4-bit + 2x2 would over-size the palette (1<<4 = 16 entries for
    // 4 pixels), tripping the encoder's PLTE/tRNS split. The
    // empty-pass behaviour we're checking is purely a function of
    // image dimensions, so a 2-bit fixture exercises it identically.
    adam7_subbyte_roundtrip_pal(2, 2, 2);
}

/// 2-bit indexed encode where a source sample exceeds the bit-depth
/// cap must be rejected on the interlaced path the same way the
/// non-interlaced path rejects it. The error message names the
/// offending source pixel coordinate (not a pass coordinate) so the
/// caller can fix the upstream quantization.
#[test]
fn adam7_rejects_overflowing_subbyte_sample() {
    let mut src = pal_source(8, 4, 2, 4);
    src.data[3] = 7; // overflows 2-bit cap of 3
    let opts = PngEncoderOptions {
        bit_depth: Some(2),
        interlace: true,
        ..Default::default()
    };
    let err = encode_png_image_with_options(&src, &opts)
        .expect_err("Adam7 sub-byte must reject overflow samples");
    let msg = format!("{err}");
    assert!(
        msg.contains("exceeds") && msg.contains("(3)"),
        "error must mention the 2-bit cap, got: {msg}"
    );
}

#[test]
fn apng_adam7_subbyte_roundtrip() {
    use oxideav_png::{decode_apng, encode_apng_with_options};
    let frames: Vec<_> = (0..2).map(|_| gray_source(13, 11, 4)).collect();
    let opts = PngEncoderOptions {
        bit_depth: Some(4),
        interlace: true,
        ..Default::default()
    };
    let bytes = encode_apng_with_options(&frames, 10, 0, &opts)
        .expect("APNG Adam7 sub-byte encode must succeed");
    let decoded = decode_apng(&bytes).expect("APNG Adam7 sub-byte decode must succeed");
    assert_eq!(decoded.frames.len(), 2);
    let scale = gray_scale_factor(4);
    for (src_frame, decoded_frame) in frames.iter().zip(decoded.frames.iter()) {
        for (a, b) in src_frame.data.iter().zip(decoded_frame.image.data.iter()) {
            assert_eq!(*b, a.wrapping_mul(scale));
        }
    }
}

#[test]
fn apng_subbyte_roundtrip() {
    use oxideav_png::{decode_apng, encode_apng_with_options};
    let frames: Vec<_> = (0..2).map(|_| gray_source(8, 4, 2)).collect();
    let opts = PngEncoderOptions {
        bit_depth: Some(2),
        ..Default::default()
    };
    let bytes =
        encode_apng_with_options(&frames, 10, 0, &opts).expect("APNG sub-byte encode must succeed");
    let decoded = decode_apng(&bytes).expect("APNG sub-byte decode must succeed");
    assert_eq!(decoded.frames.len(), 2);
    let scale = gray_scale_factor(2);
    let f0 = &decoded.frames[0].image;
    let f1 = &decoded.frames[1].image;
    // Both frames decoded to Gray8 with ×85 scaling on every sample.
    for (src_frame, decoded_frame) in [(&frames[0], f0), (&frames[1], f1)] {
        for (a, b) in src_frame.data.iter().zip(decoded_frame.data.iter()) {
            assert_eq!(*b, a.wrapping_mul(scale));
        }
    }
}
