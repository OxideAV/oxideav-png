//! PNG + APNG encoder.
//!
//! The standalone API ([`encode_png_image`] /
//! [`encode_png_image_with_options`]) takes a single [`PngImage`] and
//! emits a full standalone PNG file. The [`crate::registry`]-gated
//! [`Encoder`](oxideav_core::Encoder) trait impl wraps these
//! free-standing functions: it accepts a single video frame per
//! `send_frame` and emits a full PNG on the first `receive_packet`. If
//! multiple frames are submitted (`frame_rate` set, or multiple
//! `send_frame` calls before the first drain), the trailing frames are
//! buffered and an APNG is produced on `flush`.
//!
//! Compression level is fixed at miniz_oxide default (6). All rows use the
//! PNG §12.8 "minimum sum of absolute differences" heuristic (i.e. try all
//! 5 filters, pick the one with the smallest absolute byte sum).

use crate::error::{PngError as Error, Result};
use crate::image::{PngImage, PngPixelFormat};

// Backward-compat re-export: existing callers reach for
// `oxideav_png::encoder::make_encoder` to construct a framework-side
// encoder. Keep that path live by re-exporting the registry-side
// factory.
#[cfg(feature = "registry")]
pub use crate::registry::make_encoder;

use miniz_oxide::deflate::compress_to_vec_zlib;

use crate::chunk::{write_chunk, PNG_MAGIC};
use crate::decoder::{adam7_pass_dims, Ihdr, ADAM7};
use crate::filter::{choose_filter_heuristic, filter_row};

/// PNG encoder tuning knobs, attached via
/// `CodecParameters::options` (when the `registry` feature is on) or
/// passed directly to [`encode_png_image_with_options`].
#[derive(Debug, Clone, Default)]
pub struct PngEncoderOptions {
    /// Adam7 seven-pass interlaced encode. Sets `IHDR.interlace = 1`.
    /// Compressed payload gets ~5–15% larger but the image is
    /// progressively renderable.
    pub interlace: bool,
}

// ---- Single-image encode -----------------------------------------------

/// Encode one [`PngImage`] as a standalone PNG using default options
/// (non-interlaced). Standalone (no `oxideav-core`) entry point.
pub fn encode_png_image(image: &PngImage) -> Result<Vec<u8>> {
    encode_png_image_with_options(image, &PngEncoderOptions::default())
}

