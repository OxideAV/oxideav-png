#![no_main]

//! Splice fuzz-derived ancillary chunks into a *valid* PNG envelope and
//! drive the ancillary-chunk parsers (`parse_metadata`) plus the two
//! decode entry points across them.
//!
//! The plain `decode` target feeds raw fuzz bytes straight at
//! `parse_metadata`, but raw mutation rarely survives the 8-byte
//! signature + length-prefixed chunk framing + per-chunk CRC gate, so
//! the per-chunk parsers deep inside `parse_metadata` (keyword / `NUL`
//! splitting, the compression-method byte, zlib inflate of the
//! `zTXt` / `iTXt` / `iCCP` bodies, `sPLT` entry-stride arithmetic, the
//! `eXIf` TIFF-header probe, `bKGD` / `hIST` PLTE-index bounds) are
//! barely reached. This target spends the mutation budget *inside* those
//! parsers instead:
//!
//!   1. Build a minimal but fully valid PNG with the standalone encoder
//!      (8x8, colour type chosen so `PLTE`-dependent chunks have a
//!      palette to validate against).
//!   2. Synthesise 1..=8 ancillary chunks whose 4-byte type is drawn
//!      from the metadata set (`sBIT pHYs tIME bKGD hIST tRNS eXIf sRGB
//!      cICP gAMA cHRM mDCV cLLI sPLT tEXt zTXt iCCP iTXt`) and whose
//!      payload is fuzz-controlled — including raw bytes that land in the
//!      DEFLATE inflate of the three compressed-text chunk types.
//!   3. Frame each with the correct big-endian length prefix + a real
//!      CRC32 over `type || data`, and splice the run immediately before
//!      the `IEND` chunk so `parse_all_chunks` accepts the stream and the
//!      mutation lands in the per-chunk `::parse`.
//!
//! Asserts liveness only: `parse_metadata`, `decode_png`, and
//! `decode_png_to_rgba` must each *return* a `Result` for any spliced
//! stream — never panic / abort / index out of bounds / integer-overflow
//! (debug) / OOM, no matter how hostile the spliced payload is (a
//! truncated or bomb-shaped zlib body in particular must surface as
//! `Err`, not a crash or unbounded allocation).

use libfuzzer_sys::fuzz_target;
use oxideav_png::chunk::PNG_MAGIC;
use oxideav_png::{
    decode_png, decode_png_to_rgba, encode_png_image, parse_metadata, PngImage, PngPixelFormat,
};

/// Ancillary chunk types `parse_metadata` dispatches on (decoder.rs
/// `parse_metadata` match arms). Keeping the splice type-set aligned
/// with the dispatch is what funnels the mutation budget into a real
/// `::parse`. The trailing entries are *unrecognised* chunk names so the
/// budget also reaches the W3C PNG3 §14.2 unknown-chunk paths: `prVt`
/// (safe-to-copy ancillary → captured into `unknowns`), `prVT`
/// (unsafe-to-copy ancillary), `PrIv` (critical → hard decode error),
/// and `pH1s` (§13.1-malformed name → dropped).
const META_TYPES: &[&[u8; 4]] = &[
    b"sBIT", b"pHYs", b"tIME", b"bKGD", b"hIST", b"tRNS", b"eXIf", b"sRGB", b"cICP", b"gAMA",
    b"cHRM", b"mDCV", b"cLLI", b"sPLT", b"tEXt", b"zTXt", b"iCCP", b"iTXt", b"prVt", b"prVT",
    b"PrIv", b"pH1s",
];

/// Cap on spliced chunks per input — each one is an independent
/// `::parse` call, so a handful is plenty to keep coverage broad while
/// leaving the fuzzer headroom on iteration rate.
const MAX_CHUNKS: usize = 8;
/// Cap on a single spliced payload. Large enough to exercise the
/// multi-entry `sPLT` / `hIST` strides and a non-trivial compressed-text
/// body, small enough that a pathological (but well-formed) zlib stream
/// the inflate accepts can't expand without bound.
const MAX_PAYLOAD: usize = 512;

fuzz_target!(|data: &[u8]| {
    let Some(plan) = Plan::from_fuzz_input(data) else {
        return;
    };

    // A valid base PNG. The colour type is fuzz-selected so the
    // PLTE-dependent arms (`bKGD` palette index, `hIST`, `tRNS`
    // ct=3) see a real palette to bounds-check against.
    let Ok(base) = encode_png_image(&plan.base_image()) else {
        return;
    };

    let Some(spliced) = splice_chunks(&base, &plan.chunks) else {
        return;
    };

    // Three liveness probes over the spliced stream. Each must return a
    // Result; the value is intentionally discarded.
    let _ = parse_metadata(&spliced);
    let _ = decode_png(&spliced);
    let _ = decode_png_to_rgba(&spliced);
});

/// One synthesised ancillary chunk: a 4-byte type drawn from
/// [`META_TYPES`] and a fuzz-controlled payload.
struct MetaChunk {
    chunk_type: [u8; 4],
    payload: Vec<u8>,
}

/// Decoded fuzz input.
struct Plan {
    /// Colour-type selector: 0 grayscale, 2 RGB, 3 palette (the three
    /// the standalone 8-bit encoder accepts for an `8x8` solid image).
    colour_kind: u8,
    chunks: Vec<MetaChunk>,
}

