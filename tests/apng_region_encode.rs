//! Region-aware APNG encoder round-trips
//! ([`encode_apng_frames`] / [`encode_apng_frames_with_options`]).
//!
//! These exercise the per-frame `fcTL` surface the bulk
//! [`encode_apng`] helper cannot reach: sub-canvas frame regions
//! (`x_offset` / `y_offset` + a smaller frame extent), per-frame
//! rational delays (`delay_num` / `delay_den`), and the
//! `Disposal::{None,Background,Previous}` / `Blend::{Source,Over}`
//! operators. Each test encodes a hand-built animation, decodes it back
//! through the crate's own compositor, and asserts the composited
//! canvases match the model computed in the test.

use oxideav_png::{
    decode_apng, encode_apng_frames, encode_apng_frames_with_options, ApngBlend, ApngDisposal,
    ApngFrameSpec, PngEncoderOptions, PngImage, PngPixelFormat,
};

/// A solid-colour RGBA region of the given extent.
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

/// Read the canvas pixel at (x, y) from a composited RGBA frame.
fn pixel(img: &PngImage, x: u32, y: u32) -> [u8; 4] {
    let off = y as usize * img.stride + x as usize * 4;
    img.data[off..off + 4].try_into().unwrap()
}

#[test]
fn region_frames_no_default_first_frame_is_default() {
    // 4x4 canvas. Frame 0 fills the whole canvas red (and is the default
    // image). Frame 1 paints a 2x2 green patch at (1,1) with Source blend
    // / None dispose. Frame 2 paints a 1x1 blue dot at (0,0).
    let w = 4u32;
    let h = 4u32;
    let f0 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [255, 0, 0, 255]), 10);
    let f1 = ApngFrameSpec {
        image: solid_rgba(2, 2, [0, 255, 0, 255]),
        x_offset: 1,
        y_offset: 1,
        delay_num: 5,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Source,
    };
    let f2 = ApngFrameSpec {
        image: solid_rgba(1, 1, [0, 0, 255, 255]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 1,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Source,
    };

    let bytes = encode_apng_frames(w, h, None, &[f0, f1, f2], 0).expect("encode");
    let anim = decode_apng(&bytes).expect("decode");

    assert_eq!(anim.width, w);
    assert_eq!(anim.height, h);
    assert_eq!(anim.num_plays, 0);
    assert_eq!(anim.frames.len(), 3);

    // Frame 0: all red.
    let c0 = &anim.frames[0].image;
    for y in 0..h {
        for x in 0..w {
            assert_eq!(pixel(c0, x, y), [255, 0, 0, 255], "f0 ({x},{y})");
        }
    }
    assert_eq!(anim.frames[0].delay_cs, 10);

    // Frame 1: red except the 2x2 green patch at (1,1)..(2,2).
    let c1 = &anim.frames[1].image;
    for y in 0..h {
        for x in 0..w {
            let want = if (1..3).contains(&x) && (1..3).contains(&y) {
                [0, 255, 0, 255]
            } else {
                [255, 0, 0, 255]
            };
            assert_eq!(pixel(c1, x, y), want, "f1 ({x},{y})");
        }
    }
    assert_eq!(anim.frames[1].delay_cs, 5);

    // Frame 2: previous canvas plus a blue dot at (0,0).
    let c2 = &anim.frames[2].image;
    assert_eq!(pixel(c2, 0, 0), [0, 0, 255, 255]);
    assert_eq!(pixel(c2, 2, 2), [0, 255, 0, 255]); // green patch persists
    assert_eq!(pixel(c2, 3, 3), [255, 0, 0, 255]); // red corner persists
    assert_eq!(anim.frames[2].delay_cs, 1);
}

#[test]
fn region_frames_with_separate_default_image() {
    // The default (still) image is solid gray; it is NOT part of the
    // animation. The animation has two full-canvas frames (white, black).
    let w = 3u32;
    let h = 2u32;
    let default_img = solid_rgba(w, h, [128, 128, 128, 255]);
    let f0 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [255, 255, 255, 255]), 4);
    let f1 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [0, 0, 0, 255]), 4);

    let bytes =
        encode_apng_frames(w, h, Some(&default_img), &[f0, f1], 3).expect("encode w/ default");
    let anim = decode_apng(&bytes).expect("decode");

    // num_frames excludes the default image, so two animation frames.
    assert_eq!(anim.frames.len(), 2);
    assert_eq!(anim.num_plays, 3);
    assert_eq!(pixel(&anim.frames[0].image, 0, 0), [255, 255, 255, 255]);
    assert_eq!(pixel(&anim.frames[1].image, 2, 1), [0, 0, 0, 255]);
}

