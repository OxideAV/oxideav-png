# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
