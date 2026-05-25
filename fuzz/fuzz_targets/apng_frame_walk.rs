#![no_main]

//! APNG frame-walk fuzz target — exercises the `acTL` / `fcTL` / `fdAT`
//! chunk-tree state machine on inputs that are byte-level *valid* (CRC32
//! is recomputed after every mutation) but combinatorially adversarial
//! in their `blend_op` / `dispose_op` / `x_offset` / `y_offset` choices.
//!
//! The companion `decode.rs` target throws arbitrary bytes at the parser
//! and exercises the framing-error paths. This target is the opposite
//! end of the same surface: every input passes `parse_apng`, so what's
//! under test is the *composite* — `decode_apng_info` walks the canvas,
//! applies disposal, and blends each sub-frame. The state machine has
//! to remain panic-free for every combination of:
//!
//! * canvas vs. sub-frame dimensions (sub-frame can equal, exceed, or
//!   be entirely outside the canvas; r124's `clear_region` x-overflow
//!   panic lived in exactly this corner),
//! * `x_offset` / `y_offset` (zero, mid-canvas, equal to canvas dim,
//!   exceeding canvas dim),
//! * `dispose_op` ∈ { None, Background, Previous } — Previous snapshots
//!   the canvas, Background calls `clear_region`,
//! * `blend_op` ∈ { Source, Over } — Source overwrites alpha, Over does
//!   alpha-blend,
//! * frame ordering (Previous on frame 0 means "restore to nothing"),
//! * 1–8 frame chains so the snapshot lifecycle gets multi-step coverage.
//!
//! Construction strategy: encode a base APNG with the standalone encoder
//! (which always emits `Disposal::None` / `Blend::Source` on every frame)
//! to get a stream with valid `IHDR`, `acTL`, and per-frame compressed
//! data. Then walk the resulting byte buffer, locate every `fcTL` chunk,
//! overwrite its `dispose_op` / `blend_op` / `x_offset` / `y_offset`
//! bytes with fuzz-derived values, and recompute the chunk's CRC32 so
//! the parser still accepts it. The IDAT / fdAT payloads stay untouched
//! and decompressible. Feed the mutated stream through `parse_apng` +
//! `decode_apng_info` — both must return `Ok` or `Err`, never panic.

use libfuzzer_sys::fuzz_target;
use oxideav_png::apng::{Blend, Disposal, Fctl};
use oxideav_png::decoder::{decode_apng_info, parse_apng};
use oxideav_png::encoder::encode_apng;
use oxideav_png::filter::crc32;
use oxideav_png::image::{PngImage, PngPixelFormat};

/// Cap canvas dimensions tight enough that an 8-frame composite stays
/// well inside the fuzz time/memory budget (16×16 RGBA × 8 ≈ 8 KiB of
/// canvas snapshots even when Disposal::Previous fires on every frame).
const MAX_DIM: u32 = 16;
const MAX_FRAMES: usize = 8;

fuzz_target!(|data: &[u8]| {
    let Some((frames, mutations)) = plan_from_fuzz_input(data) else {
        return;
    };

    // Build the base APNG via the standalone encoder. This gives us a
    // stream with valid IHDR + acTL + per-frame compressed data; every
    // fcTL it emits has dispose_op=None / blend_op=Source / x=y=0.
    let Ok(mut bytes) = encode_apng(&frames, 10, 0) else {
        return;
    };

    // Walk the chunk stream and rewrite each fcTL payload (then its
    // CRC32) with a fuzz-derived (dispose, blend, x_offset, y_offset)
    // tuple. Sub-frame width/height and sequence_number must be
    // preserved — the original fdAT data was filtered/compressed for
    // exactly those dimensions, and the sequence number is consistency-
    // checked by parse_apng.
    rewrite_fctls(&mut bytes, &mutations, frames[0].width, frames[0].height);

    // Liveness contract: parse + composite never panic, regardless of
    // how absurd the mutated dispose/blend/offset combination is.
    if let Ok(info) = parse_apng(&bytes) {
        let _ = decode_apng_info(&info);
    }
});

/// Per-frame mutation drawn from fuzz input — applied to the `fcTL`
/// chunk of the corresponding frame after the base APNG is built.
struct FctlMutation {
    dispose_op: Disposal,
    blend_op: Blend,
    x_offset: u32,
    y_offset: u32,
}

