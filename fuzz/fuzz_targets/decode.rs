#![no_main]

//! Decode arbitrary fuzz-supplied bytes through every standalone PNG /
//! APNG decode entry point. None of these calls may panic / abort /
//! integer-overflow (debug) / index out of bounds / OOM regardless of
//! how malformed the input is — they must always *return* a `Result`.
//!
//! PNG's decode path is a deep attack surface:
//!   * the 8-byte signature + length-prefixed chunk framing + per-chunk
//!     CRC (`parse_all_chunks`),
//!   * the 13-byte `IHDR` (attacker-controlled width / height / bit-depth
//!     / colour-type / interlace),
//!   * the concatenated `IDAT` zlib stream (`compcol` inflate, whose
//!     output size must be cross-checked against the IHDR-derived
//!     `(1 + row_bytes) * height`),
//!   * per-row filter reconstruction (None / Sub / Up / Average / Paeth),
//!   * sub-byte (1/2/4-bit) gray + indexed unpacking,
//!   * Adam7 seven-pass interlacing (per-pass synthetic dimensions),
//!   * `PLTE` + `tRNS` palette bounds, and
//!   * the APNG container: `acTL` / `fcTL` / `fdAT`, per-frame sub-canvas
//!     dimensions, arbitrary `x_offset` / `y_offset` blit, and the
//!     None / Background / Previous disposal + Source / Over blend modes.
//!
//! Every return value is intentionally discarded — the contract under
//! test is *liveness*: malformed input yields `Err(PngError::…)`,
//! well-formed input yields `Ok`, and neither path may crash.

use libfuzzer_sys::fuzz_target;
use oxideav_png::{
    decode_apng, decode_png, decode_png_over_background, decode_png_to_rgba, parse_apng,
    parse_metadata,
};

fuzz_target!(|data: &[u8]| {
    // Static single-image decode (any colour type / bit depth / interlace).
    let _ = decode_png(data);

    // One-shot promotion to 8-bit RGBA — exercises palette resolution,
    // grayscale widening, 16->8 truncation and alpha fill on top of the
    // raw decode.
    let _ = decode_png_to_rgba(data);

    // §13.15/§13.16 background composite: resolves the bKGD chunk (or the
    // medium-grey default) and blends straight alpha over it in linear
    // light. Drives both the chunk-resolution arm (None override) and the
    // caller-override arm on top of the same decode surface.
    let _ = decode_png_over_background(data, None);
    let _ = decode_png_over_background(data, Some([0, 128, 255]));

    // Ancillary-chunk metadata parser (sBIT / pHYs / tIME / bKGD / hIST /
    // eXIf / sRGB / cICP / sPLT) — its own bounds-checking surface.
    let _ = parse_metadata(data);

    // APNG container split + full composite. `decode_apng` calls
    // `parse_apng` internally, but parsing on its own is cheap and widens
    // coverage of the acTL/fcTL/fdAT framing for inputs that fail to
    // composite.
    let _ = parse_apng(data);
    let _ = decode_apng(data);
});
