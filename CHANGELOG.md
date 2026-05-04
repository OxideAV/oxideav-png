# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
