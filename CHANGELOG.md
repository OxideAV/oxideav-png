# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- New `depth` module implementing W3C PNG3 §12.4 sample-depth scaling and
  §13.12 sample-depth rescaling. Pure primitives `rescale_sample` (the
  linear `floor(input × max_out / max_in + 0.5)` equation),
  `scale_up_bit_replication` (§12.4 left bit replication, matching the
  spec's `27@5bit → 222@8bit` worked example and property-tested to stay
  within one of linear across every depth pair), `scale_up_zero_fill`,
  and `recover_sbit` (§13.12 significant-bit recovery by right shift).
  Decoder conveniences `rescale_16bit_to_8bit` /
  `rescale_16bit_to_8bit_via_sbit` accurately reduce a decoded 16-bit
  `PngImage` (`Gray16Le` / `Rgb48Le` / `Rgba64Le`) to 8 bits, the latter
  first recovering each channel's `sBIT` significant bits. Covered by 26
  unit tests, a `tests/depth_rescale.rs` integration suite driving the
  real encode → decode path, a `depth` criterion bench, and an extension
  to the `decode` fuzz target's liveness contract. No behavioural change
  to existing decode / encode paths.

- Decode-side enforcement of the W3C PNG3 §5.1 / §5.6 chunk-ordering
  rules ("Chunks higher up shall appear before chunks lower down"). The
  shared chunk walker rejects a non-`IHDR` first chunk, a duplicate
  `IHDR`, a `PLTE` that does not precede the first `IDAT`, and a
  non-consecutive `IDAT` run (§5.6 / §11.2.3). The §5.6 Table 7
  ancillary buckets are policed before any chunk body is parsed —
  `cHRM` / `cICP` / `gAMA` / `iCCP` / `mDCV` / `cLLI` / `sBIT` / `sRGB`
  before PLTE and IDAT; `bKGD` / `hIST` / `tRNS` after PLTE (when
  present) and before IDAT; `eXIf` / `pHYs` / `sPLT` before IDAT;
  `tIME` / `tEXt` / `zTXt` / `iTXt` unconstrained. The checks fire
  uniformly on `parse_metadata`, `parse_apng`, and the container
  demuxer. New `chunk_ordering_enforcement.rs` integration test (29
  cases) drives every bucket and both the static and animated paths.

- The region-aware APNG encoder now rejects a non-zero `x_offset` /
  `y_offset` on the first frame when no separate default image is
  supplied (the first frame is then the default-image `fcTL`, which W3C
  PNG3 §11.3.5.1 requires at offset (0, 0)), with a clear §11.3.5.1
  message instead of an opaque region-bounds failure.

