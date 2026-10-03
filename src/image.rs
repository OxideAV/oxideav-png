//! The standalone image types: the shapes every `oxideav-<format>`
//! image crate shares (`IMAGE_CRATE_API`), specialised for PNG.
//!
//! * [`PngImage`] — the native-layout image [`crate::decode`] returns
//!   and [`crate::encode`] consumes: dimensions, a [`PixelFormat`] tag,
//!   one packed [`Plane`], [`ColorInfo`], [`Metadata`], an optional
//!   [`Palette`] and PNG's keyed [`Trns`] transparency.
//! * [`RgbImage`] / [`RgbaImage`] — the tightly packed 8-bit raw
//!   paths ([`crate::decode_rgb8`] / [`crate::decode_rgba8`],
//!   [`PngImage::to_rgb8`] / [`PngImage::to_rgba8`]).
//! * [`Frame`] — one entry of [`crate::decode_all`] (APNG).
//! * [`ImageInfo`] — what [`crate::info`] reads from the header walk.
//!
//! When the `registry` feature is enabled, [`crate::registry`] adds the
//! `From<PngImage> for oxideav_core::VideoFrame` conversion and its
//! inverse so the framework `Decoder` / `Encoder` are thin adapters
//! over the same functions.

use std::time::Duration;

use crate::metadata::Trns;

/// Pixel layouts the standalone `oxideav-png` API can produce / consume.
///
/// Variant names mirror `oxideav_core::PixelFormat` exactly, so the
/// [`crate::registry`] conversion layer is a 1:1 match-and-rebuild
/// rather than a re-pack. Every PNG layout is packed (one plane).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
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

/// The contract name for [`PngPixelFormat`].
pub type PixelFormat = PngPixelFormat;

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

    /// `true` when the layout carries an alpha channel of its own
    /// (`Ya8` / `Rgba` / `Rgba64Le`). Keyed (`tRNS`) and palette
    /// transparency are not part of the layout; see
    /// [`PngImage::has_alpha`].
    pub fn has_alpha(self) -> bool {
        matches!(self, Self::Ya8 | Self::Rgba | Self::Rgba64Le)
    }

    /// Bits per sample of the layout (8 or 16).
    pub fn bits_per_sample(self) -> u8 {
        match self {
            Self::Gray8 | Self::Pal8 | Self::Ya8 | Self::Rgb24 | Self::Rgba => 8,
            Self::Gray16Le | Self::Rgb48Le | Self::Rgba64Le => 16,
        }
    }
}

/// One pixel plane: `stride` bytes per row, `data` holding
/// `stride × height` bytes (rows may carry padding past the visible
/// width). PNG layouts are packed, so a [`PngImage`] has exactly one.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Plane {
    /// Bytes per row.
    pub stride: usize,
    /// Row-major bytes, `stride × height` long.
    pub data: Vec<u8>,
}

impl Plane {
    /// Wrap a plane buffer with its row stride.
    pub fn new(stride: usize, data: Vec<u8>) -> Self {
        Self { stride, data }
    }
}

/// Nominal sample range (H.273 `VideoFullRangeFlag`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorRange {
    /// No range was signalled.
    #[default]
    Unspecified,
    /// Limited (video / studio) range: `VideoFullRangeFlag == 0`.
    Limited,
    /// Full (PC) range: `VideoFullRangeFlag == 1`.
    Full,
}

/// Colour signalling of an image: the sample range plus the H.273
/// `ColourPrimaries` / `TransferCharacteristics` /
/// `MatrixCoefficients` code points (`2` = unspecified).
///
/// For PNG, [`crate::decode`] fills it from the highest-precedence
/// colour chunk the file carries (W3C PNG3 §4.3 Table 1: `cICP` >
/// `iCCP` > `sRGB` > `cHRM`/`gAMA`); see [`crate`] docs for the exact
/// mapping. PNG samples are full-range unless a `cICP` chunk says
/// otherwise, and PNG is RGB-only so `matrix` is always `0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ColorInfo {
    /// Sample range.
    pub range: ColorRange,
    /// H.273 `ColourPrimaries` code point (`1` = BT.709 / sRGB, `9` =
    /// BT.2020, `2` = unspecified).
    pub primaries: u8,
    /// H.273 `TransferCharacteristics` code point (`13` = sRGB, `16` =
    /// PQ, `18` = HLG, `2` = unspecified).
    pub transfer: u8,
    /// H.273 `MatrixCoefficients` code point (`0` = identity / RGB).
    pub matrix: u8,
}

