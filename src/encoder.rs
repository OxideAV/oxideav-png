//! PNG + APNG encoder.
//!
//! The encoder accepts a single video frame per `send_frame` and emits a
//! full, standalone PNG file on the first `receive_packet`. If multiple
//! frames are submitted (`frame_rate` set, or multiple `send_frame` calls
//! before the first drain), the trailing frames are buffered and an APNG
//! is produced on `flush` — the single output packet then contains the
//! whole animation.
//!
//! Compression level is fixed at miniz_oxide default (6). All rows use the
//! PNG §12.8 "minimum sum of absolute differences" heuristic (i.e. try all
//! 5 filters, pick the one with the smallest absolute byte sum).

use std::collections::VecDeque;

use oxideav_core::Encoder;
use oxideav_core::{
    parse_options, CodecId, CodecOptionsStruct, CodecParameters, Error, Frame, MediaType,
    OptionField, OptionKind, OptionValue, Packet, PixelFormat, Rational, Result, TimeBase,
    VideoFrame,
};

use miniz_oxide::deflate::compress_to_vec_zlib;

use crate::apng::{build_fdat, Actl, Blend, Disposal, Fctl};
use crate::chunk::{write_chunk, PNG_MAGIC};
use crate::decoder::{adam7_pass_dims, Ihdr, ADAM7};
use crate::filter::{choose_filter_heuristic, filter_row};

/// PNG encoder tuning knobs, attached via
/// [`CodecParameters::options`](oxideav_core::CodecParameters::options)
/// or passed directly to [`encode_single_with_options`].
///
/// Recognised keys (see [`CodecOptionsStruct::SCHEMA`]):
/// - `interlace` *(bool, default `false`)* — Adam7 seven-pass
///   interlaced encode. Sets `IHDR.interlace = 1`. Compressed payload
///   gets ~5–15% larger but the image is progressively renderable.
#[derive(Debug, Clone, Default)]
pub struct PngEncoderOptions {
    pub interlace: bool,
}

impl CodecOptionsStruct for PngEncoderOptions {
    const SCHEMA: &'static [OptionField] = &[OptionField {
        name: "interlace",
        kind: OptionKind::Bool,
        default: OptionValue::Bool(false),
        help: "Emit an Adam7 seven-pass interlaced PNG stream (IHDR.interlace = 1)",
    }];
    fn apply(&mut self, key: &str, v: &OptionValue) -> Result<()> {
        match key {
            "interlace" => self.interlace = v.as_bool()?,
            _ => unreachable!("guarded by SCHEMA"),
        }
        Ok(())
    }
}

pub fn make_encoder(params: &CodecParameters) -> Result<Box<dyn Encoder>> {
    let opts = parse_options::<PngEncoderOptions>(&params.options)?;
    let width = params
        .width
        .ok_or_else(|| Error::invalid("PNG encoder: missing width"))?;
    let height = params
        .height
        .ok_or_else(|| Error::invalid("PNG encoder: missing height"))?;
    let pix = params.pixel_format.unwrap_or(PixelFormat::Rgba);
    // Allowed pixel formats.
    match pix {
        PixelFormat::Rgba
        | PixelFormat::Rgb24
        | PixelFormat::Gray8
        | PixelFormat::Pal8
        | PixelFormat::Rgb48Le
        | PixelFormat::Rgba64Le
        | PixelFormat::Gray16Le
        | PixelFormat::Ya8 => {}
        other => {
            return Err(Error::unsupported(format!(
                "PNG encoder: pixel format {other:?} not supported"
            )))
        }
    }

    let mut output_params = params.clone();
    output_params.media_type = MediaType::Video;
    output_params.codec_id = CodecId::new(crate::CODEC_ID_STR);
    output_params.width = Some(width);
    output_params.height = Some(height);
    output_params.pixel_format = Some(pix);

    // APNG's on-wire delay is num/den seconds, so everything the encoder
    // emits is expressed in centiseconds regardless of the caller's
    // frame_rate — converting happens in the fcTL delay_num/delay_den fields.
    let time_base = TimeBase::new(1, 100);

    let animated_hint = params.frame_rate.is_some();

    Ok(Box::new(PngEncoder {
        output_params,
        width,
        height,
        pix,
        time_base,
        frames: Vec::new(),
        pending_out: VecDeque::new(),
        frame_rate: params.frame_rate,
        palette: params.extradata.clone(),
        animated_hint,
        eof: false,
        opts,
    }))
}

