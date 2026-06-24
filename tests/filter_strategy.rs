//! End-to-end coverage for `PngEncoderOptions::filter_strategy`.
//!
//! The encoder consults W3C PNG3 §12.7 ("Filter selection") at three
//! sites: the non-interlaced ≥ 8-bit path, the Adam7 ≥ 8-bit path, and
//! the Adam7 sub-byte path. Each test below covers one site and
//! checks two properties:
//!
//! 1. **Wire correctness.** A `Fixed(f)` strategy emits the chosen
//!    filter type byte at the start of every row (and every Adam7
//!    pass row). Empty Adam7 passes still emit no filter byte
//!    (RFC 2083 §2.6 caution). Decoding the encoder's output reproduces
//!    the input image bit-exact regardless of strategy.
//!
//! 2. **Defaults.** `PngEncoderOptions::default().filter_strategy ==
//!    FilterStrategy::Adaptive` — the registered behaviour pre-r245 is
//!    preserved on callers that never set the new field.
//!
//! Bit-exact reconstruction is asserted via the standalone
//! `decode_png` round-trip: pixel data must survive every
//! strategy because the per-row filter is a lossless transform
//! (§9.1 "All filters are linear, … and are therefore reversible").

use oxideav_png::{
    decode_png, encode_png_image_with_options, FilterStrategy, FilterType, PngEncoderOptions,
    PngImage, PngPixelFormat,
};

/// Walk a PNG file in front-to-back chunk order and pull out every
/// IDAT chunk's payload into one contiguous buffer (mirroring how the
/// decoder concatenates them before inflating). Returns the
/// decompressed filtered-row stream.
fn collect_idat_inflated(png: &[u8]) -> Vec<u8> {
    let mut idat = Vec::new();
    // Skip 8-byte magic.
    let mut p = 8usize;
    while p + 12 <= png.len() {
        let len = u32::from_be_bytes([png[p], png[p + 1], png[p + 2], png[p + 3]]) as usize;
        let ty = &png[p + 4..p + 8];
        let payload = &png[p + 8..p + 8 + len];
        if ty == b"IDAT" {
            idat.extend_from_slice(payload);
        }
        if ty == b"IEND" {
            break;
        }
        p += 12 + len;
    }
    compcol::vec::decompress_to_vec::<compcol::zlib::Zlib>(&idat).expect("zlib decompress")
}

/// Build a small synthetic RGB image with deterministic gradient
/// content so the test is reproducible without committed fixtures.
fn make_rgb_image(w: u32, h: u32) -> PngImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            data.push((x * 7) as u8);
            data.push((y * 13) as u8);
            data.push(((x + y) * 5) as u8);
        }
    }
    PngImage {
        width: w,
        height: h,
        pixel_format: PngPixelFormat::Rgb24,
        stride: (w * 3) as usize,
        data,
        palette: Vec::new(),
    }
}

/// `Fixed(f)` writes filter byte `f as u8` at the head of every row
/// in a non-interlaced ≥ 8-bit encode.
#[test]
fn fixed_filter_appears_on_every_noninterlaced_row() {
    let img = make_rgb_image(16, 8);
    let row_bytes = 16 * 3;
    for f in [
        FilterType::None,
        FilterType::Sub,
        FilterType::Up,
        FilterType::Average,
        FilterType::Paeth,
    ] {
        let opts = PngEncoderOptions {
            filter_strategy: FilterStrategy::Fixed(f),
            ..Default::default()
        };
        let png = encode_png_image_with_options(&img, &opts).expect("encode");
        let stream = collect_idat_inflated(&png);
        // (1 + row_bytes) per row, height rows.
        assert_eq!(stream.len(), (1 + row_bytes) * 8);
        for y in 0..8 {
            let head = stream[y * (1 + row_bytes)];
            assert_eq!(
                head, f as u8,
                "row {y} expected filter {f:?} ({:?}); got {head}",
                f as u8
            );
        }
        // Decode survives every strategy bit-exact.
        let decoded = decode_png(&png).expect("decode");
        assert_eq!(decoded.data, img.data, "strategy {f:?} round-trip mismatch");
    }
}

