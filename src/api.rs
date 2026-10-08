//! The root vocabulary of the image-crate contract (`IMAGE_CRATE_API`):
//! `probe` / `info` / `decode*` / `encode*`, all framework-free.

use std::io::{Read, Write};

use crate::chunk::PNG_MAGIC;
use crate::decoder::{
    check_palette_indices, decode_apng_info, decode_image, image_to_rgba_checked, parse_all_chunks,
    parse_apng_chunks, Ihdr,
};
use crate::encoder::{encode as encode_image, EncodeOptions};
use crate::error::{PngError as Error, Result};
use crate::image::{Frame, ImageInfo, PngImage, RgbImage, RgbaImage};
use crate::options::DecodeOptions;

/// `true` when `bytes` starts with the eight-byte PNG signature (W3C
/// PNG3 §5.2). Allocation-free; `false` on short input.
pub fn probe(bytes: &[u8]) -> bool {
    bytes.len() >= PNG_MAGIC.len() && bytes[..PNG_MAGIC.len()] == PNG_MAGIC
}

/// Describe a PNG / APNG from its header and chunk walk without
/// decoding pixels: dimensions, the native layout [`crate::decode`]
/// would return, the frame count (`acTL.num_frames`, else `1`), alpha,
/// colour signalling and which metadata chunks are present.
///
/// The chunk walk validates every CRC and the critical-chunk ordering
/// exactly as a decode does; the colour / metadata chunks are read in
/// the lenient (`strict = false`) mode of [`DecodeOptions`].
pub fn info(bytes: &[u8]) -> Result<ImageInfo> {
    let chunks = parse_all_chunks(bytes)?;
    let ihdr = Ihdr::parse(
        chunks
            .iter()
            .find(|c| c.is_type(b"IHDR"))
            .ok_or_else(|| Error::invalid("PNG: missing IHDR"))?
            .data,
    )?;
    let format = ihdr.output_pixel_format()?;
    let side = crate::sideinfo::extract(&chunks, false, true)?;
    let mut out = ImageInfo::new(ihdr.width, ihdr.height, format);
    out.bit_depth = ihdr.bit_depth;
    out.colour_type = ihdr.colour_type;
    out.interlaced = ihdr.interlace != 0;
    // Alpha: an alpha layout, or a tRNS chunk on the alpha-less ones
    // (for Pal8 the tail may still be all-opaque; presence is the
    // header-level answer).
    let has_trns = chunks.iter().any(|c| c.is_type(b"tRNS"));
    out.has_alpha = format.has_alpha() || (has_trns && matches!(ihdr.colour_type, 0 | 2 | 3));
    out.color = side.color;
    out.has_icc = side.has_icc;
    out.has_exif = side.has_exif;
    out.has_xmp = side.has_xmp;
    if let Some(actl) = chunks.iter().find(|c| c.is_type(b"acTL")) {
        if let Ok(a) = crate::apng::Actl::parse(actl.data) {
            out.frames = a.num_frames.max(1);
            out.num_plays = a.num_plays;
        }
    }
    Ok(out)
}

/// Decode a PNG (or the default image of an APNG) into its native
/// layout with [`DecodeOptions::default`].
pub fn decode(bytes: &[u8]) -> Result<PngImage> {
    decode_image(bytes, &DecodeOptions::default())
}

/// [`decode`] under explicit limits / strictness / metadata inflation
/// ([`DecodeOptions`]).
pub fn decode_with(bytes: &[u8], opts: &DecodeOptions) -> Result<PngImage> {
    decode_image(bytes, opts)
}

/// Decode straight to tightly packed 8-bit RGB (alpha dropped, no
/// compositing). See [`PngImage::to_rgb8`] for the per-layout kernels.
pub fn decode_rgb8(bytes: &[u8]) -> Result<RgbImage> {
    let img = decode(bytes)?;
    check_palette_indices(&img)?;
    Ok(RgbImage::new(img.width, img.height, img.to_rgb8()))
}

/// Decode straight to tightly packed 8-bit RGBA (alpha `255` where the
/// source has none; `tRNS` and palette alpha applied). See
/// [`PngImage::to_rgba8`] for the per-layout kernels.
pub fn decode_rgba8(bytes: &[u8]) -> Result<RgbaImage> {
    let img = decode(bytes)?;
    image_to_rgba_checked(&img)
}

/// Every image of the file: one [`Frame`] for a still PNG (delay
/// `None`), the composited animation frames of an APNG (each a full
/// canvas, delay from its `fcTL`). Uses [`DecodeOptions::default`].
pub fn decode_all(bytes: &[u8]) -> Result<Vec<Frame>> {
    decode_all_with(bytes, &DecodeOptions::default())
}

/// [`decode_all`] under explicit limits / strictness / metadata
/// inflation ([`DecodeOptions`]).
pub fn decode_all_with(bytes: &[u8], opts: &DecodeOptions) -> Result<Vec<Frame>> {
    let chunks = parse_all_chunks(bytes)?;
    let animated = chunks.iter().any(|c| c.is_type(b"acTL"));
    if !animated {
        let img = crate::decoder::decode_png_chunks(&chunks, opts)?;
        return Ok(vec![Frame::new(img, None)]);
    }
    let info = parse_apng_chunks(&chunks, opts)?;
    let apng = decode_apng_info(&info)?;
    Ok(apng
        .frames
        .into_iter()
        .map(|f| Frame::new(f.image, Some(f.delay)))
        .collect())
}

