# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `fuzz/fuzz_targets/apng_frame_walk.rs` cargo-fuzz target exercising
  the APNG composite state machine (`acTL` / `fcTL` / `fdAT`) on byte-
  level *valid* inputs that are combinatorially adversarial in their
  `blend_op` / `dispose_op` / `x_offset` / `y_offset` choices. Builds
  a base APNG with the standalone encoder, then walks the chunk stream
  and rewrites every `fcTL` payload (recomputing CRC32) before driving
  `parse_apng` + `decode_apng_info` across 1-8-frame chains. Pushes
  offsets into in-canvas, on-edge, just-past-canvas, and near-`u32::MAX`
  bands so the `Disposal::Background` clear, `Disposal::Previous`
  snapshot, and `Blend::Over` alpha-composite arms all get hit.
- `fuzz/fuzz_targets/encode_decode_roundtrip.rs` cargo-fuzz target
  asserting the standalone decoder is a right inverse of the standalone
  encoder for both static PNG (`encode_png_image` / `decode_png`) and
  animated PNG (`encode_apng` / `decode_apng`) on encoder-emitted
  bitstreams. Re-encodes the decoded image (and APNG frame chain) and
  decodes the result a second time to confirm image-level idempotence.
  Mirrors the `gif::roundtrip` / `flac::roundtrip` shape and covers
  the no-`oxideav-core` standalone build path.
- `fuzz/fuzz_targets/decode.rs` cargo-fuzz target driving the standalone
  decode entry points (`decode_png`, `decode_png_to_rgba`,
  `parse_metadata`, `parse_apng`, `decode_apng`) over arbitrary bytes —
  asserts the decoder never panics / aborts / overflows / OOMs on hostile
  input. Covers chunk-CRC framing, the IDAT zlib stream, per-row filters,
  sub-byte unpacking, Adam7 interlacing, PLTE/tRNS bounds, and the
  APNG acTL/fcTL/fdAT container + disposal/blend paths.

