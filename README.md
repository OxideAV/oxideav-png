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

Decode: [`parse_metadata`] returns a [`PngMetadata`] with each
supported field populated for any chunks present. Encode:
[`PngEncoderOptions`]`::metadata` holds the same struct; populated
fields are emitted at spec-compliant chunk positions (`sBIT` / `sRGB`
before `PLTE`/`IDAT`; `bKGD` / `hIST` after `PLTE`, before `IDAT`;
`pHYs`, `tIME`, and `eXIf` before `IDAT`). Duplicates of any supported
chunk on decode are rejected per the "Multiple OK? No" rule in RFC 2083
§4.3 / W3C PNG3 §5.6.

## Not preserved

- Adam7 interlaced encode (decode only — encoder always writes non-interlaced)
- Sub-byte encode (decode only — encoder always writes 8/16-bit)
- `tRNS` alpha applied to `Gray*` / `Rgb*` pixels on decode (the chunk is
  parsed + CRC-validated but not blended into the output plane; for `Pal8`
  the per-entry alpha is still carried through `extradata`)
- Colour-management + remaining metadata chunks: `cICP`, `gAMA`,
  `cHRM`, `iCCP`, `tEXt`, `zTXt`, `iTXt`, `sPLT`. Each is
  CRC-checked on read and then dropped

## Usage

```toml
[dependencies]
oxideav-png = "0.0"
```

## License

MIT — see [LICENSE](LICENSE).