impl ColorInfo {
    /// H.273 "unspecified" code point.
    pub const UNSPECIFIED: u8 = 2;
    /// H.273 `MatrixCoefficients` identity (RGB / GBR) code point.
    pub const MATRIX_IDENTITY: u8 = 0;
    /// H.273 `ColourPrimaries` BT.709 / sRGB code point.
    pub const PRIMARIES_BT709: u8 = 1;
    /// H.273 `ColourPrimaries` BT.2020 / BT.2100 code point.
    pub const PRIMARIES_BT2020: u8 = 9;
    /// H.273 `TransferCharacteristics` IEC 61966-2-1 sRGB code point.
    pub const TRANSFER_SRGB: u8 = 13;

    /// Build a description from its four parts.
    pub const fn new(range: ColorRange, primaries: u8, transfer: u8, matrix: u8) -> Self {
        Self {
            range,
            primaries,
            transfer,
            matrix,
        }
    }

    /// Every field unspecified.
    pub const fn unspecified() -> Self {
        Self::new(
            ColorRange::Unspecified,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
        )
    }

    /// PNG's documented default when the file carries no colour chunk:
    /// full-range RGB (`matrix` 0) with unspecified primaries and
    /// transfer ("device-dependent RGB", W3C PNG3 §13.14).
    pub const fn png_default() -> Self {
        Self::new(
            ColorRange::Full,
            Self::UNSPECIFIED,
            Self::UNSPECIFIED,
            Self::MATRIX_IDENTITY,
        )
    }

    /// sRGB (IEC 61966-2-1): BT.709 primaries, sRGB transfer, identity
    /// matrix, full range — what an `sRGB` chunk signals.
    pub const fn srgb() -> Self {
        Self::new(
            ColorRange::Full,
            Self::PRIMARIES_BT709,
            Self::TRANSFER_SRGB,
            Self::MATRIX_IDENTITY,
        )
    }

    /// Set the range.
    pub fn with_range(mut self, range: ColorRange) -> Self {
        self.range = range;
        self
    }

    /// Set the primaries code point.
    pub fn with_primaries(mut self, primaries: u8) -> Self {
        self.primaries = primaries;
        self
    }

    /// Set the transfer code point.
    pub fn with_transfer(mut self, transfer: u8) -> Self {
        self.transfer = transfer;
        self
    }

    /// Set the matrix code point.
    pub fn with_matrix(mut self, matrix: u8) -> Self {
        self.matrix = matrix;
        self
    }

    /// `true` when both primaries and transfer are specified (`!= 2`).
    pub fn is_specified(&self) -> bool {
        self.primaries != Self::UNSPECIFIED && self.transfer != Self::UNSPECIFIED
    }
}

impl Default for ColorInfo {
    /// [`ColorInfo::png_default`].
    fn default() -> Self {
        Self::png_default()
    }
}

/// The metadata blobs every image crate surfaces: an ICC profile, an
/// Exif payload, an XMP packet and a file gamma. PNG sources them from
/// `iCCP`, `eXIf`, the `XML:com.adobe.xmp` `iTXt` chunk and `gAMA`;
/// the full PNG chunk set is available through [`crate::parse_metadata`]
/// / [`crate::PngMetadata`].
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Metadata {
    /// ICC profile bytes (`iCCP`, decompressed).
    pub icc: Option<Vec<u8>>,
    /// Exif payload starting at the TIFF header (`eXIf`).
    pub exif: Option<Vec<u8>>,
    /// XMP packet bytes (`iTXt` keyword `XML:com.adobe.xmp`, UTF-8).
    pub xmp: Option<Vec<u8>>,
    /// File gamma (`gAMA` / 100 000, e.g. `0.45455`). Decoding never
    /// applies it; see [`crate::gamma`].
    pub gamma: Option<f32>,
}

impl Metadata {
    /// Empty metadata.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or clear) the ICC profile.
    pub fn with_icc(mut self, icc: impl Into<Option<Vec<u8>>>) -> Self {
        self.icc = icc.into();
        self
    }

    /// Set (or clear) the Exif payload.
    pub fn with_exif(mut self, exif: impl Into<Option<Vec<u8>>>) -> Self {
        self.exif = exif.into();
        self
    }

