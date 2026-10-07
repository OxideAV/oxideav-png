# oxideav-png

[![CI](https://github.com/OxideAV/oxideav-png/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-png/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-png.svg)](https://crates.io/crates/oxideav-png) [![docs.rs](https://docs.rs/oxideav-png/badge.svg)](https://docs.rs/oxideav-png) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust PNG + APNG decoder and encoder for oxideav

Part of the [oxideav](https://github.com/OxideAV/oxideav-workspace) framework — a pure-Rust media transcoding and streaming stack. Codec, container, and filter crates are implemented from the spec (no C codec libraries linked or wrapped, no `*-sys` crates). Optional hardware-engine crates (`oxideav-videotoolbox` / `-audiotoolbox` / `-vaapi` / `-vdpau` / `-nvidia` / `-vulkan-video`) bridge to OS APIs via runtime `libloading`; pass `--no-hwaccel` (or omit the `hwaccel` feature) to opt out.

## Standalone use

`oxideav-png` follows the OxideAV image-crate contract
(`IMAGE_CRATE_API`): the same small root vocabulary every
`oxideav-<format>` image crate exposes, usable with
`default-features = false` and no `oxideav-core`, returning pixels as
plain `Vec<u8>`.

```toml
[dependencies]
oxideav-png = { version = "0.2", default-features = false }
```

```rust
let bytes = std::fs::read("in.png")?;
if oxideav_png::probe(&bytes) {
    let info  = oxideav_png::info(&bytes)?;         // header only: width, height, format, frames
    let img   = oxideav_png::decode(&bytes)?;       // PngImage, native layout
    let rgba: Vec<u8> = img.to_rgba8();             // tightly packed RGBA, 4 * width bytes per row
    let (w, h) = (img.width(), img.height());

    let opts = oxideav_png::EncodeOptions::default().with_level(2);
    let out: Vec<u8> = oxideav_png::encode_rgba8(w, h, &rgba, &opts)?;
    std::fs::write("out.png", out)?;
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

| Item | Signature |
|---|---|
| `probe` | `fn(&[u8]) -> bool` — signature sniff, allocation-free |
| `info` | `fn(&[u8]) -> Result<ImageInfo, Error>` — `width`, `height`, `format`, `frames`, `has_alpha`, `color`, `has_icc` / `has_exif` / `has_xmp`, plus `bit_depth`, `colour_type`, `interlaced`, `num_plays` |
| `decode` / `decode_with` | `fn(&[u8][, &DecodeOptions]) -> Result<PngImage, Error>` — native layout, colour + metadata filled |
| `decode_rgb8` / `decode_rgba8` | `-> Result<RgbImage / RgbaImage, Error>` — `{ width, height, data }`, tightly packed, 3 / 4 bytes per pixel |
| `decode_all` / `decode_all_with` | `-> Result<Vec<Frame>, Error>` — `Frame { image, delay: Option<Duration> }`; one frame for a still, the composited chain for an APNG |
| `encode_all` | `(frames: &[Frame], opts: &EncodeOptions) -> Result<Vec<u8>, Error>` — the mirror of `decode_all`: a plain PNG for one delay-less frame, a full-canvas APNG otherwise (`EncodeOptions::num_plays` for the loop count) |
| `decode_from` | `fn<R: Read>(R) -> Result<PngImage, Error>` |
| `encode` | `fn(&PngImage, &EncodeOptions) -> Result<Vec<u8>, Error>` — as given, never a silent conversion |
| `encode_rgb8` / `encode_rgba8` | `fn(w, h, &[u8], &EncodeOptions)` — colour type 2 / 6 |
| `encode_to` | `fn<W: Write>(&PngImage, &EncodeOptions, W) -> Result<(), Error>` |
| `encode_plane` | `fn(w, h, PixelFormat, stride, &[u8], Option<&Palette>, &EncodeOptions) -> Result<Vec<u8>, Error>` - a borrowed plane in any layout, read in place; `Pal8` needs the palette (PNG-specific) |
| `PngImage` | `{ width, height, format: PixelFormat, planes: Vec<Plane>, color: ColorInfo, metadata: Metadata, palette: Option<Palette>, transparency: Option<Trns> }` with `new` / `packed` / `from_rgb8` / `from_rgba8` (each `Result`, rejecting geometry / length mismatches with `InvalidData`), `width()` / `height()` / `format()` / `stride()`, `as_bytes()` / `into_raw()`, `to_rgb8()` / `to_rgba8()` |
| `PixelFormat` | `= PngPixelFormat`: `Gray8`, `Gray16Le`, `Rgb24`, `Rgb48Le`, `Pal8`, `Ya8`, `Rgba`, `Rgba64Le` (names mirror `oxideav_core::PixelFormat`) |
| `Error` | `= PngError`: `InvalidData`, `Unsupported`, `LimitExceeded`, `Io`, `Eof`, `NeedMore`, `Other` |

`to_rgba8` is an exact integer kernel per layout: grey replicated,
palette looked up (RGBA entries), `tRNS` keyed samples → α 0 (16-bit
samples compared in full before the reduction), 16-bit samples reduced
to their high byte (W3C PNG3 §13.12 permits it; `rescale_16bit_to_8bit`
is the linear rescale), alpha 255 where the source has none. No gamma
or colour management is applied.

The pre-contract names — `decode_png`, `decode_png_to_rgba`,
`decode_png_over_background`, `encode_png_image`,
`encode_png_image_with_options`, `encode_png_image_threaded`,
`PngEncoderOptions`, `RgbaBitmap` — remain for one release as
`#[deprecated]` thin wrappers.

## Framework use

With the default-on `registry` feature the crate plugs into the
`oxideav-core` registry:

```rust
let mut ctx = oxideav_core::RuntimeContext::new();
oxideav_png::register(&mut ctx);                       // codec "png" + the PNG / APNG container
# let img = oxideav_png::decode(&std::fs::read("in.png")?)?;
# let mut params = oxideav_core::CodecParameters::video(oxideav_core::CodecId::new("png"));
# params.width = Some(img.width());
# params.height = Some(img.height());
# params.pixel_format = Some(oxideav_png::to_core_pixel_format(img.format()));
let dec = oxideav_png::make_decoder(&params)?;         // / make_encoder
let frame: oxideav_core::VideoFrame = img.into();      // From<PngImage>: plane + palette / colour-signal side-channels
let back = oxideav_png::PngImage::from_video_frame(&frame, &params)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The trait-side `Decoder` / `Encoder` are thin adapters over the
standalone functions (one implementation). `decode_png_to_frame` /
`encode_single` remain as `VideoFrame`-flavoured helpers.

## Supported layouts

Decode — IHDR colour type / bit depth → native `PixelFormat`:

| Colour type | Bit depth | `PixelFormat` | Notes |
|---|---|---|---|
| 0 greyscale | 1 / 2 / 4 / 8 | `Gray8` | sub-byte scaled ×255 / ×85 / ×17 (§13.12) |
| 0 greyscale | 16 | `Gray16Le` | |
| 2 truecolour | 8 / 16 | `Rgb24` / `Rgb48Le` | `tRNS` key on `PngImage::transparency` |
| 3 indexed | 1 / 2 / 4 / 8 | `Pal8` | one index byte per pixel; `PLTE` + `tRNS` → `Palette` (RGBA entries) |
| 4 grey + alpha | 8 / 16 | `Ya8` / `Rgba64Le` | 16-bit grey replicated (no `Ya16` layout) |
| 6 truecolour + alpha | 8 / 16 | `Rgba` / `Rgba64Le` | |

All five row filters, Adam7, split `IDAT` runs, APNG (every dispose /
blend operator, 8- and 16-bit compositing).

Encode — `PngImage::format` → IHDR:

| `PixelFormat` | Colour type / bit depth | Options |
|---|---|---|
| `Gray8` | 0 / 8, or 1 / 2 / 4 with `EncodeOptions::bit_depth` | |
| `Gray16Le` | 0 / 16 | |
| `Rgb24` / `Rgb48Le` | 2 / 8 / 16 | |
| `Pal8` | 3 / 8, or 1 / 2 / 4 with `bit_depth` | `PLTE` from every `Palette` entry, `tRNS` up to the last non-opaque one |
| `Ya8` | 4 / 8 | |
| `Rgba` / `Rgba64Le` | 6 / 8 / 16 | |

Any layout may be Adam7 interlaced. APNG: `encode_all(&[Frame],
&EncodeOptions)` is the mirror of `decode_all` — one delay-less frame
is a plain PNG, otherwise a full-canvas APNG (every frame shares frame
0's geometry and layout; a frame's `delay` becomes the finest `fcTL`
rational that fits 16 bits, so whole-millisecond delays round-trip
exactly; the loop count is `EncodeOptions::num_plays`, `0` = forever).
The depth entry points stay: `encode_apng` (full canvas, one delay)
and `encode_apng_frames` (per-frame regions, rational delays, dispose /
blend, optional separate default image). Every APNG path writes the
canvas source's `color` / `metadata` / `transparency` chunks exactly as
`encode` does. `encode` returns `Error::Unsupported` for what PNG
cannot carry (a non-identity `color.matrix`) and never converts.

## Options

`DecodeOptions` (`Default` + `with_*`): `max_width`, `max_height`,
`max_pixels`, `max_bytes` (decoded plane bytes per image; default
1 GiB, `None` lifts it) — all checked against the header before any
allocation (`Error::LimitExceeded`) — and `strict` (default `false`):
in lenient mode a malformed or duplicated ancillary colour / metadata
chunk is dropped and the pixels still decode; in strict mode those
chunks must parse and be unique and the §5.6 Table 7 ancillary
ordering is enforced. Signature, CRCs, critical-chunk ordering, IHDR
validity, `tRNS` validity and the inflate bound are enforced in both
modes.

`EncodeOptions` (`Default` + `with_*`): `compression_level`
(`with_level`, 1..=9, default 2), `filter_strategy` (adaptive §12.8
heuristic, fixed, or brute), `interlace`, `bit_depth` (1 / 2 / 4 for
`Gray8` / `Pal8`), `threads` (DEFLATE workers; bytes never depend on
it), `metadata: Option<PngMetadata>` (every ancillary chunk, wins over
the image-derived chunk).

## Metadata and colour

`PngImage::metadata` — `icc` (`iCCP`), `exif` (`eXIf`), `xmp` (the
`XML:com.adobe.xmp` `iTXt`), `gamma` (`gAMA` / 100 000).
`PngImage::color` — `{ range, primaries, transfer, matrix }` (H.273
code points), resolved by W3C PNG3 §4.3 Table 1 precedence:

| Highest colour chunk | `color` |
|---|---|
| `cICP` | its four fields (range from the full-range flag) |
| `iCCP` | `ColorInfo::png_default()` — the profile in `metadata.icc` governs |
| `sRGB` | `ColorInfo::srgb()` = full range, primaries 1, transfer 13, matrix 0 |
| `cHRM` (+ `gAMA`) | primaries 1 (BT.709) / 9 (BT.2020) when the chromaticities match, transfer unspecified; `gAMA` only ever lands in `metadata.gamma` |
| none | `ColorInfo::png_default()` — full-range RGB, primaries / transfer unspecified (§13.14 "device-dependent RGB") |

`encode` writes them back: `gAMA` / `iCCP` (named "ICC Profile") /
`eXIf` / XMP `iTXt` from `metadata`; `sRGB` (perceptual) for exactly
`ColorInfo::srgb()` unless an ICC profile is present, otherwise `cICP`
(matrix 0) whenever a code point is specified or the range is limited;
`tRNS` from `transparency` / palette alpha. `decode(encode(img)) ==
img` for planes, colour, metadata, palette and transparency
(`tests/image_crate_api.rs`). The full chunk set (`sBIT`, `pHYs`,
`tIME`, `bKGD`, `hIST`, `mDCV`, `cLLI`, `sPLT`, text chunks, unknown
ancillary chunks) is `parse_metadata` / `PngMetadata`, see *Metadata
round-trip* below.

## Limits

Every function returns `Error` on hostile input, never panics (fuzzed:
`probe` / `info` / `decode` / `decode_with` / `decode_rgb8` /
`decode_rgba8` / `decode_all` plus the APNG and metadata parsers).
`DecodeOptions` limits fire before allocation; the inflate is bounded
at the exact filtered-stream size the header implies (decompression
bombs, §13.3); inflated metadata bodies are capped at
`MAX_INFLATED_METADATA_LEN`; every CRC is validated.

## PNG specifics

### Decode support

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
  must be 0 / 0 / 0-or-1. The single gate is shared by `decode`,
  `parse_metadata`, `parse_apng`, and the demuxer.
- Chunk ordering is enforced per W3C PNG3 §5.1 / §5.6 ("Chunks higher up
  shall appear before chunks lower down"). The shared chunk walker
  rejects a non-`IHDR` first chunk, a second `IHDR`, and a `PLTE` that
  does not precede the first `IDAT` (§5.1 / §5.6 Table 7), plus a
  non-consecutive `IDAT` run (§5.6 / §11.2.3 "Multiple IDAT chunks shall
  be consecutive" — a run broken by an intervening chunk would silently
  splice two compressed segments). On top of that the §5.6 Table 7
  ancillary buckets are policed before any chunk body is parsed:
  `cHRM` / `cICP` / `gAMA` / `iCCP` / `mDCV` / `cLLI` / `sBIT` / `sRGB`
  shall come **before PLTE and IDAT**; `bKGD` / `hIST` / `tRNS` shall come
  **after PLTE (when present) and before IDAT**; `eXIf` / `pHYs` / `sPLT`
  shall come **before IDAT**; `tIME` / `tEXt` / `zTXt` / `iTXt` carry no
  ordering constraint. All checks fire uniformly on `parse_metadata`,
  `parse_apng`, and the container demuxer (the encoder already emits in
  conformant order, so its own output round-trips).
- Sub-byte grayscale scaled up to 8-bit (PNG §13.12 ×255 / ×85 / ×17)
- Sub-byte indexed expanded to one index-byte-per-pixel
- APNG: `acTL` / `fcTL` / `fdAT` with None/Background/Previous disposal and
  Source/Over blending. The frame compositor implements the non-
  premultiplied OVER (W3C PNG3 §13.16, referenced by §11.3.6.2
  `blend_op`) at **8-bit and 16-bit** per-channel precision (RGBA, gray+
  alpha, and 16-bit RGBA / Ya16-as-RGBA64). On alpha-less canvases
  (colour types 0/2/3) OVER honours `tRNS`-keyed transparency — a keyed
  gray / RGB sample or a transparent palette index leaves the canvas,
  everything else overwrites (binary, since the canvas carries no alpha).
  A first-frame `dispose_op` of `Previous` is normalised to `Background`
  (§11.3.5.1). Each `fcTL` frame region is policed against the IHDR canvas
  per W3C PNG3 §11.3.6.1: `width` / `height` must be greater than zero,
  and the region "may not fall outside of the default image"
  (`x_offset + width ≤` canvas width, `y_offset + height ≤` canvas height,
  the two sums taken in `u64` so an offset/extent pair near `u32::MAX`
  cannot wrap past the bound). The default-image `fcTL` (the one preceding
  `IDAT`) additionally must sit at offset (0, 0) with full-canvas
  dimensions (§11.3.5.1). A hostile out-of-canvas frame is rejected with
  an error on both the standalone `decode_apng` path and the demuxer
  frame-splitter rather than silently clipped. The shared `fcTL` / `fdAT`
  sequence-number stream is validated per W3C PNG3 §4.9.2: the first
  `fcTL` "shall contain sequence number 0" and the remaining `fcTL` /
  `fdAT` chunks "shall be in ascending order, with no gaps or
  duplicates" — a non-zero first sequence, a leading `fdAT`, or any gap /
  duplicate / descending step is an error ("Decoders shall treat
  out-of-order APNG chunks as an error", §4.9.1). The `acTL` must appear
  before the first `IDAT` (§4.9.1) and at most once (§5.6 "Multiple OK?
  No"); `acTL.num_frames == 0` is rejected (§4.9: "0 is not a valid
  value"); a `num_frames` value that merely *disagrees* with the actual
  `fcTL` count stays advisory (the authoritative chain is the walked
  `fcTL` / `fdAT` sequence). All checks apply on both the standalone
  `parse_apng` / `decode_apng` path and the demuxer.
- `PLTE` + `tRNS` palettes — `PLTE` drives `Pal8` index resolution and the
  demuxer preserves both verbatim in `CodecParameters::extradata` so the
  encoder can faithfully rewrite them
- `tRNS` keyed transparency on colour type 0 (grayscale) and colour
  type 2 (RGB), 8- + 16-bit, applied by `decode_rgba8` per
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

### Encode support

- 8-bit: `Rgba`, `Rgb24`, `Gray8`, `Pal8`, `Ya8`
- 16-bit: `Rgb48Le`, `Rgba64Le`, `Gray16Le`
- Sub-byte (1, 2, 4-bit) for colour type 0 (grayscale) and colour type 3
  (indexed), opt-in via `EncodeOptions::bit_depth` — accepted on
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
  caller-selectable filter strategy via `EncodeOptions::
  filter_strategy: FilterStrategy` (W3C PNG3 §12.7). `Adaptive` (the
  default) keeps the §12.8 trial-all-five heuristic. `Fixed(FilterType)`
  pins one filter for every
  row and skips the per-row trial — about 5× cheaper to run, at the
  compression cost of giving up the per-row best pick. §12.7's spec
  guidance maps to: `Fixed(FilterType::Paeth)` is most likely the
  best fixed choice on truecolour and grayscale; `Fixed(FilterType::
  None)` is recommended for colour type 3 (indexed) and for bit
  depths below 8. `Brute` is the §12.7 "try every combination … find
  what compresses best" exhaustive search, reduced to the tractable
  per-image-fixed form: the encoder filters the whole image under each
  of the six candidate row-strategies (`Adaptive` + the five `Fixed`
  types), deflates every candidate, and emits the smallest. Where
  `Adaptive` minimises a *proxy* for compressed size (the signed-byte
  absolute sum), `Brute` measures the real DEFLATE size, so its output
  is always at least as small as `Adaptive` and never larger than any
  `Fixed` choice — at the cost of six full-image deflate passes
  (slowest; opt-in). The encoder applies whatever strategy the caller
  picks at all four filter sites — the non-interlaced ≥ 8-bit path,
  the non-interlaced sub-byte path, the Adam7 ≥ 8-bit path, and the
  Adam7 sub-byte path. Registry-side
  `CodecOptions` exposes a `filter` string key with the values
  `adaptive` / `none` / `sub` / `up` / `average` / `paeth` / `brute`
  (case-insensitive); the empty string maps to `adaptive` so callers
  that set the key without picking a value get the default.
- Container timing (`registry`): the `png` demuxer's stream tick is
  `1 / lcm(delay_den…)` of the file's `fcTL`s — `1/100` for centisecond
  files — so every frame `duration` is exact; the `png` muxer writes a
  packet's `duration` as the `fcTL` fraction of the stream's `time_base`
  (kept in the stream's own denominator when the `u16` fields hold it,
  reduced otherwise), so `demux(mux(frames))` returns the input ticks
  and time base for 1/1000, 1/100 or frame-rate streams alike.
- APNG output when multiple frames submitted or `frame_rate` is set.
  `encode_apng` paints every frame full-canvas with `Disposal::None` /
  `Blend::Source` and a single shared delay. The region-aware
  `encode_apng_frames` / `encode_apng_frames_with_options` take a
  `&[ApngFrameSpec]` where each frame carries its own sub-canvas region
  (`x_offset` / `y_offset` plus a smaller frame extent that becomes the
  `fcTL` width/height), its own `delay_num` / `delay_den` rational
  duration, and its own `Disposal::{None,Background,Previous}` /
  `Blend::{Source,Over}` operators — the full W3C PNG 3rd Edition
  §11.3.6.1 frame-control surface the decoder's compositor already
  reads back. An optional separate full-canvas default (still) image is
  emitted in the `IDAT` *before* the first `fcTL` and excluded from
  `acTL.num_frames` (the image non-APNG viewers show); with no separate
  default the first frame's full-canvas pixels become the `IDAT` *and*
  the first animation frame (`fcTL` precedes `IDAT`). Frame regions are
  policed against the canvas (§11.3.6.1: non-zero extent,
  `x_offset + width ≤ canvas_width`, `y_offset + height ≤
  canvas_height`) before any compression work, and the first `fcTL`
  carries sequence number 0 with every subsequent `fcTL` / `fdAT`
  sequence number contiguous-ascending (§4.9.2). Each frame's
  sub-region is compressed against a synthetic per-frame IHDR so it
  rides in its own `fdAT` (or the `IDAT` for the default image). The
  emitted streams round-trip through `decode_apng` byte-for-byte across
  the dispose / blend / offset matrix, and the `apng_region_encode`
  fuzz target drives the encoder directly with fuzz-derived regions /
  offsets / delays / operators (215k+ executions, zero crashes).
- DEFLATE (RFC 1951) for IDAT / fdAT and the `zTXt` / `iTXt` / `iCCP`
  chunk bodies is provided by
  [`compcol`](https://crates.io/crates/compcol) (the workspace-wide
  pure-Rust compression collection). The IDAT / fdAT pixel stream's
  DEFLATE level is caller-selectable via
  `EncodeOptions::compression_level: Option<u8>` (`1..=9`; `1`
  fastest/largest, `9` slowest/smallest). PNG fixes only the
  compression *method* — "compression method 0 (deflate/inflate)" per
  RFC 2083 §5 — and says nothing about the DEFLATE effort level, so the
  knob is spec-neutral and produces a conformant stream at every value.
  `None` (the default) selects `DEFAULT_COMPRESSION_LEVEL` = `2`,
  chosen by measurement in round 464 (table below: the knee that keeps
  a 12 MP frame under half a second on one thread within ~10 % of the
  level-6 size; `Some(6)` restores the pre-464 bytes on small images);
  an out-of-`1..=9` value is an encode
  error ahead of the wire. Registry-side `CodecOptions` exposes a
  `compression_level` u32 key with `0` mapping to the default, plus
  `level` and `compression` as aliases (the framework-wide dial name:
  `oxideav convert --opt level=…`). The
  compressed metadata chunks (`zTXt` / `iTXt` / `iCCP`) keep their own
  fixed level since their payloads are small and byte-layout-pinned.
- **Streaming, segmented, optionally parallel pixel stream.** The
  non-interlaced path filters rows one at a time straight out of the
  caller's plane (honouring `stride`; 16-bit rows byte-swapped into a
  two-row ring) and deflates them directly into the `IDAT` / `fdAT`
  chunk — no filtered-image intermediate, no flattened copy, no
  separate compressed buffer. The stream is cut into independent
  DEFLATE segments of about 1 MiB of filtered rows; each segment is a
  run of RFC 1951 blocks closed by a sync flush (byte-aligned,
  `BFINAL = 0`) so the segments concatenate into one deflate stream,
  and the crate writes the RFC 1950 container around them (`CMF` /
  `FLG` header, the `01 00 00 FF FF` final empty stored block, the
  Adler-32 trailer folded from per-segment sums). The grid depends
  only on the image, so the bytes are identical at any thread count;
  a stream that fits one segment goes through compcol's zlib encoder
  with a normal `finish` and is byte-identical to the previous
  one-shot output. `EncodeOptions::threads` (and
  `encode_apng_threaded` / `encode_apng_frames_threaded`) set the thread
  budget (`std::thread::scope` workers over the segments); the
  framework encoder maps `ExecutionContext::threads` onto it via
  `set_execution_context`, so `oxideav convert` gets the parallel path
  while the un-threaded API stays serial. Segmenting costs +0.01 %
  (level 1) to +0.04 % (level 6) on a 12 MP photo.

### Metadata round-trip

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
  too; the `Pal8` path normally carries the alpha on the `Palette`
  entries (written as the `tRNS` tail up to the last non-opaque
  entry), and supplying *both* sources is an explicit encode error since a file can carry at most
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
  is round-tripped too (see the `iTXt` entry below).
- `zTXt` (compressed textual data, RFC 2083 §4.2.10 / W3C PNG3
  §11.3.3.3) — `tEXt` semantics with the body zlib-compressed on the
  wire. The on-wire payload is a 1-79-byte Latin-1 keyword + `NUL`
  separator + 1-byte compression method (only `0` = deflate/inflate
  defined; any other value rejected per §4.2.10 "The only value
  presently defined for it is 0") + zlib-compressed Latin-1 text. The
  decoder inflates the body and applies the same no-`NUL`-in-text
  rule as `tEXt`; the encoder validates the keyword, the per-codepoint
  Latin-1 / no-`NUL` text rules, and deflates the body at a
  fixed level 6 (metadata chunks are small and byte-layout-pinned). Multiple `zTXt` chunks are permitted,
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
  4 KB run of one byte round-trips at well under 200 wire bytes. The
  inflate is bounded (`MAX_INFLATED_METADATA_LEN`, 64 MiB) so an
  oversized-profile / decompression-bomb chunk errors at the bound
  (W3C PNG3 §13.3) instead of committing attacker-chosen memory; the
  emitter mirrors the bound.
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
  UTF-8 chunks in the stream. Both compressed-text chunks (`zTXt` /
  `iTXt`) share the `iCCP` inflate bound, so a compression-bomb body
  errors instead of allocating without limit; the UTF-8 / no-`NUL`
  rules apply identically to the compressed and uncompressed arms.

- Unrecognised ancillary chunks (`PngMetadata::unknowns`, W3C PNG3 §14.2
  "Behavior of PNG editors") — the PNG-*editor* round-trip. Any ancillary
  chunk type the codec does not parse (a private third-party extension, a
  future public chunk, …) is captured verbatim as an [`UnknownChunk`]
  carrying its 4-byte type, payload bytes, and an `after_idat` flag
  recording which side of the `IDAT` run it sat on. The encoder replays
  each one on the same side of `IDAT` — §14.2's one positional rule for
  the editor case ("a PNG editor shall not move the chunk from before
  IDAT to after IDAT or vice versa"); `is_safe_to_copy()` /
  `is_private()` surface the §5.4 property bits without a re-decode so a
  caller can decide whether to keep a chunk after editing critical
  chunks. An unrecognised *critical* chunk (§5.4 ancillary bit clear) is
  a hard decode error on both `parse_metadata` and the pixel `decode`
  path — "PNG editors shall terminate on encountering an unrecognized
  critical chunk type … there is no way to be certain that a valid
  datastream will result" (§14.2) / "indicate to the user that the image
  contains information it cannot safely interpret" (§5.4). A chunk whose
  name carries a non-letter byte (§13.1-malformed) is dropped rather than
  captured, since re-emitting it would propagate a non-conformant name.
  File order is preserved on decode and replayed on encode.

Decode: [`parse_metadata`] returns a [`PngMetadata`] with each
supported field populated for any chunks present. Encode:
[`EncodeOptions`]`::metadata` holds the same struct; populated
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

Colour-chunk precedence (W3C PNG3 §4.3 Table 1) is *resolved*, not
just emission-ordered: `PngMetadata::colour_source()` returns a
`ColourSource` (`Cicp` > `Iccp` > `Srgb` > `GamaChrm`, priorities
1–4) naming the signal that governs the samples — "the chunk with the
lowest Priority number should take precedence and any higher-numbered
chunk types should be ignored", restated per-chunk by §11.3.2.3 /
§11.3.2.5 ("ignored unless it is the highest-precedence color chunk").
Resolution is read-side only; every chunk still round-trips verbatim.
On the write side, §11.3.2.5 Table 17 is enforced: an `sRGB` chunk may
ride only with the sRGB-equivalent companion values ("Only the
following values shall be used") — `Gama::SRGB` (45455) and
`Chrm::SRGB` (sRGB primaries + D65) are provided as constants and a
contradicting pairing is an encode error, while non-`sRGB` streams
keep free choice of `gAMA` / `cHRM`.

### Colour management

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
  `apply_to_rgba` / `apply_gama_to_rgba` run it across an `RgbaImage`'s
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
- 16-bit decoder gamma correction (W3C PNG3 §13.13) — the §13.13 formula
  is written for an arbitrary sample depth (`sample = integer_sample /
  (2^sampledepth - 1.0)`; `framebuf_sample = floor(display_input ×
  MAX_FRAMEBUF_SAMPLE + 0.5)`, "MAX_FRAMEBUF_SAMPLE … 255 for 8-bit, 31
  for 5-bit, etc"), so the 8-bit LUT is the `MAX = 255` specialisation and
  `build_lut16()` is the `MAX = 65535` one — a 65536-entry `u16` table
  (`floor((s / 65535) ^ decoding_exponent × 65535 + 0.5)`, heap-boxed at
  128 KiB so it never lands on the stack). `apply_to_png16` /
  `apply_gama_to_png16` run it across the little-endian colour samples of a
  `PngImage` in the three 16-bit layouts (`Gray16Le` = 1 colour sample,
  `Rgb48Le` = 3, `Rgba64Le` = 3 + a linear alpha sample left untouched per
  §13.16). The same merged decoding exponent drives both widths — only the
  normalisation / frame-buffer denominators change with the depth — so the
  endpoints stay pinned (`0 → 0`, `65535 → 65535`). Non-16-bit formats
  (`Gray8` / `Rgb24` / `Pal8` / `Ya8` / `Rgba`) are a no-op `false` (the
  8-bit appliers own those widths); a `stride` wider than `width × bpp`
  corrects only the live samples and skips the trailing padding. Same
  zero-`gAMA` / non-positive-exponent no-op guards as the 8-bit path.

- sRGB linear-light conversion (IEC 61966-2-1, referenced by W3C PNG3 /
  ISO 15948 §11.3.2 for the sRGB-default colour space and §13 as the
  prerequisite for correct compositing) — the `srgb` module implements
  the standard sRGB electro-optical transfer function and its exact
  inverse, driven entirely by three committed bit-exact numeric tables
  (`png_sRGB_table` / `png_sRGB_base` / `png_sRGB_delta` under
  `docs/image/png/tables/`). `srgb_to_linear8(u8) → u16` is one lookup
  into the 256-entry Q16 EOTF table (`0 → 0`, `255 → 65535`, first steps
  `1 → 20` / `2 → 40` matching the `1/12.92` linear-segment slope);
  `srgb_from_linear(u32) → u8` inverts it from an 8-bit-scaled linear
  value (`0..=255·65535`) via the paired 512-entry base/delta tables
  (top 9 bits select the pair, low 15 bits scale the delta), saturating
  out-of-range input to white. The two tables are exact inverses at
  8-bit precision, so `srgb_from_linear(srgb_to_scaled_linear8(s)) == s`
  for all 256 values. `linearize_rgba` widens a decoded `RgbaImage`'s
  R/G/B to 16-bit linear (alpha passed through linearly per §13.16), and
  `composite_over_background` performs source-over alpha compositing in
  linear light — the §13-correct path, where a 50%-alpha white pixel
  over black re-encodes to ~188 sRGB rather than the gamma-space 128
  average. An integration suite (`tests/srgb_compositing.rs`) drives the
  whole chain against the real decode path: encode → decode an
  `sRGB`-chunked PNG, confirm the colour-space marker survives, then
  linearize and composite.

- `bKGD` background compositing (W3C PNG3 §13.15 "Background color" /
  §13.16 "Alpha channel processing" / §13.12 "Sample depth rescaling") —
  the §13.15 "display the image against a background" path for viewers
  that cannot present real transparency. `Bkgd::resolve_rgb8` turns any
  `bKGD` variant into a concrete 8-bit sRGB `[R, G, B]`: a grayscale
  sample is rescaled from the IHDR `bit_depth` to 8 bits with the §13.12
  linear equation `floor(input × 255 / (2^bit_depth − 1) + 0.5)` (a
  4-bit grey `15` → `255`, a 16-bit `0x8000` → `128` rather than a
  low-byte discard) and replicated into R = G = B; an `Rgb` variant
  rescales each channel the same way; a `Palette` index looks up the
  `R G B` triple in the `PLTE` body (a missing / too-short palette or an
  out-of-range index is an error). `decode_over_background(buf,
  override_bg)` then decodes to RGBA exactly as `decode_rgba8` and
  composites every pixel's straight alpha over the background **in linear
  light** (§13.16 "should be performed with intensity samples, not
  gamma-encoded samples"; `out = α·foreground + (1−α)·background` per
  channel) via `composite_over_background`, returning an opaque bitmap.
  The background source follows §13.15 precedence: caller `override_bg`
  (a browser "should ignore the bKGD chunk … overriding bKGD with their
  preferred background color") > the datastream's `bKGD` chunk >
  `DEFAULT_BACKGROUND_GREY` (the §13.15 "medium grey such as 153 in the
  8-bit sRGB color space" fallback when no other information is
  available). A 6-test integration suite (`tests/bkgd_compositing.rs`)
  drives the chain through the real encode → decode path across the
  default-grey fallback, the override-beats-chunk precedence, the
  RGB-chunk half-alpha blend, the indexed `tRNS` + `bKGD`-index
  transparent-entry case, and the opaque-no-op / packed-opaque-output
  invariants. The new entry point is also folded into the `decode` fuzz
  target (both the chunk-resolution and override arms).

- Sample-depth scaling (W3C PNG3 §12.4 "Sample depth scaling" / §13.12
  "Sample depth rescaling") — the `depth` module moves a sample between
  bit depths. It exposes the three spec methods as pure primitives:
  `rescale_sample` is the "most accurate" linear equation
  `floor(input × max_out / max_in + 0.5)` (u64-safe rounding, covering
  scale-up, scale-down and identity in one function);
  `scale_up_bit_replication` is §12.4 left bit replication (the spec's
  worked example scales the 5-bit `27` = `11011` up to the 8-bit `222` =
  `11011110`, "never off by more than one" from linear — a property test
  confirms this across every from/to depth pair); `scale_up_zero_fill`
  is the §12.4 "distinctly less accurate" left shift (documented as
  not-for-alpha since it cannot reproduce an all-ones maximum); and
  `recover_sbit` performs the §13.12 significant-bit recovery
  (`sample >> (stored_bits − sbit)`) — because the encoder is required
  to preserve the high-order bits when it writes `sBIT`, this shift round
  -trips every §12.4 scale-up method back to the reference sample.
  Layered on top, `rescale_16bit_to_8bit` accurately reduces a decoded
  16-bit `PngImage` to its 8-bit counterpart (`Gray16Le` → `Gray8`,
  `Rgb48Le` → `Rgb24`, `Rgba64Le` → `Rgba`) — every colour **and** alpha
  sample rescaled linearly (a depth reduction, distinct from gamma per
  §13.16), endpoints exact (`0 → 0`, `65535 → 255`, mid-scale
  `0x8000 → 128` rather than the low-byte-discard shortcut), stride
  padding dropped. `rescale_16bit_to_8bit_via_sbit` first recovers each
  channel's `sBIT` significant bits before scaling — "using sBIT to
  recover the original samples before scaling them to suit the display
  often yields a more accurate display than ignoring sBIT" (§13.12); a
  channel with `S = 16` (or an `Sbit` variant that doesn't match the
  pixel format) falls back to the plain linear path. An 8-bit input is
  returned unchanged. An integration suite (`tests/depth_rescale.rs`)
  drives the whole chain through the real encode → decode path; the pure
  primitives and the 16→8 reduction are also folded into the `decode`
  fuzz target's liveness contract.

### Chunk naming property bits

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

### Colour-type typed primitive

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

### Robustness

Every zlib inflate is output-bounded (W3C PNG3 §13.3): the IDAT /
fdAT pixel streams are capped at the exact filtered-stream size the
IHDR / fcTL dimensions imply (`Ihdr::expected_filtered_len()`, both
interlace layouts), and the compressed metadata bodies (`zTXt` /
`iTXt` / `iCCP`) at `MAX_INFLATED_METADATA_LEN` (64 MiB) — a
decompression bomb errors at the bound instead of allocating without
limit. `tests/hostile_inflate.rs` pins the whole surface: per-chunk
and spliced-into-file bombs, IDAT / fdAT bombs behind tiny declared
canvases, oversized-profile encode rejection, bad-UTF-8 / embedded-NUL
bodies in the compressed `iTXt` arm, and hand-computed
`expected_filtered_len` values including the Adam7 per-pass sum and
the max-dimension saturation case.

The decoder is fuzzed with `cargo-fuzz`. Nine targets live under `fuzz/`:

- `decode` — feeds arbitrary bytes straight at the standalone decode
  entry points (`decode`, `decode_rgba8`, `parse_metadata`,
  `parse_apng`, `decode_apng`) and asserts none of them ever panic /
  abort / overflow / OOM. Covers chunk-CRC framing, the IDAT zlib stream,
  per-row filters, sub-byte unpacking, Adam7 interlacing, PLTE/tRNS
  bounds, and the APNG container's disposal / blend paths. Also drives
  the §12.4 / §13.12 sample-depth primitives (`rescale_sample` /
  `scale_up_bit_replication` / `scale_up_zero_fill` / `recover_sbit`)
  with fuzz-derived depth pairs and the `rescale_16bit_to_8bit` /
  `_via_sbit` reduction over any successfully decoded image — pure
  integer arithmetic that must never overflow / shift out of range.
- `apng_frame_walk` — builds a valid base APNG with the standalone
  encoder, then mutates every `fcTL` chunk's `dispose_op` / `blend_op` /
  `x_offset` / `y_offset` with fuzz-derived values (recomputing CRC32
  so the parser still accepts the stream), and drives `parse_apng` +
  `decode_apng_info` across 1-8-frame chains. Drives the composite
  state machine — `Previous` snapshots, `Background` clears (including
  the out-of-canvas guard), `Source` vs `Over` blend.
- `apng_region_encode` — drives the region-aware encoder
  (`encode_apng_frames_with_options`) *directly* with fuzz-derived
  per-frame sub-regions (width/height + `x_offset` / `y_offset`
  spanning the in-canvas / on-edge / out-of-canvas bands), rational
  delays, every dispose+blend operator, the separate-default-image vs
  first-frame-is-default branch, and Adam7 interlace. Asserts encode
  liveness and — on the happy path — that any accepted encode
  re-decodes through `decode_apng` to exactly the submitted frame count
  at the submitted canvas dimensions. Funnels the budget into the
  encoder's `fcTL`-emission / `fdAT`-framing / synthetic-per-frame-IHDR
  compression path that `apng_frame_walk` (which mutates a pre-built
  stream) never reaches.
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
- `encode_options` — drives `encode` over the
  option matrix the default round-trip never visits: Adam7 interlace
  (the >=8-bit and the sub-byte 1/2/4-bit pass layouts), caller-supplied
  sub-byte `bit_depth` packing (including the rejection arms — depth on
  a non-Gray8 / non-Pal8 source, `bit_depth = 16`, non-power-of-two
  depths), every `FilterStrategy` variant (Adaptive + `Fixed` ×5), and
  the ancillary-metadata emission path (`tEXt` / `pHYs` / `tIME` /
  `gAMA` chunk ordering). Accepted output is decoded through `decode`
  + `decode_rgba8`; asserts liveness on the option-bearing encode
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
  Drives `parse_metadata` + `decode` + `decode_rgba8`;
  liveness only (a truncated or bomb-shaped zlib body must surface as
  `Err`, not a crash or unbounded allocation).
- Two optional cross-decode targets validate against a `dlopen`ed
  system PNG library when one is present (skipped when absent).

```sh
cargo +nightly fuzz run decode
```

### Benchmarks

Six criterion harnesses live under `benches/` for A/B-testing
optimisation changes against a stable baseline:

- `decode` — every supported pixel layout at "natural" sizes (1920×1080
  RGBA, 640×480 RGB, 512×512 Gray8 / Gray16Le / Rgb48Le, 320×240 Rgba64Le
  + Pal8 + `decode_rgba8`), plus a `parse_metadata` scenario that
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
- `filter` — the §6 per-row reconstruct (`unfilter_row`) and the §12.8
  filter heuristic (`choose_filter_heuristic` / `filter_row`) driven
  directly over a full image's rows, so filter-loop changes are visible
  without inflate noise.
- `crc` — the RFC 2083 §5.5 chunk CRC-32 (`crc32`) over 8 B … 1 MiB
  buffers. The CRC runs over every chunk's type + data on both decode
  (validation) and encode (emission), so on an IDAT-heavy image it is an
  O(file-size) cost. The inner loop uses the slice-by-16 algorithm:
  sixteen input bytes are consumed per iteration through sixteen
  independent tables and combined with XOR, exposing far more
  instruction-level parallelism than the byte-at-a-time recurrence while
  producing **bit-identical** output (verified against a bit-serial
  reference across every length up to several block boundaries). Measured
  ≈5.8× throughput on 1 MiB buffers (≈0.5 → ≈2.9 GiB/s) and ≈6× on 64 B
  chunks; buffers below one 16-byte block fall back to the classic
  single-byte loop. A `chunk_crc` scenario compares the two `write_chunk`
  CRC shapes — concatenating `type ++ data` into a scratch `Vec` vs.
  threading the CRC register across the two slices with `crc32_update`
  (no allocation) — showing ≈42% / ≈26% wins on 13 B / 256 B chunks.
- `depth` — the §12.4 / §13.12 sample-depth rescale inner loop
  (`rescale_16bit_to_8bit` and the `sBIT`-aware variant) over full
  Gray16Le / Rgb48Le / Rgba64Le images, isolated from decode / inflate
  cost so a change to the per-sample linear equation is visible on its
  own.

- `encode_12mp` — a plain-`main` production-scale harness (not
  Criterion): 4032×3024 encode ms / output bytes / decode ms for every
  (layout × DEFLATE level × filter strategy × thread budget) as a
  Markdown table, on synthetic photographic + flat content or on a
  real RGB24 raster (`PNG_BENCH_RAW=/path/4032x3024.rgb`), with
  `PNG_BENCH_LEVELS` / `PNG_BENCH_THREADS` / `PNG_BENCH_FILTERS` /
  `PNG_BENCH_REPS` / `PNG_BENCH_OUT` selectors and a `PNG_BENCH_STAGES` breakdown
  (heuristic / `filter_row` / deflate) so encode time can be
  attributed. Every emitted stream is decoded back and compared
  byte-for-byte.

Each scenario synthesises a fresh input on the fly with the public
encoder API — no committed fixture files — so the benches reproduce
from a clean checkout.

#### 12 MP encode (round 464)

4032×3024 RGB24, a real photograph (a system wallpaper decoded to raw
RGB), M4 Max, release build, `Adaptive` filter. Encode wall-clock in
ms by thread budget (bytes are identical across budgets):

| level | bytes | ratio | 1 thread | 4 | 8 | 16 | decode |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 13 410 064 | 36.7 % | 345 | 97 | 73 | 54 | 241 |
| 2 | 13 190 488 | 36.1 % | 412 | 112 | 69 | 65 | 226 |
| 4 | 12 812 543 | 35.0 % | 592 | 159 | 103 | 79 | 209 |
| 6 | 12 199 325 | 33.4 % | 3 301 | 857 | 516 | 398 | 187 |

The default is level 2: on one thread it is the only level that
lands under ~0.5 s within ~10 % of the level-6 size (level 4 is
0.59 s / +5.0 %, level 1 0.35 s / +10.0 %; level 9 takes 19.4 s for
−3.9 %). `--opt level=4` buys −3 % size for +45 % time, `level=6`
−8 % for 8× the time (0.4 s at 16 threads).

Decode of the same 12 MP file: 216 ms (level-2 file) / 181 ms
(level-6 file), of which compcol's inflate is 206 / 170 ms — the
decoder streams one wire row at a time into the output plane, so
reconstruction is ~10 ms and peak RSS is the file plus the plane
(51.8 MB for 13.2 MB → 36.6 MB). Encode peak RSS, 1 thread, level 2:
input 36.6 MB + output 13.2 MB + ~4 MB of row buffers and deflate
state (90.6 MB measured with the source raster still resident); 8
threads add ~20 MB of per-segment output held until assembly.

Stage attribution (1 thread, same image): §12.8 heuristic 8 ms,
`Paeth` `filter_row` 6 ms, deflate of the filtered stream 334 ms
(level 1) / 393 (2) / 579 (4) / ≈ 3 200 (6) — the filter side is
noise, the level is the dial. The heuristic is already read-only
(five sums, no filtered bytes), allocation-free per row, and skips
rows identical to the previous one; a row-sampling variant would save
at most ~2 % of a 12 MP encode, so none is offered.

End to end, `oxideav convert` on a 4032×3024 HEIC (release, M4 Max,
host thread budget): **0.22 s wall** for HEIF decode + colour
conversion + PNG encode at the default level (8.2 MB output), against
≈ 3.4 s before this round. With an explicit `--opt` the CLI's
frame-tap writer currently builds the encoder without granting a
thread budget, so those runs are serial: `--opt level=1` 0.48 s /
8.7 MB, `--opt level=4` 0.63 s / 7.8 MB, `--opt level=6` 1.95 s /
7.1 MB, `--opt filter=none` 0.52 s / 9.2 MB. Before this round the same encode was
3.3 s at level 6 (the previous default) with three full-plane
intermediates. Other layouts at 8 threads, level 2 / 6: RGBA 74 /
567 ms, Gray8 24 / 192, Rgb48 87 / 511, Gray16 28 / 166; flat
(screenshot-like) content encodes a 12 MP RGB24 frame in 11–17 ms at
any level.

Recorded numbers live in [`BENCHMARKS.md`](BENCHMARKS.md), most
recently the round-448 profile pass: decode geo-mean **+28.7 %**
across the 14 decode scenarios (rgba 1080p 237 → 281 MiB/s, rgb48
511 → 683 MiB/s, rgba64 593 → 847 MiB/s, pal8 → RGBA promotion
897 → 1721 MiB/s, APNG +30 %) and encode geo-mean +6.2 % (gray16
357 → 417 MiB/s; the level-6 zlib pixel stream dominates the
noise-content scenarios), with output verified byte-identical to the
pre-round build across 74 encode + 77 decode configurations. The §6
filter-reconstruction kernels run on register-carried lane arrays
(unfilter gray8 0.63 → 1.48 GiB/s, rgba64 2.29 → 5.15 GiB/s) and the
§12.8 heuristic evaluates its five candidate sums without
materialising filtered bytes.

```sh
cargo bench -p oxideav-png --bench decode
cargo bench -p oxideav-png --bench encode
cargo bench -p oxideav-png --bench roundtrip
cargo bench -p oxideav-png --bench filter
cargo bench -p oxideav-png --bench crc
cargo bench -p oxideav-png --bench depth
PNG_BENCH_THREADS=1,8 cargo bench -p oxideav-png --bench encode_12mp -- rgb24
```

### Constructing options and records

Every public struct with public fields — `EncodeOptions`,
`DecodeOptions`, `PngImage`, `Plane`, `ColorInfo`, `Metadata`,
`Palette`, `ImageInfo`, `Frame`, `RgbImage` / `RgbaImage`,
`PngMetadata`, the per-chunk records (`Phys`, `Time`, `Cicp`, `Text`,
…), `ApngFrameSpec`, `Ihdr`, the decoder's `ApngInfo` / `ApngFrame` /
`ApngImage` — is `#[non_exhaustive]`,
so a future field never forces a major version bump. Fields stay
public for *reading* (`decoded.width`, `meta.gama`); construction goes
through `Default` + `with_*` setters where a default makes sense, and
`new(required…)` + `with_*` for the rest:

```rust
use oxideav_png::{
    encode, FilterStrategy, FilterType, Gama, EncodeOptions,
    PngImage, PngMetadata, PngPixelFormat, Text,
};

let image = PngImage::from_rgb8(2, 1, vec![255, 0, 0, 0, 255, 0])?; // Err on a length mismatch
let meta = PngMetadata::default()
    .with_gama(Gama::new(45_455))
    .with_texts(vec![Text::new("Software".into(), "oxideav".into())]);
let opts = EncodeOptions::default()
    .with_compression_level(4)
    .with_filter_strategy(FilterStrategy::Fixed(FilterType::Paeth))
    .with_metadata(meta);
let png = encode(&image, &opts)?;
# Ok::<(), oxideav_png::PngError>(())
```

Setters for `Option` fields take `impl Into<Option<T>>`, so both
`.with_bit_depth(4)` and `.with_bit_depth(None)` read naturally.
`PngError`, `PngPixelFormat`, `FilterStrategy` and `ColourSource` are
`#[non_exhaustive]` enums (match with a `_` arm); the on-wire enums the
PNG specification closes — `FilterType`, `ColourType`, `ApngDisposal`,
`ApngBlend`, `PhysUnit`, `RenderingIntent`, `Sbit`, `Bkgd`, `Trns` —
stay exhaustive.

## License

MIT — see [LICENSE](LICENSE).
