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
//!   wraps the framework-free [`crate::decode`] / [`crate::encode`]
//!   entry points.
//! * `From<PngImage> for VideoFrame` and [`PngImage::from_video_frame`]
//!   — the plane plus the palette / colour-signal side-channels.
//! * The `From<PngError> for oxideav_core::Error` conversion + the
//!   `CodecOptionsStruct` impl for [`EncodeOptions`].
//! * [`decode_png_to_frame`] / [`encode_single`] /
//!   [`encode_single_with_options`] — `VideoFrame`-flavoured wrappers
//!   preserved for existing callers that pre-date the `PngImage` API.

use std::collections::VecDeque;

use oxideav_core::Decoder;
use oxideav_core::Encoder;
use oxideav_core::RuntimeContext;
use oxideav_core::{
    parse_options, CodecCapabilities, CodecId, CodecInfo, CodecOptionsStruct, CodecParameters,
    CodecRegistry, ColorPrimaries, ColorSignal, ContainerRegistry, ExecutionContext, Frame,
    MatrixCoefficients, MediaType, OptionField, OptionKind, OptionValue, Packet, PixelFormat,
    Rational, TimeBase, TransferCharacteristics, VideoFrame, VideoPlane,
};

use crate::decoder::CODEC_ID_STR;
use crate::encoder::{
    encode_still, encode_view, still_to_apng, EncodeOptions, FrameStream, StillLayout,
};
use crate::error::PngError;
use crate::image::{ColorInfo, ColorRange, ImageRef, Palette, PngImage, PngPixelFormat};
use crate::options::DecodeOptions;

/// Convert a [`PngError`] into the framework-shared
/// `oxideav_core::Error` so trait impls in this crate can use `?` on
/// errors returned by the framework-free decode/encode functions.
impl From<PngError> for oxideav_core::Error {
    fn from(e: PngError) -> Self {
        match e {
            PngError::InvalidData(s) => oxideav_core::Error::InvalidData(s),
            PngError::Unsupported(s) => oxideav_core::Error::Unsupported(s),
            PngError::LimitExceeded(s) => oxideav_core::Error::ResourceExhausted(s),
            PngError::Io(e) => oxideav_core::Error::Io(e),
            PngError::Eof => oxideav_core::Error::Eof,
            PngError::NeedMore => oxideav_core::Error::NeedMore,
            PngError::Other(s) => oxideav_core::Error::other(s),
        }
    }
}

/// Map a framework pixel format to [`PngPixelFormat`]. Returns `Err` for
/// pixel formats the PNG codec can't represent.
fn from_core_pixel_format(pf: PixelFormat) -> oxideav_core::Result<PngPixelFormat> {
    Ok(png_pixel_format(pf)?)
}

/// [`from_core_pixel_format`] with the crate's own error type.
fn png_pixel_format(pf: PixelFormat) -> Result<PngPixelFormat, PngError> {
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
            return Err(PngError::unsupported(format!(
                "PNG: pixel format {other:?} not supported"
            )))
        }
    })
}

/// The 1:1 name mapping from [`PngPixelFormat`] to the framework enum.
pub fn to_core_pixel_format(pf: PngPixelFormat) -> PixelFormat {
    match pf {
        PngPixelFormat::Gray8 => PixelFormat::Gray8,
        PngPixelFormat::Gray16Le => PixelFormat::Gray16Le,
        PngPixelFormat::Rgb24 => PixelFormat::Rgb24,
        PngPixelFormat::Rgb48Le => PixelFormat::Rgb48Le,
        PngPixelFormat::Pal8 => PixelFormat::Pal8,
        PngPixelFormat::Ya8 => PixelFormat::Ya8,
        PngPixelFormat::Rgba => PixelFormat::Rgba,
        PngPixelFormat::Rgba64Le => PixelFormat::Rgba64Le,
    }
}

impl From<PngPixelFormat> for PixelFormat {
    fn from(pf: PngPixelFormat) -> Self {
        to_core_pixel_format(pf)
    }
}

