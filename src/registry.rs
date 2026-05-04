//! `oxideav-core` integration layer for `oxideav-png`.
//!
//! Gated behind the default-on `registry` feature so image-library
//! consumers can depend on `oxideav-png` with `default-features = false`
//! and skip the `oxideav-core` dependency entirely.
//!
//! The module exposes:
//! * [`register`] / [`register_codecs`] / [`register_containers`] — the
//!   `CodecRegistry` / `ContainerRegistry` entry points the umbrella
//!   `oxideav` crate calls during framework initialisation.
//! * [`PngDecoder`] / [`PngEncoder`] — the trait-side surface that
//!   wraps the framework-free [`crate::decode_png`] /
//!   [`crate::encode_png_image`] entry points.
//! * The `From<PngError> for oxideav_core::Error` conversion + the
//!   `CodecOptionsStruct` impl for [`PngEncoderOptions`].
//! * [`decode_png_to_frame`] / [`encode_single`] /
//!   [`encode_single_with_options`] — `VideoFrame`-flavoured wrappers
//!   preserved for existing callers that pre-date the `PngImage` API.

use std::collections::VecDeque;

use oxideav_core::Decoder;
use oxideav_core::Encoder;
use oxideav_core::{
    parse_options, CodecCapabilities, CodecId, CodecInfo, CodecOptionsStruct, CodecParameters,
    CodecRegistry, ContainerRegistry, Frame, MediaType, OptionField, OptionKind, OptionValue,
    Packet, PixelFormat, Rational, TimeBase, VideoFrame, VideoPlane,
};

use crate::decoder::{decode_png, CODEC_ID_STR};
use crate::encoder::{encode_apng_with_options, encode_png_image_with_options, PngEncoderOptions};
use crate::error::PngError;
use crate::image::{PngImage, PngPixelFormat};

/// Convert a [`PngError`] into the framework-shared
/// `oxideav_core::Error` so trait impls in this crate can use `?` on
/// errors returned by the framework-free decode/encode functions.
impl From<PngError> for oxideav_core::Error {
    fn from(e: PngError) -> Self {
        match e {
            PngError::InvalidData(s) => oxideav_core::Error::InvalidData(s),
            PngError::Unsupported(s) => oxideav_core::Error::Unsupported(s),
            PngError::Eof => oxideav_core::Error::Eof,
            PngError::NeedMore => oxideav_core::Error::NeedMore,
            PngError::Other(s) => oxideav_core::Error::other(s),
        }
    }
}

/// Map a framework pixel format to [`PngPixelFormat`]. Returns `Err` for
/// pixel formats the PNG codec can't represent.
fn from_core_pixel_format(pf: PixelFormat) -> oxideav_core::Result<PngPixelFormat> {
    Ok(match pf {
        PixelFormat::Gray8 => PngPixelFormat::Gray8,
        PixelFormat::Gray16Le => PngPixelFormat::Gray16Le,
        PixelFormat::Rgb24 => PngPixelFormat::Rgb24,
        PixelFormat::Rgb48Le => PngPixelFormat::Rgb48Le,
        PixelFormat::Pal8 => PngPixelFormat::Pal8,
        PixelFormat::Ya8 => PngPixelFormat::Ya8,
        PixelFormat::Rgba => PngPixelFormat::Rgba,
        PixelFormat::Rgba64Le => PngPixelFormat::Rgba64Le,
        other => {
            return Err(oxideav_core::Error::unsupported(format!(
                "PNG: pixel format {other:?} not supported"
            )))
        }
    })
}

/// Convert a framework `VideoFrame` (single planar layout) into a
/// [`PngImage`]. Used by the `Encoder` trait impl to feed the
/// standalone encoder.
fn video_frame_to_png_image(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    pix: PngPixelFormat,
    palette: &[u8],
) -> oxideav_core::Result<PngImage> {
    let plane = frame
        .planes
        .first()
        .ok_or_else(|| oxideav_core::Error::invalid("PNG encoder: frame has no planes"))?;
    Ok(PngImage {
        width,
        height,
        pixel_format: pix,
        stride: plane.stride,
        data: plane.data.clone(),
        palette: palette.to_vec(),
    })
}

