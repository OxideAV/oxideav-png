#![no_main]

//! Fuzz the PNG ancillary-chunk metadata round-trip.
//!
//! Each of the metadata chunks `oxideav-png` round-trips —
//! `sBIT`, `pHYs`, `tIME`, `bKGD`, `tRNS`, `eXIf`, `sRGB`, `cICP`,
//! `iCCP`, `gAMA`, `cHRM`, `mDCV`, `cLLI`, `sPLT`, `tEXt`, `zTXt`,
//! `iTXt` — has its own length / range / variant rules per RFC 2083
//! §4.2 and W3C PNG3 §11.3. The encoder accepts a
//! [`PngMetadata`] with any subset populated; the decoder parses each
//! chunk back, applies the same bounds checks, and the round-trip
//! property is that the two `PngMetadata` values compare equal on the
//! fields the encoder emitted.
//!
//! This harness fuzzes that property directly. From the fuzz input
//! we derive:
//!
//!   * a small fixed RGBA image (2×2; encoder cost negligible),
//!   * a per-chunk on / off bitmask + per-chunk numeric seeds, so the
//!     fuzzer can explore the combinatoric "which chunks present"
//!     space (16 single-instance chunks → 2¹⁶ = 65 536 subsets) plus
//!     every chunk's own value space (sample integers, keyword bodies,
//!     vec lengths) along its own axis,
//!   * `tEXt` / `zTXt` / `iTXt` / `sPLT` are `Vec<…>` chunks; the
//!     harness emits 0..=2 instances of each so the encoder's multi-
//!     instance handling and `sPLT`'s "distinct palette name" rule both
//!     see real pressure.
//!
//! Each population step uses values that pass the encoder's own
//! per-chunk validators (well-formed keywords, in-range samples,
//! deflate-method bytes, ASCII language tags). A fuzz-derived input
//! that would trigger a known encoder reject (e.g. a `Trns::Rgb` on a
//! `Gray8` source) is skipped at population time — the harness's job
//! is to widen the *encode → decode round-trip* search, not to retread
//! the encoder's reject paths (which the top-level `decode` fuzz target
//! already exercises by feeding raw mutated chunk bytes at the parser).
//!
//! Properties asserted:
//!
//!   1. **Encode is liveness on a well-formed metadata block.**
//!      `encode_png_image_with_options(img, opts)` returns `Ok(_)` for
//!      every metadata payload the harness chose to populate.
//!   2. **Parse on the encoder output returns `Ok(_)`.** The chunk
//!      stream the encoder emits is by construction a self-consistent
//!      PNG; `parse_metadata` accepting it is a tautology, but a panic
//!      / overflow / index-out-of-bounds inside the parser on
//!      encoder-emitted bytes would point at a parser bug.
//!   3. **Round-trip equality.** Every field set in `opts.metadata`
//!      compares equal to the same field on the parsed-back
//!      `PngMetadata`. The check is field-by-field rather than full-
//!      struct equality because the encoder may also emit chunks the
//!      harness did not request (currently it does not, but the
//!      property survives any future "encoder auto-emits a default
//!      sRGB" change without rewriting the harness).
//!   4. **Pixel bytes survive.** `decode_png(encoded).data` matches
//!      `img.data` so a metadata-bearing encode never corrupts the
//!      IDAT payload.

use libfuzzer_sys::fuzz_target;
use oxideav_png::{
    decode_png, encode_png_image_with_options, parse_metadata, Bkgd, Chrm, Cicp, Clli, Exif, Gama,
    Iccp, Itxt, Mdcv, Phys, PhysUnit, PngEncoderOptions, PngImage, PngMetadata, PngPixelFormat,
    RenderingIntent, Sbit, Splt, SpltEntry, Srgb, Text, Time, Ztxt,
};

/// 2×2 RGBA "checker" — every channel covered, opaque alpha. RGBA is
/// colour type 6, so `bKGD::Rgb`, `sBIT::Rgba`, and `tRNS::Rgba` (which
/// is illegal — ct=6 has full alpha) all behave per spec. The harness
/// uses ct=6's `sBIT::Rgba` variant.
fn rgba_2x2() -> PngImage {
    PngImage {
        width: 2,
        height: 2,
        pixel_format: PngPixelFormat::Rgba,
        stride: 8,
        data: vec![
            255, 0, 0, 255, // (0, 0) red
            0, 255, 0, 255, // (1, 0) green
            0, 0, 255, 255, // (0, 1) blue
            255, 255, 255, 255, // (1, 1) white
        ],
        palette: Vec::new(),
    }
}

/// "TestKey0" / "TestKey1" — both pass `validate_keyword` (1-79 printable
/// Latin-1, no leading / trailing / consecutive spaces, no NUL). The
/// fuzzer is not driving keyword-validator coverage here (it is exercised
/// by the parser-side `decode` target with raw chunk bytes); it is
/// driving the *round-trip* of valid keywords through the encoder and
/// back through the decoder.
fn safe_keyword(i: usize) -> String {
    format!("TestKey{i}")
}