/// `Fixed(f)` writes filter byte `f as u8` at the head of every
/// pass row in an Adam7 encode. Passes with zero dimensions emit
/// no filter byte (RFC 2083 §2.6 caution); the test image is
/// large enough that every pass produces ≥ 1 row.
#[test]
fn fixed_filter_appears_on_every_adam7_pass_row() {
    let img = make_rgb_image(16, 8);
    let opts = PngEncoderOptions {
        interlace: true,
        filter_strategy: FilterStrategy::Fixed(FilterType::Paeth),
        ..Default::default()
    };
    let png = encode_png_image_with_options(&img, &opts).expect("encode");
    let stream = collect_idat_inflated(&png);
    // The seven-pass concatenation walks the wire layout; every
    // filter byte in it must read `Paeth` (= 4) regardless of
    // which pass it belongs to.
    let mut p = 0usize;
    let mut seen_rows = 0usize;
    // The encoder lays the passes out per ADAM7 strides; the
    // expected (pw, ph) pairs for a 16×8 image work out to:
    //   pass 1: 2×1   pass 2: 2×1   pass 3: 4×1   pass 4: 4×2
    //   pass 5: 8×2   pass 6: 8×4   pass 7: 16×4
    // Bytes per row = pw * 3 (RGB).
    let pass_dims = [
        (2u32, 1u32),
        (2, 1),
        (4, 1),
        (4, 2),
        (8, 2),
        (8, 4),
        (16, 4),
    ];
    for &(pw, ph) in &pass_dims {
        let pass_row_bytes = (pw * 3) as usize;
        for _y in 0..ph {
            assert!(
                p + 1 + pass_row_bytes <= stream.len(),
                "ran off stream at row {seen_rows}"
            );
            assert_eq!(stream[p], FilterType::Paeth as u8, "row {seen_rows}");
            p += 1 + pass_row_bytes;
            seen_rows += 1;
        }
    }
    // The interlaced decode round-trips bit-exact too.
    let decoded = decode_png(&png).expect("decode");
    assert_eq!(decoded.data, img.data);
}

/// Adam7 sub-byte (ct=3 indexed at depth 4) honours `Fixed(None)`.
/// §12.7 recommends filter type 0 (`None`) for indexed and for
/// bit depths below 8; this test exercises both halves of that
/// recommendation in one encode + decode round-trip.
#[test]
fn fixed_filter_none_on_adam7_subbyte_indexed() {
    // 8×4 Pal8 ramp with 16-entry palette — every byte is already
    // pre-quantized into 0..16 so a depth-4 IHDR fits.
    let w = 8u32;
    let h = 4u32;
    let mut data = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            data.push(((x + y) & 0x0F) as u8);
        }
    }
    // Palette covers exactly the max index in `data` (plus one) ×3
    // bytes — the encoder splits `image.palette` into `PLTE || tRNS`
    // by computing `PLTE` length from `max_idx + 1` entries, so a
    // matching-size buffer means no `tRNS` tail. Content is filler;
    // this test only cares about the filter-byte at the row head.
    let max_idx = data.iter().copied().max().unwrap_or(0) as usize;
    let n_entries = max_idx + 1;
    let palette: Vec<u8> = (0..n_entries as u8)
        .flat_map(|i| [i * 16, 0, 255 - i * 16])
        .collect();
    let img = PngImage {
        width: w,
        height: h,
        pixel_format: PngPixelFormat::Pal8,
        stride: w as usize,
        data: data.clone(),
        palette,
    };

    let opts = PngEncoderOptions {
        interlace: true,
        bit_depth: Some(4),
        filter_strategy: FilterStrategy::Fixed(FilterType::None),
        ..Default::default()
    };
    let png = encode_png_image_with_options(&img, &opts).expect("encode");
    let stream = collect_idat_inflated(&png);

    // Walk every non-empty Adam7 pass and assert the filter byte at
    // the head of each pass row is 0 (= `None`).
    //
    // 8×4 sub-byte (depth = 4 → pixels_per_byte = 2) Adam7 pass
    // dimensions (pw, ph) per RFC 2083 §A.8 with ADAM7 starts/strides
    // `(sr, sc, rs, cs)` of `(0,0,8,8) (0,4,8,8) (4,0,8,4) (0,2,4,4)
    // (2,0,4,2) (0,1,2,2) (1,0,2,1)` (matches `decoder::ADAM7`).
    // Pass 3 has start_row = 4 ≥ height = 4 so ph = 0 (skipped — RFC
    // 2083 §2.6 "Caution: …some passes will be entirely empty"); the
    // empty pass emits zero filter type bytes.
    //   pass 1: 1×1 → row_bytes = ceil(1*4/8) = 1
    //   pass 2: 1×1 → 1
    //   pass 3:  empty (sr = 4 ≥ height)
    //   pass 4: 2×1 → ceil(2*4/8) = 1
    //   pass 5: 4×1 → ceil(4*4/8) = 2
    //   pass 6: 4×2 → 2
    //   pass 7: 8×2 → ceil(8*4/8) = 4
    let pass_dims: &[(usize, usize, usize)] = &[
        (1, 1, 1),
        (1, 1, 1),
        // pass 3 empty — no entry
        (2, 1, 1),
        (4, 1, 2),
        (4, 2, 2),
        (8, 2, 4),
    ];
    let mut p = 0usize;
    for &(_pw, ph, row_bytes) in pass_dims {
        for _y in 0..ph {
            assert!(p + 1 + row_bytes <= stream.len());
            assert_eq!(stream[p], 0, "expected filter None at sub-byte pass row");
            p += 1 + row_bytes;
        }
    }
    assert_eq!(p, stream.len(), "consumed every filtered byte");

    // Round-trip: decode the encoded stream and confirm the original
    // indexed payload comes back identically.
    let decoded = decode_png(&png).expect("decode");
    // Sub-byte indexed decode promotes to one-byte-per-pixel `Pal8`,
    // matching the source layout.
    assert_eq!(decoded.data, data);
}

