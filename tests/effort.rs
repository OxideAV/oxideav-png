//! `EncodeOptions::effort`: one dial over the row-filter policy and the
//! DEFLATE level, and the entropy row-filter selection it uses.

use oxideav_png::chunk::ChunkIter;
use oxideav_png::filter::{choose_filter_entropy, filter_row};
use oxideav_png::{
    EncodeOptions, FilterStrategy, FilterType, Palette, PngError, PngImage, PngPixelFormat, decode,
    encode,
};

/// Smooth ramps with a little texture: content every filter can gain on.
fn picture(w: u32, h: u32, bpp: usize) -> Vec<u8> {
    let mut s = 0x1234_5678u32;
    let mut out = Vec::with_capacity((w * h) as usize * bpp);
    for y in 0..h {
        for x in 0..w {
            for c in 0..bpp as u32 {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                let base = (x * (3 + c) + y * (5 - c)) / 4;
                out.push((base + (s & 3)) as u8);
            }
        }
    }
    out
}

fn rgb(w: u32, h: u32) -> PngImage {
    PngImage::from_rgb8(w, h, picture(w, h, 3)).unwrap()
}

/// The filter-type byte of every row of a non-interlaced file.
fn row_filters(file: &[u8]) -> Vec<u8> {
    let mut idat = Vec::new();
    let (mut w, mut h, mut depth, mut ct) = (0usize, 0usize, 0usize, 0u8);
    for c in ChunkIter::new(file, 8) {
        let c = c.unwrap();
        if c.is_type(b"IHDR") {
            w = u32::from_be_bytes(c.data[0..4].try_into().unwrap()) as usize;
            h = u32::from_be_bytes(c.data[4..8].try_into().unwrap()) as usize;
            depth = c.data[8] as usize;
            ct = c.data[9];
        } else if c.is_type(b"IDAT") {
            idat.extend_from_slice(c.data);
        }
    }
    let channels = [1usize, 0, 3, 1, 2, 0, 4][ct as usize];
    let row = (w * channels * depth).div_ceil(8) + 1;
    let raw = compcol::vec::decompress_to_vec::<compcol::zlib::Zlib>(&idat).unwrap();
    assert_eq!(raw.len(), row * h);
    raw.chunks_exact(row).map(|r| r[0]).collect()
}

#[test]
fn every_effort_decodes_to_the_same_pixels_and_sizes_fall() {
    let image = rgb(96, 80);
    let sizes: Vec<usize> = (1..=9u8)
        .map(|e| {
            let file = encode(&image, &EncodeOptions::new().with_effort(e)).unwrap();
            assert!(decode(&file).unwrap() == image, "effort {e}");
            file.len()
        })
        .collect();
    // Effort 9 searches a set that contains effort 8's choice.
    assert!(sizes[8] <= sizes[7], "{sizes:?}");
    // The two ends of the dial are far apart on this content.
    assert!(sizes[8] < sizes[0], "{sizes:?}");
}

#[test]
fn effort_three_is_the_default_and_explicit_settings_win() {
    let image = rgb(64, 48);
    let default = encode(&image, &EncodeOptions::new()).unwrap();
    assert_eq!(
        encode(&image, &EncodeOptions::new().with_effort(3)).unwrap(),
        default
    );
    // Effort 1 = Sub on every row at level 1.
    let e1 = encode(&image, &EncodeOptions::new().with_effort(1)).unwrap();
    assert!(row_filters(&e1).iter().all(|&f| f == FilterType::Sub as u8));
    assert_eq!(
        e1,
        encode(
            &image,
            &EncodeOptions::new()
                .with_level(1)
                .with_filter_strategy(FilterStrategy::Fixed(FilterType::Sub))
        )
        .unwrap()
    );
    // An explicit level keeps the effort's filter; an explicit filter
    // keeps the effort's level.
    assert_eq!(
        encode(&image, &EncodeOptions::new().with_effort(1).with_level(6)).unwrap(),
        encode(
            &image,
            &EncodeOptions::new()
                .with_level(6)
                .with_filter_strategy(FilterStrategy::Fixed(FilterType::Sub))
        )
        .unwrap()
    );
    assert_eq!(
        encode(
            &image,
            &EncodeOptions::new()
                .with_effort(8)
                .with_filter_strategy(FilterStrategy::Fixed(FilterType::Up))
        )
        .unwrap(),
        encode(
            &image,
            &EncodeOptions::new()
                .with_level(9)
                .with_filter_strategy(FilterStrategy::Fixed(FilterType::Up))
        )
        .unwrap()
    );
    // Effort 8 = entropy at level 9.
    assert_eq!(
        encode(&image, &EncodeOptions::new().with_effort(8)).unwrap(),
        encode(
            &image,
            &EncodeOptions::new()
                .with_level(9)
                .with_filter_strategy(FilterStrategy::Entropy)
        )
        .unwrap()
    );
}

