//! APNG output-buffer rendering held to a model of W3C PNG3 §11.3.6.2
//! (`fcTL`) written here independently of the crate, on both decode
//! paths: the standalone [`oxideav_png::decode_apng`] and the registry
//! demuxer + decoder, which must hand out the whole canvas as displayed
//! for every frame.
//!
//! The model, straight from the `fcTL` text:
//!
//! * the output buffer starts "fully transparent black";
//! * `APNG_BLEND_OP_SOURCE`: "all color components of the frame,
//!   including alpha, overwrite the current contents of the frame's
//!   output buffer region";
//! * `APNG_BLEND_OP_OVER`: non-premultiplied OVER (§13.16 `output =
//!   alpha * foreground + (1 - alpha) * background`, carried through for
//!   a translucent buffer pixel: αo = αs + αd(1 − αs), Co = (Cs αs +
//!   Cd αd (1 − αs)) / αo), rounded to nearest;
//! * after the frame is shown, `APNG_DISPOSE_OP_NONE` leaves the buffer,
//!   `_BACKGROUND` clears the region to transparent black, `_PREVIOUS`
//!   reverts it ("treated as BACKGROUND" on the first frame).
//!
//! The first frame's two blend modes are "functionally equivalent due
//! to the clearing of the output buffer": they differ only in the colour
//! stored under fully transparent pixels, so pictures are compared with
//! every fully transparent pixel equal to every other.
#![cfg(feature = "registry")]

use std::io::Cursor;

use oxideav_core::{Frame, RuntimeContext};
use oxideav_png::chunk::write_chunk;
use oxideav_png::decode_apng;

const SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

fn zlib(raw: &[u8]) -> Vec<u8> {
    compcol::vec::compress_to_vec_with::<compcol::zlib::Zlib>(
        raw,
        compcol::zlib::EncoderConfig { level: 6 },
    )
    .expect("zlib compress")
}

/// One animation frame of the test streams.
#[derive(Clone)]
struct F {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    dispose: u8,
    blend: u8,
    /// `w × h` pixels, `ch` samples each, one byte or two per sample.
    px: Vec<u16>,
}

/// An APNG of `frames` on a `cw × ch` canvas of colour type `ct` /
/// bit depth `bd`, frame 0 the default image (it must cover the canvas).
fn apng(cw: u32, chh: u32, ct: u8, bd: u8, extra: &[(&[u8; 4], Vec<u8>)], frames: &[F]) -> Vec<u8> {
    let nch = match ct {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        _ => 4,
    };
    let mut out = SIG.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&cw.to_be_bytes());
    ihdr.extend_from_slice(&chh.to_be_bytes());
    ihdr.extend_from_slice(&[bd, ct, 0, 0, 0]);
    write_chunk(&mut out, b"IHDR", &ihdr);
    let mut actl = (frames.len() as u32).to_be_bytes().to_vec();
    actl.extend_from_slice(&0u32.to_be_bytes());
    write_chunk(&mut out, b"acTL", &actl);
    for (t, d) in extra {
        write_chunk(&mut out, t, d);
    }
    let mut seq = 0u32;
    for (i, f) in frames.iter().enumerate() {
        let mut fctl = seq.to_be_bytes().to_vec();
        for v in [f.w, f.h, f.x, f.y] {
            fctl.extend_from_slice(&v.to_be_bytes());
        }
        fctl.extend_from_slice(&1u16.to_be_bytes());
        fctl.extend_from_slice(&10u16.to_be_bytes());
        fctl.push(f.dispose);
        fctl.push(f.blend);
        write_chunk(&mut out, b"fcTL", &fctl);
        seq += 1;
        let mut raw = Vec::new();
        for row in f.px.chunks(f.w as usize * nch) {
            raw.push(0);
            for &s in row {
                if bd == 16 {
                    raw.extend_from_slice(&s.to_be_bytes());
                } else {
                    raw.push(s as u8);
                }
            }
        }
        let z = zlib(&raw);
        if i == 0 {
            write_chunk(&mut out, b"IDAT", &z);
        } else {
            let mut d = seq.to_be_bytes().to_vec();
            d.extend_from_slice(&z);
            write_chunk(&mut out, b"fdAT", &d);
            seq += 1;
        }
    }
    write_chunk(&mut out, b"IEND", &[]);
    out
}

