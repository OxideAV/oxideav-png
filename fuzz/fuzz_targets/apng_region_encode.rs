#![no_main]

//! Drive the region-aware APNG encoder
//! ([`encode_apng_frames_with_options`]) directly with fuzz-derived
//! per-frame regions / offsets / delays / dispose+blend ops, then assert
//! every accepted encode re-decodes through `decode_apng` without panic.
//!
//! Where `apng_frame_walk` mutates a *pre-built* APNG byte stream, this
//! target funnels the mutation budget into the *encoder's* fcTL-emission
//! + fdAT-framing path: the sub-region width/height the encoder must
//! compress against a synthetic per-frame IHDR, the `x_offset` /
//! `y_offset` bounds-check, the separate-default-image vs
//! first-frame-is-default branch, and the contiguous sequence-number
//! generator. Liveness target: the encoder must `return` a `Result`
//! (Ok or Err) for any plan, and any `Ok` byte stream must re-decode to
//! a `Result` — neither side may panic / abort / index out of bounds.
//!
//! It additionally asserts a *consistency* invariant on the happy path:
//! an `Ok` encode whose canvas is non-trivial must produce an APNG whose
//! decoded frame count equals the number of frames submitted (the
//! encoder writes exactly one fcTL per frame).

use libfuzzer_sys::fuzz_target;
use oxideav_png::{
    decode_apng, encode_apng_frames_with_options, ApngBlend, ApngDisposal, ApngFrameSpec,
    PngEncoderOptions, PngImage, PngPixelFormat,
};

/// Canvas cap. Encode + decode cost is O(canvas * frames); keep both
/// small so the fuzzer's iteration rate stays high.
const MAX_DIM: u32 = 16;
const MIN_DIM: u32 = 1;
const MAX_FRAMES: usize = 6;

fuzz_target!(|data: &[u8]| {
    let Some(plan) = Plan::from_fuzz_input(data) else {
        return;
    };

    let canvas_w = plan.canvas_w;
    let canvas_h = plan.canvas_h;

    // Build the per-frame specs from the plan.
    let specs: Vec<ApngFrameSpec> = plan
        .frames
        .iter()
        .map(|fp| ApngFrameSpec {
            image: solid_region(fp.w, fp.h, fp.seed),
            x_offset: fp.x,
            y_offset: fp.y,
            delay_num: fp.delay_num,
            delay_den: fp.delay_den,
            dispose_op: dispose(fp.dispose),
            blend_op: blend(fp.blend),
        })
        .collect();

    // Optional separate default image: always full canvas.
    let default_img = if plan.use_default {
        Some(solid_region(canvas_w, canvas_h, plan.default_seed))
    } else {
        None
    };

    let opts = PngEncoderOptions {
        interlace: plan.interlace,
        ..Default::default()
    };

    let result = encode_apng_frames_with_options(
        canvas_w,
        canvas_h,
        default_img.as_ref(),
        &specs,
        plan.num_plays,
        &opts,
    );

    // Any Ok encode must re-decode without panic, and its frame count
    // must equal the number of submitted frames.
    if let Ok(bytes) = result {
        if let Ok(anim) = decode_apng(&bytes) {
            assert_eq!(
                anim.frames.len(),
                specs.len(),
                "decoded frame count != submitted frame count"
            );
            assert_eq!(anim.width, canvas_w);
            assert_eq!(anim.height, canvas_h);
        }
    }
});

fn dispose(v: u8) -> ApngDisposal {
    match v % 3 {
        0 => ApngDisposal::None,
        1 => ApngDisposal::Background,
        _ => ApngDisposal::Previous,
    }
}

fn blend(v: u8) -> ApngBlend {
    match v % 2 {
        0 => ApngBlend::Source,
        _ => ApngBlend::Over,
    }
}