impl TryFrom<PixelFormat> for PngPixelFormat {
    type Error = oxideav_core::Error;
    fn try_from(pf: PixelFormat) -> oxideav_core::Result<Self> {
        from_core_pixel_format(pf)
    }
}

/// [`ColorInfo`] as the framework's [`ColorSignal`] (code points map
/// 1:1; `Unspecified` range stays unspecified).
pub fn to_color_signal(c: &ColorInfo) -> ColorSignal {
    let range = match c.range {
        ColorRange::Unspecified => oxideav_core::ColorRange::Unspecified,
        ColorRange::Limited => oxideav_core::ColorRange::Limited,
        ColorRange::Full => oxideav_core::ColorRange::Full,
    };
    ColorSignal::new(
        range,
        ColorPrimaries(c.primaries),
        TransferCharacteristics(c.transfer),
        MatrixCoefficients(c.matrix),
    )
}

/// The inverse of [`to_color_signal`].
pub fn from_color_signal(s: &ColorSignal) -> ColorInfo {
    let range = match s.range {
        oxideav_core::ColorRange::Limited => ColorRange::Limited,
        oxideav_core::ColorRange::Full => ColorRange::Full,
        _ => ColorRange::Unspecified,
    };
    ColorInfo::new(range, s.primaries.0, s.transfer.0, s.matrix.0)
}

/// Legacy palette convention of [`encode_single`] and the encoder's
/// `CodecParameters::extradata`: one `PLTE || tRNS` byte blob with no
/// recorded split point. The `PLTE` entry count is taken as the
/// highest index the pixels use plus one (so the table is as long as
/// the image needs) and whatever follows is the alpha tail.
fn palette_from_legacy_blob(blob: &[u8], indices: &[u8]) -> Option<Palette> {
    if blob.is_empty() {
        return None;
    }
    let n = usize::from(indices.iter().copied().max().unwrap_or(0)) + 1;
    let plte_len = (n * 3).min(blob.len());
    let (plte, trns) = blob.split_at(plte_len);
    Some(Palette::from_rgb(
        plte,
        if trns.is_empty() { None } else { Some(trns) },
    ))
}

/// Convert a framework `VideoFrame` (single packed plane) into a
/// [`PngImage`]. The palette, for `Pal8`, comes from the frame's
/// palette side-channel when attached, else from the legacy `PLTE ||
/// tRNS` `palette` blob; the frame's colour-signal side-channel, when
/// attached, becomes `color`.
fn video_frame_to_png_image(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    pix: PngPixelFormat,
    palette: &[u8],
) -> Result<PngImage, PngError> {
    let plane = frame
        .image_planes()
        .first()
        .ok_or_else(|| PngError::invalid("PNG encoder: frame has no planes"))?;
    let mut img = PngImage::packed(width, height, pix, plane.stride, plane.data.clone())?;
    stamp_side_channels(&mut img, frame, palette);
    Ok(img)
}

/// [`video_frame_to_png_image`] without the copy: the frame's first
/// plane as the encoder reads it, borrowed, with the same side fields.
/// The palette a `Pal8` frame gets is built into `palette`, which the
/// view borrows.
fn video_frame_view<'a>(
    frame: &'a VideoFrame,
    width: u32,
    height: u32,
    pix: PngPixelFormat,
    legacy_palette: &[u8],
    palette: &'a mut Option<Palette>,
) -> Result<ImageRef<'a>, PngError> {
    let plane = frame
        .image_planes()
        .first()
        .ok_or_else(|| PngError::invalid("PNG encoder: frame has no planes"))?;
    let mut image = ImageRef::plane(width, height, pix, plane.stride, &plane.data)?;
    let (frame_palette, color) = frame_side_fields(frame, pix, legacy_palette, &plane.data);
    *palette = frame_palette;
    let palette: &'a Option<Palette> = palette;
    image.palette = palette.as_ref();
    if let Some(color) = color {
        image.color = color;
    }
    Ok(image)
}

