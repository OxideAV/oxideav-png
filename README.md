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
- Sub-byte grayscale scaled up to 8-bit (PNG §13.12 ×255 / ×85 / ×17)
- Sub-byte indexed expanded to one index-byte-per-pixel
- APNG: `acTL` / `fcTL` / `fdAT` with None/Background/Previous disposal and
  Source/Over blending
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
- Per-row filter heuristic (min-sum-abs-delta per §12.8)
- APNG output when multiple frames submitted or `frame_rate` is set

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
`sBIT` / `sRGB` / `gAMA` / `cHRM` before `PLTE`/`IDAT`, in §4.3
Color-Chunk-Priority order; `bKGD` / `hIST` after `PLTE`, before
`IDAT`; `pHYs`, `tIME`, `eXIf`, `sPLT`, `tEXt`, `zTXt`, and `iTXt`
before `IDAT`). Single-instance chunks are rejected on decode if
repeated (the "Multiple OK? No" rule in RFC 2083 §4.3 / W3C PNG3
§5.6); `sPLT` requires distinct palette names; `tEXt`, `zTXt`, and
`iTXt` are the three chunks where identical keywords on multiple
instances are explicitly permitted (§4.2.7 ¶3 / §4.2.10 ¶6 /
§11.3.3.4).

## Not preserved

- Adam7 interlaced encode (decode only — encoder always writes non-interlaced)
- Sub-byte encode (decode only — encoder always writes 8/16-bit)
- `tRNS` chunk emission for ct=0 / ct=2 (decode applies it during
  `decode_png_to_rgba`; the standalone encoder still only carries
  per-entry alpha for `Pal8` via the palette tail)
- `mDCV` (Mastering Display Color Volume, §11.3.2.7) and `cLLI`
  (Content Light Level Information, §11.3.2.8) — HDR-side ancillary
  chunks introduced in PNG 3rd edition; CRC-checked on read and then
  dropped

## Robustness

The decoder is fuzzed with `cargo-fuzz`. Five targets live under `fuzz/`:

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
  state machine — `Previous` snapshots, `Background` clears (the r124
  out-of-canvas fix lives here), `Source` vs `Over` blend.
- `encode_decode_roundtrip` — standalone encode → decode → re-encode
  for both static PNG and APNG entry points. Asserts the decoder is a
  right inverse of the encoder on encoder-emitted bitstreams, then
  re-encodes + re-decodes to confirm image-level idempotence. Covers
  the no-`oxideav-core` standalone API path.
- `png_self_roundtrip` — encode → decode pixel-equality round-trip
  through the framework `VideoFrame` surface.
- `libpng_encode_oxideav_decode` / `oxideav_encode_libpng_decode` —
  cross-decode against a `dlopen`ed system libpng (skipped when absent).

```sh
cargo +nightly fuzz run decode
```

## Benchmarks

Three criterion harnesses live under `benches/` so future optimisation
rounds can A/B-test changes against the r154 baseline:

- `decode` — every supported pixel layout at "natural" sizes (1920×1080
  RGBA, 640×480 RGB, 512×512 Gray8 / Gray16Le / Rgb48Le, 320×240 Rgba64Le
  + Pal8 + `decode_png_to_rgba`), plus a `parse_metadata` scenario that
  isolates chunk-walk + CRC cost from the IDAT inflate, plus a 4-frame
  APNG decode covering the disposal / blend state machine, plus
  `decode_apng_frame_scan` (added r196) which sweeps 2 / 8 / 32 frames
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