#[test]
fn region_frame_background_disposal_clears_region() {
    // 4x4 canvas, opaque red default frame. Frame 1 paints a 2x2 green
    // patch at (1,1) with Background disposal → after it shows, the patch
    // region is cleared to transparent black. Frame 2 is a 1x1 dot
    // elsewhere, so the cleared region shows through as zeros.
    let w = 4u32;
    let h = 4u32;
    let f0 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [255, 0, 0, 255]), 10);
    let f1 = ApngFrameSpec {
        image: solid_rgba(2, 2, [0, 255, 0, 255]),
        x_offset: 1,
        y_offset: 1,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::Background,
        blend_op: ApngBlend::Source,
    };
    let f2 = ApngFrameSpec {
        image: solid_rgba(1, 1, [0, 0, 255, 255]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Source,
    };

    let bytes = encode_apng_frames(w, h, None, &[f0, f1, f2], 0).expect("encode");
    let anim = decode_apng(&bytes).expect("decode");

    // Frame 1 shows the green patch.
    assert_eq!(pixel(&anim.frames[1].image, 1, 1), [0, 255, 0, 255]);
    // Frame 2: the green patch region was disposed-to-background (zeros).
    let c2 = &anim.frames[2].image;
    assert_eq!(pixel(c2, 1, 1), [0, 0, 0, 0], "background-disposed region");
    assert_eq!(pixel(c2, 0, 0), [0, 0, 255, 255], "new dot");
    assert_eq!(pixel(c2, 3, 3), [255, 0, 0, 255], "untouched red corner");
}

#[test]
fn region_frame_over_blend_composites_alpha() {
    // Opaque white default. Frame 1 paints a half-transparent black 2x2
    // patch at (0,0) with Over blend → black at alpha 128 over white.
    let w = 2u32;
    let h = 2u32;
    let f0 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [255, 255, 255, 255]), 10);
    let f1 = ApngFrameSpec {
        image: solid_rgba(2, 2, [0, 0, 0, 128]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Over,
    };

    let bytes = encode_apng_frames(w, h, None, &[f0, f1], 0).expect("encode");
    let anim = decode_apng(&bytes).expect("decode");

    // Over: out = fg*a + bg*(255-a), rounded. fg=0, bg=255, a=128:
    // 0*128 + 255*127 = 32385; (32385 + 127) / 255 = 127.
    let p = pixel(&anim.frames[1].image, 0, 0);
    assert_eq!(p[0], 127, "over-blended R");
    assert_eq!(p[1], 127);
    assert_eq!(p[2], 127);
    // Alpha over: 128 + 255*(255-128)/255 ≈ 128 + 127 = 255.
    assert_eq!(p[3], 255, "over-blended A");
}

#[test]
fn region_frame_rational_delay_preserved() {
    // delay_num=1, delay_den=30 → 1/30 s ≈ 3.33 cs → centiseconds floor 3.
    let w = 2u32;
    let h = 2u32;
    let f0 = ApngFrameSpec {
        image: solid_rgba(w, h, [10, 20, 30, 255]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 1,
        delay_den: 30,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Source,
    };
    let bytes = encode_apng_frames(w, h, None, &[f0], 1).expect("encode");
    let anim = decode_apng(&bytes).expect("decode");
    // 100/30 = 3.33 → 3, decoder applies .max(1) so stays 3.
    assert_eq!(anim.frames[0].delay_cs, 3);
}

#[test]
fn region_frame_out_of_canvas_is_rejected() {
    let w = 4u32;
    let h = 4u32;
    // A 2x2 frame at offset (3,3) → right edge 5 > 4: out of canvas.
    let f0 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [1, 2, 3, 255]), 10);
    let bad = ApngFrameSpec {
        image: solid_rgba(2, 2, [0, 0, 0, 255]),
        x_offset: 3,
        y_offset: 3,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Source,
    };
    let err = encode_apng_frames(w, h, None, &[f0, bad], 0).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("exceeds canvas"), "got: {msg}");
}

#[test]
fn region_frame_partial_first_frame_without_default_is_rejected() {
    // No separate default image, but the first frame is smaller than the
    // canvas → the IDAT could not be a complete default image.
    let w = 4u32;
    let h = 4u32;
    let f0 = ApngFrameSpec {
        image: solid_rgba(2, 2, [0, 0, 0, 255]),
        x_offset: 0,
        y_offset: 0,
        delay_num: 10,
        delay_den: 100,
        dispose_op: ApngDisposal::None,
        blend_op: ApngBlend::Source,
    };
    let err = encode_apng_frames(w, h, None, &[f0], 0).unwrap_err();
    assert!(format!("{err}").contains("full canvas"));
}

#[test]
fn region_frame_mismatched_format_is_rejected() {
    let w = 2u32;
    let h = 2u32;
    let f0 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [0, 0, 0, 255]), 10);
    let mut f1 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [0, 0, 0, 255]), 10);
    f1.image.pixel_format = PngPixelFormat::Gray8;
    let err = encode_apng_frames(w, h, None, &[f0, f1], 0).unwrap_err();
    assert!(format!("{err}").contains("pixel_format"));
}

#[test]
fn region_frames_interlaced_roundtrip() {
    // Region-aware encode with Adam7 interlacing on, full-canvas frames.
    let w = 8u32;
    let h = 8u32;
    let f0 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [200, 100, 50, 255]), 10);
    let f1 = ApngFrameSpec::full_canvas(solid_rgba(w, h, [50, 100, 200, 255]), 10);
    let opts = PngEncoderOptions {
        interlace: true,
        ..Default::default()
    };
    let bytes = encode_apng_frames_with_options(w, h, None, &[f0, f1], 0, &opts)
        .expect("encode interlaced");
    let anim = decode_apng(&bytes).expect("decode");
    assert_eq!(anim.frames.len(), 2);
    assert_eq!(pixel(&anim.frames[0].image, 3, 3), [200, 100, 50, 255]);
    assert_eq!(pixel(&anim.frames[1].image, 5, 5), [50, 100, 200, 255]);
}