    /// Set (or clear) the XMP packet.
    pub fn with_xmp(mut self, xmp: impl Into<Option<Vec<u8>>>) -> Self {
        self.xmp = xmp.into();
        self
    }

    /// Set (or clear) the file gamma.
    pub fn with_gamma(mut self, gamma: impl Into<Option<f32>>) -> Self {
        self.gamma = gamma.into();
        self
    }

    /// `true` when no field is set.
    pub fn is_empty(&self) -> bool {
        self.icc.is_none() && self.exif.is_none() && self.xmp.is_none() && self.gamma.is_none()
    }
}

/// Colour table of an indexed (`Pal8`) image: RGBA entries, index `i`
/// at `entries[i]`. PNG builds it from `PLTE` (RGB) plus the optional
/// `tRNS` alpha tail (entries past the tail are opaque, W3C PNG3
/// §11.3.1.1); the encoder writes `PLTE` from every entry and a `tRNS`
/// covering the entries up to the last non-opaque one.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Palette {
    /// `[r, g, b, a]` per entry, at most 256 entries.
    pub entries: Vec<[u8; 4]>,
}

impl Palette {
    /// Wrap a list of RGBA entries.
    pub fn new(entries: Vec<[u8; 4]>) -> Self {
        Self { entries }
    }

    /// Build from packed RGB triples (`PLTE` bytes) and an optional
    /// alpha tail (`tRNS` bytes). A trailing partial triple is dropped;
    /// alpha bytes beyond the entry count are ignored.
    pub fn from_rgb(rgb: &[u8], alpha: Option<&[u8]>) -> Self {
        let alpha = alpha.unwrap_or(&[]);
        let entries = rgb
            .chunks_exact(3)
            .enumerate()
            .map(|(i, e)| [e[0], e[1], e[2], alpha.get(i).copied().unwrap_or(255)])
            .collect();
        Self { entries }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the palette has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entry `index`, if present.
    pub fn get(&self, index: u8) -> Option<[u8; 4]> {
        self.entries.get(usize::from(index)).copied()
    }

    /// Packed RGB triples (the `PLTE` payload).
    pub fn to_rgb(&self) -> Vec<u8> {
        self.entries
            .iter()
            .flat_map(|e| [e[0], e[1], e[2]])
            .collect()
    }

    /// Alpha tail up to the last non-opaque entry (the `tRNS` payload),
    /// or `None` when every entry is opaque.
    pub fn alpha_tail(&self) -> Option<Vec<u8>> {
        let last = self.entries.iter().rposition(|e| e[3] != 255)?;
        Some(self.entries[..=last].iter().map(|e| e[3]).collect())
    }

    /// `true` when any entry is not fully opaque.
    pub fn has_alpha(&self) -> bool {
        self.entries.iter().any(|e| e[3] != 255)
    }
}

/// Decoded PNG image in its native layout, as returned by
/// [`crate::decode`] and consumed by [`crate::encode`].
///
/// `planes` holds exactly one packed plane (every PNG layout is
/// packed); `color` / `metadata` are filled from the file's colour and
/// metadata chunks; `palette` is `Some` for `Pal8`; `transparency` is
/// PNG's keyed `tRNS` transparency for the alpha-less `Gray*` / `Rgb*`
/// layouts (it is what makes [`Self::to_rgba8`] exact for them).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct PngImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Native pixel layout.
    pub format: PixelFormat,
    /// Pixel planes — exactly one for PNG.
    pub planes: Vec<Plane>,
    /// Colour signalling (range + H.273 code points).
    pub color: ColorInfo,
    /// ICC / Exif / XMP / gamma.
    pub metadata: Metadata,
    /// Colour table for `Pal8`.
    pub palette: Option<Palette>,
    /// Keyed transparency (`tRNS`) for `Gray8` / `Gray16Le` / `Rgb24`
    /// / `Rgb48Le`: the one sample value that is fully transparent.
    /// `None` for every other layout, and for `Pal8` (whose alpha
    /// lives in [`Self::palette`]).
    pub transparency: Option<Trns>,
}

