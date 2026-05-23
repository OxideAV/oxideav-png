# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