/// Encode `frames` as one file — the mirror of [`decode_all`]. A single
/// frame with no delay is written as a plain PNG; anything else as an
/// APNG whose canvas is the first frame's geometry, every frame
/// painted full-canvas (`Disposal::None` / `Blend::Source`), so every
/// frame must share the first one's `width` / `height` / `format`
/// (and palette, for `Pal8`). The loop count is
/// [`EncodeOptions::num_plays`].
///
/// A frame's `delay` becomes the finest `fcTL` rational whose
/// numerator fits 16 bits — milliseconds up to 65.535 s, then
/// centiseconds, tenths and whole seconds (capped at 65535 s); `None`
/// writes `0/100` (the shortest delay the viewer supports, which
/// [`decode_all`] reads back as `Some(0)`). `decode_all(encode_all(f))
/// == f` therefore holds for frames with whole-millisecond delays.
///
/// The depth entry points [`crate::encode_apng`] (one shared delay) and
/// [`crate::encode_apng_frames`] (sub-regions, dispose / blend, a
/// separate default image) stay for callers who need them.
pub fn encode_all(frames: &[Frame], opts: &EncodeOptions) -> Result<Vec<u8>> {
    match frames {
        [] => Err(Error::invalid(
            "PNG encoder: encode_all needs at least one frame",
        )),
        [single] if single.delay.is_none() => encode_image(&single.image, opts),
        _ => {
            let regions: Vec<(&PngImage, u16, u16)> = frames
                .iter()
                .map(|f| {
                    let (num, den) = fctl_delay(f.delay);
                    (&f.image, num, den)
                })
                .collect();
            crate::encoder::encode_apng_full_canvas(&regions, opts.num_plays, opts)
        }
    }
}

/// `fcTL` `delay_num / delay_den` for a frame delay: the finest of
/// 1/1000, 1/100, 1/10 and 1 s whose numerator fits `u16`.
fn fctl_delay(delay: Option<std::time::Duration>) -> (u16, u16) {
    let Some(delay) = delay else {
        return (0, 100);
    };
    let micros = delay.as_micros();
    for den in [1000u128, 100, 10, 1] {
        let num = micros * den / 1_000_000;
        if let Ok(num) = u16::try_from(num) {
            return (num, den as u16);
        }
    }
    (u16::MAX, 1)
}

/// Read `r` to its end and [`decode`] the bytes.
pub fn decode_from<R: Read>(mut r: R) -> Result<PngImage> {
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)?;
    decode(&buf)
}

/// Encode `image` as a PNG file. See [`crate::encoder::encode`] for
/// what the image's side fields become on the wire.
pub fn encode(image: &PngImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    encode_image(image, opts)
}

/// Encode tightly packed 8-bit RGB (`3 × width × height` bytes) as a
/// colour-type-2 PNG.
pub fn encode_rgb8(width: u32, height: u32, rgb: &[u8], opts: &EncodeOptions) -> Result<Vec<u8>> {
    check_raw_len(width, height, 3, rgb.len())?;
    encode_image(&PngImage::from_rgb8(width, height, rgb.to_vec())?, opts)
}

/// Encode tightly packed 8-bit RGBA (`4 × width × height` bytes) as a
/// colour-type-6 PNG.
pub fn encode_rgba8(width: u32, height: u32, rgba: &[u8], opts: &EncodeOptions) -> Result<Vec<u8>> {
    check_raw_len(width, height, 4, rgba.len())?;
    encode_image(&PngImage::from_rgba8(width, height, rgba.to_vec())?, opts)
}

/// [`encode`] straight into a writer.
pub fn encode_to<W: Write>(image: &PngImage, opts: &EncodeOptions, mut w: W) -> Result<()> {
    let bytes = encode_image(image, opts)?;
    w.write_all(&bytes)?;
    Ok(())
}

fn check_raw_len(width: u32, height: u32, bpp: usize, len: usize) -> Result<()> {
    let need = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(bpp))
        .ok_or_else(|| Error::invalid("PNG encoder: dimensions overflow"))?;
    if len < need {
        return Err(Error::invalid(format!(
            "PNG encoder: {width}x{height} at {bpp} bytes/pixel needs {need} bytes, got {len}"
        )));
    }
    Ok(())
}

// ---- Pre-contract names, kept for one release -------------------------

/// The pre-contract name of [`decode`].
#[deprecated(note = "use oxideav_png::decode (IMAGE_CRATE_API)")]
pub fn decode_png(bytes: &[u8]) -> Result<PngImage> {
    decode(bytes)
}

/// The pre-contract name of [`decode_rgba8`].
#[deprecated(note = "use oxideav_png::decode_rgba8 (IMAGE_CRATE_API)")]
pub fn decode_png_to_rgba(bytes: &[u8]) -> Result<RgbaImage> {
    decode_rgba8(bytes)
}

/// The pre-contract name of [`crate::decode_over_background`].
#[deprecated(note = "use oxideav_png::decode_over_background (IMAGE_CRATE_API naming)")]
pub fn decode_png_over_background(bytes: &[u8], override_bg: Option<[u8; 3]>) -> Result<RgbaImage> {
    crate::decoder::decode_over_background(bytes, override_bg)
}