impl PngImage {
    /// Assemble an image from its geometry, layout and planes (one for
    /// PNG). Colour is [`ColorInfo::png_default`], metadata empty, no
    /// palette, no transparency; the `with_*` builders fill those in.
    pub fn new(width: u32, height: u32, format: PixelFormat, planes: Vec<Plane>) -> Self {
        Self {
            width,
            height,
            format,
            planes,
            color: ColorInfo::png_default(),
            metadata: Metadata::default(),
            palette: None,
            transparency: None,
        }
    }

    /// One packed plane with an explicit row stride (`stride ≥ width ×
    /// bytes_per_pixel`, `data.len() ≥ stride × height`).
    pub fn packed(
        width: u32,
        height: u32,
        format: PixelFormat,
        stride: usize,
        data: Vec<u8>,
    ) -> Self {
        Self::new(width, height, format, vec![Plane::new(stride, data)])
    }

    /// Tightly packed `Rgb24` from `3 × width × height` bytes.
    pub fn from_rgb8(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self::packed(width, height, PixelFormat::Rgb24, width as usize * 3, data)
    }

    /// Tightly packed `Rgba` from `4 × width × height` bytes.
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self::packed(width, height, PixelFormat::Rgba, width as usize * 4, data)
    }

    /// Set the colour signalling.
    pub fn with_color(mut self, color: ColorInfo) -> Self {
        self.color = color;
        self
    }

    /// Set the metadata.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set (or clear) the palette.
    pub fn with_palette(mut self, palette: impl Into<Option<Palette>>) -> Self {
        self.palette = palette.into();
        self
    }

    /// Set (or clear) the keyed transparency.
    pub fn with_transparency(mut self, transparency: impl Into<Option<Trns>>) -> Self {
        self.transparency = transparency.into();
        self
    }

    /// Image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Native pixel layout.
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// Number of bytes per pixel for [`Self::format`].
    pub fn bytes_per_pixel(&self) -> usize {
        self.format.bytes_per_pixel()
    }

    /// Row stride in bytes of the pixel plane (`0` if the image has no
    /// plane).
    pub fn stride(&self) -> usize {
        self.planes.first().map(|p| p.stride).unwrap_or(0)
    }

    /// The pixel bytes — `Some` for every PNG image that has its plane
    /// (PNG layouts are all packed), `None` only for an image built
    /// without planes.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        self.planes.first().map(|p| p.data.as_slice())
    }

    /// Consume the image and return its plane bytes (planes
    /// concatenated in order, strides as reported).
    pub fn into_raw(self) -> Vec<u8> {
        let mut planes = self.planes.into_iter();
        let mut out = planes.next().map(|p| p.data).unwrap_or_default();
        for p in planes {
            out.extend_from_slice(&p.data);
        }
        out
    }

    /// `true` when the decoded pixels can be transparent: an alpha
    /// layout, a non-opaque palette entry or a keyed `tRNS` sample.
    pub fn has_alpha(&self) -> bool {
        self.format.has_alpha()
            || self.transparency.is_some()
            || self.palette.as_ref().is_some_and(Palette::has_alpha)
    }

    /// Pixel bytes of the single plane (empty if none).
    pub(crate) fn data(&self) -> &[u8] {
        self.as_bytes().unwrap_or(&[])
    }

    /// Mutable pixel bytes of the single plane.
    pub(crate) fn data_mut(&mut self) -> &mut [u8] {
        match self.planes.first_mut() {
            Some(p) => p.data.as_mut_slice(),
            None => &mut [],
        }
    }

    /// Tightly packed 8-bit RGBA, `4 × width` bytes per row, alpha
    /// `255` where the source has none.
    ///
    /// Exact integer kernels per layout (no gamma / colour
    /// management is applied — `color` and `metadata.gamma` describe
    /// the samples, they do not transform them):
    ///
    /// | Source     | RGBA                                                    |
    /// |------------|---------------------------------------------------------|
    /// | `Gray8`    | `(g,g,g,α)`, α=0 iff `transparency == Gray(g)`           |
    /// | `Gray16Le` | high byte per sample; the full 16-bit sample is compared with `transparency` first (W3C PNG3 §11.3.1.1) |
    /// | `Rgb24`    | `(r,g,b,α)`, α=0 iff every channel matches `transparency` |
    /// | `Rgb48Le`  | high byte per channel, 16-bit compare first            |
    /// | `Pal8`     | palette lookup (RGBA entry); an index past the palette is black-transparent |
    /// | `Ya8`      | `(g,g,g,a)`                                            |
    /// | `Rgba`     | copy                                                   |
    /// | `Rgba64Le` | high byte per channel                                  |
    ///
    /// 16-bit samples reduce by dropping the low-order byte, which W3C
    /// PNG3 §13.12 permits for display; the §13.12 linear rescale is
    /// [`crate::rescale_16bit_to_8bit`].
    pub fn to_rgba8(&self) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = vec![0u8; w * h * 4];
        if w == 0 || h == 0 {
            return out;
        }
        let bpp = self.bytes_per_pixel();
        let stride = self.stride();
        let src = self.data();
        let row_bytes = w * bpp;

        let key_gray: Option<u16> = match self.transparency {
            Some(Trns::Grayscale(g)) => Some(g),
            _ => None,
        };
        let key_rgb: Option<(u16, u16, u16)> = match self.transparency {
            Some(Trns::Rgb(r, g, b)) => Some((r, g, b)),
            _ => None,
        };

        // Palette lookup table: 256 RGBA cells; entries the palette
        // does not cover are black-transparent.
        let lut: [[u8; 4]; 256] = match (&self.palette, self.format) {
            (Some(p), PixelFormat::Pal8) => {
                let mut lut = [[0u8; 4]; 256];
                for (slot, e) in lut.iter_mut().zip(p.entries.iter()) {
                    *slot = *e;
                }
                lut
            }
            _ => [[0u8; 4]; 256],
        };

        for y in 0..h {
            let Some(row) = src.get(y * stride..y * stride + row_bytes) else {
                break;
            };
            let dst = &mut out[y * w * 4..(y + 1) * w * 4];
            match self.format {
                PixelFormat::Gray8 => {
                    let key = key_gray.and_then(|k| u8::try_from(k).ok());
                    for (&g, px) in row.iter().zip(dst.chunks_exact_mut(4)) {
                        px[0] = g;
                        px[1] = g;
                        px[2] = g;
                        px[3] = if key == Some(g) { 0 } else { 255 };
                    }
                }
                PixelFormat::Gray16Le => {
                    for (s, px) in row.chunks_exact(2).zip(dst.chunks_exact_mut(4)) {
                        let hi = s[1];
                        px[0] = hi;
                        px[1] = hi;
                        px[2] = hi;
                        let sample = u16::from_le_bytes([s[0], s[1]]);
                        px[3] = if key_gray == Some(sample) { 0 } else { 255 };
                    }
                }
                PixelFormat::Rgb24 => {
                    let key = key_rgb.and_then(|(r, g, b)| {
                        Some([
                            u8::try_from(r).ok()?,
                            u8::try_from(g).ok()?,
                            u8::try_from(b).ok()?,
                        ])
                    });
                    for (s, px) in row.chunks_exact(3).zip(dst.chunks_exact_mut(4)) {
                        px[0] = s[0];
                        px[1] = s[1];
                        px[2] = s[2];
                        px[3] = if key == Some([s[0], s[1], s[2]]) {
                            0
                        } else {
                            255
                        };
                    }
                }
                PixelFormat::Rgb48Le => {
                    for (s, px) in row.chunks_exact(6).zip(dst.chunks_exact_mut(4)) {
                        px[0] = s[1];
                        px[1] = s[3];
                        px[2] = s[5];
                        let r = u16::from_le_bytes([s[0], s[1]]);
                        let g = u16::from_le_bytes([s[2], s[3]]);
                        let b = u16::from_le_bytes([s[4], s[5]]);
                        px[3] = if key_rgb == Some((r, g, b)) { 0 } else { 255 };
                    }
                }
                PixelFormat::Pal8 => {
                    for (&idx, px) in row.iter().zip(dst.chunks_exact_mut(4)) {
                        px.copy_from_slice(&lut[idx as usize]);
                    }
                }
                PixelFormat::Ya8 => {
                    for (s, px) in row.chunks_exact(2).zip(dst.chunks_exact_mut(4)) {
                        px[0] = s[0];
                        px[1] = s[0];
                        px[2] = s[0];
                        px[3] = s[1];
                    }
                }
                PixelFormat::Rgba => {
                    dst.copy_from_slice(row);
                }
                PixelFormat::Rgba64Le => {
                    for (s, px) in row.chunks_exact(8).zip(dst.chunks_exact_mut(4)) {
                        px[0] = s[1];
                        px[1] = s[3];
                        px[2] = s[5];
                        px[3] = s[7];
                    }
                }
            }
        }
        out
    }

    /// Tightly packed 8-bit RGB, `3 × width` bytes per row. Same
    /// kernels as [`Self::to_rgba8`] with the alpha dropped (no
    /// compositing: a transparent pixel keeps its colour samples).
    pub fn to_rgb8(&self) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let mut out = vec![0u8; w * h * 3];
        if w == 0 || h == 0 {
            return out;
        }
        match self.format {
            // Direct kernels for the layouts where dropping alpha is
            // cheaper than going through RGBA.
            PixelFormat::Rgb24 => {
                let stride = self.stride();
                let src = self.data();
                for y in 0..h {
                    let Some(row) = src.get(y * stride..y * stride + w * 3) else {
                        break;
                    };
                    out[y * w * 3..(y + 1) * w * 3].copy_from_slice(row);
                }
            }
            PixelFormat::Gray8 => {
                let stride = self.stride();
                let src = self.data();
                for y in 0..h {
                    let Some(row) = src.get(y * stride..y * stride + w) else {
                        break;
                    };
                    for (&g, px) in row.iter().zip(out[y * w * 3..].chunks_exact_mut(3)) {
                        px[0] = g;
                        px[1] = g;
                        px[2] = g;
                    }
                }
            }
            _ => {
                let rgba = self.to_rgba8();
                for (s, d) in rgba.chunks_exact(4).zip(out.chunks_exact_mut(3)) {
                    d.copy_from_slice(&s[..3]);
                }
            }
        }
        out
    }
}