/// The side fields a frame's side-channels give an image of layout
/// `pix` whose plane is `indices`: for `Pal8`, the palette side-channel
/// when attached, else the legacy palette blob sized to the highest
/// index used; and the colour-signal side-channel, when attached.
fn frame_side_fields(
    frame: &VideoFrame,
    pix: PngPixelFormat,
    legacy_palette: &[u8],
    indices: &[u8],
) -> (Option<Palette>, Option<ColorInfo>) {
    let palette = if pix == PngPixelFormat::Pal8 {
        match frame.palette() {
            Some(rgb) => Some(Palette::from_rgb(rgb, None)),
            None => palette_from_legacy_blob(legacy_palette, indices),
        }
    } else {
        None
    };
    let color = frame.color_signal().map(|sig| from_color_signal(&sig));
    (palette, color)
}

/// Fill `palette` / `color` of `img` from the frame's side-channels
/// (falling back to the legacy palette blob).
fn stamp_side_channels(img: &mut PngImage, frame: &VideoFrame, legacy_palette: &[u8]) {
    let (palette, color) = frame_side_fields(frame, img.format, legacy_palette, img.data());
    if img.format == PngPixelFormat::Pal8 {
        img.palette = palette;
    }
    if let Some(color) = color {
        img.color = color;
    }
}

/// Convert a [`PngImage`] into a framework `VideoFrame`: the pixel
/// plane, plus the palette side-channel for `Pal8` and the
/// colour-signal side-channel whenever the image signals more than
/// PNG's default (a specified primaries / transfer, or limited range).
fn png_image_to_video_frame(image: &PngImage, pts: Option<i64>) -> VideoFrame {
    let mut frame = VideoFrame {
        pts,
        planes: vec![VideoPlane {
            stride: image.stride(),
            data: image.data().to_vec(),
        }],
    };
    stamp_frame_side_channels(&mut frame, image);
    frame
}

/// [`png_image_to_video_frame`] moving the plane out of `image`.
fn png_image_into_video_frame(mut image: PngImage, pts: Option<i64>) -> VideoFrame {
    let stride = image.stride();
    let data = if image.planes.is_empty() {
        Vec::new()
    } else {
        std::mem::take(&mut image.planes[0].data)
    };
    let mut frame = VideoFrame {
        pts,
        planes: vec![VideoPlane { stride, data }],
    };
    stamp_frame_side_channels(&mut frame, &image);
    frame
}

fn stamp_frame_side_channels(frame: &mut VideoFrame, image: &PngImage) {
    if let (PngPixelFormat::Pal8, Some(p)) = (image.format, &image.palette) {
        frame.set_palette(p.to_rgb());
    }
    let c = image.color;
    if c.primaries != ColorInfo::UNSPECIFIED
        || c.transfer != ColorInfo::UNSPECIFIED
        || c.range == ColorRange::Limited
    {
        frame.set_color_signal(to_color_signal(&c));
    }
}

impl From<PngImage> for VideoFrame {
    /// The pixel plane (`pts` `None`), plus the palette side-channel
    /// for `Pal8` and the colour-signal side-channel when the image
    /// signals a colour space.
    fn from(image: PngImage) -> Self {
        png_image_into_video_frame(image, None)
    }
}

impl From<&PngImage> for VideoFrame {
    fn from(image: &PngImage) -> Self {
        png_image_to_video_frame(image, None)
    }
}

impl PngImage {
    /// Rebuild an image from a framework frame and the stream
    /// parameters that describe it (`width`, `height`, `pixel_format`
    /// are required; `extradata` is read as the legacy `PLTE || tRNS`
    /// blob when the frame carries no palette side-channel).
    ///
    /// Errors are the crate's own [`PngError`] (`Unsupported` for a
    /// layout PNG has no name for, `InvalidData` for missing
    /// parameters or a plane that does not match the geometry); the
    /// registry adapter maps them to `oxideav_core::Error`.
    pub fn from_video_frame(
        frame: &VideoFrame,
        params: &CodecParameters,
    ) -> Result<Self, PngError> {
        let width = params
            .width
            .ok_or_else(|| PngError::invalid("PNG: missing width"))?;
        let height = params
            .height
            .ok_or_else(|| PngError::invalid("PNG: missing height"))?;
        let pix = png_pixel_format(params.pixel_format.unwrap_or(PixelFormat::Rgba))?;
        video_frame_to_png_image(frame, width, height, pix, &params.extradata)
    }
}

