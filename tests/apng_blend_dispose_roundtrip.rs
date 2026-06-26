//! APNG encode → decode round-trips exercising every `blend_op` /
//! `dispose_op` operator through the crate's own compositor.
//!
//! `apng_region_encode.rs` covers `Disposal::None` / `Blend::Source` region
//! frames; this file pins the remaining operators end-to-end:
//!
//! * `Blend::Over` partial-alpha compositing (W3C PNG3 §13.16, the
//!   non-premultiplied OVER referenced by `APNG_BLEND_OP_OVER`).
//! * `Disposal::Background` (the frame region cleared to transparent black
//!   before the next frame).
//! * `Disposal::Previous` (the frame region reverted to its prior contents).
//!
//! Each animation is encoded with the region-aware encoder, decoded back, and
//! its composited canvases are checked against a model computed in the test.

use oxideav_png::{
    decode_apng, encode_apng_frames, ApngBlend, ApngDisposal, ApngFrameSpec, PngImage,
    PngPixelFormat,
};

fn solid_rgba(w: u32, h: u32, rgba: [u8; 4]) -> PngImage {
    let mut data = vec![0u8; w as usize * h as usize * 4];
    for px in data.chunks_exact_mut(4) {
        px.copy_from_slice(&rgba);
    }
    PngImage {
        width: w,
        height: h,
        pixel_format: PngPixelFormat::Rgba,
        stride: w as usize * 4,
        data,
        palette: Vec::new(),
    }
}

fn pixel(img: &PngImage, x: u32, y: u32) -> [u8; 4] {
    let off = y as usize * img.stride + x as usize * 4;
    img.data[off..off + 4].try_into().unwrap()
}

/// 8-bit OVER of a single channel: matches the compositor's rounded integer
/// `(fg*a + bg*(255-a) + 127) / 255`.
fn over8(fg: u8, bg: u8, a: u8) -> u8 {
    let a = a as u32;
    let inv = 255 - a;
    ((fg as u32 * a + bg as u32 * inv + 127) / 255) as u8
}

fn alpha_over8(a_src: u8, a_dst: u8) -> u8 {
    let a = a_src as u32;
    let inv = 255 - a;
    (a + (a_dst as u32 * inv + 127) / 255) as u8
}

#[test]
fn over_blend_partial_alpha_roundtrips() {
    // 2x2 canvas. Frame 0 = opaque red. Frame 1 = half-alpha green OVER.
    let w = 2u32;
    let h = 2u32;
    let f0 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [255, 0, 0, 255]), 10);
    let f1 = ApngFrameSpec {
        image: solid_rgba(w, h, [0, 255, 0, 128]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 5,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Over,
    };
    let bytes = encode_apng_frames(w, h, None, &[f0, f1], 0).expect("encode");
    let anim = decode_apng(&bytes).expect("decode");
    assert_eq!(anim.frames.len(), 2);

    // Frame 0: pure red.
    assert_eq!(pixel(&anim.frames[0].image, 0, 0), [255, 0, 0, 255]);

    // Frame 1: OVER of green(α=128) onto red.
    let exp = [
        over8(0, 255, 128),
        over8(255, 0, 128),
        over8(0, 0, 128),
        alpha_over8(128, 255),
    ];
    for y in 0..h {
        for x in 0..w {
            assert_eq!(pixel(&anim.frames[1].image, x, y), exp, "({x},{y})");
        }
    }
    // Must NOT be a Source overwrite (pure green α=128).
    assert_ne!(pixel(&anim.frames[1].image, 0, 0), [0, 255, 0, 128]);
}

