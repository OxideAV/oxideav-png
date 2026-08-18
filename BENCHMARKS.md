# oxideav-png benchmark record

Six Criterion harnesses live under `benches/` (see `README.md`
§Benchmarks for what each scenario synthesises):

```
cargo bench -p oxideav-png --bench decode     # full-file decode entry points
cargo bench -p oxideav-png --bench encode     # full-file encode entry points
cargo bench -p oxideav-png --bench roundtrip  # encode → decode loops
cargo bench -p oxideav-png --bench filter     # §6 unfilter / §12.8 heuristic / filter_row in isolation
cargo bench -p oxideav-png --bench crc        # CRC-32 kernel
cargo bench -p oxideav-png --bench depth      # §13.12 sample-depth rescale
```

Every scenario synthesises its input with a deterministic `xorshift32`
generator inside the harness — no committed fixtures, reproducible
runs.

## Round 448 profile/bench pass — before → after

Measured as a **paired A/B on one machine in one sitting** (Apple
M-series, `--warm-up-time 1 --measurement-time 2`): the pre-round tree
(`d336fac`) was benched to a fresh Criterion baseline and the r448
head benched against it immediately after, so thermal / background
drift (measured at ±6–9 % on the deflate-bound encode scenarios across
this session) cancels out of the comparison. Decoded/encoded output
was verified **byte-identical** to the pre-round build across 74
encode and 77 decode configurations before any number below was
recorded, so every delta is pure throughput.

Deltas are Criterion's reported throughput change (mid estimate);
"before" columns are derived from the after value and that delta.

### `decode` (full-file, MiB/s unless noted)

| Scenario | before | after | Δ thrpt |
| --- | ---: | ---: | ---: |
| rgba 1920×1080 | 237.5 | 280.6 | +18.1 % |
| rgba 320×240 | 245.2 | 284.9 | +16.2 % |
| rgb24 640×480 | 228.3 | 274.4 | +20.2 % |
| gray8 512×512 | 116.2 | 150.2 | +29.2 % |
| gray16 512×512 | 765.4 | 801.2 | +4.7 % |
| rgb48 512×512 | 510.6 | 683.3 | +33.8 % |
| rgba64 320×240 | 592.6 | 847.0 | +42.9 % |
| pal8 320×240 | 351.9 | 502.9 | +42.9 % |
| pal8 → RGBA 320×240 | 896.5 | 1721.4 (1.68 GiB/s) | +92.0 % |
| parse_metadata 320×240 | 2.91 GiB/s | 2.94 GiB/s | +1.1 % (untouched path) |
| apng 4×320×240 | 213.4 | 280.6 | +31.5 % |
| apng scan 2×128×128 | 223.7 | 291.0 | +30.1 % |
| apng scan 8×128×128 | 224.5 | 293.8 | +30.9 % |
| apng scan 32×128×128 | 226.6 | 293.7 | +29.6 % |

Decode geo-mean across the 14 scenarios: **+28.7 %**.

### `encode` (full-file, MiB/s — DEFLATE level 6 dominates)

| Scenario | before | after | Δ thrpt |
| --- | ---: | ---: | ---: |
| rgba 1920×1080 | 16.0 | 16.0 | −0.3 % (flat) |
| rgba 320×240 | 17.7 | 18.5 | +4.3 % |
| rgb24 640×480 | 13.6 | 14.1 | +3.9 % |
| gray8 512×512 | 20.6 | 22.1 | +7.3 % |
| gray16 512×512 | 356.6 | 417.1 | +17.0 % |
| rgb48 512×512 | 155.1 | 168.9 | +8.9 % |
| rgba64 320×240 | 188.0 | 207.3 | +10.3 % |
| pal8 320×240 | 61.5 | 66.3 | +7.8 % |
| rgba Adam7 320×240 | 17.7 | 18.2 | +3.1 % |
| apng 4×320×240 | 18.4 | 18.7 | +1.3 % |

Encode geo-mean across the 10 scenarios: **+6.2 %** (the pixel stream's
zlib compression, delegated to the workspace compression crate at
level 6, bounds what filter-side work can move on the noise-content
scenarios; the compressible-content 16-bit scenarios show the
filter/flatten share directly).

Combined decode+encode geo-mean (24 scenarios): **+18.8 %**.

### `filter` (kernel isolation, GiB/s)

| Scenario | before | after | Δ thrpt |
| --- | ---: | ---: | ---: |
| unfilter gray8 (bpp 1) | 0.63 | 1.48 | +135 % |
| unfilter rgb24 (bpp 3) | 1.07 | 2.42 | +127 % |
| unfilter rgb48 (bpp 6) | 2.03 | 3.89 | +91 % |
| unfilter rgba64 (bpp 8) | 2.29 | 5.15 | +126 % |
| heuristic gray8 | 3.55 | 4.18 | +17.9 % |
| heuristic rgb24 | 4.04 | 4.54 | +12.4 % |
| heuristic rgb48 | 3.93 | 4.61 | +17.2 % |
| heuristic rgba64 | 3.79 | 4.63 | +22.2 % |
| filter_row gray8 | 21.9 | 28.1 | +28.5 % |
| filter_row rgb48 | 27.2 | 28.8 | +6.1 % |
| filter_row rgba64 | 27.2 | 28.5 | +4.5 % |

`roundtrip` moved with its encode-dominated mix (rgb48 +8.2 %, rgba
1080p +3.4 %, the rest inside the ±3 % drift band). The `crc`
(slice-by-16, unchanged this round) and `depth` harnesses were not
re-run: their kernels were untouched.

## What changed (r448)

1. **Register-lane unfilter kernels** — §6.3/§6.5/§6.6 reconstruction
   carries the left / upper-left neighbours in const-generic `[u8; N]`
   lane arrays (bpp 1..=8) instead of re-reading just-written row
   bytes, breaking the store-to-load forwarding stall in the serial
   recurrence. Paeth uses the algebraically identical distance form
   `pa=|b−c|, pb=|a−c|, pc=|a+b−2c|` (§6.6 selection order intact).
2. **Read-only §12.8 heuristic** — the five candidate sums are folded
   directly from `row`/`prev_row`; filtered bytes are materialised
   once, for the winner only.
3. **Single-allocation decode pipeline** — the reconstruction buffer
   flows through `expand_byte_plane` → `build_png_image` by value
   (16-bit BE→LE swap in place), a single IDAT chunk is borrowed
   rather than concatenated, and `decode_png_to_rgba` /
   `decode_png_over_background` share one CRC-validating chunk walk
   with the pixel decode.
4. **Lockstep promotion / packing loops** — RGBA promotion, sub-byte
   expand/pack (const 1/2/4-bit), Adam7 scatter (contiguous rows for
   pass 7, const-width moves otherwise), APNG SOURCE-blend row blits,
   and encoder flatten fast paths.

Regeneration: save a baseline before touching code
(`cargo bench --bench <name> -- --save-baseline <tag>`), land the
change, re-run with `--baseline <tag>`, and A/B on the same machine in
the same sitting — the encode scenarios drift several percent with
machine temperature, which will otherwise masquerade as a regression.