/// Solid-colour RGBA region — the encoder always accepts well-formed
/// pixel buffers, so any rejection comes from the geometry / format
/// rules under test, not a malformed buffer.
fn solid_region(w: u32, h: u32, seed: u8) -> PngImage {
    let stride = w as usize * 4;
    let mut data = Vec::with_capacity(stride * h as usize);
    let r = seed;
    let g = seed.wrapping_mul(3);
    let b = seed.wrapping_mul(7);
    let a = 0x40u8.wrapping_add(seed);
    for _ in 0..(w as usize * h as usize) {
        data.push(r);
        data.push(g);
        data.push(b);
        data.push(a);
    }
    PngImage {
        width: w,
        height: h,
        pixel_format: PngPixelFormat::Rgba,
        stride,
        data,
        palette: Vec::new(),
    }
}

struct FramePlan {
    w: u32,
    h: u32,
    x: u32,
    y: u32,
    delay_num: u16,
    delay_den: u16,
    dispose: u8,
    blend: u8,
    seed: u8,
}

struct Plan {
    canvas_w: u32,
    canvas_h: u32,
    use_default: bool,
    default_seed: u8,
    interlace: bool,
    num_plays: u32,
    frames: Vec<FramePlan>,
}

impl Plan {
    fn from_fuzz_input(data: &[u8]) -> Option<Self> {
        // Header: 6 bytes, then 10 bytes per frame.
        //   [0] canvas_w sel
        //   [1] canvas_h sel
        //   [2] flags: bit0 use_default, bit1 interlace
        //   [3] default_seed
        //   [4] num_plays low byte
        //   [5] frame count sel (1..=MAX_FRAMES)
        // per frame (10 bytes):
        //   [0] w sel, [1] h sel, [2] x sel, [3] y sel,
        //   [4..6] delay_num le, [6..8] delay_den le, [8] ops, [9] seed
        if data.len() < 6 {
            return None;
        }
        let canvas_w = MIN_DIM + (data[0] as u32 % (MAX_DIM - MIN_DIM + 1));
        let canvas_h = MIN_DIM + (data[1] as u32 % (MAX_DIM - MIN_DIM + 1));
        let use_default = data[2] & 1 != 0;
        let interlace = data[2] & 2 != 0;
        let default_seed = data[3];
        let num_plays = data[4] as u32;
        let n_frames = ((data[5] as usize) % MAX_FRAMES) + 1;

        let need = 6 + n_frames * 10;
        if data.len() < need {
            return None;
        }

        let mut frames = Vec::with_capacity(n_frames);
        for i in 0..n_frames {
            let off = 6 + i * 10;
            // Region width/height: 1..=canvas dim (kept in-canvas mostly,
            // but the offset selector deliberately pushes some over).
            let w = 1 + (data[off] as u32 % canvas_w);
            let h = 1 + (data[off + 1] as u32 % canvas_h);
            let x = offset_for(data[off + 2], canvas_w);
            let y = offset_for(data[off + 3], canvas_h);
            let delay_num = u16::from_le_bytes([data[off + 4], data[off + 5]]);
            let delay_den = u16::from_le_bytes([data[off + 6], data[off + 7]]);
            let ops = data[off + 8];
            let seed = data[off + 9];
            frames.push(FramePlan {
                w,
                h,
                x,
                y,
                delay_num,
                delay_den,
                dispose: ops & 0x0f,
                blend: ops >> 4,
                seed,
            });
        }

        Some(Self {
            canvas_w,
            canvas_h,
            use_default,
            default_seed,
            interlace,
            num_plays,
            frames,
        })
    }
}

/// Map a selector into an in-canvas / on-edge / out-of-canvas band so the
/// encoder's §11.3.6.1 bounds check is exercised on both sides.
fn offset_for(sel: u8, canvas_dim: u32) -> u32 {
    match sel & 0x03 {
        0 | 1 => sel as u32 % canvas_dim,
        2 => canvas_dim,
        _ => canvas_dim + (sel as u32 >> 2) + 1,
    }
}