#[test]
fn background_dispose_clears_region_for_next_frame() {
    // 4x4 canvas. Frame 0 fills red, disposes BACKGROUND → its region (whole
    // canvas) is cleared to transparent black before frame 1. Frame 1 paints
    // a 2x2 green patch at (0,0) with Source; the rest of the canvas must be
    // the cleared transparent black, NOT red.
    let w = 4u32;
    let h = 4u32;
    let f0 = ApngFrameSpec {
        image: solid_rgba(w, h, [255, 0, 0, 255]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::Background,
        blend_op: ApngBlend::Source,
    };
    let f1 = ApngFrameSpec {
        image: solid_rgba(2, 2, [0, 255, 0, 255]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Source,
    };
    let bytes = encode_apng_frames(w, h, None, &[f0, f1], 0).expect("encode");
    let anim = decode_apng(&bytes).expect("decode");
    assert_eq!(anim.frames.len(), 2);
    // Frame 0: all red.
    assert_eq!(pixel(&anim.frames[0].image, 3, 3), [255, 0, 0, 255]);
    // Frame 1: green patch where painted, cleared elsewhere.
    assert_eq!(pixel(&anim.frames[1].image, 0, 0), [0, 255, 0, 255]);
    assert_eq!(
        pixel(&anim.frames[1].image, 3, 3),
        [0, 0, 0, 0],
        "outside patch should be cleared, not red"
    );
}

#[test]
fn previous_dispose_reverts_region_for_next_frame() {
    // 4x4 canvas, 3 frames.
    //   Frame 0: fill red, dispose NONE (canvas = red afterwards).
    //   Frame 1: 2x2 green patch at (1,1), dispose PREVIOUS → region reverts
    //            to the pre-draw state (red) before frame 2.
    //   Frame 2: 1x1 blue dot at (0,0), dispose NONE.
    // After frame 1 displays, the patch shows green; but for frame 2 the
    // patch is reverted to red, so frame 2's canvas is all red except the
    // single blue dot.
    let w = 4u32;
    let h = 4u32;
    let f0 = ApngFrameSpec {
        image: solid_rgba(w, h, [200, 0, 0, 255]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Source,
    };
    let f1 = ApngFrameSpec {
        image: solid_rgba(2, 2, [0, 200, 0, 255]),
        x_offset: 1,
        y_offset: 1,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::Previous,
        blend_op: ApngBlend::Source,
    };
    let f2 = ApngFrameSpec {
        image: solid_rgba(1, 1, [0, 0, 200, 255]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Source,
    };
    let bytes = encode_apng_frames(w, h, None, &[f0, f1, f2], 0).expect("encode");
    let anim = decode_apng(&bytes).expect("decode");
    assert_eq!(anim.frames.len(), 3);

    // Frame 1: green patch visible at (1,1).
    assert_eq!(pixel(&anim.frames[1].image, 1, 1), [0, 200, 0, 255]);
    // Frame 2: the patch region was reverted (PREVIOUS) to red; only the dot
    // at (0,0) is blue.
    assert_eq!(pixel(&anim.frames[2].image, 0, 0), [0, 0, 200, 255]);
    assert_eq!(
        pixel(&anim.frames[2].image, 1, 1),
        [200, 0, 0, 255],
        "patch should be reverted to red, not still green"
    );
    assert_eq!(pixel(&anim.frames[2].image, 2, 2), [200, 0, 0, 255]);
}

#[test]
fn over_fully_opaque_equals_source() {
    // An α=255 OVER frame is byte-identical to a Source overwrite.
    let w = 2u32;
    let h = 2u32;
    let base = solid_rgba(w, h, [10, 20, 30, 255]);
    let top = solid_rgba(w, h, [40, 50, 60, 255]);

    let mk = |blend: ApngBlend| -> Vec<u8> {
        let f0 = ApngFrameSpec::full_canvas(base.clone(), 10);
        let f1 = ApngFrameSpec {
            image: top.clone(),
            x_offset: 0,
            y_offset: 0,
            delay_num: 10,
            delay_den: 100,
            dispose_op: ApngDisposal::None,
            blend_op: blend,
        };
        encode_apng_frames(w, h, None, &[f0, f1], 0).expect("encode")
    };
    let over = decode_apng(&mk(ApngBlend::Over)).expect("decode over");
    let source = decode_apng(&mk(ApngBlend::Source)).expect("decode source");
    assert_eq!(
        over.frames[1].image.data, source.frames[1].image.data,
        "opaque OVER must equal SOURCE"
    );
}