impl TryFrom<(&VideoFrame, &CodecParameters)> for PngImage {
    type Error = PngError;
    fn try_from((frame, params): (&VideoFrame, &CodecParameters)) -> Result<Self, PngError> {
        PngImage::from_video_frame(frame, params)
    }
}

// ---- CodecOptionsStruct (registry-only schema for EncodeOptions) ----

impl CodecOptionsStruct for EncodeOptions {
    const SCHEMA: &'static [OptionField] = &[
        OptionField {
            name: "interlace",
            kind: OptionKind::Bool,
            default: OptionValue::Bool(false),
            help: "Emit an Adam7 seven-pass interlaced PNG stream (IHDR.interlace = 1)",
        },
        OptionField {
            name: "bit_depth",
            kind: OptionKind::U32,
            default: OptionValue::U32(0),
            help: "Sub-byte IHDR bit depth (1, 2, or 4). \
                   Only valid for Gray8 / Pal8 sources. \
                   8 is a no-op for those sources. \
                   0 (the default) keeps the source format's native depth.",
        },
        OptionField {
            name: "filter",
            kind: OptionKind::String,
            default: OptionValue::String(String::new()),
            help: "Filter-selection policy (W3C PNG3 §12.7). \
                   `adaptive` (the default) applies the §12.8 per-row \
                   min-sum-abs-delta heuristic across all five filter types. \
                   `none` / `sub` / `up` / `average` / `paeth` pin a single \
                   filter type for every row, skipping the per-row trial. \
                   `brute` filters the whole image under each of those six \
                   policies, deflates every candidate, and keeps the \
                   smallest — the §12.7 \"find what compresses best\" search \
                   (slowest; best compression). \
                   An empty value is treated as `adaptive`.",
        },
        OptionField {
            name: "compression_level",
            kind: OptionKind::U32,
            default: OptionValue::U32(0),
            help: "DEFLATE level for the IDAT / fdAT pixel stream (1..=9). \
                   1 is fastest / largest, 9 is slowest / smallest. \
                   0 (the default) selects the encoder default level 2 — \
                   12 MP RGB24 in ~0.4 s on one thread within ~8 % of the \
                   level-6 size; 4 trades +45 % time for −3 %, 6 is 8× \
                   slower for −8 % (see EncodeOptions::compression_level).",
        },
        OptionField {
            name: "level",
            kind: OptionKind::U32,
            default: OptionValue::U32(0),
            help: "Alias of `compression_level` (the framework-wide \
                   speed / size dial name): DEFLATE level 1..=9, \
                   0 = encoder default.",
        },
        OptionField {
            name: "compression",
            kind: OptionKind::U32,
            default: OptionValue::U32(0),
            help: "Alias of `compression_level`: DEFLATE level 1..=9, \
                   0 = encoder default.",
        },
    ];
    fn apply(&mut self, key: &str, v: &OptionValue) -> oxideav_core::Result<()> {
        use crate::filter::{FilterStrategy, FilterType};
        match key {
            "interlace" => self.interlace = v.as_bool()?,
            "bit_depth" => {
                let raw = v.as_u32()?;
                // `0` is the sentinel for "leave it at the source's native
                // depth" — matches `bit_depth: None`. The actual range
                // validation happens at encode time in `resolve_bit_depth`,
                // so a bogus value here surfaces as a clear encode error
                // rather than getting silently rewritten.
                self.bit_depth = if raw == 0 { None } else { Some(raw as u8) };
            }
            "compression_level" | "level" | "compression" => {
                let raw = v.as_u32()?;
                // `0` is the sentinel for "use the encoder default" —
                // matches `compression_level: None`. Range validation
                // (1..=9) happens at encode time in
                // `resolve_compression_level`, so a bogus value surfaces
                // as a clear encode error rather than getting silently
                // clamped here.
                self.compression_level = if raw == 0 { None } else { Some(raw as u8) };
            }
            "filter" => {
                let raw = v.as_str()?;
                // Case-insensitive so "Paeth" / "PAETH" / "paeth" all
                // map to the same strategy — matches W3C PNG3 §12.7's
                // mixed-case prose.
                self.filter_strategy = match raw.to_ascii_lowercase().as_str() {
                    "" | "adaptive" => FilterStrategy::Adaptive,
                    "none" => FilterStrategy::Fixed(FilterType::None),
                    "sub" => FilterStrategy::Fixed(FilterType::Sub),
                    "up" => FilterStrategy::Fixed(FilterType::Up),
                    "average" => FilterStrategy::Fixed(FilterType::Average),
                    "paeth" => FilterStrategy::Fixed(FilterType::Paeth),
                    "brute" => FilterStrategy::Brute,
                    other => {
                        return Err(oxideav_core::Error::invalid(format!(
                            "PNG encoder: option `filter` got {other:?}; \
                             expected one of adaptive / none / sub / up / average / paeth / brute"
                        )))
                    }
                };
            }
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

/// `VideoFrame`-flavoured wrapper around [`crate::decode`]. Preserved
/// for existing callers (and the container layer) that build frames
/// directly: the standalone decode, then [`From<PngImage>`] for
/// `VideoFrame` with `pts` stamped.
pub fn decode_png_to_frame(buf: &[u8], pts: Option<i64>) -> oxideav_core::Result<VideoFrame> {
    let img = crate::decoder::decode_image(buf, &DecodeOptions::default())?;
    Ok(png_image_into_video_frame(img, pts))
}

// ---- Encoder trait impl + factory ----

/// Factory for the `Encoder` trait impl — registered in the codec
/// registry and called by the framework when a `png` encode is
/// requested.
pub fn make_encoder(params: &CodecParameters) -> oxideav_core::Result<Box<dyn Encoder>> {
    let opts = parse_options::<EncodeOptions>(&params.options)?;
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
        pending: None,
        failed: None,
        pending_out: VecDeque::new(),
        frame_rate: params.frame_rate,
        palette: params.extradata.clone(),
        animated_hint,
        eof: false,
        opts,
        threads: 1,
    }))
}