/// The §11.3.6.2 model over `nch`-sample pixels (last sample alpha) at
/// alpha scale `max`. Returns every frame's buffer as shown.
fn model(cw: usize, chh: usize, nch: usize, max: f64, frames: &[F]) -> Vec<Vec<u16>> {
    let mut buf = vec![0u16; cw * chh * nch];
    let mut shown = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let before = buf.clone();
        for yy in 0..f.h as usize {
            for xx in 0..f.w as usize {
                let s = &f.px[(yy * f.w as usize + xx) * nch..][..nch];
                let at = ((f.y as usize + yy) * cw + f.x as usize + xx) * nch;
                let d = &mut buf[at..at + nch];
                if f.blend == 0 {
                    d.copy_from_slice(s);
                    continue;
                }
                // The formula evaluated exactly (rationals over max²),
                // then rounded to nearest, half up.
                let m = max as u128;
                let (a_s, a_d) = (u128::from(s[nch - 1]), u128::from(d[nch - 1]));
                if a_s == 0 {
                    continue;
                }
                let a_o = a_s * m + a_d * (m - a_s); // αo · max²
                for c in 0..nch - 1 {
                    let num = u128::from(s[c]) * a_s * m + u128::from(d[c]) * a_d * (m - a_s);
                    d[c] = round_div(num, a_o) as u16;
                }
                d[nch - 1] = round_div(a_o, m) as u16;
            }
        }
        shown.push(buf.clone());
        let dispose = if i == 0 && f.dispose == 2 {
            1
        } else {
            f.dispose
        };
        match dispose {
            1 => {
                for yy in 0..f.h as usize {
                    let at = ((f.y as usize + yy) * cw + f.x as usize) * nch;
                    buf[at..at + f.w as usize * nch].fill(0);
                }
            }
            2 => buf = before,
            _ => {}
        }
    }
    shown
}

/// `n / d` rounded to nearest, half up.
fn round_div(n: u128, d: u128) -> u128 {
    (2 * n + d) / (2 * d)
}

/// Same picture: equal samples, every fully transparent pixel alike.
fn same(a: &[u16], b: &[u16], nch: usize) -> bool {
    a.len() == b.len()
        && a.chunks(nch)
            .zip(b.chunks(nch))
            .all(|(p, q)| p == q || (p[nch - 1] == 0 && q[nch - 1] == 0))
}

/// The registry path: demux the file, decode every packet.
fn registry_frames(file: &[u8]) -> Vec<(usize, Vec<u8>)> {
    let mut ctx = RuntimeContext::new();
    oxideav_png::register(&mut ctx);
    let mut dm = ctx
        .containers
        .open_demuxer("png", Box::new(Cursor::new(file.to_vec())), &ctx.codecs)
        .unwrap();
    let params = dm.streams()[0].params.clone();
    let mut dec = ctx.codecs.first_decoder(&params).unwrap();
    let mut out = Vec::new();
    while let Ok(p) = dm.next_packet() {
        dec.send_packet(&p).unwrap();
        let Frame::Video(vf) = dec.receive_frame().unwrap() else {
            panic!("video frame expected");
        };
        let plane = &vf.image_planes()[0];
        out.push((plane.stride, plane.data.clone()));
    }
    out
}

fn samples(bytes: &[u8], sixteen: bool) -> Vec<u16> {
    if sixteen {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| u16::from_le_bytes(b))
            .collect()
    } else {
        bytes.iter().map(|&b| u16::from(b)).collect()
    }
}

/// Both decode paths against the model, and against each other byte
/// for byte.
fn check(file: &[u8], cw: usize, chh: usize, nch: usize, sixteen: bool, frames: &[F]) {
    let max = if sixteen { 65535.0 } else { 255.0 };
    let want = model(cw, chh, nch, max, frames);
    let std = decode_apng(file).unwrap();
    let reg = registry_frames(file);
    assert_eq!(std.frames.len(), frames.len());
    assert_eq!(reg.len(), frames.len());
    for (i, w) in want.iter().enumerate() {
        let s = std.frames[i].image.as_bytes().unwrap();
        let got = samples(s, sixteen);
        if let Some(p) =
            (0..w.len() / nch).find(|&p| !same(&got[p * nch..][..nch], &w[p * nch..][..nch], nch))
        {
            eprintln!(
                "px {p}: got {:?} want {:?}",
                &got[p * nch..][..nch],
                &w[p * nch..][..nch]
            );
        }
        assert!(
            same(&got, w, nch),
            "frame {i}: standalone decode differs from the §11.3.6.2 model"
        );
        let (stride, r) = &reg[i];
        assert_eq!(*stride, cw * nch * if sixteen { 2 } else { 1 });
        assert_eq!(
            r.as_slice(),
            s,
            "frame {i}: registry decode differs from the standalone decode"
        );
    }
}