#[test]
fn default_strategy_is_adaptive_and_matches_pre_r245_output() {
    // The default-options encode (no `filter_strategy` field set)
    // must produce exactly the same bytes as `Adaptive` set
    // explicitly — pre-r245 callers see no change.
    let img = make_rgb_image(8, 8);
    let a = encode_png_image_with_options(&img, &PngEncoderOptions::default()).expect("a");
    let b = encode_png_image_with_options(
        &img,
        &PngEncoderOptions {
            filter_strategy: FilterStrategy::Adaptive,
            ..Default::default()
        },
    )
    .expect("b");
    assert_eq!(a, b, "default options must equal explicit Adaptive");

    // And `FilterStrategy::default()` is `Adaptive` itself.
    assert_eq!(FilterStrategy::default(), FilterStrategy::Adaptive);
    assert_eq!(
        PngEncoderOptions::default().filter_strategy,
        FilterStrategy::Adaptive
    );
}

/// Empty Adam7 passes still emit no filter type bytes, even with a
/// `Fixed(_)` strategy active. An image whose width or height is
/// small enough triggers several empty passes per RFC 2083 §A.8 /
/// §2.6 caution ("some passes will be entirely empty"); this test
/// uses a 1×1 image where only pass 7 produces a row.
#[test]
fn fixed_filter_skips_empty_adam7_passes() {
    let img = PngImage {
        width: 1,
        height: 1,
        pixel_format: PngPixelFormat::Rgb24,
        stride: 3,
        data: vec![10, 20, 30],
        palette: Vec::new(),
    };
    let opts = PngEncoderOptions {
        interlace: true,
        filter_strategy: FilterStrategy::Fixed(FilterType::Sub),
        ..Default::default()
    };
    let png = encode_png_image_with_options(&img, &opts).expect("encode");
    let stream = collect_idat_inflated(&png);
    // For a 1×1 image only pass 7 (final pass) has one 1×1 row.
    // The filtered stream therefore holds exactly `1 + 3 = 4` bytes:
    // one filter byte (= `Sub` = 1) and the RGB triple. No other
    // filter bytes appear because every other pass is empty.
    assert_eq!(stream.len(), 4);
    assert_eq!(stream[0], FilterType::Sub as u8);
}

// ---- Brute (whole-image exhaustive) -------------------------------------

/// The encoded length of `img` under `strat`. The IDAT byte count is the
/// only part of the file the filter strategy moves, so the whole-file
/// length is a faithful comparator across strategies.
fn encoded_len(
    img: &PngImage,
    strat: FilterStrategy,
    interlace: bool,
    bit_depth: Option<u8>,
) -> usize {
    let opts = PngEncoderOptions {
        interlace,
        bit_depth,
        filter_strategy: strat,
        ..Default::default()
    };
    encode_png_image_with_options(img, &opts)
        .expect("encode")
        .len()
}