/// PNG `Encoder` trait impl. Emits a single PNG (one frame) or an APNG
/// (more frames, or a frame rate set) on flush.
///
/// Each frame is compressed in `send_frame`, read in place from the
/// caller's frame, so the encoder holds compressed frames, never a copy
/// of a frame's pixels. A frame that cannot be encoded fails the whole
/// file: its `send_frame` returns the error, later frames of the file
/// are dropped, `flush` returns the same error and no packet is
/// emitted. Frames sent after that `flush` start a new file.
pub struct PngEncoder {
    output_params: CodecParameters,
    width: u32,
    height: u32,
    pix: PngPixelFormat,
    time_base: TimeBase,
    /// The frames sent since the last packet, compressed.
    pending: Option<PendingFile>,
    /// The error that failed the file being built; `flush` (or, after
    /// one, `receive_packet`) returns it and starts a new file.
    failed: Option<PngError>,
    pending_out: VecDeque<Packet>,
    frame_rate: Option<Rational>,
    /// Raw palette + optional trns carried on `extradata`. Only used when
    /// encoding Pal8: layout is `PLTE_bytes || tRNS_bytes` per the container.
    palette: Vec<u8>,
    animated_hint: bool,
    eof: bool,
    opts: EncodeOptions,
    /// Thread budget granted through `set_execution_context`; `1`
    /// (serial) until the executor says otherwise.
    threads: usize,
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
                if self.failed.is_some() {
                    // The file this frame belongs to has already failed;
                    // `flush` reports why.
                    return Ok(());
                }
                self.compress_frame(v).map_err(|e| {
                    self.pending = None;
                    self.failed = Some(copy_error(&e));
                    e.into()
                })
            }
            _ => Err(oxideav_core::Error::invalid(
                "PNG encoder: video frames only",
            )),
        }
    }

    fn receive_packet(&mut self) -> oxideav_core::Result<Packet> {
        if let Some(p) = self.pending_out.pop_front() {
            return Ok(p);
        }
        if self.eof {
            if let Some(e) = self.failed.take() {
                return Err(e.into());
            }
            // Produce output now if we haven't already.
            self.finalize();
            if let Some(p) = self.pending_out.pop_front() {
                return Ok(p);
            }
            return Err(oxideav_core::Error::Eof);
        }
        Err(oxideav_core::Error::NeedMore)
    }

    fn flush(&mut self) -> oxideav_core::Result<()> {
        self.eof = true;
        if let Some(e) = self.failed.take() {
            return Err(e.into());
        }
        if self.pending_out.is_empty() {
            self.finalize();
        }
        Ok(())
    }

    /// The IDAT / fdAT DEFLATE pass is cut into independent segments
    /// that `ctx.threads` workers compress concurrently (the emitted
    /// bytes do not depend on the budget). Serial until granted.
    fn set_execution_context(&mut self, ctx: &ExecutionContext) {
        self.threads = ctx.threads.max(1);
    }
}

