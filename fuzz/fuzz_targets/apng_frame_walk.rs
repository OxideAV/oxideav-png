#![no_main]

//! Drive the APNG composite state machine (`acTL` / `fcTL` / `fdAT`)
//! across byte-level *valid* inputs that are combinatorially adversarial
//! in their `blend_op` / `dispose_op` / `x_offset` / `y_offset` choices.
//!
//! Strategy: build a base APNG with the standalone encoder (which always
//! writes `dispose = None` / `blend = Source` and zero offsets, full-
//! canvas frames), then walk the chunk stream and rewrite every `fcTL`
//! payload with a fuzz-derived `(dispose, blend, x_offset, y_offset)`
//! tuple while preserving the sequence number + sub-frame width/height
//! (so the downstream `fdAT` payload still parses + decompresses).
//! Recompute the chunk's CRC32 over `type || data` so the parser still
//! accepts the mutated stream.
//!
//! The mutated stream is then driven through `parse_apng` +
//! `decode_apng_info` across 1-8-frame chains with canvases up to
//! 16x16 RGBA. Exercises:
//!   * `Disposal::Previous` canvas snapshots,
//!   * `Disposal::Background` `clear_region` calls with offsets >=
//!     canvas dim (the exact site of the r124 fix),
//!   * `Blend::Source` vs `Blend::Over` alpha composite paths.
//!
//! Asserts liveness only — the decoder must `return` a `Result` for any
//! input the parser accepted, never panic / abort / index out of bounds.

use libfuzzer_sys::fuzz_target;
use oxideav_png::chunk::PNG_MAGIC;
use oxideav_png::{
    decode_apng_info, encode_apng, parse_apng, ApngFrameImage, ApngImage, PngImage, PngPixelFormat,
};

/// Canvas dimensions cap. APNG composite cost is O(canvas * frames),
/// so keep both small to leave headroom for the fuzzer's iteration rate.
const MAX_DIM: u32 = 16;
const MIN_DIM: u32 = 2;
/// Frame-count cap. Each extra frame is one more fcTL + fdAT round
/// through the composite state machine.
const MAX_FRAMES: usize = 8;

fuzz_target!(|data: &[u8]| {
    let Some(plan) = Plan::from_fuzz_input(data) else {
        return;
    };

    // Build a base APNG via the standalone encoder. Every frame is full
    // canvas (encoder convention), dispose=None, blend=Source.
    let frames: Vec<PngImage> = (0..plan.num_frames)
        .map(|i| solid_frame(plan.width, plan.height, plan.seed.wrapping_add(i as u8)))
        .collect();
    let Ok(base) = encode_apng(&frames, plan.delay_cs, plan.num_plays) else {
        return;
    };

    // Walk the base stream, patch every fcTL payload with one (dispose,
    // blend, x_offset, y_offset) tuple from `plan.fctl_mutations` (cycled).
    let Some(mutated) = mutate_fctl_chunks(&base, &plan.fctl_mutations) else {
        return;
    };

    // Drive parse + composite. Neither call may panic; the Result is
    // intentionally discarded.
    let _ = parse_apng(&mutated);
    if let Ok(info) = parse_apng(&mutated) {
        let _: oxideav_png::Result<ApngImage> = decode_apng_info(&info);
    }
});

/// One per-`fcTL` mutation: replace `(x_offset, y_offset, dispose_op,
/// blend_op)` on the next `fcTL` chunk encountered. The on-disk sub-
/// frame width/height + sequence number are preserved so the matching
/// `fdAT` payload still decompresses against the patched header.
#[derive(Clone, Copy, Debug)]
struct FctlMutation {
    x_offset: u32,
    y_offset: u32,
    dispose_op: u8,
    blend_op: u8,
}

/// Decoded fuzz input.
struct Plan {
    width: u32,
    height: u32,
    num_frames: usize,
    num_plays: u32,
    delay_cs: u16,
    seed: u8,
    fctl_mutations: Vec<FctlMutation>,
}

impl Plan {
    fn from_fuzz_input(data: &[u8]) -> Option<Self> {
        // Layout: 8 header bytes + 4 bytes per fcTL mutation.
        //   [0]    width selector
        //   [1]    height selector
        //   [2]    num_frames selector (1..=MAX_FRAMES)
        //   [3]    num_plays low byte
        //   [4..6] delay_cs (le u16)
        //   [6]    seed
        //   [7]    fctl-mutation count selector (1..=num_frames)
        //   then 4 bytes per fctl mutation: [x_sel, y_sel, dispose, blend]
        if data.len() < 8 {
            return None;
        }
        let width = MIN_DIM + (data[0] as u32 % (MAX_DIM - MIN_DIM + 1));
        let height = MIN_DIM + (data[1] as u32 % (MAX_DIM - MIN_DIM + 1));
        let num_frames = ((data[2] as usize) % MAX_FRAMES) + 1;
        let num_plays = data[3] as u32;
        let delay_cs = u16::from_le_bytes([data[4], data[5]]) % 200;
        let seed = data[6];
        let n_mut = ((data[7] as usize) % num_frames) + 1;

        let need = 8 + n_mut * 4;
        if data.len() < need {
            return None;
        }

        let mut fctl_mutations = Vec::with_capacity(n_mut);
        for i in 0..n_mut {
            let off = 8 + i * 4;
            let x_sel = data[off];
            let y_sel = data[off + 1];
            // dispose 0..=2 valid; blend 0..=1 valid.
            let dispose = data[off + 2] % 3;
            let blend = data[off + 3] % 2;
            // Project the offset selector across an in-range / on-edge /
            // out-of-canvas band so the fuzzer reliably hits all three
            // arms of the composite path (the r124 fix lives in the
            // out-of-canvas Background-disposal arm).
            let x_offset = offset_for(x_sel, width);
            let y_offset = offset_for(y_sel, height);
            fctl_mutations.push(FctlMutation {
                x_offset,
                y_offset,
                dispose_op: dispose,
                blend_op: blend,
            });
        }

        Some(Self {
            width,
            height,
            num_frames,
            num_plays,
            delay_cs,
            seed,
            fctl_mutations,
        })
    }
}