#[test]
fn out_of_range_effort_is_an_error() {
    let image = rgb(4, 4);
    for bad in [0u8, 10, 255] {
        for opts in [
            EncodeOptions::new().with_effort(bad),
            // Checked even when both settings are given explicitly.
            EncodeOptions::new()
                .with_effort(bad)
                .with_level(3)
                .with_filter_strategy(FilterStrategy::Fixed(FilterType::Up)),
        ] {
            match encode(&image, &opts) {
                Err(PngError::InvalidData(m)) => assert!(m.contains("effort"), "{m}"),
                other => panic!("effort {bad}: {other:?}"),
            }
        }
    }
}

#[test]
fn indexed_and_sub_byte_images_use_filter_none_below_effort_nine() {
    let palette = Palette::new((0..16u8).map(|i| [i * 16, 255 - i * 16, i, 255]).collect());
    let indices: Vec<u8> = (0..40 * 30u32)
        .map(|i| ((i * 7 + i / 40) % 16) as u8)
        .collect();
    let pal = PngImage::packed(40, 30, PngPixelFormat::Pal8, 40, indices.clone())
        .unwrap()
        .with_palette(palette);
    let gray4 = PngImage::packed(40, 30, PngPixelFormat::Gray8, 40, indices).unwrap();
    for effort in 1..=8u8 {
        let file = encode(&pal, &EncodeOptions::new().with_effort(effort)).unwrap();
        assert!(
            row_filters(&file).iter().all(|&f| f == 0),
            "pal effort {effort}"
        );
        let file = encode(
            &gray4,
            &EncodeOptions::new().with_effort(effort).with_bit_depth(4),
        )
        .unwrap();
        assert!(
            row_filters(&file).iter().all(|&f| f == 0),
            "4-bit effort {effort}"
        );
        // The same samples at 8 bits are predictable: not all None.
        if effort >= 2 {
            let file = encode(&gray4, &EncodeOptions::new().with_effort(effort)).unwrap();
            assert!(
                row_filters(&file).iter().any(|&f| f != 0),
                "8-bit effort {effort}"
            );
        }
    }
    // Effort 9 searches; whatever it picks is no larger than None.
    let none = encode(&pal, &EncodeOptions::new().with_effort(8)).unwrap();
    let brute = encode(&pal, &EncodeOptions::new().with_effort(9)).unwrap();
    assert!(brute.len() <= none.len());
    assert!(decode(&brute).unwrap() == pal);
}

#[test]
fn bytes_do_not_depend_on_the_thread_budget_at_any_effort() {
    // Wide enough for several 1 MiB segments.
    let image = rgb(1200, 700);
    for effort in [1u8, 2, 4] {
        let serial = encode(&image, &EncodeOptions::new().with_effort(effort)).unwrap();
        for threads in [3usize, 8] {
            let parallel = encode(
                &image,
                &EncodeOptions::new()
                    .with_effort(effort)
                    .with_threads(threads),
            )
            .unwrap();
            assert!(serial == parallel, "effort {effort}, {threads} threads");
        }
        assert!(decode(&serial).unwrap() == image);
    }
}