/// The frames of the next packet, compressed as they were sent.
struct PendingFile {
    /// Frame 0 as a complete still PNG: the packet itself when it stays
    /// a still, else the source of the APNG's header chunks, default
    /// image and trailer (see [`still_to_apng`]).
    still: Vec<u8>,
    /// Where the parts of `still` lie.
    layout: StillLayout,
    /// How frames 1 and up are compressed, derived from frame 0.
    stream: FrameStream,
    /// The `fcTL` / `fdAT` pairs of frames 1 and up, in order.
    later_frames: Vec<u8>,
    /// Frames compressed so far.
    frames: u32,
    /// Frame 0's timestamp: the packet's.
    pts: Option<i64>,
}

/// Per-frame `fcTL` delay in centiseconds: from the stream's frame
/// rate, else 10 cs (10 Hz).
fn delay_cs(frame_rate: Option<Rational>) -> u16 {
    match frame_rate {
        Some(r) if r.num > 0 && r.den > 0 => (100 * r.den as u32 / r.num as u32) as u16,
        _ => 10,
    }
}

/// A second [`PngError`] equal to `e`, to report one error twice (from
/// `send_frame` and from `flush`). An I/O error keeps its kind and
/// message; the encoder writes to memory, so it raises none.
fn copy_error(e: &PngError) -> PngError {
    match e {
        PngError::InvalidData(s) => PngError::InvalidData(s.clone()),
        PngError::Unsupported(s) => PngError::Unsupported(s.clone()),
        PngError::LimitExceeded(s) => PngError::LimitExceeded(s.clone()),
        PngError::Io(io) => PngError::Io(std::io::Error::new(io.kind(), io.to_string())),
        PngError::Eof => PngError::Eof,
        PngError::NeedMore => PngError::NeedMore,
        PngError::Other(s) => PngError::Other(s.clone()),
    }
}

impl PngEncoder {
    /// Compress `frame` into the pending file, reading its plane in
    /// place. Frame 0 becomes a complete still PNG; every later frame is
    /// appended as the `fcTL` / `fdAT` pair an APNG carries it in (its
    /// palette and colour are not read: the APNG takes them from frame
    /// 0, as [`crate::encode_apng`] does).
    fn compress_frame(&mut self, frame: &VideoFrame) -> Result<(), PngError> {
        let Some(file) = self.pending.as_mut() else {
            let mut palette = None;
            let image = video_frame_view(
                frame,
                self.width,
                self.height,
                self.pix,
                &self.palette,
                &mut palette,
            )?;
            let (still, layout) = encode_still(&image, &self.opts, self.threads)?;
            // Derived once, from frame 0 with its palette, as
            // `encode_apng_threaded` derives it from its first frame; the
            // still above already passed the same checks.
            let stream = FrameStream::new(&image, &self.opts)?;
            self.pending = Some(PendingFile {
                still,
                layout,
                stream,
                later_frames: Vec::new(),
                frames: 1,
                pts: frame.pts,
            });
            return Ok(());
        };
        let plane = frame
            .image_planes()
            .first()
            .ok_or_else(|| PngError::invalid("PNG encoder: frame has no planes"))?;
        let image = ImageRef::plane(self.width, self.height, self.pix, plane.stride, &plane.data)?;
        // Frame n's fcTL carries sequence 2n - 1 and its fdAT 2n; frame
        // 0's fcTL is sequence 0 (W3C PNG3 §4.9.2).
        let sequence = 2 * file.frames - 1;
        file.stream.write_frame(
            &mut file.later_frames,
            &image,
            &self.opts,
            self.threads,
            sequence,
            delay_cs(self.frame_rate),
            100,
        )?;
        file.frames += 1;
        Ok(())
    }

