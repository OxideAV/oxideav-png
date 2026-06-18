# oxideav-png

Pure-Rust PNG + APNG decoder and encoder for oxideav

Part of the [oxideav](https://github.com/OxideAV/oxideav-workspace) framework — a pure-Rust media transcoding and streaming stack. Codec, container, and filter crates are implemented from the spec (no C codec libraries linked or wrapped, no `*-sys` crates). Optional hardware-engine crates (`oxideav-videotoolbox` / `-audiotoolbox` / `-vaapi` / `-vdpau` / `-nvidia` / `-vulkan-video`) bridge to OS APIs via runtime `libloading`; pass `--no-hwaccel` (or omit the `hwaccel` feature) to opt out.

## Decode support

- Colour type 0 (grayscale) at 1/2/4/8/16-bit
- Colour type 2 (RGB) at 8/16-bit
- Colour type 3 (indexed) at 1/2/4/8-bit
- Colour type 4 (grayscale + alpha) at 8/16-bit
- Colour type 6 (RGBA) at 8/16-bit
- All five PNG row filters (None / Sub / Up / Average / Paeth)
- Adam7 seven-pass interlacing
- IHDR field validity is gated at the wire-decode boundary
  (`Ihdr::parse` → `Ihdr::validate`, W3C PNG3 §11.2.1): zero width or
  height is rejected ("Zero is an invalid value"); the (colour type, bit
  depth) pair must be one of the §11.2.1 Table 12 allowed combinations
  (greyscale 1/2/4/8/16, truecolor 8/16, indexed 1/2/4/8, both alpha
  types 8/16), so a 1-bit truecolor row, a 16-bit indexed row, an
  invented colour type (1/5/7/…) or a non-Table-12 depth (0/3/…) is an
  `InvalidData` error rather than a late "not implemented" or a silently
  empty decode; and the compression / filter / interlace method bytes
  must be 0 / 0 / 0-or-1. The single gate is shared by `decode_png`,
  `parse_metadata`, `parse_apng`, and the demuxer.
- Sub-byte grayscale scaled up to 8-bit (PNG §13.12 ×255 / ×85 / ×17)
- Sub-byte indexed expanded to one index-byte-per-pixel
- APNG: `acTL` / `fcTL` / `fdAT` with None/Background/Previous disposal and
  Source/Over blending. Each `fcTL` frame region is policed against the
  IHDR canvas per W3C PNG3 §11.3.6.1: `width` / `height` must be greater
  than zero, and the region "may not fall outside of the default image"
  (`x_offset + width ≤` canvas width, `y_offset + height ≤` canvas height,
  the two sums taken in `u64` so an offset/extent pair near `u32::MAX`
  cannot wrap past the bound). A hostile out-of-canvas frame is rejected
  with an error on both the standalone `decode_apng` path and the demuxer
  frame-splitter rather than silently clipped. The shared `fcTL` / `fdAT`
  sequence-number stream is validated per W3C PNG3 §4.9.2: the first
  `fcTL` "shall contain sequence number 0" and the remaining `fcTL` /
  `fdAT` chunks "shall be in ascending order, with no gaps or
  duplicates" — a non-zero first sequence, a leading `fdAT`, or any gap /
  duplicate / descending step is an error ("Decoders shall treat
  out-of-order APNG chunks as an error", §4.9.1). `acTL.num_frames == 0`
  is rejected (§4.9: "0 is not a valid value"); a `num_frames` value that
  merely *disagrees* with the actual `fcTL` count stays advisory (the
  authoritative chain is the walked `fcTL` / `fdAT` sequence). All checks
  apply on both the standalone `parse_apng` / `decode_apng` path and the
  demuxer.
- `PLTE` + `tRNS` palettes — `PLTE` drives `Pal8` index resolution and the
  demuxer preserves both verbatim in `CodecParameters::extradata` so the
  encoder can faithfully rewrite them
- `tRNS` keyed transparency on colour type 0 (grayscale) and colour
  type 2 (RGB), 8- + 16-bit, applied by `decode_png_to_rgba` per
  RFC 2083 §4.2.9. The 2-byte BE gray sample (or 3 × 2-byte BE RGB
  triple) names one source pixel value that emerges with α=0; every
  other pixel stays opaque. The match is done at the source bit depth
  *before* the 16→8 promotion drops the low byte (§4.2.9 note: a 16-bit
  gray sample of 0x0001 keyed transparent must not flag 0x0002 as
  transparent too). `tRNS` is rejected outright on colour type 4 / 6
  ("prohibited" per §4.2.9 final paragraph), and is length-policed on
  ct=0 (exactly 2 bytes), ct=2 (exactly 6 bytes), and ct=3 (≤ PLTE
  entry count); ct=0 / ct=2 samples are bounds-checked against
  `(1 << bit_depth) - 1`.

## Encode support

- 8-bit: `Rgba`, `Rgb24`, `Gray8`, `Pal8`, `Ya8`
- 16-bit: `Rgb48Le`, `Rgba64Le`, `Gray16Le`
- Sub-byte (1, 2, 4-bit) for colour type 0 (grayscale) and colour type 3
  (indexed), opt-in via `PngEncoderOptions::bit_depth` — accepted on
  `Gray8` / `Pal8` sources only, mirroring the RFC 2083 §11.2.2
  allowed-combinations table (colour types 2 / 4 / 6 reject sub-byte
  depths). Source bytes are treated as already pre-quantized to
  `0..=(1 << bit_depth) - 1`; an over-range sample is an encode error
  ahead of the wire. Pixels pack MSB-first with the leftmost pixel in
  the high-order bits of each output byte (§2.3). Rows whose pixel
  count does not divide `8 / bit_depth` get the trailing byte's
  low-order bits zero-padded for determinism (§2.3 "the contents of
  these wasted bits are unspecified"). Combinable with `interlace =
  true` — each Adam7 pass is laid out "as though it were a complete
  image of the appropriate dimensions" (RFC 2083 §2.6) and packed
  into its own `ceil(pw * bit_depth / 8)` wire row bytes. Filtering
  is per-pass independent with a zero prior row at the top of each
  pass (§6.3 "the entire prior scanline must be treated as being
  zeroes for the first scanline … of a pass of an interlaced image").
  Empty passes (the §2.6 caution for images with fewer than five rows
  or columns) emit no filter-type bytes. The Adam7 sub-byte path is
  available on both the standalone PNG encoder and APNG output.
- Per-row filter heuristic (min-sum-abs-delta per §12.8) by default;
  caller-selectable filter strategy via `PngEncoderOptions::
  filter_strategy: FilterStrategy` (W3C PNG3 §12.7). `Adaptive` (the
  default) keeps the §12.8 trial-all-five heuristic. `Fixed(FilterType)`
  pins one filter for every
  row and skips the per-row trial — about 5× cheaper to run, at the
  compression cost of giving up the per-row best pick. §12.7's spec
  guidance maps to: `Fixed(FilterType::Paeth)` is most likely the
  best fixed choice on truecolour and grayscale; `Fixed(FilterType::
  None)` is recommended for colour type 3 (indexed) and for bit
  depths below 8. The encoder applies whatever strategy the caller
  picks at all three filter sites — the non-interlaced ≥ 8-bit path,
  the Adam7 ≥ 8-bit path, and the Adam7 sub-byte path. Registry-side
  `CodecOptions` exposes a `filter` string key with the values
  `adaptive` / `none` / `sub` / `up` / `average` / `paeth`
  (case-insensitive); the empty string maps to `adaptive` so callers
  that set the key without picking a value get the default.
- APNG output when multiple frames submitted or `frame_rate` is set
- DEFLATE/zlib (RFC 1950/1951) framing for IDAT / fdAT and the
  `zTXt` / `iTXt` / `iCCP` chunk bodies is provided by
  [`compcol`](https://crates.io/crates/compcol) (the workspace-wide
  pure-Rust compression collection). The IDAT / fdAT pixel stream's
  DEFLATE level is caller-selectable via
  `PngEncoderOptions::compression_level: Option<u8>` (`1..=9`; `1`
  fastest/largest, `9` slowest/smallest). PNG fixes only the
  compression *method* — "compression method 0 (deflate/inflate)" per
  RFC 2083 §5 — and says nothing about the DEFLATE effort level, so the
  knob is spec-neutral and produces a conformant stream at every value.
  `None` (the default) selects level `6` — the conventional default
  effort level; an out-of-`1..=9` value is an encode
  error ahead of the wire. Registry-side `CodecOptions` exposes a
  `compression_level` u32 key with `0` mapping to the default. The
  compressed metadata chunks (`zTXt` / `iTXt` / `iCCP`) keep their own
  fixed level since their payloads are small and byte-layout-pinned.

## Metadata round-trip

- `sBIT` (significant bits, RFC 2083 §4.2.6) — per-channel; variant matches
  the IHDR colour type (`Grayscale` / `Rgb` / `GrayscaleAlpha` / `Rgba`).
  Bounds-checked against `sample_depth` (8 for indexed, IHDR bit-depth
  otherwise).
- `pHYs` (physical pixel dimensions, RFC 2083 §4.2.5) — `Metre` or
  `Unknown` unit; `.dpi()` helper applies the spec's 0.0254 m/inch
  conversion.
- `tIME` (last-modification time, RFC 2083 §4.2.8) — UTC; accepts the
  `second = 60` leap-second sentinel, rejects 61.
- `bKGD` (background colour, RFC 2083 §4.2.1 / W3C PNG3 §11.3.4.1) —
  per-IHDR-colour-type variant (`Grayscale(u16)` / `Rgb(u16,u16,u16)` /
  `Palette(u8)`). Grayscale + RGB samples are range-checked against
  `(1 << IHDR.bit_depth) - 1` so a sub-16-bit image can't carry stray
  high bits (PNG3 §11.3.4.1 final paragraph). Indexed `bKGD` indices are
  bounds-checked against the `PLTE` entry count.
- `hIST` (palette histogram, RFC 2083 §4.2.4 / W3C PNG3 §11.3.4.2) —
  one `u16` per `PLTE` entry; the entry count must match exactly.
  Requires `PLTE` to be present (decode rejects orphan `hIST`).
- `tRNS` (simple transparency, RFC 2083 §4.2.9 / W3C PNG3 §11.3.1.1) —
  per-IHDR-colour-type variant (`Grayscale(u16)` for ct=0, `Rgb(u16,
  u16, u16)` for ct=2, `Palette(Vec<u8>)` for ct=3). Encoder honours
  `PngMetadata::trns` for ct=0/ct=2 keyed-sample emission (closes the
  long-standing "Not preserved" entry) and accepts a ct=3 alpha table
  too; for backwards-compat the `Pal8` path still routes the alpha
  tail through `image.palette = PLTE || tRNS`, and supplying *both*
  sources is an explicit encode error since a file can carry at most
  one `tRNS` chunk (PNG3 §5.6 Table 1 "Multiple OK? No"). Variant /
  IHDR colour-type mismatch (e.g. `Rgb` on `Gray8`) and keyed-sample
  values past `(1 << bit_depth) - 1` are encode errors so a malformed
  payload cannot reach the wire. Decode rejects `tRNS` outright on
  ct=4/ct=6 ("prohibited" per §4.2.9 final paragraph). 16-bit keyed
  samples preserve both bytes through the round-trip per §4.2.9 note
  (`0x0001` keyed transparent must NOT flag `0x0002` as transparent
  too). Emitted "After PLTE; before IDAT" per §5.6 Table 1; emission
  ordering pins land in a dedicated test.
- `eXIf` (Exif profile, W3C PNG3 §11.3.4.5) — carried as an opaque
  TIFF blob. Decode validates only the byte-order header (`II`/42 LE
  or `MM`/42 BE, §11.3.4.5.2) and round-trips the bytes verbatim; the
  TIFF directory is not interpreted. Emitted before `IDAT` (§5.6).
- `sRGB` (standard RGB colour space, W3C PNG3 §11.3.2.5) — one-byte
  ICC rendering intent (`RenderingIntent`: `Perceptual` / `Relative
  colorimetric` / `Saturation` / `Absolute colorimetric`, Table 16).
  Reserved values `4..=255` are rejected on decode. Emitted before
  `PLTE` + `IDAT` (§5.6 Table 1) alongside `sBIT`. The chunk's presence
  asserts sRGB-space samples; the codec records the intent and leaves
  any actual colour transform to the caller.
- `gAMA` (image gamma, RFC 2083 §4.2.3 / W3C PNG3 §11.3.2.2) — a single
  4-byte big-endian unsigned integer equal to the image gamma "times
  100000" (γ 0.45 ⇒ `45000`). Stored verbatim, so a round-trip is
  byte-exact; the `.gamma()` helper divides by 100000.0. A `0` payload
  "is meaningless but … decoders should ignore it" (PNG3 §11.3.2.2) — a
  `should`, not a `shall`, so the codec preserves the raw integer rather
  than rejecting it and leaves any discard to the caller. Emitted before
  `PLTE` + `IDAT`.
- `cHRM` (primary chromaticities + white point, RFC 2083 §4.2.2 / W3C
  PNG3 §11.3.2.1) — eight 4-byte big-endian unsigned integers (white-
  point x/y, red x/y, green x/y, blue x/y), each the 1931 CIE x or y
  value "times 100000" (0.3127 ⇒ `31270`). 32 bytes total; other lengths
  rejected. `.white_point()` / `.red()` / `.green()` / `.blue()` return
  the `(x, y)` pair as floats. Emitted before `PLTE` + `IDAT`. Both
  colour chunks are the lowest-precedence members of the §4.3 "Color
  Chunk Priority" table (cICP `1` > sRGB `3` > cHRM/gAMA `4`), so the
  encoder writes them after `cICP` / `sBIT` / `sRGB`, `gAMA` before
  `cHRM`.
- `cICP` (coding-independent code points, W3C PNG3 §11.3.2.6 / Table
  18) — four `u8`s naming the ITU-T H.273 code points for color
  primaries, transfer function, matrix coefficients, and the
  full-range flag. `matrix_coefficients` is pinned at `0` per §11.3.2.6
  ("RGB is currently the only supported color model in PNG, and as
  such Matrix Coefficients shall be set to `0`"); decode rejects any
  non-zero value. `video_full_range_flag` is bounds-checked to `0..=1`
  (H.273 §8.3 reserves every other value). The other two bytes are
  round-tripped verbatim (the H.273 registries include "Reserved"
  entries we deliberately don't gate, so forward-compatible code
  points still survive a copy through the codec). Emitted before
  `PLTE` + `IDAT` (§5.6 Table 1) and ahead of `sBIT` / `sRGB` —
  Table 1 in §4.3 makes `cICP` the highest-precedence colour chunk.
- `sPLT` (suggested palette, W3C PNG3 §11.3.4.4) — one or more named
  standalone palettes (independent of `PLTE`) that a viewer may use to
  display a truecolour image on indexed hardware. Each carries a
  1-79-byte Latin-1 palette name (`tEXt`-keyword rules: printable
  `0x20..=0x7E` / `0xA1..=0xFF`, no leading / trailing / consecutive
  spaces), an 8- or 16-bit sample depth, and a list of `RGBA` +
  `frequency` entries (6-byte stride at depth 8, 10-byte at depth 16).
  `sPLT` is one of two metadata chunks PNG permits to repeat; the
  decoder accepts multiple instances but rejects duplicate palette
  names. Emitted before `IDAT` (§5.6 Table 7); held as a `Vec<Splt>`
  so order is preserved.
- `tEXt` (textual data, RFC 2083 §4.2.7 / W3C PNG3 §11.3.3.3) — a
  Latin-1 keyword (1-79 printable bytes, no leading / trailing /
  consecutive spaces, no null, case-sensitive) followed by a NUL
  separator and zero-or-more Latin-1 bytes of free-form text (no
  null permitted — chunk length is the only end marker). `tEXt`
  is the most permissive metadata chunk PNG defines: any number of
  instances may appear, and more than one with the same keyword
  is allowed (§4.2.7 paragraph 3). The decoder preserves file order
  and the encoder replays it via `Vec<Text>`. Emitted before `IDAT`
  alongside `sPLT` (Table 1's "Multiple OK? Yes / Ordering
  constraints: None" bucket). Keyword validation is shared verbatim
  with `sPLT`'s palette-name predicate. International (`iTXt`) text
  remains on the "not preserved" list.
- `zTXt` (compressed textual data, RFC 2083 §4.2.10 / W3C PNG3
  §11.3.3.3) — `tEXt` semantics with the body zlib-compressed on the
  wire. The on-wire payload is a 1-79-byte Latin-1 keyword + `NUL`
  separator + 1-byte compression method (only `0` = deflate/inflate
  defined; any other value rejected per §4.2.10 "The only value
  presently defined for it is 0") + zlib-compressed Latin-1 text. The
  decoder inflates the body and applies the same no-`NUL`-in-text
  rule as `tEXt`; the encoder validates the keyword, the per-codepoint
  Latin-1 / no-`NUL` text rules, and deflates the body at the
  project's default level (6). Multiple `zTXt` chunks are permitted,
  including with identical keywords (§4.2.10 ¶6 "Any number of zTXt
  and tEXt chunks can appear in the same file"). Emitted before `IDAT`
  alongside `tEXt`; the encoder writes `tEXt` ahead of `zTXt` so a
  streaming reader sees the cheap-to-display plain annotations first.
  A 4 KB run of one character round-trips through the codec at well
  under 200 wire bytes.
- `iCCP` (embedded ICC profile, W3C PNG3 §11.3.2.3) — a named
  ICC.1 / ISO 15076-1 colour-management profile carried as an opaque
  zlib-compressed blob. On-wire payload: 1-79-byte Latin-1 profile
  name (`tEXt`-keyword rules) + `NUL` separator + 1-byte compression
  method (only `0` = deflate defined; any other value rejected per
  §11.3.2.3 "The only compression method defined in this
  specification is method 0") + zlib-compressed profile. The codec
  stores the *decompressed* profile bytes; callers do not need to know
  the chunk was compressed on the wire. The profile internals belong
  to the ICC and are not interpreted here — only the chunk framing
  (keyword rules, deflate compression method, zlib round-trip) is
  validated. Emitted before `PLTE` and `IDAT` in §4.3 Color-Chunk-
  Priority order (cICP `1` > iCCP `2` > sRGB `3` > cHRM/gAMA `4`).
  Single-instance only — duplicate `iCCP` is rejected on parse. A
  4 KB run of one byte round-trips at well under 200 wire bytes.
- `mDCV` (Mastering Display Color Volume, W3C PNG3 §11.3.2.7) — 24-byte
  HDR static metadata pairing the `cICP` colour-volume signal. Carries
  three RGB display-primary `(x, y)` CIE 1931 chromaticity pairs, the
  display white-point `(x, y)`, and the mastering display's
  maximum/minimum luminance in cd/m². Stored as the "stored integer"
  per §11.3.2.7 Table 19 (chromaticities × 50000, luminances × 10000),
  so a round-trip is byte-exact; convenience accessors
  (`primary_r/g/b()`, `white_point()`, `max_luminance_cd_m2()`,
  `min_luminance_cd_m2()`) re-divide for callers that want floats.
  Single-instance only — duplicate `mDCV` is rejected on parse per §5.6
  Table 1. Emitted before `PLTE` and `IDAT` (§11.3.2.7 "MUST come
  before the PLTE and IDAT chunks") after the §4.3-ranked colour
  chunks; pairs naturally with `cICP` for HDR10 streams (BT.2100
  primaries + PQ transfer + full-range + ST 2086 mDCV + cLLI).
- `cLLI` (Content Light Level Information, W3C PNG3 §11.3.2.8) — 8-byte
  HDR static metadata: MaxCLL (peak per-pixel cd/m²) and MaxFALL
  (peak frame-average cd/m²) of the playback sequence, both `u32` BE
  with the same `0.0001 cd/m²` divisor as `mDCV`. A zero value is the
  spec's "unknown or not currently calculable" sentinel (§11.3.2.8) —
  preserved verbatim rather than rejected, so a live APNG encoder that
  cannot yet compute the peak values can emit a placeholder `cLLI` and
  rewrite the bytes when the stream ends. Single-instance; same
  ordering bucket as `mDCV`, emitted right after it.
- `iTXt` (international textual data, W3C PNG3 §11.3.3.4) — the
  UTF-8 successor to `tEXt`. On-wire payload: 1-79-byte Latin-1
  keyword + `NUL` + compression flag (0 = uncompressed, 1 =
  zlib-compressed) + compression method (only `0` = deflate defined
  when the flag is `1`; ignored when the flag is `0` per "decoders
  shall ignore it") + language tag (BCP47, 0+ ASCII bytes, may be
  empty for "language unspecified") + `NUL` + translated keyword
  (0+ UTF-8 bytes, no `NUL`) + `NUL` + text (0+ UTF-8 bytes, no
  `NUL`). The translated keyword and text are UTF-8 [rfc3629] and
  "neither shall contain a zero byte" (§11.3.3.4); embedded `NUL` is
  rejected on parse. The language tag is checked for ASCII bytes (a
  prerequisite for a well-formed BCP47 tag) but the codec does not
  validate against the IANA language-subtag registry — that requires
  online lookup, and the spec frames the subtag-registry constraint
  as encoder-side. Multiple `iTXt` chunks are permitted, including
  with identical keywords (same rule as `tEXt` / `zTXt`). Emitted
  before `IDAT` alongside `tEXt` / `zTXt`; the encoder writes them
  after `zTXt` so the Latin-1 chunks (cheap to decode for callers
  that only need byte-exact metadata) lead the internationalised
  UTF-8 chunks in the stream.

Decode: [`parse_metadata`] returns a [`PngMetadata`] with each
supported field populated for any chunks present. Encode:
[`PngEncoderOptions`]`::metadata` holds the same struct; populated
fields are emitted at spec-compliant chunk positions (`cICP` / `iCCP` /
`sBIT` / `sRGB` / `gAMA` / `cHRM` / `mDCV` / `cLLI` before
`PLTE`/`IDAT`, the four §4.3 colour-priority chunks first then the
HDR-side colour-volume pair; `bKGD` / `hIST` / `tRNS` after `PLTE`,
before `IDAT` (`tRNS` shares the post-PLTE slot per RFC 2083 §4.2.9
"must precede the first IDAT chunk, and must follow the PLTE chunk,
if any"); `pHYs`, `tIME`, `eXIf`, `sPLT`, `tEXt`, `zTXt`, and `iTXt`
before `IDAT`). Single-instance chunks are rejected on decode if
repeated (the "Multiple OK? No" rule in RFC 2083 §4.3 / W3C PNG3
§5.6); `sPLT` requires distinct palette names; `tEXt`, `zTXt`, and
`iTXt` are the three chunks where identical keywords on multiple
instances are explicitly permitted (§4.2.7 ¶3 / §4.2.10 ¶6 /
§11.3.3.4).

## Colour management

- Decoder gamma handling (W3C PNG3 §13.13 / RFC 2083 §10.5) — an opt-in
  transform a caller invokes when it wants gamma-corrected pixels for a
  known display. The codec itself round-trips the raw `gAMA` integer
  verbatim and leaves samples untouched (the README "leaves any actual
  colour transform to the caller" promise); the `gamma` module is the
  separate stage that performs the §13.13 maths. `GammaParams` carries
  the three spec exponents — `file_gamma` (the `gAMA` value),
  `display_exponent`, and `user_exponent` — and exposes the merged
  decoding exponent `user_exponent / (file_gamma * display_exponent)`.
  `build_lut()` precomputes the 256-entry 8-bit correction table
  `floor((s / 255) ^ decoding_exponent × 255 + 0.5)` (§13.13: "only 256
  calculations per image … not one or three calculations per pixel"), and
  `apply_to_rgba` / `apply_gama_to_rgba` run it across an `RgbaBitmap`'s
  R/G/B bytes in place. Defaults are the spec's recommendations:
  `display_exponent = 2.2` ("A display exponent of 2.2 should be used
  unless detailed calibration measurements are available"),
  `user_exponent = 1.0`, and `file_gamma = 1/2.2` for the
  unknown-gamma fallback (§13.13). The alpha byte is never gamma-corrected
  ("alpha is always represented linearly", §13.16). A zero `gAMA` is
  "meaningless … Decoders should ignore it" (§13.13), so `from_gama`
  returns `None` and the appliers leave the bitmap unchanged; the same
  no-op guard fires for any non-positive `file_gamma` / `display_exponent`
  (which would divide by zero or raise to a meaningless power). A
  `user_exponent > 1` darkens mid-tones, `< 1` lightens them (§13.13). The
  endpoints are fixed for any positive exponent: `0 → 0` ("Zero raised to
  any positive power is zero") and `255 → 255`.
- Indexed-image palette gamma correction (W3C PNG3 §13.13) — the spec's
  explicit "one-time correction of the palette is sufficient"
  optimisation for colour type 3. `apply_to_palette` / `apply_gama_to_palette`
  run the same `build_lut()` table over a `Pal8` image's
  `PngImage::palette` (the `PLTE` `R/G/B` triples) once, so a viewer
  resolving indices reads already-corrected entries instead of
  gamma-correcting every output pixel. The `plte_len` argument names the
  byte length of the `PLTE` portion; any `tRNS` alpha tail at/after that
  offset is left untouched ("alpha is always represented linearly",
  §13.16). A `plte_len` that is not a multiple of 3 or runs past the
  buffer is clamped to the largest whole-triple prefix that fits, so a
  malformed length is defensive rather than a panic. Same zero-`gAMA` /
  non-positive-exponent no-op guards as the full-colour path.

## Chunk naming property bits

`ChunkType` wraps a four-byte chunk name and exposes the W3C PNG 3rd
Edition §5.4 ("Chunk naming conventions") property bits as `const fn`
predicates: `is_ancillary` / `is_critical`, `is_private` / `is_public`,
`is_reserved_bit_set`, `is_safe_to_copy` / `is_unsafe_to_copy`, plus the
§13.1 `is_well_formed_name` letter-only check. Bit 5 (value `0x20`) of
each name byte is the property bit — uppercase = `0`, lowercase = `1`.
A `ChunkRef::type_code()` bridge drops a borrowed chunk straight into
the typed accessor without copying the four bytes through a local. The
property bits are "an inherent part of the chunk type, and hence are
fixed for any chunk type" (§5.4), so e.g. `acTL` / `fcTL` / `fdAT`
read as private even though W3C PNG 3rd Edition now documents APNG —
the chunks were minted with a lowercase second letter under the
original Mozilla extension and §5.4 freezes the property bit there.
The accessor is pure read-only inspection; nothing in the decode /
encode flow has changed.

## Colour-type typed primitive

`ColourType` wraps the IHDR colour-type byte (W3C PNG3 §11.2.1 "Color
type is a single-byte integer") and surfaces the §6.1 / Table 9
named encoding — `Greyscale` (0), `Truecolor` (2), `IndexedColor` (3),
`GreyscaleAlpha` (4), `TruecolorAlpha` (6). The five variants are the
*only* values §6.1 / Table 9 defines; `ColourType::from_byte` rejects
every other byte (1, 5, 7, anything ≥ 8) so a malformed IHDR cannot
slip an invented combination past the typed gate. The §6.1 component
bits — `1` palette used, `2` truecolor used, `4` alpha used — are
exposed as `palette_used` / `truecolor_used` / `alpha_used`
predicates and the §4.5 pixel-channel count rolls up through
`channels` (1 / 3 / 1 / 2 / 4 across the five rows). The
`allows_bit_depth` predicate decodes W3C PNG3 §11.2.1 Table 12
("Allowed combinations of color type and bit depth") in one place:
greyscale accepts 1, 2, 4, 8, 16; indexed accepts 1, 2, 4, 8
(no 16-bit); truecolor / greyscale-with-alpha / truecolor-with-alpha
all accept only 8 and 16. `requires_plte` flags the one Table 12
row where a `PLTE` chunk is mandatory (colour type 3). `Ihdr` grows
a `colour_type_typed()` accessor that lifts the raw `u8` field into
the typed enum without breaking the existing `colour_type: u8` field,
and `is_allowed_combination()` returns the Table 12 verdict for the
(colour_type, bit_depth) pair on the parsed IHDR — handy as a single
gate at decode entry where the byte combinations used to be
re-derived inline. Pure typed-primitive addition; no behavioural
change to existing decode / encode paths.

## Robustness

The decoder is fuzzed with `cargo-fuzz`. Nine targets live under `fuzz/`:

- `decode` — feeds arbitrary bytes straight at the standalone decode
  entry points (`decode_png`, `decode_png_to_rgba`, `parse_metadata`,
  `parse_apng`, `decode_apng`) and asserts none of them ever panic /
  abort / overflow / OOM. Covers chunk-CRC framing, the IDAT zlib stream,
  per-row filters, sub-byte unpacking, Adam7 interlacing, PLTE/tRNS
  bounds, and the APNG container's disposal / blend paths.
- `apng_frame_walk` — builds a valid base APNG with the standalone
  encoder, then mutates every `fcTL` chunk's `dispose_op` / `blend_op` /
  `x_offset` / `y_offset` with fuzz-derived values (recomputing CRC32
  so the parser still accepts the stream), and drives `parse_apng` +
  `decode_apng_info` across 1-8-frame chains. Drives the composite
  state machine — `Previous` snapshots, `Background` clears (including
  the out-of-canvas guard), `Source` vs `Over` blend.
- `encode_decode_roundtrip` — standalone encode → decode → re-encode
  for both static PNG and APNG entry points. Asserts the decoder is a
  right inverse of the encoder on encoder-emitted bitstreams, then
  re-encodes + re-decodes to confirm image-level idempotence. Covers
  the no-`oxideav-core` standalone API path.
- `png_self_roundtrip` — encode → decode pixel-equality round-trip
  through the framework `VideoFrame` surface.
- `filter_roundtrip` — drives `filter_row` + `unfilter_row` directly
  with fuzz-derived `(FilterType, bpp, row_size, prev_row, row)`
  tuples. Bypasses the chunk-CRC / IDAT-inflate / IHDR-shape gates so
  the mutation budget lands inside the §6.2..§6.6 reconstruction
  arithmetic — every filter type at every `bpp` value (1..=8 — the
  full range `Ihdr::bpp_for_filter` emits) with row sizes up to 2 KB
  and arbitrary prior-row bytes. Asserts (1) liveness on equal-length
  slices (the only documented `Err` path is a row / prev_row length
  mismatch — impossible by construction here) and (2) the §6.1
  reversibility property `unfilter(filter(row)) == row` for every
  shape sampled.
- `encode_options` — drives `encode_png_image_with_options` over the
  option matrix the default round-trip never visits: Adam7 interlace
  (the >=8-bit and the sub-byte 1/2/4-bit pass layouts), caller-supplied
  sub-byte `bit_depth` packing (including the rejection arms — depth on
  a non-Gray8 / non-Pal8 source, `bit_depth = 16`, non-power-of-two
  depths), every `FilterStrategy` variant (Adaptive + `Fixed` ×5), and
  the ancillary-metadata emission path (`tEXt` / `pHYs` / `tIME` /
  `gAMA` chunk ordering). Accepted output is decoded through `decode_png`
  + `decode_png_to_rgba`; asserts liveness on the option-bearing encode
  path plus decode-liveness + dimension preservation on its bytes. A
  rejected option bundle is a contract outcome, not a crash.
- `metadata_chunk_splice` — builds a valid 8x8 base PNG (grayscale /
  RGB / palette) with the standalone encoder, then splices 1..=8
  fuzz-derived ancillary chunks immediately before `IEND`, each framed
  with a correct length prefix + CRC32. The 4-byte type is drawn from
  the `parse_metadata` dispatch set (`sBIT pHYs tIME bKGD hIST tRNS
  eXIf sRGB cICP gAMA cHRM mDCV cLLI sPLT tEXt zTXt iCCP iTXt`) and the
  payload is fuzz-controlled, so the mutation budget lands *inside* the
  per-chunk `::parse` routines — keyword / `NUL` splitting, the
  compression-method byte, the zlib inflate of the `zTXt` / `iTXt` /
  `iCCP` bodies, `sPLT` entry strides, the `eXIf` TIFF probe, and the
  `bKGD` / `hIST` PLTE-index bounds — rather than being rejected at the
  signature / framing / CRC gate the raw-bytes `decode` target hits.
  Drives `parse_metadata` + `decode_png` + `decode_png_to_rgba`;
  liveness only (a truncated or bomb-shaped zlib body must surface as
  `Err`, not a crash or unbounded allocation).
- Two optional cross-decode targets validate against a `dlopen`ed
  system PNG library when one is present (skipped when absent).

```sh
cargo +nightly fuzz run decode
```

## Benchmarks

Three criterion harnesses live under `benches/` for A/B-testing
optimisation changes against a stable baseline:

- `decode` — every supported pixel layout at "natural" sizes (1920×1080
  RGBA, 640×480 RGB, 512×512 Gray8 / Gray16Le / Rgb48Le, 320×240 Rgba64Le
  + Pal8 + `decode_png_to_rgba`), plus a `parse_metadata` scenario that
  isolates chunk-walk + CRC cost from the IDAT inflate, plus a 4-frame
  APNG decode covering the disposal / blend state machine, plus
  `decode_apng_frame_scan` which sweeps 2 / 8 / 32 frames
  at 128×128 RGBA so the timing curve isolates per-frame decode-loop
  overhead from per-pixel inflate work.
- `encode` — symmetric encode harness with an extra Adam7 seven-pass
  scenario at 320×240 RGBA and a 4-frame APNG encode.
- `roundtrip` — paired encode → decode at the same sizes, so a perf
  regression that silently mis-encodes surfaces as a panic rather than
  a deceptively-cheaper number.

Each scenario synthesises a fresh input on the fly with the public
encoder API — no committed fixture files — so the benches reproduce
from a clean checkout.

```sh
cargo bench -p oxideav-png --bench decode
cargo bench -p oxideav-png --bench encode
cargo bench -p oxideav-png --bench roundtrip
```

## Usage

```toml
[dependencies]
oxideav-png = "0.0"
```

## License

MIT — see [LICENSE](LICENSE).