#[test]
fn entropy_selection_picks_the_filter_with_the_fewest_distinct_values() {
    // A row that repeats the previous one: Up filters it to all zeros.
    let prev: Vec<u8> = (0..64u32).map(|i| (i * 37 % 251) as u8).collect();
    let mut scratch = vec![0u8; 64];
    assert_eq!(
        choose_filter_entropy(&prev, &prev, 1, &mut scratch),
        FilterType::Up
    );
    // A constant-step ramp: Sub filters it to one repeated value (but
    // for the first byte), every other filter leaves many values.
    let ramp: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(3)).collect();
    let noise: Vec<u8> = (0..64u32).map(|i| (i * i * 31 % 253) as u8).collect();
    assert_eq!(
        choose_filter_entropy(&ramp, &noise, 1, &mut scratch),
        FilterType::Sub
    );
    // The choice is the argmax of the documented score, ties to the
    // earlier type: recompute it the slow way for a few rows.
    let score = |bytes: &[u8]| -> f64 {
        let mut hist = [0u32; 256];
        for &b in bytes {
            hist[b as usize] += 1;
        }
        hist.iter()
            .filter(|&&n| n > 1)
            .map(|&n| n as f64 * (n as f64).log2())
            .sum()
    };
    for seed in 0..40u32 {
        let row: Vec<u8> = (0..96u32)
            .map(|i| ((i / (1 + seed % 5)) * (seed + 3) % 17 * 15) as u8)
            .collect();
        let prev: Vec<u8> = (0..96u32).map(|i| ((i + seed) % 9 * 28) as u8).collect();
        let mut out = vec![0u8; 96];
        let scores: Vec<f64> = [
            FilterType::None,
            FilterType::Sub,
            FilterType::Up,
            FilterType::Average,
            FilterType::Paeth,
        ]
        .iter()
        .map(|&ft| {
            filter_row(ft, &row, &prev, 3, &mut out);
            score(&out)
        })
        .collect();
        let picked = choose_filter_entropy(&row, &prev, 3, &mut scratch_for(96)) as usize;
        let best = scores.iter().cloned().fold(f64::MIN, f64::max);
        // The integer score has 16 fraction bits per logarithm; allow
        // the picked filter to be within that of the float maximum.
        assert!(
            best - scores[picked] < 0.01 * 96.0,
            "seed {seed}: {scores:?} picked {picked}"
        );
    }
}

fn scratch_for(n: usize) -> Vec<u8> {
    vec![0u8; n]
}

#[cfg(feature = "registry")]
#[test]
fn registry_option_effort_reaches_the_encoder() {
    use oxideav_core::{CodecId, CodecParameters, Frame, PixelFormat, VideoFrame, VideoPlane};
    let image = rgb(64, 48);
    let encode_with = |effort: &str| -> oxideav_core::Result<Vec<u8>> {
        let mut p = CodecParameters::video(CodecId::new("png"));
        p.width = Some(64);
        p.height = Some(48);
        p.pixel_format = Some(PixelFormat::Rgb24);
        p.options = p.options.clone().set("effort", effort);
        let mut enc = oxideav_png::make_encoder(&p)?;
        enc.send_frame(&Frame::Video(VideoFrame {
            pts: Some(0),
            planes: vec![VideoPlane {
                stride: 64 * 3,
                data: image.as_bytes().unwrap().to_vec(),
            }],
        }))?;
        enc.flush()?;
        Ok(enc.receive_packet()?.data)
    };
    for effort in [1u8, 5, 8] {
        assert_eq!(
            encode_with(&effort.to_string()).unwrap(),
            encode(&image, &EncodeOptions::new().with_effort(effort)).unwrap(),
            "effort {effort}"
        );
    }
    assert_eq!(
        encode_with("0").unwrap(),
        encode(&image, &EncodeOptions::new()).unwrap()
    );
    assert!(encode_with("12").is_err());
}