/// Convert a [`PngImage`] into a framework `VideoFrame`.
fn png_image_to_video_frame(image: &PngImage, pts: Option<i64>) -> VideoFrame {
    VideoFrame {
        pts,
        planes: vec![VideoPlane {
            stride: image.stride,
            data: image.data.clone(),
        }],
    }
}

// ---- CodecOptionsStruct (registry-only schema for PngEncoderOptions) ----

impl CodecOptionsStruct for PngEncoderOptions {
    const SCHEMA: &'static [OptionField] = &[OptionField {
        name: "interlace",
        kind: OptionKind::Bool,
        default: OptionValue::Bool(false),
        help: "Emit an Adam7 seven-pass interlaced PNG stream (IHDR.interlace = 1)",
    }];
    fn apply(&mut self, key: &str, v: &OptionValue) -> oxideav_core::Result<()> {
        match key {
            "interlace" => self.interlace = v.as_bool()?,
            _ => unreachable!("guarded by SCHEMA"),
        }
        Ok(())
    }
}

// ---- Decoder trait impl + factory ----

/// Factory for the `Decoder` trait impl — registered in the codec
/// registry and called by the framework when a `png` packet stream
/// needs decoding.
pub fn make_decoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Decoder>> {
    Ok(Box::new(PngDecoder {
        codec_id: params.codec_id.clone(),
        pending: None,
        eof: false,
    }))
}

/// PNG `Decoder` trait impl: each `send_packet` carries one full PNG
/// file (or APNG animation frame) and the matching `receive_frame`
/// returns the decoded `VideoFrame`.
pub struct PngDecoder {
    codec_id: CodecId,
    pending: Option<Packet>,
    eof: bool,
}