/// xorshift64*, so the streams are reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random frames: regions anywhere on the canvas, every dispose / blend
/// op, alpha mostly 0 / max with translucent values mixed in.
fn random_frames(rng: &mut Rng, cw: u32, chh: u32, nch: usize, max: u16, n: usize) -> Vec<F> {
    (0..n)
        .map(|i| {
            let (w, h, x, y) = if i == 0 {
                (cw, chh, 0, 0)
            } else {
                let w = 1 + rng.below(u64::from(cw)) as u32;
                let h = 1 + rng.below(u64::from(chh)) as u32;
                let x = rng.below(u64::from(cw - w + 1)) as u32;
                let y = rng.below(u64::from(chh - h + 1)) as u32;
                (w, h, x, y)
            };
            let px = (0..(w * h) as usize * nch)
                .map(|k| {
                    if k % nch == nch - 1 {
                        match rng.below(3) {
                            0 => 0,
                            1 => max,
                            _ => rng.below(u64::from(max) + 1) as u16,
                        }
                    } else {
                        rng.below(u64::from(max) + 1) as u16
                    }
                })
                .collect();
            F {
                x,
                y,
                w,
                h,
                dispose: rng.below(3) as u8,
                blend: rng.below(2) as u8,
                px,
            }
        })
        .collect()
}

#[test]
fn random_streams_match_the_model_rgba8_and_gray_alpha8() {
    for (ct, nch) in [(6u8, 4usize), (4, 2)] {
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let frames = random_frames(&mut rng, 24, 16, nch, 255, 8);
            let file = apng(24, 16, ct, 8, &[], &frames);
            check(&file, 24, 16, nch, false, &frames);
        }
    }
}

#[test]
fn random_streams_match_the_model_rgba16() {
    for seed in 1..=20u64 {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
        let frames = random_frames(&mut rng, 12, 10, 4, 65535, 6);
        let file = apng(12, 10, 6, 16, &[], &frames);
        check(&file, 12, 10, 4, true, &frames);
    }
}

/// The gateway report: a square moving over transparency, `SOURCE`
/// frames with `BACKGROUND` disposal — the layout ImageMagick 7.1.2
/// writes for `magick -size 64x64 … -dispose Background … apng:out.png`
/// (fixture: that command's output, regions 64×64+0+0, 48×36+16+0,
/// 16×16+32+16, 16×16+48+32). `ffmpeg -f framemd5 -pix_fmt rgba` of the
/// fixture: 48a2f6dff343fd8f17321f8aefc5663e,
/// b7a5f8350648f7f4aa5f75fa5e64d289, 5ed2ac88a7ac47a622ed8b812416092f,
/// 80fb35c26517350e0346cc96de4ae6f6 — the pictures both paths now give.
/// Before, the registry decoder returned each frame at its own region
/// size (48×36, 16×16, 16×16), which the gateway drew at +0+0 over the
/// old canvas.
#[test]
fn magick_moving_square_renders_whole_canvas_frames() {
    let file = include_bytes!("fixtures/magick_square_dispose_background.png");
    let std = decode_apng(file).unwrap();
    let reg = registry_frames(file);
    assert_eq!(reg.len(), 4);
    // The red 16×16 square of each frame (one colour); everything else
    // is transparent: SOURCE wrote the frame's transparent pixels over
    // the previous square and BACKGROUND cleared the region after it.
    for (k, (stride, px)) in reg.iter().enumerate() {
        assert_eq!(*stride, 64 * 4, "frame {k} spans the canvas");
        assert_eq!(px.len(), 64 * 64 * 4);
        assert_eq!(px.as_slice(), std.frames[k].image.as_bytes().unwrap());
        for y in 0..64 {
            for x in 0..64 {
                let p = &px[(y * 64 + x) * 4..][..4];
                let (sx, sy) = [(0, 0), (16, 8), (32, 16), (48, 32)][k];
                let inside = (sx..sx + 16).contains(&x) && (sy..sy + 16).contains(&y);
                if inside {
                    assert_eq!(p, &px[(sy * 64 + sx) * 4..][..4], "frame {k}");
                    assert_eq!((p[0], p[3]), (255, 255), "frame {k} ({x},{y}) red");
                } else {
                    assert_eq!(p[3], 0, "frame {k} ({x},{y}) left over");
                }
            }
        }
    }
}

/// OVER onto a translucent buffer pixel: the buffer's own alpha takes
/// part (black-box: ffmpeg renders the same stream to (170,0,84,192)
/// and (255,0,0,128); exact rounding gives blue 84.78 → 85). The old
/// formula, Cs·αs + Cd·(1 − αs), ignored the buffer alpha and gave
/// (128,0,127,192) and a darkened (128,0,0,128) over transparency.
#[test]
fn over_onto_translucent_buffer_uses_its_alpha() {
    let frames = [
        F {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            dispose: 0,
            blend: 0,
            px: vec![0, 0, 255, 128, 0, 0, 0, 0],
        },
        F {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            dispose: 0,
            blend: 1,
            px: vec![255, 0, 0, 128, 255, 0, 0, 128],
        },
    ];
    let file = apng(2, 1, 6, 8, &[], &frames);
    let img = decode_apng(&file).unwrap();
    assert_eq!(
        img.frames[1].image.as_bytes().unwrap(),
        [170, 0, 85, 192, 255, 0, 0, 128]
    );
    check(&file, 2, 1, 4, false, &frames);
}