/// Tightly packed 8-bit RGB image: `width × height × 3` bytes,
/// row-major, channel order `R, G, B`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// `width × height × 3` bytes.
    pub data: Vec<u8>,
}

impl RgbImage {
    /// Wrap a tightly packed `width × height × 3` RGB buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume the image and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    /// Stride (bytes per row) — always `width × 3`.
    pub fn stride(&self) -> usize {
        self.width as usize * 3
    }
}

/// Tightly packed 8-bit RGBA image: `width × height × 4` bytes,
/// row-major, channel order `R, G, B, A`. Opaque source layouts are
/// promoted with `α = 255`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RgbaImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes.
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// Wrap a tightly packed `width × height × 4` RGBA buffer.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self {
            width,
            height,
            data,
        }
    }

    /// The pixel bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume the image and return the pixel bytes.
    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    /// Stride (bytes per row) — always `width × 4`.
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }
}

/// The pre-contract name of [`RgbaImage`] (same fields).
#[deprecated(note = "use oxideav_png::RgbaImage (IMAGE_CRATE_API)")]
pub type RgbaBitmap = RgbaImage;

/// One image of a multi-image file ([`crate::decode_all`]): for APNG,
/// a fully composited canvas and its display delay.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Frame {
    /// The composited canvas.
    pub image: PngImage,
    /// Display delay (`fcTL` `delay_num / delay_den`, a zero
    /// denominator meaning 1/100 s); `None` for a still image.
    pub delay: Option<Duration>,
}