struct PngEncoder {
    output_params: CodecParameters,
    width: u32,
    height: u32,
    pix: PixelFormat,
    time_base: TimeBase,
    frames: Vec<VideoFrame>,
    pending_out: VecDeque<Packet>,
    frame_rate: Option<Rational>,
    /// Raw palette + optional trns carried on `extradata`. Only used when
    /// encoding Pal8: layout is `PLTE_bytes || tRNS_bytes` per the container.
    palette: Vec<u8>,
    animated_hint: bool,
    eof: bool,
    opts: PngEncoderOptions,
}

impl Encoder for PngEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.output_params.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }

    fn send_frame(&mut self, frame: &Frame) -> Result<()> {
        match frame {
            Frame::Video(v) => {
                // Stream-level format / dimensions live on the
                // CodecParameters (set at make_encoder); the frame just
                // carries pts + planes. Trust the caller to feed us
                // matching geometry.
                self.frames.push(v.clone());
                // Non-animated shortcut: if we only ever get one frame and
                // there's no animation hint, we emit eagerly on flush. Keep
                // buffered.
                Ok(())
            }
            _ => Err(Error::invalid("PNG encoder: video frames only")),
        }
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        if !self.pending_out.is_empty() {
            return Ok(self.pending_out.pop_front().unwrap());
        }
        if self.eof {
            // Produce output now if we haven't already.
            if !self.frames.is_empty() {
                self.finalize()?;
                if let Some(p) = self.pending_out.pop_front() {
                    return Ok(p);
                }
            }
            return Err(Error::Eof);
        }
        Err(Error::NeedMore)
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        // Build the packet up front so receive_packet can pick it up.
        if !self.frames.is_empty() && self.pending_out.is_empty() {
            self.finalize()?;
        }
        Ok(())
    }
}

impl PngEncoder {
    fn finalize(&mut self) -> Result<()> {
        let is_animated = self.frames.len() > 1 || self.animated_hint;
        let bytes = if is_animated {
            encode_apng(self)?
        } else {
            encode_single_with_options(
                &self.frames[0],
                self.width,
                self.height,
                self.pix,
                &self.palette,
                &self.opts,
            )?
        };
        let mut pkt = Packet::new(0, self.time_base, bytes);
        pkt.pts = self.frames[0].pts;
        pkt.dts = pkt.pts;
        pkt.flags.keyframe = true;
        self.pending_out.push_back(pkt);
        self.frames.clear();
        Ok(())
    }
}

// ---- Single-image encode -----------------------------------------------

/// Encode one [`VideoFrame`] as a standalone PNG using default options
/// (non-interlaced). Thin wrapper around
/// [`encode_single_with_options`] preserved for existing callers.
/// `width` and `height` come from the caller's `CodecParameters`.
pub fn encode_single(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    pix: PixelFormat,
    palette: &[u8],
) -> Result<Vec<u8>> {
    encode_single_with_options(
        frame,
        width,
        height,
        pix,
        palette,
        &PngEncoderOptions::default(),
    )
}