/// An alpha-less canvas clears to its tRNS key: BACKGROUND (and the
/// initial buffer) is "fully transparent black", which a palette canvas
/// spells as its first alpha-0 entry, not index 0. Black-box: ffmpeg
/// renders this stream's frame 2 as transparent where the blue square
/// was disposed.
#[test]
fn background_dispose_on_palette_canvas_writes_the_transparent_index() {
    let plte = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9];
    let trns = vec![255, 255, 255, 0];
    let frames = [
        F {
            x: 0,
            y: 0,
            w: 4,
            h: 2,
            dispose: 0,
            blend: 0,
            px: vec![1; 8],
        },
        F {
            x: 1,
            y: 0,
            w: 2,
            h: 2,
            dispose: 1,
            blend: 0,
            px: vec![2; 4],
        },
        F {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
            dispose: 0,
            blend: 1,
            px: vec![0],
        },
    ];
    let file = apng(4, 2, 3, 8, &[(b"PLTE", plte), (b"tRNS", trns)], &frames);
    let std = decode_apng(&file).unwrap();
    let got: Vec<&[u8]> = std
        .frames
        .iter()
        .map(|f| f.image.as_bytes().unwrap())
        .collect();
    assert_eq!(got[0], [1, 1, 1, 1, 1, 1, 1, 1]);
    assert_eq!(got[1], [1, 2, 2, 1, 1, 2, 2, 1]);
    assert_eq!(got[2], [0, 3, 3, 1, 1, 3, 3, 1]);
    let reg = registry_frames(&file);
    for (i, (_, px)) in reg.iter().enumerate() {
        assert_eq!(px.as_slice(), got[i], "frame {i}");
    }
}

/// A keyed-RGB canvas clears to the tRNS colour.
#[test]
fn background_dispose_on_keyed_rgb_canvas_writes_the_key() {
    let trns = vec![0, 7, 0, 8, 0, 9];
    let frames = [
        F {
            x: 0,
            y: 0,
            w: 2,
            h: 1,
            dispose: 1,
            blend: 0,
            px: vec![200, 100, 50, 1, 2, 3],
        },
        F {
            x: 1,
            y: 0,
            w: 1,
            h: 1,
            dispose: 0,
            blend: 1,
            px: vec![10, 20, 30],
        },
    ];
    let file = apng(2, 1, 2, 8, &[(b"tRNS", trns)], &frames);
    let std = decode_apng(&file).unwrap();
    assert_eq!(
        std.frames[1].image.as_bytes().unwrap(),
        [7, 8, 9, 10, 20, 30]
    );
}

/// PREVIOUS restores the region exactly, including after an OVER, and
/// on the first frame acts as BACKGROUND.
#[test]
fn previous_dispose_restores_and_first_frame_previous_clears() {
    let px = |v: [u16; 4], n: usize| v.repeat(n);
    let frames = [
        F {
            x: 0,
            y: 0,
            w: 3,
            h: 1,
            dispose: 2,
            blend: 0,
            px: px([10, 20, 30, 255], 3),
        },
        F {
            x: 0,
            y: 0,
            w: 3,
            h: 1,
            dispose: 0,
            blend: 0,
            px: px([1, 2, 3, 200], 3),
        },
        F {
            x: 1,
            y: 0,
            w: 2,
            h: 1,
            dispose: 2,
            blend: 1,
            px: px([255, 255, 255, 100], 2),
        },
        F {
            x: 2,
            y: 0,
            w: 1,
            h: 1,
            dispose: 0,
            blend: 1,
            px: px([0, 0, 0, 0], 1),
        },
    ];
    let file = apng(3, 1, 6, 8, &[], &frames);
    let std = decode_apng(&file).unwrap();
    // Frame 1 drew over a cleared buffer (frame 0's PREVIOUS = BACKGROUND).
    assert_eq!(
        std.frames[1].image.as_bytes().unwrap(),
        px([1, 2, 3, 200], 3)
            .iter()
            .map(|&v| v as u8)
            .collect::<Vec<_>>()
    );
    // Frame 3 sees frame 2's region reverted to frame 1's pixels.
    assert_eq!(
        std.frames[3].image.as_bytes().unwrap(),
        std.frames[1].image.as_bytes().unwrap()
    );
    check(&file, 3, 1, 4, false, &frames);
}