/// Decode a UTF-8 fragment from `seed_bytes` of bounded length. Falls
/// back to a default if the fragment is empty or contains a `NUL`
/// (which the `tEXt` / `zTXt` / `iTXt` validators reject). Keeps the
/// fuzz harness inside the round-trip surface rather than the validator
/// reject surface.
fn safe_utf8_text(seed_bytes: &[u8], max_len: usize, fallback: &str) -> String {
    if seed_bytes.is_empty() {
        return fallback.to_string();
    }
    let take = seed_bytes.len().min(max_len);
    // Substitute every NUL (illegal in text body) with a safe ASCII
    // sentinel; replace every non-Latin-1 byte with '?' so the `tEXt`
    // / `zTXt` Latin-1 emission paths stay happy. (`iTXt` accepts full
    // UTF-8, so for that chunk a wider body would also work — we keep
    // the helper uniform.)
    let mut out = String::with_capacity(take);
    for &b in &seed_bytes[..take] {
        let c = match b {
            0 => 'x',
            0x01..=0x7F => b as char,
            _ => '?',
        };
        out.push(c);
    }
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

fuzz_target!(|data: &[u8]| {
    // Header: 4 bytes of bitmask (which chunks are present) + a stream
    // of value bytes after. Below this size we cannot populate even
    // an empty `PngMetadata`.
    if data.len() < 4 {
        return;
    }

    let mask = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let body = &data[4..];

    // Index into the value-bytes stream as we populate each chunk. A
    // single shared cursor keeps the input shape compact and lets the
    // fuzzer's coverage feedback steer towards chunk combinations it
    // hasn't seen.
    let mut cur = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        if cur + n > body.len() {
            None
        } else {
            let s = &body[cur..cur + n];
            cur += n;
            Some(s)
        }
    };

    let mut meta = PngMetadata::default();

    // --- Single-instance numeric chunks -----------------------------

    // bit 0: sBIT (RGBA variant for ct=6; each significant-bit count
    // 1..=8 per RFC 2083 §4.2.6).
    if mask & (1 << 0) != 0 {
        if let Some(b) = take(4) {
            let r = (b[0] % 8) + 1;
            let g = (b[1] % 8) + 1;
            let bl = (b[2] % 8) + 1;
            let a = (b[3] % 8) + 1;
            meta.sbit = Some(Sbit::Rgba(r, g, bl, a));
        }
    }

    // bit 1: pHYs (any u32; unit ∈ {Unknown, Metre}).
    if mask & (1 << 1) != 0 {
        if let Some(b) = take(9) {
            let x = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            let y = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
            let unit = if b[8] & 1 == 0 {
                PhysUnit::Unknown
            } else {
                PhysUnit::Metre
            };
            meta.phys = Some(Phys {
                pixels_per_unit_x: x,
                pixels_per_unit_y: y,
                unit,
            });
        }
    }

    // bit 2: tIME (year/month/day/hour/minute/second — every field
    // clamped into its valid range per RFC 2083 §4.2.8 so we don't hit
    // the encoder's range check).
    if mask & (1 << 2) != 0 {
        if let Some(b) = take(7) {
            let year = u16::from_le_bytes([b[0], b[1]]);
            let month = (b[2] % 12) + 1; // 1..=12
            let day = (b[3] % 31) + 1; // 1..=31
            let hour = b[4] % 24; // 0..=23
            let minute = b[5] % 60; // 0..=59
            let second = b[6] % 61; // 0..=60 (leap-second sentinel)
            meta.time = Some(Time {
                year,
                month,
                day,
                hour,
                minute,
                second,
            });
        }
    }

    // bit 3: bKGD::Rgb for ct=6, samples 0..=255 (IHDR bit depth 8 →
    // (2^8) - 1 cap; we mask to u8 before zero-extending to u16 to stay
    // in range).
    if mask & (1 << 3) != 0 {
        if let Some(b) = take(3) {
            meta.bkgd = Some(Bkgd::Rgb(b[0] as u16, b[1] as u16, b[2] as u16));
        }
    }

    // bit 4: tRNS — ct=6 (RGBA) bans tRNS per RFC 2083 §4.2.9 final
    // paragraph. We DO NOT populate `trns` here; the harness has no
    // colour-type-flexibility budget without growing in scope. The bit
    // is reserved for a future extension that builds different source
    // pixel formats.

    // bit 5: eXIf — opaque blob with a valid TIFF byte-order header.
    if mask & (1 << 5) != 0 {
        if let Some(b) = take(8) {
            // Pick between II / MM headers via low bit; remainder bytes
            // are arbitrary "TIFF body" the codec does not interpret.
            let mut payload = if b[0] & 1 == 0 {
                vec![0x49, 0x49, 0x2A, 0x00] // "II" + LE 42
            } else {
                vec![0x4D, 0x4D, 0x00, 0x2A] // "MM" + BE 42
            };
            payload.extend_from_slice(&b[1..]);
            meta.exif = Some(Exif { data: payload });
        }
    }

    // bit 6: sRGB rendering intent (0..=3, Table 16).
    if mask & (1 << 6) != 0 {
        if let Some(b) = take(1) {
            let intent = match b[0] % 4 {
                0 => RenderingIntent::Perceptual,
                1 => RenderingIntent::RelativeColorimetric,
                2 => RenderingIntent::Saturation,
                _ => RenderingIntent::AbsoluteColorimetric,
            };
            meta.srgb = Some(Srgb {
                rendering_intent: intent,
            });
        }
    }

    // bit 7: cICP — color_primaries + transfer_function arbitrary,
    // matrix_coefficients pinned at 0 (encoder reject otherwise per
    // PNG3 §11.3.2.6 "PNG is RGB-only"), full-range bounded to 0..=1.
    if mask & (1 << 7) != 0 {
        if let Some(b) = take(3) {
            meta.cicp = Some(Cicp {
                color_primaries: b[0],
                transfer_function: b[1],
                matrix_coefficients: 0,
                video_full_range_flag: b[2] & 1,
            });
        }
    }

    // bit 8: gAMA (any u32 — the spec's "should ignore 0" is a `should`,
    // not a `shall`, so 0 round-trips too).
    if mask & (1 << 8) != 0 {
        if let Some(b) = take(4) {
            meta.gama = Some(Gama {
                gamma_times_100000: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            });
        }
    }

    // bit 9: cHRM (eight u32 fields, any value).
    if mask & (1 << 9) != 0 {
        if let Some(b) = take(32) {
            let be = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
            meta.chrm = Some(Chrm {
                white_point_x: be(0),
                white_point_y: be(4),
                red_x: be(8),
                red_y: be(12),
                green_x: be(16),
                green_y: be(20),
                blue_x: be(24),
                blue_y: be(28),
            });
        }
    }

    // bit 10: mDCV — 24 bytes of fuzz-derived integers; the codec
    // round-trips them verbatim (no internal range check).
    if mask & (1 << 10) != 0 {
        if let Some(b) = take(24) {
            let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
            let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
            meta.mdcv = Some(Mdcv {
                primary_r_x: u16_at(0),
                primary_r_y: u16_at(2),
                primary_g_x: u16_at(4),
                primary_g_y: u16_at(6),
                primary_b_x: u16_at(8),
                primary_b_y: u16_at(10),
                white_point_x: u16_at(12),
                white_point_y: u16_at(14),
                max_luminance: u32_at(16),
                min_luminance: u32_at(20),
            });
        }
    }

    // bit 11: cLLI (two u32 fields, any value).
    if mask & (1 << 11) != 0 {
        if let Some(b) = take(8) {
            meta.clli = Some(Clli {
                max_content_light_level: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                max_frame_average_light_level: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            });
        }
    }

    // bit 12: iCCP — opaque ICC profile blob with a safe Latin-1 name.
    // Profile body is fuzz-derived raw bytes; the codec round-trips them
    // verbatim through a zlib compress / decompress cycle on the wire.
    if mask & (1 << 12) != 0 {
        // Bounded profile length so the encoder's deflate stage stays
        // cheap even on saturated inputs.
        let prof_len = 16;
        if let Some(b) = take(prof_len) {
            meta.iccp = Some(Iccp {
                name: "ICCprofile".to_string(),
                profile: b.to_vec(),
            });
        }
    }

    // --- Multi-instance Vec chunks ---------------------------------

    // bit 16+17 select 0/1/2 sPLT instances. Each instance carries a
    // distinct keyword (validator requires distinct names across the
    // datastream) and one fuzz-derived RGBA + frequency entry at 8-bit
    // sample depth.
    let n_splt = ((mask >> 16) & 0x3) as usize;
    for i in 0..n_splt {
        if let Some(b) = take(6) {
            meta.splt.push(Splt {
                name: safe_keyword(i),
                sample_depth: 8,
                entries: vec![SpltEntry {
                    red: b[0] as u16,
                    green: b[1] as u16,
                    blue: b[2] as u16,
                    alpha: b[3] as u16,
                    frequency: u16::from_le_bytes([b[4], b[5]]),
                }],
            });
        }
    }

    // bit 18+19 select 0/1/2 tEXt instances. Identical keywords are
    // permitted across instances (§4.2.7 ¶3), so we reuse `TestKey` for
    // both — exercising the "Multiple OK? Yes" path the parser must
    // not deduplicate.
    let n_text = ((mask >> 18) & 0x3) as usize;
    for _ in 0..n_text {
        if let Some(b) = take(8) {
            meta.texts.push(Text {
                keyword: "TestKey".to_string(),
                text: safe_utf8_text(b, 64, "fuzz"),
            });
        }
    }

    // bit 20+21 select 0/1/2 zTXt instances.
    let n_ztxt = ((mask >> 20) & 0x3) as usize;
    for _ in 0..n_ztxt {
        if let Some(b) = take(8) {
            meta.ztxts.push(Ztxt {
                keyword: "TestKey".to_string(),
                text: safe_utf8_text(b, 64, "fuzz"),
            });
        }
    }

    // bit 22+23 select 0/1/2 iTXt instances. Language tag is empty
    // ("language unspecified" per §11.3.3.4); translated keyword + text
    // come from the fuzz body.
    let n_itxt = ((mask >> 22) & 0x3) as usize;
    for i in 0..n_itxt {
        if let Some(b) = take(16) {
            let compressed = b[0] & 1 == 1;
            // Halve the buffer for translated keyword / text bodies.
            let mid = b.len() / 2;
            meta.itxts.push(Itxt {
                keyword: "TestKey".to_string(),
                compressed,
                language_tag: if i == 0 {
                    String::new()
                } else {
                    "en".to_string()
                },
                translated_keyword: safe_utf8_text(&b[1..mid], 32, ""),
                text: safe_utf8_text(&b[mid..], 32, "fuzz"),
            });
        }
    }

    // --- Encode --------------------------------------------------------

    let image = rgba_2x2();
    let opts = PngEncoderOptions {
        interlace: false,
        metadata: Some(meta.clone()),
        bit_depth: None,
    };
    let encoded = match encode_png_image_with_options(&image, &opts) {
        Ok(b) => b,
        // Every fuzz-derived metadata block above is constructed inside
        // the encoder's acceptance domain (sample bounds, keyword
        // shape, distinct sPLT names, RGB-pinned cICP matrix coeff).
        // An encoder reject on a metadata payload the harness believes
        // is well-formed is a discrepancy worth surfacing.
        Err(e) => panic!("encode_png_image_with_options rejected harness-built metadata: {e}"),
    };

    // --- Decode --------------------------------------------------------

    let parsed = match parse_metadata(&encoded) {
        Ok(m) => m,
        Err(e) => panic!("parse_metadata failed on encoder-emitted bytes: {e}"),
    };

    // Property 3: every populated field round-trips. We compare per
    // field because the encoder is allowed to add chunks (e.g. a future
    // version may auto-emit sRGB) without invalidating the round-trip
    // property of the explicitly-requested ones.
    assert_eq!(parsed.sbit, meta.sbit, "sBIT round-trip drift");
    assert_eq!(parsed.phys, meta.phys, "pHYs round-trip drift");
    assert_eq!(parsed.time, meta.time, "tIME round-trip drift");
    assert_eq!(parsed.bkgd, meta.bkgd, "bKGD round-trip drift");
    assert_eq!(parsed.trns, meta.trns, "tRNS round-trip drift");
    assert_eq!(parsed.exif, meta.exif, "eXIf round-trip drift");
    assert_eq!(parsed.srgb, meta.srgb, "sRGB round-trip drift");
    assert_eq!(parsed.cicp, meta.cicp, "cICP round-trip drift");
    assert_eq!(parsed.iccp, meta.iccp, "iCCP round-trip drift");
    assert_eq!(parsed.gama, meta.gama, "gAMA round-trip drift");
    assert_eq!(parsed.chrm, meta.chrm, "cHRM round-trip drift");
    assert_eq!(parsed.mdcv, meta.mdcv, "mDCV round-trip drift");
    assert_eq!(parsed.clli, meta.clli, "cLLI round-trip drift");
    assert_eq!(parsed.splt, meta.splt, "sPLT round-trip drift");
    assert_eq!(parsed.texts, meta.texts, "tEXt round-trip drift");
    assert_eq!(parsed.ztxts, meta.ztxts, "zTXt round-trip drift");
    assert_eq!(parsed.itxts, meta.itxts, "iTXt round-trip drift");

    // Property 4: the IDAT payload survived intact. A metadata-bearing
    // encode that silently corrupts pixel bytes would be the bug
    // category most likely to slip past the existing roundtrip targets,
    // which encode with `PngEncoderOptions::default()`.
    let decoded = match decode_png(&encoded) {
        Ok(img) => img,
        Err(e) => panic!("decode_png failed on encoder output: {e}"),
    };
    assert_eq!(decoded.width, image.width);
    assert_eq!(decoded.height, image.height);
    assert_eq!(decoded.pixel_format, image.pixel_format);
    assert_eq!(decoded.data, image.data, "pixel data corrupted by metadata");
});