    /// Turn the pending file, if any, into a packet: the still when one
    /// frame was sent and no frame rate was set, else the APNG of every
    /// frame.
    fn finalize(&mut self) {
        let Some(file) = self.pending.take() else {
            return;
        };
        let bytes = if file.frames > 1 || self.animated_hint {
            still_to_apng(
                &file.still,
                &file.layout,
                &file.later_frames,
                file.frames,
                0,
                delay_cs(self.frame_rate),
                100,
            )
        } else {
            file.still
        };
        let mut pkt = Packet::new(0, self.time_base, bytes);
        pkt.pts = file.pts;
        pkt.dts = pkt.pts;
        pkt.flags.keyframe = true;
        self.pending_out.push_back(pkt);
    }
}

/// `VideoFrame`-flavoured wrapper around [`crate::encode`].
/// Preserved for existing callers. `palette` is the legacy `PLTE ||
/// tRNS` blob for `Pal8` (ignored when the frame carries a palette
/// side-channel).
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
        &EncodeOptions::default(),
    )
}

/// `VideoFrame`-flavoured wrapper around [`crate::encode`] with
/// options. Preserved for existing callers.
pub fn encode_single_with_options(
    frame: &VideoFrame,
    width: u32,
    height: u32,
    pix: PixelFormat,
    palette: &[u8],
    opts: &EncodeOptions,
) -> oxideav_core::Result<Vec<u8>> {
    let pix = from_core_pixel_format(pix)?;
    let mut frame_palette = None;
    let image = video_frame_view(frame, width, height, pix, palette, &mut frame_palette)?;
    Ok(encode_view(&image, opts, opts.threads.max(1))?)
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
            .encoder_options::<EncodeOptions>(),
    );
}

/// Register the PNG / APNG container (demuxer + muxer + extensions + probe).
pub fn register_containers(reg: &mut ContainerRegistry) {
    crate::container::register(reg);
}

/// Unified registration entry point — installs the PNG codec into the
/// codec sub-registry and the PNG/APNG container into the container
/// sub-registry of the supplied [`RuntimeContext`].
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
    register_containers(&mut ctx.containers);
}

oxideav_core::register!("png", register);

#[cfg(test)]
mod register_tests {
    use super::*;

    #[test]
    fn register_via_runtime_context_installs_both_sides() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let id = CodecId::new(CODEC_ID_STR);
        assert!(
            ctx.codecs.has_decoder(&id),
            "PNG decoder factory not installed via RuntimeContext"
        );
        assert!(
            ctx.codecs.has_encoder(&id),
            "PNG encoder factory not installed via RuntimeContext"
        );
        assert_eq!(
            ctx.containers.container_for_extension("png"),
            Some("png"),
            "PNG container extension not installed via RuntimeContext"
        );
    }

    #[test]
    fn filter_option_parses_brute_and_rejects_unknown() {
        use crate::filter::FilterStrategy;
        use oxideav_core::OptionValue;

        // `brute` (any case) maps to the whole-image exhaustive search.
        for raw in ["brute", "BRUTE", "Brute"] {
            let mut opts = EncodeOptions::default();
            opts.apply("filter", &OptionValue::String(raw.to_string()))
                .expect("filter=brute should parse");
            assert_eq!(opts.filter_strategy, FilterStrategy::Brute, "raw {raw:?}");
        }

        // An unknown filter value still errors, and the message now lists
        // `brute` among the accepted values.
        let mut opts = EncodeOptions::default();
        let err = opts
            .apply("filter", &OptionValue::String("wibble".into()))
            .unwrap_err();
        assert!(err.to_string().contains("brute"), "got {err}");
    }
}
