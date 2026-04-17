# oxideav-png

Pure-Rust PNG + APNG decoder and encoder for oxideav

Part of the [oxideav](https://github.com/OxideAV/oxideav-workspace) framework — a
100% pure Rust media transcoding and streaming stack. No C libraries, no FFI
wrappers, no `*-sys` crates.

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

## Not preserved

- Adam7 interlaced encode (decode only — encoder always writes non-interlaced)
- Sub-byte encode (decode only — encoder always writes 8/16-bit)
- `tRNS` alpha applied to `Gray*` / `Rgb*` pixels on decode (the chunk is
  parsed + CRC-validated but not blended into the output plane; for `Pal8`
  the per-entry alpha is still carried through `extradata`)
- Colour-management + metadata chunks: `cICP`, `sRGB`, `gAMA`, `cHRM`,
  `iCCP`, `tEXt`, `zTXt`, `iTXt`, `tIME`, `pHYs`, `sBIT`, `bKGD`, `hIST`,
  `sPLT`. Each is CRC-checked on read and then dropped

## Usage

```toml
[dependencies]
oxideav-png = "0.0"
```

## License

MIT — see [LICENSE](LICENSE).