impl Frame {
    /// Pair an image with its display delay.
    pub fn new(image: PngImage, delay: Option<Duration>) -> Self {
        Self { image, delay }
    }
}

/// What [`crate::info`] learns from the header and chunk walk without
/// decoding pixels.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ImageInfo {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// The layout [`crate::decode`] would return.
    pub format: PixelFormat,
    /// Number of images: `1` for a still PNG, `acTL.num_frames` for an
    /// APNG.
    pub frames: u32,
    /// `true` when pixels can be transparent (alpha layout or `tRNS`).
    pub has_alpha: bool,
    /// Colour signalling, resolved as [`crate::decode`] would.
    pub color: ColorInfo,
    /// An `iCCP` chunk is present.
    pub has_icc: bool,
    /// An `eXIf` chunk is present.
    pub has_exif: bool,
    /// An XMP `iTXt` chunk is present.
    pub has_xmp: bool,
    /// IHDR bit depth (1 / 2 / 4 / 8 / 16).
    pub bit_depth: u8,
    /// IHDR colour type (0 / 2 / 3 / 4 / 6).
    pub colour_type: u8,
    /// Adam7 interlaced.
    pub interlaced: bool,
    /// APNG loop count (`acTL.num_plays`, `0` = forever); `0` for a
    /// still PNG.
    pub num_plays: u32,
}

