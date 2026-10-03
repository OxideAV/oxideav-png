//! Round 464: the IDAT / fdAT pixel stream is filtered row by row and
//! deflated straight into the chunk, cut into independent ~1 MiB
//! segments that a thread budget may compress concurrently. These
//! tests pin the three properties that make that safe:
//!
//! * the emitted bytes never depend on the thread budget (the segment
//!   grid is a function of the image alone);
//! * every layout / stride / depth / interlace / APNG combination
//!   decodes back byte-exact through the crate's own decoder; and
//! * the framework-side encoder honours `set_execution_context` and
//!   still produces the serial bytes.

// The framework path: needs `oxideav-core` (default-on `registry`).
#![cfg(feature = "registry")]

use oxideav_core::{
    CodecId, CodecParameters, ExecutionContext, Frame, PixelFormat, VideoFrame, VideoPlane,
};
use oxideav_png::{
    decode, decode_apng, encode, encode_apng_frames_threaded, encode_apng_threaded, ApngFrameSpec,
    EncodeOptions, FilterStrategy, FilterType, PngImage, PngPixelFormat,
};

/// Deterministic "photographic" plane: gradients + 4 bits of noise, so
/// the heuristic picks different filters per row and the deflate
/// segments carry real match structure.
fn synth(width: u32, height: u32, pf: PngPixelFormat, stride_pad: usize) -> PngImage {
    let bpp = pf.bytes_per_pixel();
    let row_bytes = width as usize * bpp;
    let stride = row_bytes + stride_pad;
    let mut data = vec![0u8; stride * height as usize];
    let mut s: u32 = 0x9E37_79B9 ^ width ^ (height << 8);
    for y in 0..height as usize {
        for x in 0..row_bytes {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let base = ((x / bpp) as u32 * 255 / width.max(1)) as u8;
            let noise = (s & 0x0f) as u8;
            data[y * stride + x] = base
                .wrapping_add((y as u8).wrapping_mul(3))
                .wrapping_add(noise);
        }
        // Repeat every 7th row so the identical-row early-out fires.
        if y % 7 == 6 {
            let (prev, cur) = data.split_at_mut(y * stride);
            cur[..row_bytes].copy_from_slice(&prev[(y - 1) * stride..(y - 1) * stride + row_bytes]);
        }
    }
    PngImage::packed(width, height, pf, stride, data)
}

/// Strip stride padding so a decoded (tightly packed) plane compares.
fn packed(img: &PngImage) -> Vec<u8> {
    let row_bytes = img.width as usize * img.bytes_per_pixel();
    (0..img.height as usize)
        .flat_map(|y| {
            img.planes[0].data[y * img.stride()..y * img.stride() + row_bytes]
                .iter()
                .copied()
        })
        .collect()
}

/// Image width; heights are chosen per layout so every plane spans
/// at least two 1 MiB segments (≈ 1.2 MB of wire rows) while debug-build
/// test time stays short.
const W: u32 = 1024;
const H: u32 = 600;

fn rows_for(pf: PngPixelFormat) -> u32 {
    (1_200_000 / (W as usize * pf.bytes_per_pixel())).max(64) as u32
}

#[test]
fn threaded_output_is_identical_to_serial_and_round_trips() {
    for (pf, stride_pad) in [
        (PngPixelFormat::Rgb24, 13),
        (PngPixelFormat::Rgba, 0),
        (PngPixelFormat::Gray8, 5),
        (PngPixelFormat::Ya8, 0),
        (PngPixelFormat::Gray16Le, 0),
        (PngPixelFormat::Rgb48Le, 2),
        (PngPixelFormat::Rgba64Le, 0),
    ] {
        let img = synth(W, rows_for(pf), pf, stride_pad);
        for strategy in [
            FilterStrategy::Adaptive,
            FilterStrategy::Fixed(FilterType::Paeth),
        ] {
            let opts = EncodeOptions::default()
                .with_filter_strategy(strategy)
                .with_compression_level(Some(1));
            let serial = encode(&img, &opts).expect("serial encode");
            for threads in [2usize, 8] {
                let par = encode(&img, &opts.clone().with_threads(threads)).expect("threaded");
                assert_eq!(
                    par, serial,
                    "{pf:?} {strategy:?} threads={threads}: bytes differ"
                );
            }
            let back = decode(&serial).expect("decode");
            assert_eq!(back.format, pf);
            assert_eq!(
                back.planes[0].data,
                packed(&img),
                "{pf:?} {strategy:?} round-trip"
            );
        }
    }
}

/// `Brute` compresses every candidate through the same segmented path,
/// so it stays no larger than any single candidate and thread-stable.
#[test]
fn brute_is_thread_stable_and_smallest_on_segmented_streams() {
    let img = synth(W, H / 3, PngPixelFormat::Rgb24, 0);
    let brute_opts = EncodeOptions::default()
        .with_filter_strategy(FilterStrategy::Brute)
        .with_compression_level(Some(1));
    let brute = encode(&img, &brute_opts).unwrap();
    assert_eq!(
        encode(&img, &brute_opts.clone().with_threads(4)).unwrap(),
        brute
    );
    for cand in FilterStrategy::BRUTE_CANDIDATES {
        let opts = EncodeOptions::default()
            .with_filter_strategy(cand)
            .with_compression_level(Some(1));
        let len = encode(&img, &opts).unwrap().len();
        assert!(brute.len() <= len, "brute {} > {cand:?} {len}", brute.len());
    }
    assert_eq!(decode(&brute).unwrap().planes[0].data, img.planes[0].data);
}