/// `Brute` re-decodes bit-exact and is never larger than `Adaptive` or
/// any `Fixed` choice on the non-interlaced ≥ 8-bit path (W3C PNG3
/// §12.7 "find what compresses best"). It measures real compressed size,
/// so by construction it picks the smallest of the six candidate streams.
#[test]
fn brute_is_smallest_noninterlaced() {
    let img = make_rgb_image(64, 48);
    let brute = encoded_len(&img, FilterStrategy::Brute, false, None);

    for cand in [
        FilterStrategy::Adaptive,
        FilterStrategy::Fixed(FilterType::None),
        FilterStrategy::Fixed(FilterType::Sub),
        FilterStrategy::Fixed(FilterType::Up),
        FilterStrategy::Fixed(FilterType::Average),
        FilterStrategy::Fixed(FilterType::Paeth),
    ] {
        let len = encoded_len(&img, cand, false, None);
        assert!(
            brute <= len,
            "Brute ({brute}) must not exceed candidate {cand:?} ({len})"
        );
    }

    // Bit-exact round-trip.
    let opts = PngEncoderOptions {
        filter_strategy: FilterStrategy::Brute,
        ..Default::default()
    };
    let png = encode_png_image_with_options(&img, &opts).expect("encode");
    let decoded = decode_png(&png).expect("decode");
    assert_eq!(decoded.data, img.data, "Brute round-trip mismatch");
}

/// `Brute` works through the Adam7 ≥ 8-bit path too: bit-exact
/// round-trip and no larger than the per-image candidates.
#[test]
fn brute_is_smallest_adam7() {
    let img = make_rgb_image(48, 32);
    let brute = encoded_len(&img, FilterStrategy::Brute, true, None);
    for cand in [
        FilterStrategy::Adaptive,
        FilterStrategy::Fixed(FilterType::Paeth),
        FilterStrategy::Fixed(FilterType::Up),
        FilterStrategy::Fixed(FilterType::None),
    ] {
        assert!(brute <= encoded_len(&img, cand, true, None));
    }
    let opts = PngEncoderOptions {
        interlace: true,
        filter_strategy: FilterStrategy::Brute,
        ..Default::default()
    };
    let png = encode_png_image_with_options(&img, &opts).expect("encode");
    assert_eq!(decode_png(&png).expect("decode").data, img.data);
}

/// `Brute` runs the per-sample bit-depth validation exactly once even
/// though it filters the image six times — an over-range sub-byte sample
/// is still a single clean encode error, not a panic or six errors.
#[test]
fn brute_subbyte_roundtrip_and_validation() {
    let w = 16u32;
    let h = 12u32;
    // 4-bit indexed (`Pal8`) ramp, all indices in 0..16. Indexed decode
    // returns the indices one-byte-per-pixel unchanged (unlike `Gray8`
    // sub-byte, which the decoder ×17-scales up per §13.12), so the
    // round-trip comparison is against the raw `data` directly.
    let mut data = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            data.push(((x + y) & 0x0F) as u8);
        }
    }
    // 16-entry palette so a depth-4 IHDR fits with no `tRNS` tail.
    let palette: Vec<u8> = (0u8..16).flat_map(|i| [i * 16, 0, 255 - i * 16]).collect();
    let img = PngImage {
        width: w,
        height: h,
        pixel_format: PngPixelFormat::Pal8,
        stride: w as usize,
        data: data.clone(),
        palette,
    };

    // Non-interlaced sub-byte Brute: round-trip bit-exact.
    let opts = PngEncoderOptions {
        bit_depth: Some(4),
        filter_strategy: FilterStrategy::Brute,
        ..Default::default()
    };
    let png = encode_png_image_with_options(&img, &opts).expect("encode");
    assert_eq!(decode_png(&png).expect("decode").data, data);

    // Adam7 sub-byte Brute: round-trip bit-exact.
    let opts_i = PngEncoderOptions {
        interlace: true,
        bit_depth: Some(4),
        filter_strategy: FilterStrategy::Brute,
        ..Default::default()
    };
    let png_i = encode_png_image_with_options(&img, &opts_i).expect("encode");
    assert_eq!(decode_png(&png_i).expect("decode").data, data);

    // Over-range sample → exactly one encode error under Brute.
    let mut bad = img.clone();
    bad.data[0] = 0xFF; // > 15
    let err = encode_png_image_with_options(&bad, &opts).unwrap_err();
    assert!(
        err.to_string().contains("exceeds"),
        "expected over-range error, got {err}"
    );
}