- The container demuxer now enforces the APNG `acTL` placement /
  multiplicity rules (W3C PNG3 §4.9.1 "an acTL chunk must appear in the
  stream before any IDAT chunks"; §5.6 ordering table acTL "Multiple OK?
  No") at the demux boundary, matching the standalone `parse_apng` gate,
  rather than splitting frames off an ill-formed stream. Factored into a
  shared `validate_apng_chunk_placement` helper with inline coverage.

- APNG `APNG_BLEND_OP_OVER` now honours `tRNS`-keyed transparency on the
  alpha-less canvas formats (colour types 0/2/3). W3C PNG3 §11.3.6.2 says
  an OVER frame is "composited onto the output buffer based on its
  alpha"; for grayscale / truecolour / indexed the alpha is carried by
  `tRNS` (RFC 2083 §4.2.9), not by the pixel. Previously OVER on these
  formats degraded to a plain Source overwrite, ignoring the keyed
  transparency. The compositor now derives a frame-invariant transparency
  key (palette per-index alpha tail, or the keyed gray / RGB sample
  scaled to the canvas byte layout) and leaves the canvas untouched where
  the source pixel is fully transparent. Because the composited canvas is
  itself alpha-less, the behaviour is binary (transparent → skip, opaque
  → write) — there is no representable partial blend. Covered by
  `apng_trns_over_compositing.rs` (palette skip/overwrite, grayscale key,
  truecolour key).

- End-to-end APNG `blend_op` / `dispose_op` round-trip coverage
  (`apng_blend_dispose_roundtrip.rs`): the region-aware encoder builds
  animations exercising `Blend::Over` partial-alpha compositing,
  `Disposal::Background` region clear, and `Disposal::Previous` region
  revert; each is decoded back through the compositor and checked against
  a hand-computed model, including the opaque-OVER-equals-SOURCE
  invariant.

- APNG `acTL` placement / multiplicity validation: `parse_apng` now
  rejects an `acTL` that appears after the first `IDAT` (W3C PNG3 §4.9.1:
  "an acTL chunk must appear in the stream before any IDAT chunks") and a
  stream carrying more than one `acTL` (W3C PNG3 §5.6 ordering table:
  acTL "Multiple OK? No"). Covered by `apng_actl_ordering.rs`.

- APNG default-image `fcTL` restrictions (W3C PNG3 §11.3.5.1): when the
  static image is the first animation frame (an `fcTL` precedes `IDAT`),
  that `fcTL`'s `x_offset` / `y_offset` must be 0 and its `width` /
  `height` must equal the IHDR fields. `parse_apng` now rejects a
  malformed default-image `fcTL`. The restriction is correctly scoped:
  an `fcTL` appearing *after* `IDAT` (separate static image, not part of
  the animation) may still carry offsets and a sub-canvas extent.

- APNG first-frame disposal normalisation (W3C PNG3 §11.3.5.1): "If the
  first fcTL chunk uses a dispose_op of APNG_DISPOSE_OP_PREVIOUS it
  should be treated as APNG_DISPOSE_OP_BACKGROUND." The compositor now
  maps a first-frame `Previous` disposal to `Background` (region clear)
  rather than attempting to revert to a non-existent prior buffer state.
  Covered by `apng_default_fctl_restrictions.rs`.

- APNG `APNG_BLEND_OP_OVER` compositing at 16-bit-per-channel precision.
  The frame-compositor previously implemented the non-premultiplied OVER
  operation (W3C PNG3 §13.16 "Alpha Channel Processing", referenced by
  the §11.3.5 `blend_op` description) only for 8-bit RGBA and 8-bit
  gray+alpha canvases; 16-bit canvases (colour type 6 bit depth 16, and
  colour type 4 bit depth 16 expanded internally to RGBA64) fell through
  to a Source overwrite, silently dropping the alpha blend. The
  compositor now blends little-endian 16-bit samples with the same
  rounded integer OVER arithmetic scaled to a 65535 denominator,
  including the `a_out = a_src + a_dst·(1 − a_src)` resultant-alpha rule.
  New `apng_16bit_compositing.rs` pins the partial-alpha blend, the
  fully-opaque overwrite shortcut, the fully-transparent leave-canvas
  shortcut, and the gray+alpha-16 path.

- `FilterStrategy::Brute` — whole-image exhaustive filter search (W3C
  PNG3 §12.7: "An encoder could try every combination of filters to find
  what compresses best for a given image … if compression efficiency is
  valued over speed of compression"). Rather than the intractable
  `5^rows` per-row combinatorial search, `Brute` builds the whole-image
  filtered byte stream under each of the six candidate row-strategies —
  the §12.8 `Adaptive` min-sum-abs-delta heuristic plus each of the five
  `Fixed` filter types — deflates every candidate, and emits the one
  whose DEFLATE output is smallest. `Adaptive` minimises a *proxy* for
  compressed size (signed-byte absolute sum); `Brute` measures the real
  compressed size, so its output is always at least as small as
  `Adaptive` and never larger than any `Fixed` choice (verified by the
  new size-optimality tests). It is the slowest strategy (six full-image
  deflate passes) and is opt-in. Wired through all four encode paths —
  the non-interlaced ≥ 8-bit path, the non-interlaced sub-byte path, the
  Adam7 ≥ 8-bit path, and the Adam7 sub-byte path — by factoring the
  per-row/per-pass filter loop out of each `deflate_encode_pixels*`
  function into a strategy-parametric stream builder, then dispatching to
  a shared `brute_compress` compare loop over
  `FilterStrategy::BRUTE_CANDIDATES`. The sub-byte per-sample bit-depth
  validation (`v > (1 << bit_depth) - 1`) runs exactly once before the
  six filter trials, so an over-range sample is still a single clean
  encode error, not a panic or six errors. Registry-side the `filter`
  option gains a `brute` value (case-insensitive); the rejection message
  now lists it. 4 new tests (3 in `tests/filter_strategy.rs` covering the
  size-optimality property + bit-exact round-trip on the non-interlaced,
  Adam7, and sub-byte paths; 1 in `registry.rs` covering `filter=brute`
  parsing + the updated unknown-value error). Default-options and
  `Adaptive` / `Fixed` callers are byte-for-byte unchanged.

- Unrecognised-ancillary-chunk preservation for the PNG *editor*
  round-trip (W3C PNG3 §14.2). `parse_metadata` now captures any
  ancillary chunk type the codec does not parse into a new
  `PngMetadata::unknowns: Vec<UnknownChunk>`, recording the chunk's
  4-byte type, payload bytes, and an `after_idat` flag (which side of the
  `IDAT` run it sat on). The encoder replays each captured chunk on the
  same side of `IDAT` — §14.2's one positional rule for the editor case.
  `UnknownChunk::is_safe_to_copy()` / `is_private()` surface the §5.4
  property bits. An unrecognised *critical* chunk is now a hard decode
  error on both `parse_metadata` and `decode_png` (§5.4 / §14.2:
  decoders/editors must terminate rather than silently produce a
  possibly-wrong image); a §13.1-malformed name (non-letter byte) is
  dropped rather than captured. New `tests/unknown_chunk_preservation.rs`
  (9 cases) covers before/after-IDAT capture, round-trip side
  preservation, critical rejection on both decode paths, malformed-name
  dropping, and file-order preservation. The `metadata_chunk_splice`
  fuzz target's splice set gains `prVt` / `prVT` / `PrIv` / `pH1s` so the
  budget reaches the new unknown-chunk paths.

- `decode_png_over_background` — decode a PNG and composite it over a
  solid background, returning an opaque 8-bit `RgbaBitmap` (the §13.15 /
  §13.16 "display the image against a background" path). Decoding runs as
  `decode_png_to_rgba`, then every pixel's straight alpha is composited
  over the background in **linear light** (§13.16 "should be performed
  with intensity samples, not gamma-encoded samples") via
  `composite_over_background`. Background source precedence (§13.15):
  caller `override_bg` > the datastream's `bKGD` chunk (resolved through
  `Bkgd::resolve_rgb8`) > `DEFAULT_BACKGROUND_GREY` (the §13.15
  medium-grey `153`). New public const `DEFAULT_BACKGROUND_GREY`. A
  6-test integration suite (`tests/bkgd_compositing.rs`) covers the
  default-grey fallback, override precedence, RGB-chunk half-alpha blend,
  the indexed `tRNS`/`bKGD`-index transparent-entry path, the opaque
  no-op, and the packed-opaque-output invariant. The new entry point is
  also driven by the `decode` fuzz target (chunk-resolution + override
  arms).
- `Bkgd::resolve_rgb8` — resolve a `bKGD` chunk into a concrete 8-bit
  sRGB background colour `[R, G, B]`, the §13.15 form a viewer needs to
  fill transparent pixels and screen space. Grayscale and RGB samples are
  rescaled from the IHDR `bit_depth` to 8 bits with the §13.12 linear
  equation `floor(input * 255 / (2^bit_depth - 1) + 0.5)` (sub-byte grey
  `15`@4-bit → `255`, not `15`; 16-bit `0x8000` → `128` rather than a
  low-byte discard) and grey replicated into R = G = B; a `Palette` index
  selects an `R G B` triple from the `PLTE` body (missing / too-short
  palette or an out-of-range index is an error). 8 unit tests.
- sRGB linear-light conversion (`srgb` module) — the IEC 61966-2-1 sRGB
  electro-optical transfer function and its exact inverse, referenced by
  W3C PNG 3rd Edition / ISO 15948 §11.3.2 for the sRGB-default colour
  space and §13 as the prerequisite for correct compositing. Driven by
  three committed bit-exact numeric tables (`png_sRGB_table` /
  `png_sRGB_base` / `png_sRGB_delta` under `docs/image/png/tables/`):
  `srgb_to_linear8(u8) → u16` (8-bit sRGB → 16-bit Q16 linear, one
  lookup), `srgb_from_linear(u32) → u8` (8-bit-scaled linear → sRGB byte
  via base/delta, exact inverse for all 256 values), `srgb_to_scaled_linear8`,
  `linearize_rgba` (alpha passed through linearly per §13.16), and
  `composite_over_background` performing source-over alpha compositing in
  linear light (a 50%-white pixel over black re-encodes to ~188 sRGB, not
  the gamma-space 128). 8 unit tests + a 4-test integration suite
  (`tests/srgb_compositing.rs`) round-trip an `sRGB`-chunked PNG through
  the decoder and composite it.

- region-aware APNG encoder (W3C PNG 3rd Edition §11.3.6 / §4.9) — new
  `encode_apng_frames` / `encode_apng_frames_with_options` plus the
  `ApngFrameSpec` struct and `ApngBlend` / `ApngDisposal` re-exports.
  Where the existing `encode_apng` paints every frame full-canvas with
  `Disposal::None` / `Blend::Source` and one shared delay, the new
  entry points take a `&[ApngFrameSpec]` where each frame carries its
  own sub-canvas region (`x_offset` / `y_offset` + a smaller frame
  extent that becomes the `fcTL` width/height), its own `delay_num` /
  `delay_den` rational duration, and its own
  `Disposal::{None,Background,Previous}` / `Blend::{Source,Over}`
  operators. An optional separate full-canvas default (still) image is
  written into the `IDAT` before the first `fcTL` and excluded from
  `acTL.num_frames`; with no separate default the first frame's
  full-canvas pixels become the `IDAT` *and* the first animation frame
  (`fcTL` precedes `IDAT`, the `first_frame_is_default` path). Frame
  regions are policed against §11.3.6.1 (non-zero extent,
  `x_offset + width ≤ canvas_width`, `y_offset + height ≤
  canvas_height`) before any compression work; the first `fcTL` carries
  sequence number 0 with every subsequent `fcTL` / `fdAT` sequence
  number contiguous-ascending (§4.9.2); each frame's sub-region is
  compressed against a synthetic per-frame IHDR (Adam7 interlace
  included). 9 round-trip tests (`tests/apng_region_encode.rs`) encode
  hand-built animations and decode them back through the crate's own
  compositor, covering the separate-default-image case, partial-region
  frames, `Background` disposal clearing, `Over` alpha blending,
  rational-delay preservation, and the geometry / format rejection
  paths. A new `apng_region_encode` fuzz target drives the encoder
  directly with fuzz-derived regions / offsets / delays / operators
  (215k+ executions, zero crashes) and asserts encode→decode frame-count
  consistency.
- decoder gamma handling (W3C PNG3 §13.13 / RFC 2083 §10.5) — opt-in
  colour transform that undoes a file's `gAMA`-encoded samples and
  re-applies a target display's gamma. New `gamma` module exposes
  `GammaParams { file_gamma, display_exponent, user_exponent }` with the
  spec's merged decoding exponent `user_exponent / (file_gamma *
  display_exponent)`, a 256-entry 8-bit correction LUT
  (`GammaParams::build_lut`, `floor((s/255)^e * 255 + 0.5)`), and
  in-place `RgbaBitmap` appliers (`apply_to_rgba` / `apply_gama_to_rgba`).
  Defaults follow the spec: `display_exponent = 2.2` ("A display exponent
  of 2.2 should be used unless detailed calibration measurements are
  available", §13.13), `user_exponent = 1.0`, and `file_gamma = 1/2.2`
  for the unknown-gamma case. Alpha is never gamma-corrected ("alpha is
  always represented linearly", §13.16) — only R/G/B bytes pass through
  the LUT. A zero `gAMA` is treated as "no usable file gamma" and ignored
  (§13.13 "Decoders should ignore it"), as are non-positive
  display/file-gamma factors. The codec proper still round-trips the raw
  `gAMA` integer verbatim; this is a separate caller-invoked stage. 10
  unit tests cover identity exponents, endpoint fixing (0→0, 255→255),
  the spec rounding, alpha preservation, the darken/lighten user-exponent
  behaviour, and the zero/degenerate guards.
- indexed-image palette gamma correction (W3C PNG3 §13.13) — the spec's
  explicit "one-time correction of the palette is sufficient"
  optimisation for colour type 3. `apply_to_palette` /
  `apply_gama_to_palette` run the same `build_lut()` table over a `Pal8`
  image's `PngImage::palette` `PLTE` `R/G/B` triples once (rather than
  gamma-correcting every output pixel); `plte_len` bounds the `PLTE`
  portion and any `tRNS` alpha tail at/after that offset is left
  untouched (§13.16). A malformed `plte_len` (not a multiple of 3, or
  past the buffer) is clamped to the largest whole-triple prefix that
  fits. 5 unit tests cover LUT parity with the full-colour path, the
  `tRNS` alpha-tail preservation, the clamp, and the zero-`gAMA` /
  identity-exponent no-ops.
- 16-bit decoder gamma correction (W3C PNG3 §13.13) — the §13.13 transform
  is bit-depth-general (`sample = integer_sample / (2^sampledepth - 1.0)`;
  `framebuf_sample = floor(display_input × MAX_FRAMEBUF_SAMPLE + 0.5)`,
  "MAX_FRAMEBUF_SAMPLE … 255 for 8-bit, 31 for 5-bit, etc"), so the 8-bit
  LUT is the `MAX = 255` specialisation and the new
  `GammaParams::build_lut16` is the `MAX = 65535` one — a 65536-entry
  `u16` table (`floor((s/65535)^e × 65535 + 0.5)`), heap-boxed (128 KiB)
  so it never materialises on the stack. `apply_to_png16` /
  `apply_gama_to_png16` run it across the little-endian colour samples of
  a `PngImage` in the three 16-bit layouts (`Gray16Le` = 1 colour sample,
  `Rgb48Le` = 3, `Rgba64Le` = 3 + a §13.16-linear alpha sample left
  untouched). The same merged decoding exponent drives both widths, so the
  endpoints stay pinned (`0 → 0`, `65535 → 65535`). Non-16-bit formats
  (`Gray8` / `Rgb24` / `Pal8` / `Ya8` / `Rgba`) are a no-op `false` (the
  8-bit appliers own those widths); a `stride` wider than `width × bpp`
  corrects only the live samples and skips trailing padding. Re-exported
  as `apply_gamma_to_png16` / `apply_gama_to_png16`. 10 unit tests cover
  LUT16 endpoints / identity, the spec rounding, 8-bit/16-bit endpoint
  parity, per-channel correction on all three layouts, alpha preservation
  on `Rgba64Le`, the non-16-bit rejection, wider-stride padding
  preservation, and the zero-`gAMA` / non-positive-factor no-ops.

### Changed

- The chunk CRC-32 (`filter::crc32`, RFC 2083 §5.5) now uses the
  slice-by-16 algorithm: sixteen input bytes are consumed per iteration
  through sixteen independent lookup tables (derived from the base
  `0xEDB88320`-reflected table) and combined with XOR, which exposes far
  more instruction-level parallelism than the byte-at-a-time recurrence.
  Output is bit-identical to the previous byte-at-a-time table loop — a
  new `crc_slice_matches_bitwise_across_lengths` test verifies equality
  against the bit-serial reference `crc32_loop` at every length from 0 to
  200 (crossing several 16-byte block boundaries and every remainder
  size), plus an all-`0x00` / all-`0xFF` degenerate-input check. The CRC
  runs over every chunk's type + data on both decode (validation) and
  encode (emission), so on an IDAT-heavy image it is an O(file-size)
  cost. Measured throughput on the new `crc` bench: ≈5.8× on 1 MiB
  buffers (≈0.5 → ≈2.9 GiB/s) and ≈6× on 64 B / 1 KiB inputs. Buffers
  below one 16-byte block fall back to the classic single-byte loop.

- `chunk::write_chunk` now computes each chunk's CRC incrementally
  (`filter::crc32_update` / `CRC32_INIT`) over the type slice then the
  data slice, rather than concatenating `type ++ data` into a throwaway
  `Vec` first. This removes one heap allocation + copy per chunk emitted.
  The `chunk_crc` bench scenario measures the concat-vs-incremental gap at
  ≈42% (13 B chunk) / ≈26% (256 B) — larger the smaller the chunk, since
  the eliminated cost is the fixed allocation. Output is byte-identical
  (every encode→decode roundtrip test re-validates the emitted CRC on the
  decode side); a `crc_incremental_matches_contiguous` unit test asserts
  `crc32_update(crc32_update(INIT, a), b) ^ 0xFFFF_FFFF == crc32(a ++ b)`
  across split points spanning several block boundaries.

- New `crc` criterion bench (`benches/crc.rs`) drives `crc32` over
  8 B … 1 MiB buffers so the CRC inner loop can be A/B-ed in isolation
  from the surrounding chunk walk and DEFLATE cost, plus a `chunk_crc`
  scenario comparing the concat and incremental CRC shapes.

### Other

- fuzz: `metadata_chunk_splice` target — build a valid 8x8 base PNG
  (grayscale / RGB / palette) with the standalone encoder, then splice
  1..=8 fuzz-derived ancillary chunks (type drawn from the
  `parse_metadata` dispatch set: `sBIT pHYs tIME bKGD hIST tRNS eXIf sRGB
  cICP gAMA cHRM mDCV cLLI sPLT tEXt zTXt iCCP iTXt`, payload
  fuzz-controlled) immediately before `IEND` with a correct length prefix
  + CRC32. Funnels the mutation budget *inside* the per-chunk `::parse`
  routines — keyword / `NUL` splitting, the compression-method byte, the
  zlib inflate of the `zTXt` / `iTXt` / `iCCP` bodies, `sPLT` entry
  strides, the `eXIf` TIFF probe, and `bKGD` / `hIST` PLTE-index bounds —
  that the raw-bytes `decode` target rarely reaches past the signature /
  framing / CRC gate. Drives `parse_metadata` + `decode_png` +
  `decode_png_to_rgba`; asserts liveness only (no panic / abort / OOB /
  OOM on any spliced input). 866k runs clean at ~7.2k exec/s
- enforce the APNG shared `fcTL`/`fdAT` sequence-number rules (W3C PNG3
  §4.9.2): the first `fcTL` must carry sequence number 0, and every
  subsequent `fcTL`/`fdAT` must be contiguous-ascending with no gaps or
  duplicates ("Decoders shall treat out-of-order APNG chunks as an
  error", §4.9.1); also reject `acTL.num_frames == 0` (§4.9: "0 is not a
  valid value"). Applied on both the standalone `parse_apng` / `decode_apng`
  path and the demuxer frame-splitter; `num_frames` *mismatch* with the
  fcTL count stays advisory as before
- caller-selectable IDAT/fdAT DEFLATE compression level (1..=9) via
  `PngEncoderOptions::compression_level` and the registry
  `compression_level` option; `None`/`0` keeps the historical level 6
  (RFC 2083 §5 fixes only the deflate/inflate *method*, not the level)
- gate IHDR field validity at the wire-decode boundary (W3C PNG3 §11.2.1):
  reject zero width/height, non-Table-12 colour-type/bit-depth pairs, and
  unknown compression/filter/interlace methods

## [0.1.8](https://github.com/OxideAV/oxideav-png/compare/v0.1.7...v0.1.8) - 2026-06-12

### Other

- migrate DEFLATE/zlib from miniz_oxide to compcol
- police fcTL frame region against IHDR canvas (W3C PNG3 §11.3.6.1)
- typed ColourType primitive for the IHDR colour-type byte
- typed ChunkType accessor for W3C PNG3 §5.4 property bits
- drop release-plz.toml — use release-plz defaults across the workspace
- caller-selectable per-row filter strategy (W3C PNG3 §12.7)
- Adam7 interlaced sub-byte encode for ct=0 / ct=3 (depth 1/2/4)
- filter_roundtrip target — direct per-row §6.2..§6.6 reconstruction
- sub-byte (1/2/4-bit) encode for colour type 0 / 3
- scrub bait phrase from CRC-32 comment (cite RFC 2083 Annex D directly)
- tRNS round-trip for ct=0 / ct=2 / ct=3 via PngMetadata::trns
- round-trip the mDCV + cLLI HDR static-metadata chunks
- round-trip the iCCP + iTXt remaining-gap ancillary chunks
- round-trip the zTXt compressed-textual-data chunk
- parametric APNG frame-scan decode-loop throughput target (r196)

### Changed

- Replaced `miniz_oxide` with `compcol` (the workspace-wide pure-Rust
  compression collection, `zlib` feature only) as the DEFLATE/zlib
  (RFC 1950/1951) provider for IDAT / fdAT pixel streams and the
  `zTXt` / `iTXt` / `iCCP` chunk bodies. The crate's whole compression
  surface now goes through two thin one-shot wrappers in a new private
  `zlibvec` module (`compress_to_vec_zlib(data, level)` /
  `decompress_to_vec_zlib(data)`), so the dependency choice is a
  single-file concern. Decode output is byte-identical (verified
  old-vs-new over an externally generated corpus covering gray 1/4/8/
  16-bit, indexed, RGB/RGBA 8/16-bit, and Adam7-interlaced inputs);
  encoded files carry different compressed bytes (different deflate
  implementation, still level 6 — the zlib default — with the same
  CMF/FLG `0x78` framing) but self-roundtrip bit-exactly and validate
  externally (CRC + RFC 1950 stream incl. Adler-32, plus third-party
  decoder pixel-match on plain and interlaced output). Compressed
  output measures ~0.6 % larger at level 6 on a 1080p plasma frame.

### Added

- `encode_options` cargo-fuzz target exercising
  `encode_png_image_with_options` over the option matrix the existing
  `encode_decode_roundtrip` target (which only ever uses
  `PngEncoderOptions::default()`) never reaches: Adam7 interlace (both
  the >=8-bit and the sub-byte 1/2/4-bit pass layouts), caller-supplied
  sub-byte `bit_depth` packing — including the rejection arms (depth on
  a non-Gray8 / non-Pal8 source, `bit_depth = 16`, non-power-of-two
  depths), every `FilterStrategy` variant (Adaptive plus `Fixed` ×5),
  and the ancillary-metadata emission path (`tEXt` / `pHYs` / `tIME` /
  `gAMA`). Accepted output is decoded through `decode_png` +
  `decode_png_to_rgba`; the target asserts liveness of the
  option-bearing encode path and decode-liveness + dimension
  preservation on its bytes. Cleared a 180 s / ~557 k-exec local run
  with no panic / abort / overflow.
- Typed `ColourType` primitive wrapping the IHDR colour-type byte
  (W3C PNG3 §11.2.1 "Color type is a single-byte integer") with the
  §6.1 / Table 9 named encoding: `Greyscale` (0), `Truecolor` (2),
  `IndexedColor` (3), `GreyscaleAlpha` (4), `TruecolorAlpha` (6).
  `ColourType::from_byte` rejects every value outside that set so a
  malformed IHDR cannot slip an undefined combination (1 = palette
  without truecolor, 5 / 7 = palette + alpha) past the typed gate.
  The §6.1 component bits — `1` palette used, `2` truecolor used,
  `4` alpha used — are surfaced as `palette_used` / `truecolor_used`
  / `alpha_used` `const fn` predicates so callers do not re-derive
  the bit math at every branch. `channels` returns 1 / 3 / 1 / 2 / 4
  per §4.5 pixel layouts and `requires_plte` flags the colour-type-3
  row of Table 12 where a `PLTE` chunk is mandatory.
  `allows_bit_depth` decodes W3C PNG3 §11.2.1 Table 12 ("Allowed
  combinations of color type and bit depth") in one place — greyscale
  accepts 1/2/4/8/16, indexed 1/2/4/8 (no 16-bit), truecolor /
  greyscale-with-alpha / truecolor-with-alpha 8 and 16 only.
  `Ihdr::colour_type_typed()` lifts the raw `u8` field into the
  typed enum without breaking the existing `colour_type: u8` field;
  `Ihdr::is_allowed_combination()` returns the Table 12 verdict for
  the parsed (colour_type, bit_depth) pair. `Ihdr::channels()` was
  rewired through the typed primitive so the channel-count math
  lives behind a single typed accessor. Seven unit tests cross-
  check the typed primitive against the worked entries in §6.1 /
  Table 9 (every named row + every undefined byte rejected), the
  §6.1 component-bit decomposition (palette / truecolor / alpha
  bits set on exactly the expected variants), §4.5 channel counts
  (1, 3, 1, 2, 4 across the five rows), Table 12 acceptance for
  each colour type's allowed bit depths, and the indexed-only
  `requires_plte` rider. Re-exported from the crate root as
  `oxideav_png::ColourType`. Pure typed-primitive addition; no
  decode / encode behavioural change.

- Typed `ChunkType` accessor exposing the W3C PNG 3rd Edition §5.4
  ("Chunk naming conventions") property bits — `is_ancillary`,
  `is_critical`, `is_private`, `is_public`, `is_reserved_bit_set`,
  `is_safe_to_copy`, `is_unsafe_to_copy` — all `const fn` for use in
  compile-time tables. Wraps the existing four-byte `chunk_type`
  `[u8; 4]` so the bit-5 (value `0x20`) property-bit decoding lives in
  one place instead of being re-derived at every call site. Backed by
  a `ChunkRef::type_code()` bridge so callers that hold a borrowed
  chunk can drop straight into the typed accessor without copying the
  four bytes through a local. Also exposes `is_well_formed_name()`
  which enforces the §13.1 "type names shall consist of letters"
  constraint (ASCII A..Z / a..z only — digits, punctuation, and
  control bytes all rejected), and `as_str()` which mirrors the
  `ChunkRef::type_str` `"????"` fallback for non-letter bytes.
  `From<[u8; 4]>` and `Into<[u8; 4]>` round-trip the wrapped name.
  Eleven unit tests cross-check the property bits against the §5.4
  worked example (`cHNk` → ancillary, public, reserved-bit-clear,
  safe-to-copy), the four critical chunks (`IHDR` / `PLTE` / `IDAT` /
  `IEND` all uppercase first letter), every §11.3 ancillary chunk
  the codec round-trips, the §11.3.2 colour-space chunks plus `tIME`
  carrying the unsafe-to-copy bit set, the §11.3.6 APNG chunks
  (`acTL` / `fcTL` / `fdAT`) reading as private (their second letter
  is lowercase from the original Mozilla minting and §5.4's "property
  bits are an inherent part of the chunk type" rule freezes that),
  and the reserved-bit / well-formedness rejection of synthesised
  ill-formed names (digits, underscores, high-bit bytes). Pure
  accessor — no parsing or chunk-stream behaviour changes, additive
  to the existing `chunk` module surface. Re-exported from the crate
  root as `oxideav_png::ChunkType` so consumers don't need to import
  through `chunk::`.

- Encoder filter-selection strategy (W3C PNG3 §12.7). New
  `PngEncoderOptions::filter_strategy: FilterStrategy` field plumbed
  through every encode path — non-interlaced ≥ 8-bit, Adam7 ≥ 8-bit,
  and Adam7 sub-byte (and both the standalone `encode_png_image_with_
  options` and `encode_apng_with_options` entry points). Two variants:
  `Adaptive` (the default — keeps the long-standing per-row §12.8
  min-sum-abs-delta heuristic across all five filter types) and
  `Fixed(FilterType)` which pins one filter type for every row and
  skips the heuristic trial. §12.7's spec mapping is preserved as a
  doc reference: `Fixed(Paeth)` is most likely the best fixed choice
  on truecolour / grayscale, `Fixed(None)` is recommended for indexed
  (colour type 3) and bit depths below 8. Registry-side schema gains
  a `filter` string option that accepts `adaptive` / `none` / `sub` /
  `up` / `average` / `paeth` (case-insensitive); the empty string maps
  to `adaptive` for back-compat with callers that bind the field but
  leave it empty. Default-options encodes produce the same bytes as
  pre-r245 so callers that never touch the new field see no change.

- Adam7 interlaced sub-byte encode. `PngEncoderOptions::interlace =
  true` combined with `bit_depth = Some(1 | 2 | 4)` is now supported
  on colour type 0 (grayscale) and colour type 3 (indexed) sources.
  Each of the seven Adam7 passes is gathered into its own
  `pw × ph` sub-image, packed MSB-first into `ceil(pw * bit_depth /
  8)` wire row bytes per RFC 2083 §2.6 ("The data within each pass is
  laid out as though it were a complete image of the appropriate
  dimensions … each such scanline is padded as needed to fill an
  integral number of bytes"), then filtered with the per-row min-sum-
  abs heuristic (§12.8) against a zero prior row at the top of each
  pass (§6.3 "the entire prior scanline must be treated as being
  zeroes for the first scanline … of a pass of an interlaced image").
  Empty passes (the §2.6 caution for fewer-than-five-columns / rows
  inputs) emit no filter-type bytes. Available on both the standalone
  PNG encoder (`encode_png_image_with_options`) and APNG output
  (`encode_apng_with_options`). Source sample range remains validated
  against `(1 << bit_depth) - 1` per pixel.

### Removed

- The "Adam7 + sub-byte not implemented" rejection on
  `encode_png_image_with_options` and `encode_apng_with_options`.

### Added (existing entries)

- New `filter_roundtrip` fuzz target driving `filter_row` +
  `unfilter_row` directly with fuzz-derived `(FilterType, bpp,
  row_size, prev_row, row)` tuples. Bypasses the chunk-CRC / IDAT-
  inflate / IHDR-shape gates so the mutation budget lands inside the
  RFC 2083 §6.2..§6.6 reconstruction arithmetic — all five filter
  types, every `bpp` ∈ 1..=8 (the full range
  `Ihdr::bpp_for_filter` emits), row sizes up to 2 KB, and arbitrary
  prior-row bytes. Asserts two properties: (1) liveness on equal-
  length slices — the only documented `Err` path is a row / prev_row
  length mismatch, impossible by construction — and (2) the §6.1
  reversibility property `unfilter(filter(row)) == row` for every
  shape sampled. Counterpart to the existing `decode` target, which
  reaches `unfilter_row` only after the framing gates have absorbed
  most of the mutation budget; the new target widens filter-path
  coverage without re-paying the CRC + inflate cost on every
  iteration. Six fuzz targets total now under `fuzz/fuzz_targets/`.
- Sub-byte encode for colour type 0 (grayscale) and colour type 3
  (indexed) at bit depths 1, 2, and 4. Opt-in via the new
  `PngEncoderOptions::bit_depth: Option<u8>` field (also reachable
  through the registry-side `CodecParameters::options` schema as a
  `u32` named `"bit_depth"`, with `0` standing for "leave native"). On
  `Gray8` and `Pal8` sources only — the RFC 2083 §11.2.2 allowed-
  combinations table forbids sub-byte depths on colour types 2 / 4 / 6,
  so any other source `PngPixelFormat` paired with a sub-byte option
  is rejected as an encode error. Each source byte is treated as a
  pre-quantized sample value or palette index in
  `0..=(1 << bit_depth) - 1`; an over-range sample is rejected ahead of
  the wire so the encoder cannot emit a payload whose high bits would
  spill into a neighbouring pixel after the bit-pack. Packing follows
  PNG §2.3 / W3C PNG3 §11.1.2: pixels lie left-to-right with the
  leftmost pixel in the high-order bits of each byte, the rightmost in
  the low-order bits. Rows whose pixel count is not a multiple of
  `8 / bit_depth` (the spec's "Scanlines always begin on byte
  boundaries" rule) get the trailing byte's low-order positions
  zero-padded for deterministic output; §2.3 marks these padding bits
  unspecified. The on-wire row count is `ceil(width * bit_depth / 8)`
  per the spec; the §6 filters operate on the packed bytes (`bpp = 1`
  per the existing `Ihdr::bpp_for_filter` rule for sub-byte depths,
  matching the decoder's reconstruction path). APNG sub-byte encode
  rides the same path — the IHDR is fixed across the whole APNG, so a
  single `bit_depth` covers every frame.
  Closes the long-standing "Not preserved: sub-byte encode (decode
  only — encoder always writes 8/16-bit)" line in the crate README.
  Adam7 interlaced sub-byte encode (`interlace = true` paired with a
  sub-byte `bit_depth`) is rejected with a clear error for now — each
  pass would need its own sub-byte pack, deferred to a follow-up round.
  Non-interlaced sub-byte and Adam7-with-`bit_depth: None` encode are
  both supported, including the round-trip through the standalone
  `decode_png` path (1-bit gray ×255, 2-bit ×85, 4-bit ×17 §13.12
  scale-up on the decode side). 23 new integration tests in
  `tests/subbyte_encode.rs` cover the gray + indexed round-trip at
  every depth (multiple-of-byte and odd widths each), the IHDR
  bit-depth field placement, the MSB-first packing on a known 8-pixel
  source, the four negative cases (RGB / RGBA source rejection;
  over-range sample rejection; bit_depth = `0/3/5/6/7/9/16/32`
  rejection; interlace + sub-byte rejection), the `bit_depth = Some(8)`
  no-op behaviour for `Gray8` / `Pal8`, and an APNG sub-byte 2-frame
  round-trip. The registry-side `CodecOptionsStruct` schema grows a
  second `OptionField` so framework-side callers can request sub-byte
  encode through the same string-typed `CodecParameters::options` map
  the rest of the codec set uses.

- `tRNS` (simple transparency, RFC 2083 §4.2.9 / W3C PNG3 §11.3.1.1)
  round-trip via `PngMetadata::trns`. Closes the long-standing
  "Not preserved: `tRNS` chunk emission for ct=0 / ct=2" line in the
  crate README. Surfaced as `metadata::Trns`, a three-variant enum
  whose discriminant matches the IHDR colour type — `Grayscale(u16)`
  for ct=0 (2-byte BE keyed gray sample, bounds-checked against
  `(1<<bit_depth)-1`), `Rgb(u16, u16, u16)` for ct=2 (6-byte BE keyed
  RGB triple, same per-channel bound), and `Palette(Vec<u8>)` for
  ct=3 (1..=PLTE-entry-count alpha tail; the spec's "missing trailing
  entries are opaque" rule from §4.2.9 is preserved by reading the
  vector verbatim and letting the existing decode-side promotion
  default uncovered indices to 255). Colour types 4 and 6 are
  rejected on parse — "tRNS is prohibited for color types 4 and 6,
  since a full alpha channel is already present" (§4.2.9 final
  paragraph). The encoder gains a `resolve_trns_bytes` helper that
  reconciles the new metadata path against the long-standing
  `image.palette = PLTE || tRNS` tail (Pal8 only); supplying both is
  an explicit "duplicate tRNS chunk would be emitted" encode error
  per §5.6 Table 1 "Multiple OK? No". Variant-vs-IHDR-colour-type
  mismatch (e.g. `Trns::Rgb` on a Gray8 image) is also an explicit
  encode error so a malformed payload cannot reach the wire. The
  chunk lands "After PLTE; before IDAT" per the same table — the
  encoder writes it in the existing PLTE/tRNS slot for both static
  PNG and APNG paths. The §4.2.9 "compare both bytes of a 16-bit
  sample" rule (a 16-bit gray of `0x0001` keyed transparent must NOT
  flag `0x0002` as transparent too) is preserved by the existing
  `png_image_to_rgba` 16-bit comparison path; a fresh integration
  test pins the round-trip of a `Trns::Grayscale(0x0001)` value
  through `Gray16Le`. 16 new unit tests in `metadata.rs` (variant
  round-trip at 8/16-bit, wrong-length rejection, bit-depth-cap
  rejection, ct=4/6 prohibition, ct=3 shorter-than-PLTE acceptance,
  ct=3 longer-than-PLTE rejection, missing-PLTE rejection, the
  `matches_colour_type` discriminator) plus 9 new integration tests
  in `metadata_roundtrip.rs` (ct=0/ct=2/ct=3 encoder round-trip,
  Gray16Le both-bytes preservation, palette-path-vs-metadata-path
  equivalence, dual-source duplicate rejection, variant/IHDR
  mismatch rejection, sample-beyond-bit-depth rejection,
  PLTE→tRNS→IDAT ordering, duplicate-chunk rejection via byte splice,
  and ct=6 prohibition rejection via byte splice).

- `mDCV` (Mastering Display Color Volume, W3C PNG3 §11.3.2.7) and
  `cLLI` (Content Light Level Information, W3C PNG3 §11.3.2.8)
  HDR static-metadata round-trip. Surfaced as `metadata::Mdcv`
  (three primary chromaticities, the white-point chromaticity, plus
  `max_luminance` / `min_luminance`) and `metadata::Clli`
  (`max_content_light_level` / `max_frame_average_light_level`), held
  by `PngMetadata::mdcv: Option<Mdcv>` and `PngMetadata::clli:
  Option<Clli>`. Both chunks are short fixed-layout payloads (24 bytes
  for `mDCV` per §11.3.2.7 Table 19; 8 bytes for `cLLI` per §11.3.2.8
  Table 20); the codec stores the "stored integer" big-endian samples
  verbatim so a round-trip is byte-exact (chromaticities × 50000,
  luminances × 10000), and convenience accessors re-divide for
  callers that want floats. Single-instance only — duplicate `mDCV` /
  `cLLI` is rejected on parse per §5.6 Table 1 "Multiple OK? No".
  Both chunks must precede `PLTE` and `IDAT` per the same table; the
  encoder emits them after the §4.3-ranked colour chunks (cICP →
  iCCP → sBIT → sRGB → gAMA → cHRM → mDCV → cLLI) so the basic
  colour-space signal leads the file and the HDR supplemental
  colour-volume metadata trails — pairing naturally with `cICP` for
  HDR10 streams (BT.2100 primaries + PQ transfer + full-range mDCV +
  cLLI). A zero in either `cLLI` field is the spec's "unknown / not
  currently calculable" sentinel (§11.3.2.8): preserved verbatim so a
  live APNG encoder can emit a placeholder and rewrite the value when
  the stream ends. 8 new unit tests + 11 new integration tests cover
  round-trip on the §11.3.2.7 BT.2100 + Display P3 worked examples
  (Examples 5-9 hex bytes), the §11.3.2.8 1000/250 cd/m² HDR10 case
  (Examples 13-14), all-zero placeholder preservation,
  wrong-length rejection, duplicate-chunk rejection, ordering-rule
  enforcement (mDCV/cLLI precede PLTE + IDAT and trail the §4.3
  colour chunks), and a combined HDR10 cICP + mDCV + cLLI
  round-trip. Closes the "Not preserved" README entry for both
  chunks.

- `iCCP` (embedded ICC profile, W3C PNG3 §11.3.2.3) round-trip.
  Surfaced as `metadata::Iccp` (`pub name`, `pub profile: Vec<u8>`)
  and held by `PngMetadata::iccp: Option<Iccp>`. The on-wire payload
  is a 1-79-byte Latin-1 profile name + `NUL` separator + 1-byte
  compression method (only `0` = deflate defined per §11.3.2.3 "The
  only compression method defined in this specification is method 0";
  the codec rejects any other value) + zlib-compressed profile bytes.
  The decoder reuses the shared keyword validator for the profile
  name, inflates the body via `miniz_oxide`, and surfaces the
  decompressed profile as an opaque `Vec<u8>` (PNG cites ICC.1 /
  ISO 15076-1 for the internal structure and the codec does not
  interpret it). The encoder re-validates the name and deflates the
  profile at the project's default level (6). Single-instance only —
  duplicate `iCCP` is rejected on parse per §5.6 Table 1 "Multiple
  OK? No". Emitted before `PLTE` and `IDAT` in §4.3 Color-Chunk-
  Priority order, between `cICP` (rank 1) and `sRGB` (rank 3). 8 new
  unit + 5 integration tests cover round-trip, empty-profile,
  unknown-method rejection, corrupted-zlib rejection, missing-NUL
  rejection, missing-method-byte rejection, invalid-name rejection,
  large-payload compression (4 KB run-of-one → <200 wire bytes),
  duplicate rejection on parse, and chunk-precedes-PLTE/IDAT
  ordering. Closes the README "Not preserved" entry for `iCCP`.

- `iTXt` (international textual data, W3C PNG3 §11.3.3.4) round-trip.
  Surfaced as `metadata::Itxt` (`pub keyword`, `pub compressed`,
  `pub language_tag`, `pub translated_keyword`, `pub text`) and held
  by `PngMetadata::itxts: Vec<Itxt>`. The UTF-8 successor to `tEXt`
  pairs a Latin-1 keyword with a language-tagged UTF-8 text body,
  optionally zlib-compressed. The on-wire payload is `keyword || NUL
  || compression_flag (0/1) || compression_method (1 B, only 0 =
  deflate defined when flag=1, ignored when flag=0) || language_tag
  || NUL || translated_keyword || NUL || text` per §11.3.3.4. The
  decoder reuses the shared keyword validator, accepts any method
  byte when the flag is `0` (per "decoders shall ignore it"),
  validates UTF-8 round-tripping of the translated keyword + text,
  enforces the no-`NUL`-in-text rule on the decompressed body, and
  validates the language tag against the ASCII subset of BCP47 (the
  IANA subtag-registry constraint is encoder-side per the spec and
  offline-only at the project level). The encoder validates the
  keyword, the language tag's ASCII bytes, and the no-`NUL` rule on
  the translated keyword + text, then deflates the text body when
  `compressed = true`. `iTXt` is the third metadata chunk PNG
  explicitly permits to repeat with identical keywords (§11.3.3.4
  inherits §4.2.7 ¶3); file order is preserved on decode and
  replayed on encode. Emitted before `IDAT` alongside `tEXt` /
  `zTXt`; the encoder writes them after `zTXt` so the Latin-1
  chunks lead the internationalised UTF-8 chunks in the stream. 16
  new unit + 9 integration tests cover uncompressed / compressed
  round-trip, UTF-8 translated keyword + text (Japanese, French),
  empty-language-and-translated-keyword, multi-instance + identical-
  keyword, large-payload compression, unknown-flag rejection,
  unknown-method rejection when compressed, method-byte ignored when
  uncompressed, missing-keyword-NUL / missing-language-NUL /
  missing-translated-keyword-NUL rejection, corrupted-zlib rejection
  when compressed, invalid UTF-8 text rejection, non-ASCII language
  tag rejection on both encode and decode, `NUL`-in-translated-
  keyword / `NUL`-in-text rejection on encode,
  `NUL`-in-decompressed-text rejection on parse, chunk-ordering vs
  `IDAT`, and coexistence with `tEXt` / `zTXt` in one file. Closes
  the README "Not preserved" entry for `iTXt`; the only remaining
  ancillary-chunk gaps are the PNG 3rd edition HDR-side `mDCV`
  (§11.3.2.7) and `cLLI` (§11.3.2.8).

- `zTXt` (compressed textual data, RFC 2083 §4.2.10 / W3C PNG3
  §11.3.3.3) round-trip. Surfaced as `metadata::Ztxt` (`pub keyword`,
  `pub text`) and held by `PngMetadata::ztxts: Vec<Ztxt>`. Semantically
  equivalent to `tEXt` — Latin-1 keyword (1-79 printable bytes, no
  leading / trailing / consecutive spaces, no NUL) + Latin-1 text body
  — but the body is zlib-compressed on the wire. The on-wire payload
  layout is `keyword || NUL || compression_method (1 B, only 0 =
  deflate defined) || zlib-compressed text` per §4.2.10 ("A zTXt chunk
  contains"). The decoder reuses the existing keyword validator,
  rejects any compression method other than `0` ("The only value
  presently defined for it is 0"), inflates the body via the same
  `miniz_oxide` path used for IDAT, and enforces the no-NUL-in-text
  rule from §4.2.7 ("Neither the keyword nor the text string can
  contain a null character") on the decompressed payload. The encoder
  re-validates the keyword and text codepoints (Latin-1 single-byte,
  no NUL) and deflates the body at the project's default compression
  level (6) — `miniz_oxide`'s `compress_to_vec_zlib`. `zTXt` is the
  second metadata chunk PNG allows to repeat without uniqueness
  constraints (§4.2.10 ¶6 "Any number of zTXt and tEXt chunks can
  appear in the same file"); decode preserves file order and encode
  replays it via `Vec<Ztxt>`. Emitted before `IDAT` alongside `tEXt`
  in the §5.6 Table 1 "Multiple OK? Yes / Ordering: None" bucket; the
  encoder writes `tEXt` ahead of `zTXt` so a streaming reader sees the
  cheap-to-display plain annotations first. 14 new unit + integration
  tests cover round-trip, multi-instance + same-keyword, large-payload
  compression (4 KB run-of-one → <200 wire bytes), unknown-method
  rejection, corrupted-zlib rejection, decompressed-NUL rejection,
  invalid-keyword rejection, chunk-ordering vs `IDAT`, ordering vs
  `tEXt`, and coexistence with `tEXt` in one file. Closes the README
  "Not preserved" entry for `zTXt`; only `iCCP` and `iTXt` remain on
  that list.

- benches: parametric `decode_apng_frame_scan` group sweeping 2 / 8 / 32
  frames at 128×128 RGBA — measures per-frame decode-loop cost
  (fcTL+fdAT sequence-number walk, disposal/blend state machine,
  per-frame inflate setup) so future optimisation rounds can A/B-test
  loop overhead in isolation from per-pixel inflate work (r196).

## [0.1.7](https://github.com/OxideAV/oxideav-png/compare/v0.1.6...v0.1.7) - 2026-05-29

### Other

- round-trip gAMA + cHRM colour-management chunks
- apply tRNS keyed transparency to Gray*/Rgb* in decode_png_to_rgba (RFC 2083 §4.2.9)
- tEXt round-trip (RFC 2083 §4.2.7 / W3C PNG3 §11.3.3.3)
- add criterion decode/encode/roundtrip harnesses (r154)
- add apng_frame_walk + encode_decode_roundtrip targets
- add decode target; fix APNG out-of-canvas fcTL offset panic
- round-trip the sPLT suggested-palette chunk
- round-trip the cICP (coding-independent code points) chunk
- round-trip the sRGB chunk (W3C PNG3 §11.3.2.5)
- round-trip the eXIf (Exif profile) ancillary chunk
- round-trip bKGD + hIST ancillary chunks
- round-trip sBIT / pHYs / tIME ancillary chunks

### Added

- `gAMA` (image gamma, RFC 2083 §4.2.3 / W3C PNG3 §11.3.2.2) and `cHRM`
  (primary chromaticities + white point, RFC 2083 §4.2.2 / W3C PNG3
  §11.3.2.1) metadata round-trip. `gAMA` is one 4-byte big-endian
  unsigned integer equal to the image gamma × 100000 (γ 0.45 ⇒ `45000`),
  preserved verbatim (the spec's "decoders should ignore a zero gamma"
  is a `should`, so the raw integer round-trips). `cHRM` is eight 4-byte
  big-endian integers — white-point x/y, red x/y, green x/y, blue x/y —
  each the 1931 CIE coordinate × 100000 (0.3127 ⇒ `31270`). Both new
  structs expose float-coordinate helpers (`Gama::gamma`,
  `Chrm::{white_point,red,green,blue}`). The encoder emits them before
  `PLTE`/`IDAT` in §4.3 "Color Chunk Priority" order (cICP `1` > sRGB
  `3` > cHRM/gAMA `4`), `gAMA` ahead of `cHRM`; the decoder reads them
  back in `parse_metadata` and rejects malformed lengths and duplicate
  chunks. Closes the README "Not preserved" entries for `gAMA` / `cHRM`.

- `tRNS` keyed-transparency application during `decode_png_to_rgba` for
  colour type 0 (grayscale, 8 / 16-bit) and colour type 2 (truecolor,
  8 / 16-bit) per RFC 2083 §4.2.9. The chunk's 2-byte BE gray sample
  (or 3 × 2-byte BE RGB triple) names one source pixel value that must
  emerge from the 8-bit-RGBA promotion with α=0; every other pixel
  stays opaque (α=255). The match is performed against the source
  sample at its full bit depth *before* the 16→8 promotion drops the
  low byte (§4.2.9 note: "Although decoders may drop the low-order
  byte of the samples for display, this must not occur until after
  the data has been tested for transparency. For example, if the
  grayscale level 0x0001 is specified to be transparent, it would be
  incorrect to compare only the high-order byte and decide that
  0x0002 is also transparent"). Closes the README "Not preserved"
  bullet "tRNS alpha applied to Gray*/Rgb* pixels on decode".

- `tRNS` structural validation in `decode_png` and `parse_apng`
  (RFC 2083 §4.2.9): colour type 0 enforces a 2-byte payload; colour
  type 2 enforces a 6-byte payload; colour types 4 and 6 reject the
  chunk outright ("tRNS is prohibited for color types 4 and 6, since
  a full alpha channel is already present"); colour type 3 caps the
  alpha-list length at the `PLTE` entry count ("tRNS chunk must not
  contain more alpha values than there are palette entries"); the
  sample value for ct=0 / ct=2 must fit `(1 << bit_depth) - 1`
  (§4.2.9 "range 0 .. (2^bitdepth)-1"). Previously the decoder
  accepted any `tRNS` length on ct=0 / ct=2 / ct=4 / ct=6 without
  diagnostic. Twelve integration tests in
  `tests/trns_gray_rgb_promotion.rs` cover each accept + reject
  path.

- `tEXt` round-trip (RFC 2083 §4.2.7 / W3C PNG3 §11.3.3.3): a Latin-1
  keyword (1-79 printable bytes, no leading / trailing / consecutive
  spaces, no null, case-sensitive) plus NUL separator plus zero-or-more
  Latin-1 text bytes (no null permitted in the text; chunk length is
  the only end marker). Surfaced as `metadata::Text` (`pub keyword`,
  `pub text`) and held by `PngMetadata::texts: Vec<Text>`. `tEXt` is
  the most permissive metadata chunk PNG defines: any number of
  instances may appear, and more than one with the same keyword is
  explicitly allowed (§4.2.7 paragraph 3) — the decoder preserves
  file order and the encoder replays it. Keyword validation is
  shared verbatim with the existing `sPLT` palette-name predicate
  (`validate_keyword`). Emitted before `IDAT` in the same "Multiple
  OK? Yes / Ordering: None" bucket as `sPLT` (RFC 2083 §4.3
  Table 1). The encoder re-validates on `to_bytes` — refuses any
  keyword that fails the rules, any text codepoint above Latin-1
  (U+0100+), and any NUL inside the text — so a malformed `Text`
  value can't silently corrupt the output PNG. Unit tests cover
  every reject path (empty keyword, 80-byte keyword, leading
  space, consecutive spaces, non-breaking space at U+00A0, DEL at
  U+007F, NUL in text on both parse and encode, non-Latin-1
  codepoint in text); integration tests in
  `tests/metadata_roundtrip.rs` cover end-to-end encode → decode
  with single-instance, multi-instance-same-keyword,
  empty-text-string, and Latin-1 high-byte payloads, plus a
  chunk-position check that the `tEXt` chunk precedes `IDAT` in
  the output PNG.
- Criterion bench harnesses (`benches/decode.rs`, `benches/encode.rs`,
  `benches/roundtrip.rs`) covering the PNG + APNG decoder and encoder
  hot paths across every pixel layout the codec supports: 1920×1080
  RGBA (the 1080p baseline), 320×240 RGBA / RGB24 / Pal8 / Rgba64Le,
  640×480 RGB24, 512×512 Gray8 / Gray16Le / Rgb48Le, plus Adam7
  seven-pass interlaced encode + decode and a 4-frame APNG round-trip
  that exercises the `acTL` / `fcTL` / `fdAT` framing. A dedicated
  `parse_metadata` scenario isolates the chunk-walk / CRC cost from
  the IDAT inflate path. Each scenario synthesises a fresh input on
  the fly with the public encoder API — no committed fixture files —
  so the benches stay reproducible from a clean checkout. Mirrors
  the `oxideav-bmp` / `oxideav-gif` / `oxideav-tiff` bench shape per
  the workspace "saturated → fuzz/bench/profile" memo so r155+
  optimisation rounds have a stable r154 baseline to A/B against.
  Initial r154 numbers on this dev box: decode_rgba_320x240
  ~302 MiB/s, decode_rgba_1920x1080 ~325 MiB/s, encode_rgba_320x240
  ~13 MiB/s (encoder is filter-heuristic + miniz-deflate bound).
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