/// Encode one [`PngImage`] as a standalone PNG, honouring the supplied
/// options (e.g. `interlace: true` for Adam7). Standalone (no
/// `oxideav-core`) entry point.
pub fn encode_png_image_with_options(
    image: &PngImage,
    opts: &PngEncoderOptions,
) -> Result<Vec<u8>> {
    let (mut ihdr, row_bytes, plte_bytes, trns_bytes) = ihdr_and_row_bytes(image)?;
    if opts.interlace {
        ihdr.interlace = 1;
    }
    let raw_pixels = flatten_and_normalise_pixels(image, row_bytes)?;
    let idat = if opts.interlace {
        deflate_encode_pixels_adam7(
            &raw_pixels,
            image.width as usize,
            image.height as usize,
            &ihdr,
        )?
    } else {
        deflate_encode_pixels(&raw_pixels, row_bytes, image.height as usize, &ihdr)?
    };

    let mut out = Vec::with_capacity(64 + idat.len());
    out.extend_from_slice(&PNG_MAGIC);
    write_chunk(&mut out, b"IHDR", &ihdr.to_bytes());
    if let Some(p) = plte_bytes.as_deref() {
        write_chunk(&mut out, b"PLTE", p);
    }
    if let Some(t) = trns_bytes.as_deref() {
        write_chunk(&mut out, b"tRNS", t);
    }
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// IHDR + row byte count + optional PLTE / tRNS chunk payloads.
type IhdrAndRowInfo = (Ihdr, usize, Option<Vec<u8>>, Option<Vec<u8>>);

/// Given a [`PngImage`], produce an IHDR + row byte count + optional
/// PLTE / tRNS chunk payloads.
fn ihdr_and_row_bytes(image: &PngImage) -> Result<IhdrAndRowInfo> {
    let (bit_depth, colour_type, channels): (u8, u8, usize) = match image.pixel_format {
        PngPixelFormat::Gray8 => (8, 0, 1),
        PngPixelFormat::Gray16Le => (16, 0, 1),
        PngPixelFormat::Rgb24 => (8, 2, 3),
        PngPixelFormat::Rgb48Le => (16, 2, 3),
        PngPixelFormat::Pal8 => (8, 3, 1),
        PngPixelFormat::Ya8 => (8, 4, 2),
        PngPixelFormat::Rgba => (8, 6, 4),
        PngPixelFormat::Rgba64Le => (16, 6, 4),
    };
    let row_bytes = channels * (bit_depth as usize / 8) * image.width as usize;
    let ihdr = Ihdr {
        width: image.width,
        height: image.height,
        bit_depth,
        colour_type,
        compression: 0,
        filter: 0,
        interlace: 0,
    };

    // Split palette bytes into PLTE + tRNS. Caller convention: `palette`
    // is `PLTE || tRNS` as one buffer. We derive PLTE entry count from
    // the frame's max-index + 1.
    let (plte, trns) = if colour_type == 3 {
        if image.palette.is_empty() {
            // Default: 1-entry black palette — useful fallback, but the test
            // harness will usually supply one.
            (Some(vec![0u8, 0, 0]), None)
        } else {
            let max_idx = image.data.iter().copied().max().unwrap_or(0) as usize;
            let n = max_idx + 1;
            let plte_len = (n * 3).min(image.palette.len());
            let trns_len = image.palette.len().saturating_sub(plte_len);
            let plte = image.palette[..plte_len].to_vec();
            let trns = if trns_len > 0 {
                Some(image.palette[plte_len..plte_len + trns_len].to_vec())
            } else {
                None
            };
            (Some(plte), trns)
        }
    } else {
        (None, None)
    };

    Ok((ihdr, row_bytes, plte, trns))
}

/// Pack `image` into a flat BE-oriented row-major byte buffer that matches
/// the PNG wire format (before filtering / DEFLATE). `row_bytes` is the
/// expected byte count per row.
fn flatten_and_normalise_pixels(image: &PngImage, row_bytes: usize) -> Result<Vec<u8>> {
    let h = image.height as usize;
    let w = image.width as usize;
    let stride = image.stride;
    let mut out = vec![0u8; row_bytes * h];

    match image.pixel_format {
        PngPixelFormat::Gray8
        | PngPixelFormat::Rgb24
        | PngPixelFormat::Rgba
        | PngPixelFormat::Pal8
        | PngPixelFormat::Ya8 => {
            // Row-by-row copy; honour source stride.
            for y in 0..h {
                let sstart = y * stride;
                let dstart = y * row_bytes;
                out[dstart..dstart + row_bytes]
                    .copy_from_slice(&image.data[sstart..sstart + row_bytes]);
            }
        }
        PngPixelFormat::Gray16Le => {
            // Source is LE per sample; PNG needs BE.
            for y in 0..h {
                for x in 0..w {
                    let lo = image.data[y * stride + x * 2];
                    let hi = image.data[y * stride + x * 2 + 1];
                    out[y * row_bytes + x * 2] = hi;
                    out[y * row_bytes + x * 2 + 1] = lo;
                }
            }
        }
        PngPixelFormat::Rgb48Le => {
            for y in 0..h {
                for i in 0..(w * 3) {
                    let lo = image.data[y * stride + i * 2];
                    let hi = image.data[y * stride + i * 2 + 1];
                    out[y * row_bytes + i * 2] = hi;
                    out[y * row_bytes + i * 2 + 1] = lo;
                }
            }
        }
        PngPixelFormat::Rgba64Le => {
            for y in 0..h {
                for i in 0..(w * 4) {
                    let lo = image.data[y * stride + i * 2];
                    let hi = image.data[y * stride + i * 2 + 1];
                    out[y * row_bytes + i * 2] = hi;
                    out[y * row_bytes + i * 2 + 1] = lo;
                }
            }
        }
    }
    Ok(out)
}

/// Filter each row (per the PNG spec's sum-of-abs heuristic), prepend the
/// filter-type byte, then zlib compress. Returns the compressed IDAT bytes.
pub(crate) fn deflate_encode_pixels(
    raw: &[u8],
    row_bytes: usize,
    height: usize,
    ihdr: &Ihdr,
) -> Result<Vec<u8>> {
    let bpp = ihdr.bpp_for_filter()?;
    // 1 filter byte + row_bytes per row.
    let mut filtered = vec![0u8; (1 + row_bytes) * height];
    let mut scratch = vec![0u8; row_bytes];
    let zero_row = vec![0u8; row_bytes];
    for y in 0..height {
        let row = &raw[y * row_bytes..(y + 1) * row_bytes];
        let prev: &[u8] = if y == 0 {
            &zero_row
        } else {
            &raw[(y - 1) * row_bytes..y * row_bytes]
        };
        let ft = choose_filter_heuristic(row, prev, bpp, &mut scratch);
        let dst_off = y * (1 + row_bytes);
        filtered[dst_off] = ft as u8;
        let data_slot = &mut filtered[dst_off + 1..dst_off + 1 + row_bytes];
        // The heuristic's scratch buffer holds whichever filter it tried
        // last, not necessarily the winner — re-filter into the output slot.
        filter_row(ft, row, prev, bpp, data_slot);
    }
    Ok(compress_to_vec_zlib(&filtered, 6))
}

/// Adam7 counterpart to [`deflate_encode_pixels`]: gather each of the
/// seven passes into its own sub-image, filter its rows (per-pass
/// heuristic), and concatenate `(1 + pass_row_bytes) * pass_height`
/// filtered bytes from each pass. The full concatenation is zlib-
/// compressed into one IDAT/fdAT payload.
pub(crate) fn deflate_encode_pixels_adam7(
    raw: &[u8],
    width: usize,
    height: usize,
    ihdr: &Ihdr,
) -> Result<Vec<u8>> {
    let bpp = ihdr.bpp_for_filter()?;
    let bytes_per_pixel = ihdr.decoded_bytes_per_pixel()?;
    let full_row_bytes = width * bytes_per_pixel;

    let mut filtered_all = Vec::new();
    for (pass, &(sr, sc, rs, cs)) in ADAM7.iter().enumerate() {
        let (pw, ph) = adam7_pass_dims(width, height, pass);
        if pw == 0 || ph == 0 {
            continue;
        }

        // Gather the pass sub-image into a contiguous buffer at the
        // pass's own row_bytes (= pw * bytes_per_pixel, since we never
        // emit sub-byte encodes).
        let pass_row_bytes = pw * bytes_per_pixel;
        let mut pass_raw = vec![0u8; pass_row_bytes * ph];
        for py in 0..ph {
            let src_y = sr + py * rs;
            for px in 0..pw {
                let src_x = sc + px * cs;
                let src_off = src_y * full_row_bytes + src_x * bytes_per_pixel;
                let dst_off = py * pass_row_bytes + px * bytes_per_pixel;
                pass_raw[dst_off..dst_off + bytes_per_pixel]
                    .copy_from_slice(&raw[src_off..src_off + bytes_per_pixel]);
            }
        }

        // Filter this pass's rows independently, exactly like the
        // non-interlaced path.
        let prev_start = filtered_all.len();
        filtered_all.resize(prev_start + (1 + pass_row_bytes) * ph, 0);
        let mut scratch = vec![0u8; pass_row_bytes];
        let zero_row = vec![0u8; pass_row_bytes];
        for y in 0..ph {
            let row = &pass_raw[y * pass_row_bytes..(y + 1) * pass_row_bytes];
            let prev: &[u8] = if y == 0 {
                &zero_row
            } else {
                &pass_raw[(y - 1) * pass_row_bytes..y * pass_row_bytes]
            };
            let ft = choose_filter_heuristic(row, prev, bpp, &mut scratch);
            let dst_off = prev_start + y * (1 + pass_row_bytes);
            filtered_all[dst_off] = ft as u8;
            let data_slot = &mut filtered_all[dst_off + 1..dst_off + 1 + pass_row_bytes];
            filter_row(ft, row, prev, bpp, data_slot);
        }
    }
    Ok(compress_to_vec_zlib(&filtered_all, 6))
}

// ---- APNG encode --------------------------------------------------------

/// Build an APNG file from the supplied sequence of [`PngImage`]s.
/// Standalone (no `oxideav-core`) entry point. `delay_centiseconds` is
/// applied uniformly to every frame.
///
/// All frames must share the same `width`, `height`, and `pixel_format`
/// — APNG's IHDR is fixed across the whole file. The first frame's
/// palette (for `Pal8`) is used for the file's PLTE / tRNS chunks.
pub fn encode_apng(
    frames: &[PngImage],
    delay_centiseconds: u16,
    num_plays: u32,
) -> Result<Vec<u8>> {
    encode_apng_with_options(
        frames,
        delay_centiseconds,
        num_plays,
        &PngEncoderOptions::default(),
    )
}

/// Same as [`encode_apng`] but honours [`PngEncoderOptions`]
/// (e.g. `interlace: true` for Adam7).
pub fn encode_apng_with_options(
    frames: &[PngImage],
    delay_centiseconds: u16,
    num_plays: u32,
    opts: &PngEncoderOptions,
) -> Result<Vec<u8>> {
    use crate::apng::{build_fdat, Actl, Blend, Disposal, Fctl};

    if frames.is_empty() {
        return Err(Error::invalid("PNG encoder: no frames for APNG"));
    }
    let pix = frames[0].pixel_format;
    let w = frames[0].width;
    let h = frames[0].height;
    for f in &frames[1..] {
        if f.width != w || f.height != h || f.pixel_format != pix {
            return Err(Error::invalid(
                "PNG encoder: APNG frames must share width / height / pixel_format",
            ));
        }
    }
    let (mut ihdr, row_bytes, plte, trns) = ihdr_and_row_bytes(&frames[0])?;
    if opts.interlace {
        ihdr.interlace = 1;
    }

    let actl = Actl {
        num_frames: frames.len() as u32,
        num_plays,
    };

    let mut out = Vec::new();
    out.extend_from_slice(&PNG_MAGIC);
    write_chunk(&mut out, b"IHDR", &ihdr.to_bytes());
    write_chunk(&mut out, b"acTL", &actl.to_bytes());
    if let Some(p) = plte.as_deref() {
        write_chunk(&mut out, b"PLTE", p);
    }
    if let Some(t) = trns.as_deref() {
        write_chunk(&mut out, b"tRNS", t);
    }

    let mut seq: u32 = 0;
    for (idx, frame) in frames.iter().enumerate() {
        let fctl = Fctl {
            sequence_number: seq,
            width: ihdr.width,
            height: ihdr.height,
            x_offset: 0,
            y_offset: 0,
            delay_num: delay_centiseconds,
            delay_den: 100,
            dispose_op: Disposal::None,
            blend_op: Blend::Source,
        };
        write_chunk(&mut out, b"fcTL", &fctl.to_bytes());
        seq += 1;

        let raw = flatten_and_normalise_pixels(frame, row_bytes)?;
        let compressed = if opts.interlace {
            deflate_encode_pixels_adam7(&raw, ihdr.width as usize, ihdr.height as usize, &ihdr)?
        } else {
            deflate_encode_pixels(&raw, row_bytes, ihdr.height as usize, &ihdr)?
        };

        if idx == 0 {
            // First frame is the default image → IDAT.
            write_chunk(&mut out, b"IDAT", &compressed);
        } else {
            let payload = build_fdat(seq, &compressed);
            write_chunk(&mut out, b"fdAT", &payload);
            seq += 1;
        }
    }

    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}