/// Map a fuzz-derived byte into one of:
///   - 0..canvas               (in-canvas, normal composite path)
///   - canvas                  (boundary — width collapses to zero)
///   - canvas+1..canvas+128    (out-of-canvas, clamp / early-return path)
///   - u32::MAX-32 .. u32::MAX (extreme overflow band)
///
/// Hitting all four bands is what catches the r124 panic.
fn offset_for(sel: u8, canvas_dim: u32) -> u32 {
    let band = sel & 0x03;
    let val = sel >> 2; // 0..=63
    match band {
        0 => val as u32 % canvas_dim,
        1 => canvas_dim,
        2 => canvas_dim + (val as u32) + 1,
        _ => u32::MAX - (val as u32),
    }
}

/// Build a solid-colour Rgba frame so the encoder always succeeds.
fn solid_frame(width: u32, height: u32, seed: u8) -> PngImage {
    let bpp = 4usize;
    let stride = width as usize * bpp;
    let mut data = Vec::with_capacity(stride * height as usize);
    // Vary alpha across the seed so the Over-blend path actually blends.
    let r = seed;
    let g = seed.wrapping_mul(3);
    let b = seed.wrapping_mul(7);
    let a = 0x40u8.wrapping_add(seed); // 64-319 → wraps; produces a mix
    for _ in 0..height as usize {
        for _ in 0..width as usize {
            data.push(r);
            data.push(g);
            data.push(b);
            data.push(a);
        }
    }
    PngImage {
        width,
        height,
        pixel_format: PngPixelFormat::Rgba,
        stride,
        data,
        palette: Vec::new(),
    }
}

/// Walk `png`, find every `fcTL` chunk, rewrite its payload's offset /
/// dispose / blend bytes per the cycled mutation list, and recompute the
/// chunk's CRC32. Sub-frame width/height + sequence number are preserved
/// so the matching `fdAT` payload still decompresses against the patched
/// header. Returns `None` if the input doesn't parse as a PNG framing
/// stream (the standalone encoder's output always parses, so this is
/// just a defensive guard).
fn mutate_fctl_chunks(png: &[u8], mutations: &[FctlMutation]) -> Option<Vec<u8>> {
    if png.len() < PNG_MAGIC.len() || png[..PNG_MAGIC.len()] != PNG_MAGIC {
        return None;
    }
    if mutations.is_empty() {
        return Some(png.to_vec());
    }

    let mut out = png.to_vec();
    let mut pos = PNG_MAGIC.len();
    let mut mut_idx = 0usize;

    while pos + 8 <= out.len() {
        let len = u32::from_be_bytes([out[pos], out[pos + 1], out[pos + 2], out[pos + 3]]);
        let type_start = pos + 4;
        let data_start = type_start + 4;
        let len_usize = len as usize;
        let data_end = data_start.checked_add(len_usize)?;
        let crc_end = data_end.checked_add(4)?;
        if crc_end > out.len() {
            return None;
        }

        let mut chunk_type = [0u8; 4];
        chunk_type.copy_from_slice(&out[type_start..data_start]);

        if &chunk_type == b"fcTL" && len_usize == 26 {
            let m = mutations[mut_idx % mutations.len()];
            mut_idx += 1;
            // fcTL layout: seq(4) | width(4) | height(4) | x_off(4) | y_off(4)
            //            | delay_num(2) | delay_den(2) | dispose(1) | blend(1)
            // We rewrite bytes 12..=25.
            out[data_start + 12..data_start + 16].copy_from_slice(&m.x_offset.to_be_bytes());
            out[data_start + 16..data_start + 20].copy_from_slice(&m.y_offset.to_be_bytes());
            out[data_start + 24] = m.dispose_op;
            out[data_start + 25] = m.blend_op;

            // PNG CRC32 covers `type || data`.
            let crc = crc32(&out[type_start..data_end]);
            out[data_end..crc_end].copy_from_slice(&crc.to_be_bytes());
        }

        pos = crc_end;
        if &chunk_type == b"IEND" {
            break;
        }
    }

    Some(out)
}

/// Bog-standard PNG CRC32 (RFC 2083 §15). Tiny inline impl so the fuzz
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

// Keep these as compile-time witnesses that the fuzz harness's
// understanding of the public surface matches the crate's. Unused at
// runtime — the optimiser will drop them.
#[allow(dead_code)]
fn _type_witness(_: ApngFrameImage) {}