/// Encode one [`VideoFrame`] as a standalone PNG, honouring the
/// supplied options (e.g. `interlace: true` for Adam7).
pub fn encode_single_with_options(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    pix: PixelFormat,
    palette: &[u8],
    opts: &PngEncoderOptions,
) -> Result<Vec<u8>> {
    let (mut ihdr, row_bytes, plte_bytes, trns_bytes) =
        ihdr_and_row_bytes(frame, width, height, pix, palette)?;
    if opts.interlace {
        ihdr.interlace = 1;
    }
    let raw_pixels = flatten_and_normalise_pixels(frame, width, height, pix, row_bytes)?;
    let idat = if opts.interlace {
        deflate_encode_pixels_adam7(&raw_pixels, width as usize, height as usize, &ihdr)?
    } else {
        deflate_encode_pixels(&raw_pixels, row_bytes, height as usize, &ihdr)?
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

/// Given the encoder configuration + first frame, produce an IHDR + row byte
/// count + optional PLTE / tRNS chunk payloads.
fn ihdr_and_row_bytes(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    pix: PixelFormat,
    palette: &[u8],
) -> Result<IhdrAndRowInfo> {
    let w = width;
    let (bit_depth, colour_type, channels): (u8, u8, usize) = match pix {
        PixelFormat::Gray8 => (8, 0, 1),
        PixelFormat::Gray16Le => (16, 0, 1),
        PixelFormat::Rgb24 => (8, 2, 3),
        PixelFormat::Rgb48Le => (16, 2, 3),
        PixelFormat::Pal8 => (8, 3, 1),
        PixelFormat::Ya8 => (8, 4, 2),
        PixelFormat::Rgba => (8, 6, 4),
        PixelFormat::Rgba64Le => (16, 6, 4),
        other => {
            return Err(Error::unsupported(format!(
                "PNG encoder: unsupported pixel format {other:?}"
            )))
        }
    };
    let row_bytes = channels * (bit_depth as usize / 8) * w as usize;
    let ihdr = Ihdr {
        width: w,
        height,
        bit_depth,
        colour_type,
        compression: 0,
        filter: 0,
        interlace: 0,
    };

    // Split palette bytes into PLTE + tRNS. Convention: if len is a multiple
    // of 3, the whole thing is PLTE. Otherwise, interpret as PLTE (first
    // ceil(len/3)*3 bytes) + tRNS (trailing bytes up to num_entries).
    let (plte, trns) = if colour_type == 3 {
        if palette.is_empty() {
            // Default: 1-entry black palette — useful fallback, but the test
            // harness will usually supply one.
            (Some(vec![0u8, 0, 0]), None)
        } else {
            // Encoder convention: caller passes `palette = PLTE || tRNS`.
            // We need to know where PLTE ends. Assume `palette` was packed
            // as `3*N RGB triples followed by M alpha bytes (M<=N)`. We
            // derive N from the plane data's max index + 1, but to keep
            // the interface simple we assume the whole `palette` is PLTE
            // iff its length is a multiple of 3. Otherwise the remainder
            // (palette.len() % 3 != 0) is interpreted as having trailing
            // tRNS bytes — but a cleaner, fully-unambiguous layout is: the
            // first N*3 bytes are PLTE and the rest are tRNS. Implement
            // that by scanning the frame for max index.
            let max_idx = frame
                .planes
                .first()
                .map(|p| p.data.iter().copied().max().unwrap_or(0))
                .unwrap_or(0) as usize;
            let n = max_idx + 1;
            let plte_len = (n * 3).min(palette.len());
            let trns_len = palette.len().saturating_sub(plte_len);
            let plte = palette[..plte_len].to_vec();
            let trns = if trns_len > 0 {
                Some(palette[plte_len..plte_len + trns_len].to_vec())
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

/// Pack `frame` into a flat BE-oriented row-major byte buffer that matches
/// the PNG wire format (before filtering / DEFLATE). `row_bytes` is the
/// expected byte count per row.
fn flatten_and_normalise_pixels(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    pix: PixelFormat,
    row_bytes: usize,
) -> Result<Vec<u8>> {
    let h = height as usize;
    let w = width as usize;
    let src = &frame.planes[0];
    let mut out = vec![0u8; row_bytes * h];

    match pix {
        PixelFormat::Gray8
        | PixelFormat::Rgb24
        | PixelFormat::Rgba
        | PixelFormat::Pal8
        | PixelFormat::Ya8 => {
            // Row-by-row copy; honour source stride.
            for y in 0..h {
                let sstart = y * src.stride;
                let dstart = y * row_bytes;
                out[dstart..dstart + row_bytes]
                    .copy_from_slice(&src.data[sstart..sstart + row_bytes]);
            }
        }
        PixelFormat::Gray16Le => {
            // Source is LE per sample; PNG needs BE.
            for y in 0..h {
                for x in 0..w {
                    let lo = src.data[y * src.stride + x * 2];
                    let hi = src.data[y * src.stride + x * 2 + 1];
                    out[y * row_bytes + x * 2] = hi;
                    out[y * row_bytes + x * 2 + 1] = lo;
                }
            }
        }
        PixelFormat::Rgb48Le => {
            for y in 0..h {
                for i in 0..(w * 3) {
                    let lo = src.data[y * src.stride + i * 2];
                    let hi = src.data[y * src.stride + i * 2 + 1];
                    out[y * row_bytes + i * 2] = hi;
                    out[y * row_bytes + i * 2 + 1] = lo;
                }
            }
        }
        PixelFormat::Rgba64Le => {
            for y in 0..h {
                for i in 0..(w * 4) {
                    let lo = src.data[y * src.stride + i * 2];
                    let hi = src.data[y * src.stride + i * 2 + 1];
                    out[y * row_bytes + i * 2] = hi;
                    out[y * row_bytes + i * 2 + 1] = lo;
                }
            }
        }
        other => {
            return Err(Error::unsupported(format!(
                "PNG encoder: flatten unsupported for {other:?}"
            )))
        }
    }
    Ok(out)
}

/// Filter each row (per the PNG spec's sum-of-abs heuristic), prepend the
/// filter-type byte, then zlib compress. Returns the compressed IDAT bytes.
fn deflate_encode_pixels(
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
///
/// Invariants shared with the non-interlaced path:
/// - `raw` is row-major, tightly packed at the full image's `row_bytes`
///   per row (same buffer produced by [`flatten_and_normalise_pixels`]).
/// - The encoder never writes sub-8-bit depths, so `bytes_per_pixel`
///   is always 1, 2, 3, 4, 6, or 8 — no bit-packing required.
///
/// The decoder's [`decode_adam7`](crate::decoder) inverts this: it
/// splits the decompressed stream back into seven per-pass sub-images,
/// reconstructs each via the standard filter loop, and scatters each
/// pixel to `(sr + py*rs, sc + px*cs)` on the output canvas.
fn deflate_encode_pixels_adam7(
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

fn encode_apng(enc: &PngEncoder) -> Result<Vec<u8>> {
    if enc.frames.is_empty() {
        return Err(Error::invalid("PNG encoder: no frames for APNG"));
    }
    let pix = enc.pix;
    let (mut ihdr, row_bytes, plte, trns) =
        ihdr_and_row_bytes(&enc.frames[0], enc.width, enc.height, pix, &enc.palette)?;
    if enc.opts.interlace {
        ihdr.interlace = 1;
    }

    let num_plays: u32 = 0; // loop forever by default
    let actl = Actl {
        num_frames: enc.frames.len() as u32,
        num_plays,
    };

    // Default delay per frame: derived from frame_rate or 10cs = 10Hz.
    let default_delay: (u16, u16) = match enc.frame_rate {
        Some(r) if r.num > 0 && r.den > 0 => (r.den as u16, r.num as u16),
        _ => (10, 100),
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
    for (idx, frame) in enc.frames.iter().enumerate() {
        let fctl = Fctl {
            sequence_number: seq,
            width: ihdr.width,
            height: ihdr.height,
            x_offset: 0,
            y_offset: 0,
            delay_num: default_delay.0,
            delay_den: default_delay.1,
            dispose_op: Disposal::None,
            blend_op: Blend::Source,
        };
        write_chunk(&mut out, b"fcTL", &fctl.to_bytes());
        seq += 1;

        let raw = flatten_and_normalise_pixels(frame, enc.width, enc.height, pix, row_bytes)?;
        let compressed = if enc.opts.interlace {
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
