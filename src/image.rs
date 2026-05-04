//! Standalone image type for `oxideav-png`'s framework-free decode /
//! encode API.
//!
//! When the `registry` feature is enabled, the gated
//! [`crate::registry`] module provides conversions to / from
//! `oxideav_core::VideoFrame` so the trait-side surface (`Decoder` /
//! `Encoder`) keeps working unchanged.

/// Pixel layouts the standalone `oxideav-png` API can produce / consume.
///
/// These mirror the subset of `oxideav_core::PixelFormat` the codec
/// uses. The variants stay byte-for-byte compatible with the framework
/// types so the [`crate::registry`] conversion layer is a 1:1
/// match-and-rebuild rather than a re-pack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PngPixelFormat {
    /// 8-bit grayscale, 1 byte per pixel.
    Gray8,
    /// 16-bit grayscale, little-endian, 2 bytes per pixel.
    Gray16Le,
    /// 8-bit RGB, 3 bytes per pixel.
    Rgb24,
    /// 16-bit RGB, little-endian per channel, 6 bytes per pixel.
    Rgb48Le,
    /// 8-bit palette index (1 byte per pixel). The matching palette
    /// lives on [`PngImage::palette`].
    Pal8,
    /// 8-bit grayscale + alpha, 2 bytes per pixel.
    Ya8,
    /// 8-bit RGBA, 4 bytes per pixel.
    Rgba,
    /// 16-bit RGBA, little-endian per channel, 8 bytes per pixel.
    Rgba64Le,
}

impl PngPixelFormat {
    /// Bytes per pixel for the given pixel format.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Gray8 | Self::Pal8 => 1,
            Self::Gray16Le | Self::Ya8 => 2,
            Self::Rgb24 => 3,
            Self::Rgba => 4,
            Self::Rgb48Le => 6,
            Self::Rgba64Le => 8,
        }
    }
}

/// Decoded PNG image returned by [`crate::decode_png`].
///
/// Carries the raw pixel buffer, its dimensions, and the pixel format
/// produced from the PNG bitstream's IHDR. For palette-indexed images
/// (`Pal8`), `palette` carries the source `PLTE` bytes followed by the
/// optional `tRNS` bytes.
#[derive(Clone, Debug)]
pub struct PngImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Pixel format the buffer is laid out in.
    pub pixel_format: PngPixelFormat,
    /// Stride (bytes per row) of `data`. Always equals
    /// `width * bytes_per_pixel` for decoder output, but may exceed it
    /// for caller-provided encoder input.
    pub stride: usize,
    /// Tightly packed pixel buffer: `stride * height` bytes.
    pub data: Vec<u8>,
    /// Palette payload for `Pal8` images: PLTE bytes (RGB triples)
    /// optionally followed by tRNS alpha bytes. Empty for non-palette
    /// formats.
    pub palette: Vec<u8>,
}

impl PngImage {
    /// Number of bytes per pixel for [`Self::pixel_format`].
    pub fn bytes_per_pixel(&self) -> usize {
        self.pixel_format.bytes_per_pixel()
    }
}

/// Decoded animated PNG (APNG): one [`PngImage`] per frame plus a
/// per-frame delay in centiseconds (1/100 s — APNG's native unit).
#[derive(Clone, Debug)]
pub struct ApngImage {
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
    /// Pixel format every composited frame is laid out in.
    pub pixel_format: PngPixelFormat,
    /// Composited frames in playback order. Each frame's `width` /
    /// `height` matches the canvas (frames are pre-composited per
    /// APNG disposal / blend rules).
    pub frames: Vec<ApngFrameImage>,
    /// Loop count: `0` for infinite, otherwise the number of plays.
    pub num_plays: u32,
}

/// One composited APNG animation frame.
#[derive(Clone, Debug)]
pub struct ApngFrameImage {
    /// Composited canvas at this animation step.
    pub image: PngImage,
    /// Frame display duration in centiseconds (1/100 s).
    pub delay_cs: u32,
}