/// Interlaced and sub-byte paths route their pass-concatenated stream
/// through the same segmented compressor.
#[test]
fn adam7_and_subbyte_are_thread_stable() {
    let rgb = synth(W, H / 2, PngPixelFormat::Rgb24, 0);
    let opts = EncodeOptions::default()
        .with_interlace(true)
        .with_compression_level(Some(1));
    let serial = encode(&rgb, &opts).unwrap();
    assert_eq!(encode(&rgb, &opts.clone().with_threads(6)).unwrap(), serial);
    assert_eq!(decode(&serial).unwrap().planes[0].data, rgb.planes[0].data);

    // 4-bit grayscale, 2048 wide × 1100 tall = 1 024 B/row → 1.1 MB.
    // The decoder scales 4-bit samples up by 17 (W3C PNG3 §13.12), so
    // compare against the scaled source.
    let mut gray = synth(2048, 1100, PngPixelFormat::Gray8, 0);
    for b in gray.planes[0].data.iter_mut() {
        *b &= 0x0f;
    }
    let scaled: Vec<u8> = gray.planes[0].data.iter().map(|&v| v * 17).collect();
    for interlace in [false, true] {
        let opts = EncodeOptions::default()
            .with_interlace(interlace)
            .with_bit_depth(Some(4))
            .with_compression_level(Some(1));
        let serial = encode(&gray, &opts).unwrap();
        assert_eq!(
            encode(&gray, &opts.clone().with_threads(5)).unwrap(),
            serial
        );
        assert_eq!(
            decode(&serial).unwrap().planes[0].data,
            scaled,
            "interlace={interlace}"
        );
    }
}

/// Every frame of an APNG — IDAT and fdAT, full-canvas and region —
/// is streamed into its chunk; sequence numbers and bytes stay stable.
#[test]
fn apng_frames_stream_into_chunks_thread_stably() {
    let a = synth(W, H / 4, PngPixelFormat::Rgba, 0);
    let mut b = a.clone();
    for px in b.planes[0].data.chunks_exact_mut(4) {
        px[0] = px[0].wrapping_add(40);
    }
    let opts = EncodeOptions::default().with_compression_level(Some(1));
    let serial = encode_apng_threaded(&[a.clone(), b.clone()], 5, 0, &opts, 1).unwrap();
    assert_eq!(
        encode_apng_threaded(&[a.clone(), b.clone()], 5, 0, &opts, 4).unwrap(),
        serial
    );
    let anim = decode_apng(&serial).unwrap();
    assert_eq!(anim.frames.len(), 2);
    assert_eq!(anim.frames[0].image.planes[0].data, a.planes[0].data);
    assert_eq!(anim.frames[1].image.planes[0].data, b.planes[0].data);

    // Region-aware: a half-height sub-frame at an offset.
    let region = synth(W, H / 8, PngPixelFormat::Rgba, 0);
    let frames = vec![
        ApngFrameSpec::full_canvas(a.clone(), 5),
        (ApngFrameSpec::full_canvas(region.clone(), 5)).with_y_offset(H / 8),
    ];
    let serial = encode_apng_frames_threaded(W, H / 4, None, &frames, 0, &opts, 1).unwrap();
    assert_eq!(
        encode_apng_frames_threaded(W, H / 4, None, &frames, 0, &opts, 3).unwrap(),
        serial
    );
    let anim = decode_apng(&serial).unwrap();
    assert_eq!(anim.frames.len(), 2);
    assert_eq!(anim.frames[0].image.planes[0].data, a.planes[0].data);
}

/// The framework encoder: `set_execution_context` is honoured (no
/// panic, same bytes), the buffered frame's plane is moved — not
/// copied — into the encode, and the packet decodes byte-exact.
#[test]
fn registry_encoder_honours_execution_context() {
    let img = synth(W, H / 2, PngPixelFormat::Rgb24, 0);
    let frame = VideoFrame {
        pts: Some(7),
        planes: vec![VideoPlane {
            stride: img.stride(),
            data: img.planes[0].data.clone(),
        }],
    };
    let mut params = CodecParameters::video(CodecId::new("png"));
    params.width = Some(W);
    params.height = Some(H / 2);
    params.pixel_format = Some(PixelFormat::Rgb24);
    let run = |threads: usize| {
        let mut enc = oxideav_png::encoder::make_encoder(&params).expect("make_encoder");
        enc.set_execution_context(&ExecutionContext::with_threads(threads));
        enc.send_frame(&Frame::Video(frame.clone())).unwrap();
        enc.flush().unwrap();
        enc.receive_packet().unwrap()
    };
    let serial = run(1);
    let par = run(8);
    assert_eq!(
        serial.data, par.data,
        "framework encoder bytes depend on the thread budget"
    );
    assert_eq!(par.pts, Some(7));
    assert_eq!(
        decode(&par.data).unwrap().planes[0].data,
        img.planes[0].data
    );
}

/// A plane shorter than `height × stride` is an encode error, not a
/// panic, on the row-streaming path.
#[test]
fn short_plane_is_an_error_not_a_panic() {
    let mut img = synth(64, 64, PngPixelFormat::Rgb24, 0);
    img.planes[0].data.pop();
    let err = encode(&img, &EncodeOptions::default()).unwrap_err();
    assert!(err.to_string().contains("bytes"), "{err}");
    let mut narrow = synth(64, 64, PngPixelFormat::Rgb24, 0);
    narrow.planes[0].stride = 100;
    let err = encode(&narrow, &EncodeOptions::default()).unwrap_err();
    assert!(err.to_string().contains("stride"), "{err}");
}