impl Decoder for PngDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> oxideav_core::Result<()> {
        if self.pending.is_some() {
            return Err(oxideav_core::Error::other(
                "PNG decoder: receive_frame must be called before sending another packet",
            ));
        }
        self.pending = Some(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> oxideav_core::Result<Frame> {
        let Some(pkt) = self.pending.take() else {
            return if self.eof {
                Err(oxideav_core::Error::Eof)
            } else {
                Err(oxideav_core::Error::NeedMore)
            };
        };
        let vf = decode_png_to_frame(&pkt.data, pkt.pts)?;
        Ok(Frame::Video(vf))
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        Ok(())
    }
}

/// `VideoFrame`-flavoured wrapper around [`decode_png`]. Preserved for
/// existing callers (and the container layer) that build frames
/// directly.
pub fn decode_png_to_frame(buf: &[u8], pts: Option<i64>) -> oxideav_core::Result<VideoFrame> {
    let img = decode_png(buf)?;
    Ok(png_image_to_video_frame(&img, pts))
}

// ---- Encoder trait impl + factory ----

/// Factory for the `Encoder` trait impl — registered in the codec
/// registry and called by the framework when a `png` encode is
/// requested.
pub fn make_encoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Encoder>> {
    let opts = parse_options::<PngEncoderOptions>(&params.options)?;
    let width = params
        .width
        .ok_or_else(|| oxideav_core::Error::invalid("PNG encoder: missing width"))?;
    let height = params
        .height
        .ok_or_else(|| oxideav_core::Error::invalid("PNG encoder: missing height"))?;
    let pix_core = params.pixel_format.unwrap_or(PixelFormat::Rgba);
    let pix = from_core_pixel_format(pix_core)?;

    let mut output_params = params.clone();
    output_params.media_type = MediaType::Video;
    output_params.codec_id = CodecId::new(CODEC_ID_STR);
    output_params.width = Some(width);
    output_params.height = Some(height);
    output_params.pixel_format = Some(pix_core);

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

/// PNG `Encoder` trait impl. Buffers up to N frames before emitting a
/// single PNG (one frame) or APNG (multiple frames) on flush.
pub struct PngEncoder {
    output_params: CodecParameters,
    width: u32,
    height: u32,
    pix: PngPixelFormat,
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

    fn send_frame(&mut self, frame: &Frame) -> oxideav_core::Result<()> {
        match frame {
            Frame::Video(v) => {
                self.frames.push(v.clone());
                Ok(())
            }
            _ => Err(oxideav_core::Error::invalid(
                "PNG encoder: video frames only",
            )),
        }
    }

    fn receive_packet(&mut self) -> oxideav_core::Result<Packet> {
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
            return Err(oxideav_core::Error::Eof);
        }
        Err(oxideav_core::Error::NeedMore)
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        if !self.frames.is_empty() && self.pending_out.is_empty() {
            self.finalize()?;
        }
        Ok(())
    }
}

impl PngEncoder {
    fn finalize(&mut self) -> oxideav_core::Result<()> {
        let is_animated = self.frames.len() > 1 || self.animated_hint;
        let bytes = if is_animated {
            // Default delay per frame: derived from frame_rate or
            // 10cs = 10Hz.
            let delay_cs: u16 = match self.frame_rate {
                Some(r) if r.num > 0 && r.den > 0 => (100 * r.den as u32 / r.num as u32) as u16,
                _ => 10,
            };
            let frames: Vec<PngImage> = self
                .frames
                .iter()
                .map(|f| {
                    video_frame_to_png_image(f, self.width, self.height, self.pix, &self.palette)
                })
                .collect::<oxideav_core::Result<_>>()?;
            encode_apng_with_options(&frames, delay_cs, 0, &self.opts)?
        } else {
            let img = video_frame_to_png_image(
                &self.frames[0],
                self.width,
                self.height,
                self.pix,
                &self.palette,
            )?;
            encode_png_image_with_options(&img, &self.opts)?
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

/// `VideoFrame`-flavoured wrapper around [`encode_png_image`].
/// Preserved for existing callers.
pub fn encode_single(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    pix: PixelFormat,
    palette: &[u8],
) -> oxideav_core::Result<Vec<u8>> {
    encode_single_with_options(
        frame,
        width,
        height,
        pix,
        palette,
        &PngEncoderOptions::default(),
    )
}

/// `VideoFrame`-flavoured wrapper around
/// [`encode_png_image_with_options`]. Preserved for existing callers.
pub fn encode_single_with_options(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    pix: PixelFormat,
    palette: &[u8],
    opts: &PngEncoderOptions,
) -> oxideav_core::Result<Vec<u8>> {
    let pix = from_core_pixel_format(pix)?;
    let img = video_frame_to_png_image(frame, width, height, pix, palette)?;
    Ok(encode_png_image_with_options(&img, opts)?)
}

// ---- Container + registration ----

/// Register the PNG codec (decoder + encoder) into `reg`.
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("png_sw")
        .with_intra_only(true)
        .with_lossless(true)
        .with_max_size(16384, 16384)
        .with_pixel_formats(vec![
            PixelFormat::Rgba,
            PixelFormat::Rgb24,
            PixelFormat::Gray8,
            PixelFormat::Pal8,
            PixelFormat::Rgb48Le,
            PixelFormat::Rgba64Le,
        ]);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .encoder(make_encoder)
            .encoder_options::<PngEncoderOptions>(),
    );
}

/// Register the PNG / APNG container (demuxer + muxer + extensions + probe).
pub fn register_containers(reg: &mut ContainerRegistry) {
    crate::container::register(reg);
}

/// Combined registration: codecs + containers.
pub fn register(codecs: &mut CodecRegistry, containers: &mut ContainerRegistry) {
    register_codecs(codecs);
    register_containers(containers);
}