- `Splt` / `SpltEntry` covering the `sPLT` suggested-palette chunk (W3C
  PNG3 §11.3.4.4 / Table 25). A named standalone palette independent of
  `PLTE`: a 1-79-byte Latin-1 palette name (`tEXt`-keyword rules per
  §11.3.3.1 — printable `0x20..=0x7E` / `0xA1..=0xFF`, no leading /
  trailing / consecutive spaces), an 8- or 16-bit sample depth, and a
  list of `RGBA` + `frequency` entries (6-byte stride at depth 8,
  10-byte at depth 16; the post-depth payload must divide evenly by the
  stride). Decode rejects a missing `NUL` separator, an invalid name, a
  sample depth other than 8/16, and a misaligned entry region; encode
  additionally rejects an 8-bit sample value > 255. `sPLT` is the lone
  PNG metadata chunk that permits multiple instances (Table 7 "Multiple
  OK? Yes"), so `PngMetadata::splt` is a `Vec<Splt>`; the decoder
  accepts repeats but rejects duplicate palette names, and the encoder
  emits each instance before `IDAT` (§5.6 Table 7) in `Vec` order.
- `metadata` module: `Sbit`, `Phys`, `PhysUnit`, `Time`, `PngMetadata`
  with round-trip parsers + encoders for the `sBIT`, `pHYs`, and `tIME`
  ancillary chunks (RFC 2083 §4.2.5 / §4.2.6 / §4.2.8).
- `parse_metadata(&[u8]) -> PngMetadata` standalone entry point.
- `PngEncoderOptions::metadata: Option<PngMetadata>` so encoders can
  embed any subset of the three chunks at spec-compliant positions
  (sBIT before PLTE+IDAT; pHYs / tIME before IDAT).
- `Bkgd` (`Grayscale(u16)` / `Rgb(u16,u16,u16)` / `Palette(u8)`) and
  `Hist { frequencies: Vec<u16> }` covering the `bKGD` and `hIST`
  ancillary chunks (RFC 2083 §4.2.1 / §4.2.4, W3C PNG3 §11.3.4.1 /
  §11.3.4.2). Both round-trip through `PngMetadata` and emit at the
  spec-mandated "after PLTE, before IDAT" position. `bKGD` grayscale
  / RGB samples are range-checked against `(1 << IHDR.bit_depth) - 1`;
  indexed `bKGD` rejects palette indices ≥ the PLTE entry count; `hIST`
  decode rejects orphan chunks (no `PLTE`) and length mismatches.
- `Exif { data: Vec<u8> }` covering the `eXIf` ancillary chunk (W3C
  PNG3 §11.3.4.5). Carried as an opaque TIFF blob: decode validates the
  byte-order header (`II`/42 LE or `MM`/42 BE, §11.3.4.5.2) and rejects
  any other value, but does not interpret the TIFF directory. Round-trips
  through `PngMetadata`, emitted before `IDAT` (§5.6 Table 1); duplicate
  `eXIf` chunks are rejected on decode.
- `Srgb { rendering_intent: RenderingIntent }` covering the `sRGB`
  ancillary chunk (W3C PNG3 §11.3.2.5). One-byte payload selecting the
  ICC rendering intent (`0` Perceptual / `1` Relative colorimetric /
  `2` Saturation / `3` Absolute colorimetric, Table 16); reserved values
  `4..=255` are rejected on decode. Round-trips through `PngMetadata`,
  emitted before `PLTE` + `IDAT` (§5.6 Table 1) alongside `sBIT`;
  duplicate `sRGB` chunks are rejected on decode.
- `Cicp { color_primaries, transfer_function, matrix_coefficients,
  video_full_range_flag }` covering the `cICP` ancillary chunk (W3C
  PNG3 §11.3.2.6 / Table 18). Four-byte payload re-using the ITU-T
  H.273 colour-primaries / transfer-function / video-range registries;
  `matrix_coefficients` is pinned at `0` per §11.3.2.6 ("PNG is
  RGB-only") and `video_full_range_flag` is bounds-checked to `0..=1`
  (anything else is reserved by H.273 §8.3) — both rejected on decode
  outside their ranges. The first two bytes are round-tripped verbatim
  so forward-compatible H.273 code points still survive the codec.
  Round-trips through `PngMetadata`, emitted ahead of `sBIT` / `sRGB`
  in the pre-`PLTE` / pre-`IDAT` bucket (§4.3 Table 1 ranks `cICP` as
  the highest-precedence colour chunk); duplicate `cICP` chunks are
  rejected on decode.

### Fixed

- `decode_apng`: a malformed APNG `fcTL` whose `x_offset` / `y_offset`
  placed a frame entirely outside the canvas panicked in the
  Background-disposal clear path (`clear_region` turned the out-of-canvas
  start column into a byte offset past the canvas buffer and indexed an
  empty slice out of bounds). The clear now returns early when the start
  column is at or beyond the canvas width — the visible region is zero so
  there is nothing to clear. Found via the new `decode` fuzz target's
  attack-surface analysis; regression test in
  `tests/apng_malformed_offset.rs`.

## [0.1.6](https://github.com/OxideAV/oxideav-png/compare/v0.1.5...v0.1.6) - 2026-05-06

### Other

- reframe FFI claim — HW-engine crates use OS FFI by necessity
- drop dead `linkme` dep
- re-export __oxideav_entry from registry sub-module
- auto-register via oxideav_core::register! macro (linkme distributed slice)
- unify entry point on register(&mut RuntimeContext) ([#502](https://github.com/OxideAV/oxideav-png/pull/502))

## [0.1.5](https://github.com/OxideAV/oxideav-png/compare/v0.1.4...v0.1.5) - 2026-05-04

### Added

- decode_png_to_rgba convenience helper
- standalone-friendly retrofit (registry feature gate)

### Fixed

- *(clippy)* underscore-prefix unused height arg in rgba_video_frame

### Other

- pending cleanup after standalone refactor
- add external libpng cross-roundtrip (640x480 RGBA)
- add cargo-fuzz harness mirroring oxideav-webp

### Added

- Standalone-friendly retrofit: `oxideav-core` is now gated behind the
  default-on `registry` feature. Image-library consumers can depend on
  `oxideav-png` with `default-features = false` to skip the framework
  dependency tree entirely. The standalone API exposes
  `decode_png` / `encode_png_image` / `decode_apng` / `encode_apng`
  plus crate-local `PngImage` / `PngError` types using std primitives
  only. `Decoder` / `Encoder` / `Demuxer` / `Muxer` trait impls and
  `register*` helpers stay behind the `registry` feature gate.
- New `decode_png_to_rgba(&[u8]) -> Result<RgbaBitmap>` convenience
  entry point (and matching `RgbaBitmap` struct) for callers that just
  want pixels to blit. Promotes every supported source pixel format
  (`Gray8` / `Gray16Le` / `Rgb24` / `Rgb48Le` / `Pal8` with
  `PLTE` + `tRNS` / `Ya8` / `Rgba` / `Rgba64Le`) to 8-bit RGBA with
  α-fill for opaque sources. Eliminates the need for downstream
  consumers (e.g. scribe's CBDT path on Pal8 emoji glyphs) to walk
  `PLTE` + `tRNS` chunks themselves.

## [0.1.4](https://github.com/OxideAV/oxideav-png/compare/v0.1.3...v0.1.4) - 2026-05-03

### Other

- cargo fmt: pending rustfmt cleanup
- replace never-match regex with semver_check = false
- migrate to centralized OxideAV/.github reusable workflows
- drop duplicated #[allow(clippy::too_many_arguments)] on blit_sub_into_canvas
- adopt slim VideoFrame shape
- pin release-plz to patch-only bumps

## [0.1.3](https://github.com/OxideAV/oxideav-png/compare/v0.1.2...v0.1.3) - 2026-04-25

### Other

- drop oxideav-codec/oxideav-container shims, import from oxideav-core

## [0.1.2](https://github.com/OxideAV/oxideav-png/compare/v0.1.1...v0.1.2) - 2026-04-24

### Other

- bump miniz_oxide 0.7 → 0.9

## [0.1.1](https://github.com/OxideAV/oxideav-png/compare/v0.1.0...v0.1.1) - 2026-04-19

### Other

- cargo fmt
- add Adam7 interlaced encode via PngEncoderOptions

## [0.0.5](https://github.com/OxideAV/oxideav-png/compare/v0.0.4...v0.0.5) - 2026-04-19

### Other

- bump oxideav-container dep to "0.1"
- drop Cargo.lock — this crate is a library
- bump oxideav-pixfmt dep to "0.1"
- bump to oxideav-core 0.1.1 + codec 0.1.1
- migrate register() to CodecInfo builder
- bump oxideav-core + oxideav-codec deps to "0.1"
- thread &dyn CodecResolver through open()
- drop dead bindings, fold redundant branches

## [0.0.4](https://github.com/OxideAV/oxideav-png/compare/v0.0.3...v0.0.4) - 2026-04-17

### Other

- precisely describe ancillary chunk handling