impl ImageInfo {
    /// Build a header description; `frames` 1, no alpha / metadata
    /// flags, default colour — fill the rest with field assignment.
    pub fn new(width: u32, height: u32, format: PixelFormat) -> Self {
        Self {
            width,
            height,
            format,
            frames: 1,
            has_alpha: format.has_alpha(),
            color: ColorInfo::png_default(),
            has_icc: false,
            has_exif: false,
            has_xmp: false,
            bit_depth: format.bits_per_sample(),
            colour_type: 0,
            interlaced: false,
            num_plays: 0,
        }
    }
}

/// Decoded animated PNG (APNG): one [`PngImage`] per frame plus a
/// per-frame delay in centiseconds (1/100 s — APNG's native unit), and
/// the loop count. [`crate::decode_all`] is the contract view of the
/// same frames.
#[derive(Clone, Debug)]
#[non_exhaustive]
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

impl ApngImage {
    /// Assemble an animation from its canvas geometry, frames and loop count (`0` = forever).
    pub fn new(
        width: u32,
        height: u32,
        pixel_format: PngPixelFormat,
        frames: Vec<ApngFrameImage>,
        num_plays: u32,
    ) -> Self {
        Self {
            width,
            height,
            pixel_format,
            frames,
            num_plays,
        }
    }
}

/// One composited APNG animation frame.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ApngFrameImage {
    /// Composited canvas at this animation step.
    pub image: PngImage,
    /// Frame display duration in centiseconds (1/100 s).
    pub delay_cs: u32,
    /// Exact frame display duration (`fcTL` `delay_num / delay_den`).
    pub delay: Duration,
}

impl ApngFrameImage {
    /// Pair a composited frame with its display delay in centiseconds.
    pub fn new(image: PngImage, delay_cs: u32) -> Self {
        Self {
            image,
            delay_cs,
            delay: Duration::from_millis(u64::from(delay_cs) * 10),
        }
    }

    /// Set the exact display duration.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_rgb_tail_roundtrip() {
        let p = Palette::from_rgb(&[1, 2, 3, 4, 5, 6, 7, 8, 9], Some(&[10, 255]));
        assert_eq!(p.len(), 3);
        assert_eq!(p.get(0), Some([1, 2, 3, 10]));
        assert_eq!(p.get(2), Some([7, 8, 9, 255]));
        assert_eq!(p.to_rgb(), vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(p.alpha_tail(), Some(vec![10]));
        let opaque = Palette::from_rgb(&[1, 2, 3], None);
        assert_eq!(opaque.alpha_tail(), None);
        assert!(!opaque.has_alpha());
    }

    #[test]
    fn to_rgba8_pal8_with_tail_and_out_of_range_index() {
        let img = PngImage::packed(3, 1, PixelFormat::Pal8, 3, vec![0, 1, 9])
            .with_palette(Palette::from_rgb(&[10, 20, 30, 40, 50, 60], Some(&[128])));
        assert_eq!(
            img.to_rgba8(),
            vec![10, 20, 30, 128, 40, 50, 60, 255, 0, 0, 0, 0]
        );
        assert_eq!(img.to_rgb8(), vec![10, 20, 30, 40, 50, 60, 0, 0, 0]);
    }

    #[test]
    fn to_rgba8_keyed_gray16_compares_both_bytes() {
        let img = PngImage::packed(2, 1, PixelFormat::Gray16Le, 4, vec![0x34, 0x12, 0x35, 0x12])
            .with_transparency(Trns::Grayscale(0x1234));
        assert_eq!(
            img.to_rgba8(),
            vec![0x12, 0x12, 0x12, 0, 0x12, 0x12, 0x12, 255]
        );
    }

    #[test]
    fn to_rgba8_honours_stride_padding() {
        let img = PngImage::packed(1, 2, PixelFormat::Rgb24, 4, vec![1, 2, 3, 99, 4, 5, 6, 99]);
        assert_eq!(img.to_rgba8(), vec![1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(img.to_rgb8(), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn into_raw_and_as_bytes() {
        let img = PngImage::from_rgba8(1, 1, vec![1, 2, 3, 4]);
        assert_eq!(img.as_bytes(), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(img.stride(), 4);
        assert_eq!(img.into_raw(), vec![1, 2, 3, 4]);
    }
}