impl Plan {
    fn from_fuzz_input(data: &[u8]) -> Option<Self> {
        // Header: [0] colour-kind selector, [1] chunk-count selector.
        // Then, per chunk: [type-selector u8][len-hi u8][len-lo u8] +
        // `len` payload bytes (len clamped to MAX_PAYLOAD).
        if data.len() < 2 {
            return None;
        }
        let colour_kind = data[0] % 3;
        let n_chunks = (data[1] as usize % MAX_CHUNKS) + 1;

        let mut pos = 2usize;
        let mut chunks = Vec::with_capacity(n_chunks);
        for _ in 0..n_chunks {
            if pos + 3 > data.len() {
                break;
            }
            let type_sel = data[pos];
            let len = ((data[pos + 1] as usize) << 8 | data[pos + 2] as usize) % (MAX_PAYLOAD + 1);
            pos += 3;
            if pos + len > data.len() {
                // Run out of bytes mid-payload: take what's left so the
                // tail of the input still produces a (shorter) chunk.
                let payload = data[pos..].to_vec();
                let chunk_type = *META_TYPES[type_sel as usize % META_TYPES.len()];
                chunks.push(MetaChunk {
                    chunk_type,
                    payload,
                });
                break;
            }
            let payload = data[pos..pos + len].to_vec();
            pos += len;
            let chunk_type = *META_TYPES[type_sel as usize % META_TYPES.len()];
            chunks.push(MetaChunk {
                chunk_type,
                payload,
            });
        }

        if chunks.is_empty() {
            return None;
        }
        Some(Self {
            colour_kind,
            chunks,
        })
    }

    /// Build the 8x8 base image whose colour type matches `colour_kind`.
    /// Palette images carry a 4-entry palette so the spliced
    /// PLTE-dependent chunks have something to validate against.
    fn base_image(&self) -> PngImage {
        const W: u32 = 8;
        const H: u32 = 8;
        match self.colour_kind {
            // Grayscale 8-bit.
            0 => {
                let data = vec![0x80u8; (W * H) as usize];
                PngImage {
                    width: W,
                    height: H,
                    pixel_format: PngPixelFormat::Gray8,
                    stride: W as usize,
                    data,
                    palette: Vec::new(),
                }
            }
            // RGB 8-bit.
            1 => {
                let stride = (W * 3) as usize;
                let data = vec![0x40u8; stride * H as usize];
                PngImage {
                    width: W,
                    height: H,
                    pixel_format: PngPixelFormat::Rgb24,
                    stride,
                    data,
                    palette: Vec::new(),
                }
            }
            // Palette 8-bit, 4-entry palette, all pixels index 0.
            _ => {
                let data = vec![0u8; (W * H) as usize];
                let palette = vec![
                    0, 0, 0, // entry 0
                    255, 0, 0, // entry 1
                    0, 255, 0, // entry 2
                    0, 0, 255, // entry 3
                ];
                PngImage {
                    width: W,
                    height: H,
                    pixel_format: PngPixelFormat::Pal8,
                    stride: W as usize,
                    data,
                    palette,
                }
            }
        }
    }
}

/// Splice every chunk in `chunks` immediately before the `IEND` chunk of
/// `png`, each framed with the correct big-endian length prefix and a
/// CRC32 over `type || data`. Returns `None` only if the base stream
/// doesn't parse as PNG framing (the standalone encoder's output always
/// does — this is a defensive guard).
fn splice_chunks(png: &[u8], chunks: &[MetaChunk]) -> Option<Vec<u8>> {
    if png.len() < PNG_MAGIC.len() || png[..PNG_MAGIC.len()] != PNG_MAGIC {
        return None;
    }

    // Locate the IEND chunk's start by walking the framing.
    let mut pos = PNG_MAGIC.len();
    let mut iend_start = None;
    while pos + 8 <= png.len() {
        let len = u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
        let type_start = pos + 4;
        let data_start = type_start + 4;
        let data_end = data_start.checked_add(len)?;
        let crc_end = data_end.checked_add(4)?;
        if crc_end > png.len() {
            return None;
        }
        if &png[type_start..data_start] == b"IEND" {
            iend_start = Some(pos);
            break;
        }
        pos = crc_end;
    }
    let iend_start = iend_start?;

    let mut out = Vec::with_capacity(png.len() + chunks.len() * 16);
    out.extend_from_slice(&png[..iend_start]);
    for c in chunks {
        // Length prefix (big-endian u32). Payload is bounded by
        // MAX_PAYLOAD so this never overflows.
        out.extend_from_slice(&(c.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&c.chunk_type);
        out.extend_from_slice(&c.payload);
        // CRC32 over type || data.
        let crc_start = out.len() - c.payload.len() - 4;
        let crc = crc32(&out[crc_start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out.extend_from_slice(&png[iend_start..]);
    Some(out)
}

/// PNG CRC32 (RFC 2083 §15 / RFC 2083 Annex D). Inline so the fuzz
/// target keeps zero non-`oxideav-*` dependencies beyond `libfuzzer-sys`.
fn crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (n, slot) in t.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        t
    });

    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}