/// Derive a (frames, per-frame-mutation) plan from the fuzz buffer.
///
/// Layout: `[hdr_byte][frame_byte × n_frames][pixel_bytes...]`.
/// * `hdr_byte` low nibble → canvas width (1..=MAX_DIM)
/// * `hdr_byte` high nibble → canvas height (1..=MAX_DIM)
/// * Each `frame_byte` encodes that frame's dispose/blend/offset
///   mutation (see `decode_frame_byte`).
/// * `pixel_bytes` fills the RGBA canvas of every frame round-robin —
///   exact value doesn't matter, the decoder will see whatever the
///   encoder produced, but varying the content avoids degenerate
///   all-zero compressed streams.
///
/// Returns `None` when there isn't enough data for a useful frame.
fn plan_from_fuzz_input(data: &[u8]) -> Option<(Vec<PngImage>, Vec<FctlMutation>)> {
    let (&hdr, rest) = data.split_first()?;
    let w = ((hdr & 0x0F) as u32 % MAX_DIM) + 1;
    let h = (((hdr >> 4) & 0x0F) as u32 % MAX_DIM) + 1;
    let canvas_bytes = (w as usize) * (h as usize) * 4;

    let (n_frames_byte, rest) = rest.split_first()?;
    let n_frames = ((*n_frames_byte as usize) % MAX_FRAMES) + 1;

    if rest.len() < n_frames {
        return None;
    }
    let (frame_bytes, pixel_pool) = rest.split_at(n_frames);

    if pixel_pool.is_empty() {
        return None;
    }

    let mut frames = Vec::with_capacity(n_frames);
    let mut mutations = Vec::with_capacity(n_frames);
    for (i, &fb) in frame_bytes.iter().enumerate() {
        // Fill the frame's pixel buffer by repeating the pool offset
        // by the frame index so successive frames differ.
        let mut pixels = vec![0u8; canvas_bytes];
        for (j, p) in pixels.iter_mut().enumerate() {
            *p = pixel_pool[(j + i) % pixel_pool.len()];
        }
        frames.push(PngImage {
            width: w,
            height: h,
            pixel_format: PngPixelFormat::Rgba,
            stride: (w as usize) * 4,
            data: pixels,
            palette: Vec::new(),
        });
        mutations.push(decode_frame_byte(fb, w, h));
    }
    Some((frames, mutations))
}

/// Decode one byte into a frame mutation. The byte's bits map to:
///   bits 0..=1 → dispose_op (0/1/2; 3 wraps to 0)
///   bit 2      → blend_op (0=Source, 1=Over)
///   bits 3..=4 → x_offset bucket (0 / w/2 / w / w+8)
///   bits 5..=6 → y_offset bucket (0 / h/2 / h / h+8)
///   bit 7      → reserved
///
/// The "≥ canvas dim" buckets are the ones that historically panicked
/// in `clear_region` (r124 fix) and that exercise the Background-clear
/// fast-return path.
fn decode_frame_byte(b: u8, w: u32, h: u32) -> FctlMutation {
    let dispose_op = match b & 0b11 {
        0 => Disposal::None,
        1 => Disposal::Background,
        _ => Disposal::Previous,
    };
    let blend_op = if (b >> 2) & 1 == 0 {
        Blend::Source
    } else {
        Blend::Over
    };
    let x_offset = match (b >> 3) & 0b11 {
        0 => 0,
        1 => w / 2,
        2 => w,
        _ => w + 8,
    };
    let y_offset = match (b >> 5) & 0b11 {
        0 => 0,
        1 => h / 2,
        2 => h,
        _ => h + 8,
    };
    FctlMutation {
        dispose_op,
        blend_op,
        x_offset,
        y_offset,
    }
}

/// Walk the PNG byte stream, locate each `fcTL` chunk in order, and
/// overwrite its dispose/blend/offset fields with the matching
/// `mutations[i]`. The chunk's length, type, sequence_number, and
/// sub-frame width/height are preserved so the downstream `fdAT` data
/// still parses + decompresses correctly. CRC32 is recomputed.
fn rewrite_fctls(bytes: &mut [u8], mutations: &[FctlMutation], sub_w: u32, sub_h: u32) {
    let magic_len = 8;
    if bytes.len() < magic_len {
        return;
    }
    let mut pos = magic_len;
    let mut frame_idx = 0;
    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let type_start = pos + 4;
        let data_start = pos + 8;
        let Some(data_end) = data_start.checked_add(len) else {
            return;
        };
        let crc_end = data_end + 4;
        if crc_end > bytes.len() {
            return;
        }
        let chunk_type = &bytes[type_start..type_start + 4];
        if chunk_type == b"fcTL" && len == 26 && frame_idx < mutations.len() {
            let m = &mutations[frame_idx];
            // sequence_number: bytes[0..4] — preserve.
            // width / height: bytes[4..12] — preserve (must match the
            // compressed payload the encoder emitted).
            let sub_w_be = sub_w.to_be_bytes();
            let sub_h_be = sub_h.to_be_bytes();
            bytes[data_start + 4..data_start + 8].copy_from_slice(&sub_w_be);
            bytes[data_start + 8..data_start + 12].copy_from_slice(&sub_h_be);
            // x_offset / y_offset: bytes[12..20] — overwrite.
            bytes[data_start + 12..data_start + 16].copy_from_slice(&m.x_offset.to_be_bytes());
            bytes[data_start + 16..data_start + 20].copy_from_slice(&m.y_offset.to_be_bytes());
            // delay_num / delay_den: bytes[20..24] — leave whatever the
            // encoder wrote (parser tolerates anything; delay_den==0 is
            // rewritten to 100 by Fctl::parse).
            // dispose_op / blend_op: bytes[24..26] — overwrite.
            bytes[data_start + 24] = m.dispose_op as u8;
            bytes[data_start + 25] = m.blend_op as u8;

            // Recompute CRC over type + data (PNG-flavour CRC32).
            let crc = crc32(&bytes[type_start..data_end]);
            let crc_be = crc.to_be_bytes();
            bytes[data_end..data_end + 4].copy_from_slice(&crc_be);

            // Sanity: Fctl::parse must still accept what we wrote.
            // (Dropping the Result is fine — we're just exercising the
            // round-trip; the *fuzzer* will pick up any panic.)
            let _ = Fctl::parse(&bytes[data_start..data_end]);

            frame_idx += 1;
        }
        pos = crc_end;
    }
}
