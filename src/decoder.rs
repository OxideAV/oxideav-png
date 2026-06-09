//! PNG + APNG decoder.
//!
//! High level flow:
//!
//! 1. Verify the 8-byte magic, then read chunks until IEND.
//! 2. `IHDR` gives width / height / bit depth / colour type.
//! 3. `PLTE` + `tRNS` feed the palette for colour type 3 (and alpha for 0/2).
//! 4. `acTL` + `fcTL` + `fdAT` carry animation frame metadata / data.
//! 5. Each frame's compressed stream = concatenation of `IDAT` (for default
//!    image) or `fdAT[4..]` (for animation frames) → `miniz_oxide` zlib
//!    decode → reverse per-row filters → fill a [`PngImage`] (or, behind
//!    the `registry` feature, an `oxideav_core::VideoFrame`).
//!
//! Output pixel formats (no internal conversion — the framework's pixfmt
//! graph node handles that):
//!
//! - colour type 0 / 1-2-4-bit → `Gray8` (scaled up via ×255/×85/×17)
//! - colour type 0 / 8-bit  → `Gray8`
//! - colour type 0 / 16-bit → `Gray16Le` (network byte order collapsed to LE on output)
//! - colour type 2 / 8-bit  → `Rgb24`
//! - colour type 2 / 16-bit → `Rgb48Le`
//! - colour type 3 / 1-2-4-bit → `Pal8` (one index byte per pixel after unpacking)
//! - colour type 3 / 8-bit  → `Pal8` (palette embedded into extradata)
//! - colour type 4 / 8-bit  → `Ya8` (gray + alpha)
//! - colour type 4 / 16-bit → `Rgba64Le` (PNG has no native Ya16 — we expand)
//! - colour type 6 / 8-bit  → `Rgba`
//! - colour type 6 / 16-bit → `Rgba64Le`
//!
//! Adam7 interlaced streams (IHDR interlace=1) are decoded seven passes at
//! a time per §A.8 and scattered into the final canvas.

use crate::error::{PngError as Error, Result};
use crate::image::{ApngFrameImage, ApngImage, PngImage, PngPixelFormat, RgbaBitmap};

// Backward-compat re-exports: existing callers reach for
// `oxideav_png::decoder::make_decoder` and
// `oxideav_png::decoder::decode_png_to_frame`. Keep both paths live
// by re-exporting the registry-side surface.
#[cfg(feature = "registry")]
pub use crate::registry::{decode_png_to_frame, make_decoder};

use miniz_oxide::inflate::decompress_to_vec_zlib;

use crate::apng::{parse_fdat, Actl, Blend, Disposal, Fctl};
use crate::chunk::{read_chunk, ChunkRef, PNG_MAGIC};
use crate::filter::{unfilter_row, FilterType};
use crate::metadata::{
    Bkgd, Chrm, Cicp, Clli, Exif, Gama, Hist, Iccp, Itxt, Mdcv, Phys, PngMetadata, Sbit, Splt,
    Srgb, Text, Time, Trns, Ztxt,
};

pub const CODEC_ID_STR: &str = "png";

/// Backward-compat alias for [`decode_apng_info`] — older callers
/// reach for `decode_apng_frames(&ApngInfo)`. Returns the per-frame
/// `VideoFrame` list when the `registry` feature is on, otherwise the
/// composited [`ApngImage::frames`] list. New code should call
/// [`decode_apng_info`] (standalone) or
/// [`crate::registry::decode_png_to_frame`] (framework-side).
#[cfg(feature = "registry")]
pub fn decode_apng_frames(info: &ApngInfo) -> oxideav_core::Result<Vec<oxideav_core::VideoFrame>> {
    let img = decode_apng_info(info)?;
    let mut out = Vec::with_capacity(img.frames.len());
    let mut pts: i64 = 0;
    for f in &img.frames {
        out.push(oxideav_core::VideoFrame {
            pts: Some(pts),
            planes: vec![oxideav_core::VideoPlane {
                stride: f.image.stride,
                data: f.image.data.clone(),
            }],
        });
        pts += f.delay_cs as i64;
    }
    Ok(out)
}

// ---- IHDR ---------------------------------------------------------------

/// Parsed IHDR chunk (13 bytes).
#[derive(Clone, Copy, Debug)]
pub struct Ihdr {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub colour_type: u8,
    pub compression: u8,
    pub filter: u8,
    pub interlace: u8,
}

impl Ihdr {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 13 {
            return Err(Error::invalid(format!(
                "PNG IHDR: expected 13 bytes, got {}",
                data.len()
            )));
        }
        let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        Ok(Self {
            width,
            height,
            bit_depth: data[8],
            colour_type: data[9],
            compression: data[10],
            filter: data[11],
            interlace: data[12],
        })
    }

    pub fn to_bytes(&self) -> [u8; 13] {
        let mut out = [0u8; 13];
        out[0..4].copy_from_slice(&self.width.to_be_bytes());
        out[4..8].copy_from_slice(&self.height.to_be_bytes());
        out[8] = self.bit_depth;
        out[9] = self.colour_type;
        out[10] = self.compression;
        out[11] = self.filter;
        out[12] = self.interlace;
        out
    }

    /// Number of channels implied by `colour_type`.
    pub fn channels(&self) -> Result<usize> {
        Ok(self.colour_type_typed()?.channels())
    }

    /// Decode the raw IHDR colour-type byte into the typed
    /// [`crate::chunk::ColourType`] enum (W3C PNG3 §6.1 / Table 9).
    /// Errors with `InvalidData` when the byte is outside the
    /// `{0, 2, 3, 4, 6}` set of valid values — the same rejection
    /// the existing `channels()` returned.
    pub fn colour_type_typed(&self) -> Result<crate::chunk::ColourType> {
        crate::chunk::ColourType::from_byte(self.colour_type)
            .ok_or_else(|| Error::invalid(format!("PNG: bad colour type {}", self.colour_type)))
    }

    /// W3C PNG3 §11.2.1 Table 12 ("Allowed combinations of color type
    /// and bit depth"): `true` when the IHDR (colour_type, bit_depth)
    /// pair is one of the table's rows. Returns `Err` when
    /// `colour_type` itself is out of range. The decoder's wire-byte
    /// validation routes through this so the Table 12 acceptance
    /// criterion lives in one place rather than being re-derived at
    /// every gate.
    pub fn is_allowed_combination(&self) -> Result<bool> {
        Ok(self.colour_type_typed()?.allows_bit_depth(self.bit_depth))
    }

    /// Bytes per full pixel (rounded up to at least 1 for filtering
    /// purposes). For sub-byte bit depths this is 1 regardless of channel
    /// count, per the PNG spec.
    pub fn bpp_for_filter(&self) -> Result<usize> {
        let channels = self.channels()?;
        let bits = channels * self.bit_depth as usize;
        Ok(bits.div_ceil(8))
    }

    /// Bytes per row of unfiltered pixel data.
    pub fn row_bytes(&self) -> Result<usize> {
        let channels = self.channels()?;
        let bits_per_pixel = channels * self.bit_depth as usize;
        let bits_per_row = bits_per_pixel * self.width as usize;
        Ok(bits_per_row.div_ceil(8))
    }

    /// Pixel format the decoder produces for this IHDR.
    pub fn output_pixel_format(&self) -> Result<PngPixelFormat> {
        Ok(match (self.colour_type, self.bit_depth) {
            // Grayscale sub-8-bit is expanded to Gray8 during decode (scale
            // from bit_depth-max to 255).
            (0, 1) | (0, 2) | (0, 4) | (0, 8) => PngPixelFormat::Gray8,
            (0, 16) => PngPixelFormat::Gray16Le,
            (2, 8) => PngPixelFormat::Rgb24,
            (2, 16) => PngPixelFormat::Rgb48Le,
            // Indexed sub-8-bit is expanded to Pal8 (one index byte per
            // pixel) during decode.
            (3, 1) | (3, 2) | (3, 4) | (3, 8) => PngPixelFormat::Pal8,
            (4, 8) => PngPixelFormat::Ya8,
            (4, 16) => PngPixelFormat::Rgba64Le,
            (6, 8) => PngPixelFormat::Rgba,
            (6, 16) => PngPixelFormat::Rgba64Le,
            (ct, bd) => {
                return Err(Error::unsupported(format!(
                    "PNG: colour type {ct} bit depth {bd} not implemented"
                )))
            }
        })
    }

    /// Number of bytes in one logical pixel of the *decoded* byte-plane that
    /// the decoder hands to `build_png_image`. For sub-8-bit gray/indexed,
    /// this is 1 after expansion. For ≥8-bit it's `channels * (bit_depth/8)`.
    pub fn decoded_bytes_per_pixel(&self) -> Result<usize> {
        if self.bit_depth < 8 {
            // Only valid for grayscale / indexed — RGB, Ya, RGBA forbid
            // sub-byte depths per the PNG spec.
            if self.colour_type != 0 && self.colour_type != 3 {
                return Err(Error::invalid(format!(
                    "PNG: colour type {} cannot have bit depth {}",
                    self.colour_type, self.bit_depth
                )));
            }
            return Ok(1);
        }
        let channels = self.channels()?;
        Ok(channels * (self.bit_depth as usize / 8))
    }
}

// ---- The actual decode --------------------------------------------------

/// Iterate chunks from `buf[8..]`, returning a vector. The signature is
/// borrowed into the returned references. Fails fast on CRC error.
pub(crate) fn parse_all_chunks(buf: &[u8]) -> Result<Vec<ChunkRef<'_>>> {
    if buf.len() < 8 || buf[0..8] != PNG_MAGIC {
        return Err(Error::invalid("PNG: missing magic bytes"));
    }
    let mut out = Vec::new();
    let mut pos = 8;
    loop {
        let (chunk, next) = read_chunk(buf, pos)?;
        let is_iend = chunk.chunk_type == *b"IEND";
        out.push(chunk);
        pos = next;
        if is_iend {
            break;
        }
        if pos >= buf.len() {
            return Err(Error::invalid("PNG: stream ended before IEND"));
        }
    }
    Ok(out)
}

/// Extract round-trippable PNG ancillary metadata (`sBIT`, `pHYs`,
/// `tIME`, `bKGD`, `hIST`, `eXIf`, `sRGB`, `cICP`, `gAMA`, `cHRM`,
/// `mDCV`, `cLLI`, `sPLT`, `tEXt`, `zTXt`, `iCCP`, `iTXt`) from a
/// PNG / APNG file.
///
/// Standalone (no `oxideav-core`) entry point: works whether or not
/// the `registry` feature is enabled. Returns
/// [`PngMetadata::default`] (all-`None`, empty `splt`) when none of the
/// supported chunks are present. Reports an `InvalidData` error if any
/// single-instance chunk appears more than once — RFC 2083 §4.3 / W3C
/// PNG3 §5.6 mark them all as "Multiple OK? No". `sPLT` ("Multiple OK?
/// Yes") instead requires distinct palette names; a repeated name is an
/// `InvalidData` error.
///
/// CRC validation is performed by the underlying chunk walker
/// ([`crate::chunk::read_chunk`]) so a tampered chunk fails before
/// reaching this parser.
///
/// Cross-chunk constraints enforced here:
/// * `hIST` requires a `PLTE` (W3C PNG3 §11.3.4.2 "A histogram chunk
///   can appear only when a PLTE chunk appears"); the entry count must
///   match the `PLTE` entry count.
/// * `bKGD` for indexed images (colour type 3) similarly requires a
///   `PLTE` chunk; the palette index byte must address an existing
///   entry.
pub fn parse_metadata(buf: &[u8]) -> Result<PngMetadata> {
    let chunks = parse_all_chunks(buf)?;
    let ihdr = Ihdr::parse(
        chunks
            .iter()
            .find(|c| c.is_type(b"IHDR"))
            .ok_or_else(|| Error::invalid("PNG: missing IHDR"))?
            .data,
    )?;
    // sBIT sample-depth ceiling: 8 for indexed (RFC 2083 §4.2.6 final
    // paragraph "the sample depth, which is 8 for indexed-color images,
    // and the bit depth given in IHDR for other color types").
    let sample_depth = if ihdr.colour_type == 3 {
        8
    } else {
        ihdr.bit_depth
    };
    // PLTE length / entry count — driven by IHDR's colour type. The
    // chunk's payload byte count must be a multiple of 3; the entry
    // count is that / 3. We need it to validate hIST length and bKGD
    // palette indices.
    let plte_entries = chunks
        .iter()
        .find(|c| c.is_type(b"PLTE"))
        .map(|c| {
            if c.data.len() % 3 != 0 {
                return Err(Error::invalid(format!(
                    "PNG PLTE: payload length {} not a multiple of 3",
                    c.data.len()
                )));
            }
            Ok(c.data.len() / 3)
        })
        .transpose()?;

    let mut out = PngMetadata::default();
    for c in &chunks {
        match &c.chunk_type {
            b"sBIT" => {
                if out.sbit.is_some() {
                    return Err(Error::invalid("PNG: duplicate sBIT chunk"));
                }
                out.sbit = Some(Sbit::parse(c.data, ihdr.colour_type, sample_depth)?);
            }
            b"pHYs" => {
                if out.phys.is_some() {
                    return Err(Error::invalid("PNG: duplicate pHYs chunk"));
                }
                out.phys = Some(Phys::parse(c.data)?);
            }
            b"tIME" => {
                if out.time.is_some() {
                    return Err(Error::invalid("PNG: duplicate tIME chunk"));
                }
                out.time = Some(Time::parse(c.data)?);
            }
            b"bKGD" => {
                if out.bkgd.is_some() {
                    return Err(Error::invalid("PNG: duplicate bKGD chunk"));
                }
                let b = Bkgd::parse(c.data, ihdr.colour_type, ihdr.bit_depth)?;
                if let Bkgd::Palette(idx) = b {
                    match plte_entries {
                        None => {
                            return Err(Error::invalid(
                                "PNG bKGD: indexed colour type requires a PLTE chunk",
                            ))
                        }
                        Some(n) if (idx as usize) >= n => {
                            return Err(Error::invalid(format!(
                                "PNG bKGD: palette index {idx} out of range (PLTE has {n} entries)",
                            )))
                        }
                        _ => {}
                    }
                }
                out.bkgd = Some(b);
            }
            b"hIST" => {
                if out.hist.is_some() {
                    return Err(Error::invalid("PNG: duplicate hIST chunk"));
                }
                let n = plte_entries.ok_or_else(|| {
                    Error::invalid("PNG hIST: chunk requires a PLTE chunk (PNG3 §11.3.4.2)")
                })?;
                out.hist = Some(Hist::parse(c.data, n)?);
            }
            b"tRNS" => {
                // W3C PNG3 §5.6 Table 1: tRNS is "Multiple OK? No".
                // RFC 2083 §4.2.9 forbids the chunk on colour types 4 / 6
                // and constrains the variant to the IHDR colour type
                // (Trns::parse enforces both). For colour type 3 the
                // chunk length is bounded by the PLTE entry count;
                // missing PLTE on ct=3 is itself an error.
                if out.trns.is_some() {
                    return Err(Error::invalid("PNG: duplicate tRNS chunk"));
                }
                out.trns = Some(Trns::parse(
                    c.data,
                    ihdr.colour_type,
                    ihdr.bit_depth,
                    plte_entries,
                )?);
            }
            b"eXIf" => {
                if out.exif.is_some() {
                    return Err(Error::invalid("PNG: duplicate eXIf chunk"));
                }
                out.exif = Some(Exif::parse(c.data)?);
            }
            b"sRGB" => {
                if out.srgb.is_some() {
                    return Err(Error::invalid("PNG: duplicate sRGB chunk"));
                }
                out.srgb = Some(Srgb::parse(c.data)?);
            }
            b"cICP" => {
                if out.cicp.is_some() {
                    return Err(Error::invalid("PNG: duplicate cICP chunk"));
                }
                out.cicp = Some(Cicp::parse(c.data)?);
            }
            b"gAMA" => {
                if out.gama.is_some() {
                    return Err(Error::invalid("PNG: duplicate gAMA chunk"));
                }
                out.gama = Some(Gama::parse(c.data)?);
            }
            b"cHRM" => {
                if out.chrm.is_some() {
                    return Err(Error::invalid("PNG: duplicate cHRM chunk"));
                }
                out.chrm = Some(Chrm::parse(c.data)?);
            }
            b"mDCV" => {
                // W3C PNG3 §5.6 Table 1: mDCV is "Multiple OK? No". The
                // §4.3 colour-chunk-priority table does not list mDCV
                // explicitly (it is supplemental HDR static metadata that
                // a §11.3.2.7 reader interprets *together with* cICP);
                // we round-trip the bytes verbatim and leave that
                // policy choice to the caller.
                if out.mdcv.is_some() {
                    return Err(Error::invalid("PNG: duplicate mDCV chunk"));
                }
                out.mdcv = Some(Mdcv::parse(c.data)?);
            }
            b"cLLI" => {
                // W3C PNG3 §5.6 Table 1: cLLI is "Multiple OK? No". Like
                // mDCV, it is HDR static metadata that pairs with cICP
                // for tone-mapping decisions and is round-tripped
                // verbatim. A zero value is the spec's "unknown / not
                // currently calculable" sentinel (§11.3.2.8); the codec
                // preserves it rather than rejecting.
                if out.clli.is_some() {
                    return Err(Error::invalid("PNG: duplicate cLLI chunk"));
                }
                out.clli = Some(Clli::parse(c.data)?);
            }
            b"sPLT" => {
                // Multiple sPLT chunks are permitted, but each shall have
                // a distinct palette name (W3C PNG3 §11.3.4.4 final
                // paragraph). Reject a repeated name.
                let splt = Splt::parse(c.data)?;
                if out.splt.iter().any(|s| s.name == splt.name) {
                    return Err(Error::invalid(format!(
                        "PNG sPLT: duplicate palette name {:?} (names must be distinct)",
                        splt.name
                    )));
                }
                out.splt.push(splt);
            }
            b"tEXt" => {
                // RFC 2083 §4.2.7: "Any number of tEXt chunks can
                // appear, and more than one with the same keyword is
                // permissible." No uniqueness check — only the per-chunk
                // structural validation in Text::parse. Preserve file
                // order so the encoder can replay it verbatim.
                out.texts.push(Text::parse(c.data)?);
            }
            b"zTXt" => {
                // RFC 2083 §4.2.10 ¶6: "Any number of zTXt and tEXt
                // chunks can appear in the same file." Same
                // no-uniqueness-check rule as `tEXt`; preserve file
                // order so the encoder can replay it verbatim.
                out.ztxts.push(Ztxt::parse(c.data)?);
            }
            b"iCCP" => {
                // W3C PNG3 §5.6 Table 1: "Multiple OK? No" for iCCP.
                // The chunk also has higher-precedence members of the
                // §4.3 colour-chunk table (cICP `1` > iCCP `2`); the
                // codec keeps them as independent fields and leaves
                // policy to the caller.
                if out.iccp.is_some() {
                    return Err(Error::invalid("PNG: duplicate iCCP chunk"));
                }
                out.iccp = Some(Iccp::parse(c.data)?);
            }
            b"iTXt" => {
                // W3C PNG3 §11.3.3.4: iTXt obeys the same multi-instance
                // rule as `tEXt` / `zTXt` — "Any number of [textual]
                // chunks can appear in the same file" (implicit in the
                // Table 1 `Multiple OK? Yes / Ordering: None` row).
                // Preserve file order so the encoder can replay it
                // verbatim.
                out.itxts.push(Itxt::parse(c.data)?);
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Decode a single non-animated PNG file (or the "default image" of an
/// APNG) into a [`PngImage`]. Standalone (no `oxideav-core`) entry
/// point: works whether or not the `registry` feature is enabled.
pub fn decode_png(buf: &[u8]) -> Result<PngImage> {
    let chunks = parse_all_chunks(buf)?;
    let ihdr_chunk = chunks
        .iter()
        .find(|c| c.is_type(b"IHDR"))
        .ok_or_else(|| Error::invalid("PNG: missing IHDR"))?;
    let ihdr = Ihdr::parse(ihdr_chunk.data)?;
    if ihdr.interlace != 0 && ihdr.interlace != 1 {
        return Err(Error::invalid(format!(
            "PNG: unknown interlace method {}",
            ihdr.interlace
        )));
    }
    if ihdr.compression != 0 {
        return Err(Error::invalid("PNG: unknown compression method"));
    }
    if ihdr.filter != 0 {
        return Err(Error::invalid("PNG: unknown filter method"));
    }

    let mut plte: Option<&[u8]> = None;
    let mut trns: Option<&[u8]> = None;
    let mut idat_total_len = 0usize;
    for c in &chunks {
        if c.is_type(b"PLTE") {
            plte = Some(c.data);
        } else if c.is_type(b"tRNS") {
            trns = Some(c.data);
        } else if c.is_type(b"IDAT") {
            idat_total_len += c.data.len();
        }
    }
    validate_trns(&ihdr, trns, plte)?;

    let mut idat_concat = Vec::with_capacity(idat_total_len);
    for c in &chunks {
        if c.is_type(b"IDAT") {
            idat_concat.extend_from_slice(c.data);
        }
    }
    if idat_concat.is_empty() {
        return Err(Error::invalid("PNG: no IDAT chunks"));
    }

    let pixels = decompress_to_vec_zlib(&idat_concat)
        .map_err(|e| Error::invalid(format!("PNG: zlib decompress failed: {e:?}")))?;

    let frame_pixels = decode_image_pixels(&pixels, &ihdr)?;
    build_png_image(&ihdr, &frame_pixels, plte, trns)
}

/// Enforce the per-IHDR rules RFC 2083 §4.2.9 places on a `tRNS` chunk.
///
/// * Colour type 0 (grayscale): exactly 2 bytes — the transparent gray
///   sample. The 2-byte width is fixed regardless of `bit_depth`
///   (§4.2.9 "For consistency, 2 bytes are used regardless of the
///   image bit depth").
/// * Colour type 2 (truecolor): exactly 6 bytes — the transparent RGB
///   triple, 2 bytes per channel.
/// * Colour type 3 (indexed): one byte per palette entry, *at most* one
///   alpha value per PLTE entry (§4.2.9 "tRNS chunk must not contain
///   more alpha values than there are palette entries"). Fewer than
///   the full PLTE count is permitted; missing tail entries are opaque.
/// * Colour types 4 + 6: `tRNS` is "prohibited" (§4.2.9 final
///   paragraph). The presence of the chunk is an InvalidData error.
///
/// `plte_len_bytes` is the byte-length of any PLTE chunk so the indexed
/// case can range-check `trns.len()` against the entry count.
fn validate_trns(ihdr: &Ihdr, trns: Option<&[u8]>, plte: Option<&[u8]>) -> Result<()> {
    let Some(t) = trns else {
        return Ok(());
    };
    match ihdr.colour_type {
        0 => {
            if t.len() != 2 {
                return Err(Error::invalid(format!(
                    "PNG tRNS (colour type 0): expected 2 bytes, got {}",
                    t.len()
                )));
            }
            // The 2-byte sample is stored as the gray value at the
            // image's bit depth (RFC 2083 §4.2.9: "range 0 ..
            // (2^bitdepth)-1"). The high-order bits beyond bit_depth
            // must therefore be zero — anything else is a malformed
            // tRNS and would never match a real sample.
            let v = u16::from_be_bytes([t[0], t[1]]);
            let max = (1u32 << ihdr.bit_depth) - 1;
            if (v as u32) > max {
                return Err(Error::invalid(format!(
                    "PNG tRNS (colour type 0, bit depth {}): gray value {v} exceeds {}",
                    ihdr.bit_depth, max
                )));
            }
        }
        2 => {
            if t.len() != 6 {
                return Err(Error::invalid(format!(
                    "PNG tRNS (colour type 2): expected 6 bytes, got {}",
                    t.len()
                )));
            }
            let max = (1u32 << ihdr.bit_depth) - 1;
            for (i, name) in ["R", "G", "B"].iter().enumerate() {
                let v = u16::from_be_bytes([t[i * 2], t[i * 2 + 1]]);
                if (v as u32) > max {
                    return Err(Error::invalid(format!(
                        "PNG tRNS (colour type 2, bit depth {}): {name} value {v} exceeds {}",
                        ihdr.bit_depth, max
                    )));
                }
            }
        }
        3 => {
            let plte_len = plte.map(|p| p.len()).unwrap_or(0);
            if plte_len == 0 {
                return Err(Error::invalid(
                    "PNG tRNS (colour type 3): missing PLTE chunk",
                ));
            }
            let plte_entries = plte_len / 3;
            if t.len() > plte_entries {
                return Err(Error::invalid(format!(
                    "PNG tRNS (colour type 3): {} alpha values exceed {} PLTE entries",
                    t.len(),
                    plte_entries
                )));
            }
        }
        4 | 6 => {
            return Err(Error::invalid(format!(
                "PNG tRNS: prohibited for colour type {} (alpha channel already present)",
                ihdr.colour_type
            )));
        }
        _ => {
            // Unknown colour types fail earlier in IHDR parsing; nothing
            // additional to validate here.
        }
    }
    Ok(())
}

/// Decode a PNG (any supported colour type / bit depth) and promote the
/// result to an 8-bit-per-channel [`RgbaBitmap`].
///
/// One-shot convenience entry point for callers that just want pixels
/// to blit — palette resolution (`Pal8` + `PLTE` + `tRNS`), grayscale
/// widening, 16→8 truncation and α-fill for opaque formats are all
/// handled internally. For colour type 0 / 2 a `tRNS` chunk (RFC 2083
/// §4.2.9) is applied at this layer too: the named source sample
/// emerges with α=0, every other pixel with α=255 (when no `tRNS` is
/// present the table column below collapses to "α=255 always").
///
/// | Source format | RGBA promotion                                       |
/// |---------------|------------------------------------------------------|
/// | `Gray8`       | `(g,g,g, α)` — α=0 iff `tRNS.gray == g`              |
/// | `Gray16Le`    | `(hi,hi,hi, α)` — α=0 iff `tRNS.gray == sample16` (both bytes per §4.2.9 note) |
/// | `Rgb24`       | `(r,g,b, α)` — α=0 iff every channel matches `tRNS`  |
/// | `Rgb48Le`     | `(r_hi,g_hi,b_hi, α)` — α=0 iff every 16-bit channel matches `tRNS` |
/// | `Pal8`        | `PLTE` lookup + `tRNS` alpha (`255` for entries past `tRNS`'s end) |
/// | `Ya8`         | `(g,g,g,a)`                                          |
/// | `Rgba`        | identity — bytes copied through unchanged            |
/// | `Rgba64Le`    | `(r_hi,g_hi,b_hi,a_hi)` — high byte per channel      |
///
/// For palette PNGs the `PLTE` + `tRNS` chunks are walked directly off
/// the source bitstream so the original chunk lengths are preserved
/// (the [`PngImage::palette`] side-channel concatenates the two without
/// recording where the split is).
///
/// Standalone (no `oxideav-core`) entry point: works whether or not
/// the `registry` feature is enabled.
pub fn decode_png_to_rgba(buf: &[u8]) -> Result<RgbaBitmap> {
    // Re-walk chunks so we can read the original PLTE / tRNS lengths
    // separately. `decode_png` collapses them into a single
    // `palette = PLTE || tRNS` blob with no explicit split point.
    let chunks = parse_all_chunks(buf)?;
    let mut plte: Option<&[u8]> = None;
    let mut trns: Option<&[u8]> = None;
    for c in &chunks {
        if c.is_type(b"PLTE") {
            plte = Some(c.data);
        } else if c.is_type(b"tRNS") {
            trns = Some(c.data);
        }
    }

    let img = decode_png(buf)?;
    png_image_to_rgba(&img, plte, trns)
}

/// Promote an arbitrary [`PngImage`] (any supported pixel format) into
/// an 8-bit-per-channel [`RgbaBitmap`]. `plte` / `trns` are used only
/// for `Pal8` source images.
///
/// For colour types 0 (`Gray8` / `Gray16Le`) and 2 (`Rgb24` /
/// `Rgb48Le`) a `tRNS` chunk names a single transparent sample value
/// (RFC 2083 §4.2.9). Pixels matching it exactly are emitted with
/// α=0; every other pixel stays opaque (α=255). The match is performed
/// at the source bit depth — for 16-bit sources both bytes of the
/// sample are compared *before* the 8-bit promotion drops the low
/// byte, per §4.2.9 ("Although decoders may drop the low-order byte
/// of the samples for display, this must not occur until after the
/// data has been tested for transparency").
fn png_image_to_rgba(
    img: &PngImage,
    plte: Option<&[u8]>,
    trns: Option<&[u8]>,
) -> Result<RgbaBitmap> {
    let w = img.width as usize;
    let h = img.height as usize;
    let n = w * h;
    let mut out = vec![0u8; n * 4];

    // For Gray*/Rgb* the tRNS payload (when present) is a single
    // sample value at the source bit depth. RFC 2083 §4.2.9 stores
    // every channel as a 2-byte big-endian value regardless of bit
    // depth, so we decode it to u16 once and compare against the
    // 16-bit-promoted source sample. For 8-bit sources the source
    // sample is widened to u16 (the high byte is zero per spec) so
    // the comparison stays uniform.
    let trns_gray16: Option<u16> = match (img.pixel_format, trns) {
        (PngPixelFormat::Gray8, Some(t)) | (PngPixelFormat::Gray16Le, Some(t)) if t.len() == 2 => {
            Some(u16::from_be_bytes([t[0], t[1]]))
        }
        _ => None,
    };
    let trns_rgb16: Option<(u16, u16, u16)> = match (img.pixel_format, trns) {
        (PngPixelFormat::Rgb24, Some(t)) | (PngPixelFormat::Rgb48Le, Some(t)) if t.len() == 6 => {
            Some((
                u16::from_be_bytes([t[0], t[1]]),
                u16::from_be_bytes([t[2], t[3]]),
                u16::from_be_bytes([t[4], t[5]]),
            ))
        }
        _ => None,
    };

    match img.pixel_format {
        PngPixelFormat::Gray8 => {
            for i in 0..n {
                let g = img.data[i];
                out[i * 4] = g;
                out[i * 4 + 1] = g;
                out[i * 4 + 2] = g;
                // tRNS keyed sample for ct=0 / bit_depth=8: the
                // 2-byte tRNS stores the gray sample in the low
                // byte (high byte is zero per spec), so the match
                // condition is `tRNS_gray16 as u8 == g` *and* the
                // high byte zero (already guaranteed by validation).
                out[i * 4 + 3] = match trns_gray16 {
                    Some(k) if (k as u8) == g => 0,
                    _ => 255,
                };
            }
        }
        PngPixelFormat::Gray16Le => {
            // Stored little-endian per sample: (lo, hi). Take the high
            // byte for an 8-bit promotion, but reconstruct the full
            // 16-bit value first so tRNS matching compares both bytes
            // (RFC 2083 §4.2.9 note: "it is important to compare both
            // bytes of the sample values to determine whether a pixel
            // is transparent").
            for i in 0..n {
                let lo = img.data[i * 2];
                let hi = img.data[i * 2 + 1];
                out[i * 4] = hi;
                out[i * 4 + 1] = hi;
                out[i * 4 + 2] = hi;
                let sample16 = u16::from_le_bytes([lo, hi]);
                out[i * 4 + 3] = match trns_gray16 {
                    Some(k) if k == sample16 => 0,
                    _ => 255,
                };
            }
        }
        PngPixelFormat::Rgb24 => {
            for i in 0..n {
                let r = img.data[i * 3];
                let g = img.data[i * 3 + 1];
                let b = img.data[i * 3 + 2];
                out[i * 4] = r;
                out[i * 4 + 1] = g;
                out[i * 4 + 2] = b;
                out[i * 4 + 3] = match trns_rgb16 {
                    Some((rk, gk, bk)) if (rk as u8) == r && (gk as u8) == g && (bk as u8) == b => {
                        0
                    }
                    _ => 255,
                };
            }
        }
        PngPixelFormat::Rgb48Le => {
            for i in 0..n {
                let r_lo = img.data[i * 6];
                let r_hi = img.data[i * 6 + 1];
                let g_lo = img.data[i * 6 + 2];
                let g_hi = img.data[i * 6 + 3];
                let b_lo = img.data[i * 6 + 4];
                let b_hi = img.data[i * 6 + 5];
                out[i * 4] = r_hi;
                out[i * 4 + 1] = g_hi;
                out[i * 4 + 2] = b_hi;
                let r16 = u16::from_le_bytes([r_lo, r_hi]);
                let g16 = u16::from_le_bytes([g_lo, g_hi]);
                let b16 = u16::from_le_bytes([b_lo, b_hi]);
                out[i * 4 + 3] = match trns_rgb16 {
                    Some((rk, gk, bk)) if rk == r16 && gk == g16 && bk == b16 => 0,
                    _ => 255,
                };
            }
        }
        PngPixelFormat::Pal8 => {
            let plte = plte.ok_or_else(|| {
                Error::invalid("PNG: Pal8 image missing PLTE chunk for RGBA promotion")
            })?;
            if plte.len() % 3 != 0 {
                return Err(Error::invalid(format!(
                    "PNG: PLTE chunk length {} is not a multiple of 3",
                    plte.len()
                )));
            }
            let entries = plte.len() / 3;
            let trns = trns.unwrap_or(&[]);
            for i in 0..n {
                let idx = img.data[i] as usize;
                if idx >= entries {
                    return Err(Error::invalid(format!(
                        "PNG: palette index {idx} out of bounds (PLTE has {entries} entries)"
                    )));
                }
                out[i * 4] = plte[idx * 3];
                out[i * 4 + 1] = plte[idx * 3 + 1];
                out[i * 4 + 2] = plte[idx * 3 + 2];
                // tRNS (per spec): leading entries carry alpha; all
                // entries past tRNS.len() are fully opaque.
                out[i * 4 + 3] = if idx < trns.len() { trns[idx] } else { 255 };
            }
        }
        PngPixelFormat::Ya8 => {
            for i in 0..n {
                let g = img.data[i * 2];
                let a = img.data[i * 2 + 1];
                out[i * 4] = g;
                out[i * 4 + 1] = g;
                out[i * 4 + 2] = g;
                out[i * 4 + 3] = a;
            }
        }
        PngPixelFormat::Rgba => {
            out.copy_from_slice(&img.data);
        }
        PngPixelFormat::Rgba64Le => {
            for i in 0..n {
                out[i * 4] = img.data[i * 8 + 1];
                out[i * 4 + 1] = img.data[i * 8 + 3];
                out[i * 4 + 2] = img.data[i * 8 + 5];
                out[i * 4 + 3] = img.data[i * 8 + 7];
            }
        }
    }

    Ok(RgbaBitmap {
        width: img.width,
        height: img.height,
        data: out,
    })
}

/// Pack the raw decoded byte plane into a [`PngImage`] with the IHDR's
/// output pixel format. For 16-bit formats this swaps PNG's BE samples
/// to little-endian; for colour type 4 / 16-bit it expands `(gray,
/// alpha)` into `(gray, gray, gray, alpha)` because we have no native
/// Ya16 pixel format.
fn build_png_image(
    ihdr: &Ihdr,
    raw: &[u8],
    plte: Option<&[u8]>,
    trns: Option<&[u8]>,
) -> Result<PngImage> {
    let pf = ihdr.output_pixel_format()?;
    let w = ihdr.width as usize;
    let h = ihdr.height as usize;

    let (stride, data) = match pf {
        PngPixelFormat::Gray8 => (w, raw.to_vec()),
        PngPixelFormat::Pal8 => {
            let _plte =
                plte.ok_or_else(|| Error::invalid("PNG: colour type 3 requires PLTE chunk"))?;
            (w, raw.to_vec())
        }
        PngPixelFormat::Gray16Le => {
            let mut out = vec![0u8; w * h * 2];
            for i in 0..w * h {
                let be = &raw[i * 2..i * 2 + 2];
                out[i * 2] = be[1];
                out[i * 2 + 1] = be[0];
            }
            (w * 2, out)
        }
        PngPixelFormat::Rgb24 => (w * 3, raw.to_vec()),
        PngPixelFormat::Rgba => (w * 4, raw.to_vec()),
        PngPixelFormat::Rgb48Le => {
            let mut out = vec![0u8; w * h * 6];
            for i in 0..w * h * 3 {
                out[i * 2] = raw[i * 2 + 1];
                out[i * 2 + 1] = raw[i * 2];
            }
            (w * 6, out)
        }
        PngPixelFormat::Rgba64Le => {
            // Two cases: genuinely RGBA 16 (ct=6, bd=16) or gray+alpha 16 (ct=4, bd=16).
            if ihdr.colour_type == 6 {
                let mut out = vec![0u8; w * h * 8];
                for i in 0..w * h * 4 {
                    out[i * 2] = raw[i * 2 + 1];
                    out[i * 2 + 1] = raw[i * 2];
                }
                (w * 8, out)
            } else {
                // colour type 4 + 16 bit → (G16, A16) in BE per sample.
                // Expand to (G,G,G,A) LE.
                let mut out = vec![0u8; w * h * 8];
                for i in 0..w * h {
                    let g_hi = raw[i * 4];
                    let g_lo = raw[i * 4 + 1];
                    let a_hi = raw[i * 4 + 2];
                    let a_lo = raw[i * 4 + 3];
                    for c in 0..3 {
                        out[i * 8 + c * 2] = g_lo;
                        out[i * 8 + c * 2 + 1] = g_hi;
                    }
                    out[i * 8 + 6] = a_lo;
                    out[i * 8 + 7] = a_hi;
                }
                (w * 8, out)
            }
        }
        PngPixelFormat::Ya8 => (w * 2, raw.to_vec()),
    };

    let palette = if pf == PngPixelFormat::Pal8 {
        let mut pal = Vec::new();
        if let Some(p) = plte {
            pal.extend_from_slice(p);
        }
        if let Some(t) = trns {
            pal.extend_from_slice(t);
        }
        pal
    } else {
        Vec::new()
    };

    Ok(PngImage {
        width: ihdr.width,
        height: ihdr.height,
        pixel_format: pf,
        stride,
        data,
        palette,
    })
}

/// Decompressed-zlib → unfiltered → (optionally expanded sub-byte, and for
/// Adam7 interlaced streams, scattered into the full canvas) byte plane.
///
/// The output layout is always "row-major, `decoded_bytes_per_pixel` bytes
/// per pixel, tightly packed (stride = width * bpp)".
pub(crate) fn decode_image_pixels(decompressed: &[u8], ihdr: &Ihdr) -> Result<Vec<u8>> {
    if ihdr.interlace == 0 {
        let raw = reconstruct_filtered(decompressed, ihdr)?;
        expand_byte_plane(&raw, ihdr, ihdr.width as usize, ihdr.height as usize)
    } else {
        // Adam7: seven passes, reconstructed independently, scattered into
        // the full canvas.
        decode_adam7(decompressed, ihdr)
    }
}

/// Adam7 pass table — (starting_row, starting_col, row_spacing, column_spacing).
/// From PNG spec §A.8 Table E.3 (pass 1 = index 0, etc.).
pub(crate) const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8), // pass 1
    (0, 4, 8, 8), // pass 2
    (4, 0, 8, 4), // pass 3
    (0, 2, 4, 4), // pass 4
    (2, 0, 4, 2), // pass 5
    (0, 1, 2, 2), // pass 6
    (1, 0, 2, 1), // pass 7
];

/// Dimensions of an Adam7 pass for a given full image size.
pub(crate) fn adam7_pass_dims(img_w: usize, img_h: usize, pass: usize) -> (usize, usize) {
    let (sr, sc, rs, cs) = ADAM7[pass];
    let pw = if img_w > sc {
        (img_w - sc).div_ceil(cs)
    } else {
        0
    };
    let ph = if img_h > sr {
        (img_h - sr).div_ceil(rs)
    } else {
        0
    };
    (pw, ph)
}

/// Decode an Adam7 interlaced image. Produces a final byte plane whose
/// layout matches what the non-interlaced path would output (1 byte per
/// pixel for sub-byte Gray/Pal, native bpp otherwise).
fn decode_adam7(decompressed: &[u8], ihdr: &Ihdr) -> Result<Vec<u8>> {
    let img_w = ihdr.width as usize;
    let img_h = ihdr.height as usize;
    let out_bpp = ihdr.decoded_bytes_per_pixel()?;
    let mut canvas = vec![0u8; img_w * img_h * out_bpp];

    let mut cursor = 0usize;
    for (pass, &(sr, sc, rs, cs)) in ADAM7.iter().enumerate() {
        let (pw, ph) = adam7_pass_dims(img_w, img_h, pass);
        if pw == 0 || ph == 0 {
            continue;
        }

        // Per-pass synthetic IHDR.
        let pass_ihdr = Ihdr {
            width: pw as u32,
            height: ph as u32,
            ..*ihdr
        };
        let row_bytes = pass_ihdr.row_bytes()?;
        let pass_bytes = (1 + row_bytes) * ph;
        if cursor + pass_bytes > decompressed.len() {
            return Err(Error::invalid(format!(
                "PNG Adam7: pass {} wants {} bytes, only {} remaining",
                pass + 1,
                pass_bytes,
                decompressed.len().saturating_sub(cursor)
            )));
        }
        let pass_slice = &decompressed[cursor..cursor + pass_bytes];
        cursor += pass_bytes;

        let raw = reconstruct_filtered(pass_slice, &pass_ihdr)?;
        let expanded = expand_byte_plane(&raw, ihdr, pw, ph)?;

        // Scatter `expanded` (pw × ph, out_bpp bytes/pixel) into `canvas`.
        for py in 0..ph {
            let dst_y = sr + py * rs;
            for px in 0..pw {
                let dst_x = sc + px * cs;
                let src_off = (py * pw + px) * out_bpp;
                let dst_off = (dst_y * img_w + dst_x) * out_bpp;
                canvas[dst_off..dst_off + out_bpp]
                    .copy_from_slice(&expanded[src_off..src_off + out_bpp]);
            }
        }
    }
    if cursor != decompressed.len() {
        return Err(Error::invalid(format!(
            "PNG Adam7: trailing {} bytes after last pass",
            decompressed.len() - cursor
        )));
    }
    Ok(canvas)
}

/// Given a raw (unfiltered) PNG byte plane at native bit depth, expand it to
/// the byte layout consumed by `build_png_image`. For sub-byte gray/pal,
/// this means unpacking 2/4/8 pixels per byte and (for grayscale) scaling
/// up to 8-bit. For ≥8-bit data this is a straight copy.
///
/// `w`/`h` are the logical pixel dimensions of the image the raw bytes
/// represent (the *pass* dimensions for an Adam7 pass, or the full image
/// dimensions otherwise).
fn expand_byte_plane(raw: &[u8], ihdr: &Ihdr, w: usize, h: usize) -> Result<Vec<u8>> {
    if ihdr.bit_depth >= 8 {
        // Sanity check — caller passed us matching-sized data.
        let bpp = ihdr.decoded_bytes_per_pixel()?;
        let expected = w * h * bpp;
        if raw.len() != expected {
            return Err(Error::invalid(format!(
                "PNG: expand_byte_plane expected {expected} bytes, got {}",
                raw.len()
            )));
        }
        return Ok(raw.to_vec());
    }

    // Sub-byte: only colour type 0 (grayscale) or 3 (indexed) allowed.
    let bd = ihdr.bit_depth as usize;
    if ihdr.colour_type != 0 && ihdr.colour_type != 3 {
        return Err(Error::invalid(format!(
            "PNG: colour type {} cannot have bit depth {}",
            ihdr.colour_type, bd
        )));
    }
    let mask: u8 = (1u16 << bd) as u8 - 1;
    let pixels_per_byte = 8 / bd;
    let row_bytes_packed = (w * bd).div_ceil(8);
    let expected = row_bytes_packed * h;
    if raw.len() != expected {
        return Err(Error::invalid(format!(
            "PNG: expand_byte_plane (sub-byte) expected {expected} bytes, got {}",
            raw.len()
        )));
    }

    // Scale table for grayscale: 1-bit → ×255, 2-bit → ×85, 4-bit → ×17.
    // (PNG spec §13.12.)
    let scale = match (ihdr.colour_type, bd) {
        (0, 1) => 255,
        (0, 2) => 85,
        (0, 4) => 17,
        _ => 1, // indexed: raw index (not scaled)
    };

    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let byte_idx = y * row_bytes_packed + x / pixels_per_byte;
            // Pixels in a byte are MSB-first per PNG spec.
            let shift_in_byte = (pixels_per_byte - 1 - (x % pixels_per_byte)) * bd;
            let v = (raw[byte_idx] >> shift_in_byte) & mask;
            out[y * w + x] = if ihdr.colour_type == 0 {
                v.wrapping_mul(scale)
            } else {
                v
            };
        }
    }
    Ok(out)
}

/// Apply the 5 per-row filters, returning a flat raw-pixel buffer of
/// `row_bytes * height` bytes.
pub(crate) fn reconstruct_filtered(filtered: &[u8], ihdr: &Ihdr) -> Result<Vec<u8>> {
    let row_bytes = ihdr.row_bytes()?;
    let bpp = ihdr.bpp_for_filter()?;
    let height = ihdr.height as usize;

    // Each row is 1 filter byte + row_bytes data.
    let expected = (1 + row_bytes) * height;
    if filtered.len() != expected {
        return Err(Error::invalid(format!(
            "PNG: decompressed length {} != expected {}",
            filtered.len(),
            expected
        )));
    }

    let mut raw = vec![0u8; row_bytes * height];
    let zero_row = vec![0u8; row_bytes];
    for y in 0..height {
        let src_start = y * (1 + row_bytes);
        let filter_byte = filtered[src_start];
        let filter_type = FilterType::from_u8(filter_byte)?;
        let data_start = src_start + 1;
        // Copy row's raw bytes into dst.
        let dst_start = y * row_bytes;
        raw[dst_start..dst_start + row_bytes]
            .copy_from_slice(&filtered[data_start..data_start + row_bytes]);
        // Unfilter in place.
        let (prev_rows, curr_rows) = raw.split_at_mut(dst_start);
        let curr = &mut curr_rows[..row_bytes];
        let prev: &[u8] = if y == 0 {
            &zero_row
        } else {
            &prev_rows[(y - 1) * row_bytes..(y - 1) * row_bytes + row_bytes]
        };
        unfilter_row(filter_type, curr, prev, bpp)?;
    }
    Ok(raw)
}

// ---- APNG: multi-frame iterator ----------------------------------------

/// Static description of an APNG, including its per-frame compressed data
/// segments ready for decompression. The demuxer uses this to split a PNG
/// file into per-frame packets.
#[derive(Debug)]
pub struct ApngInfo {
    pub ihdr: Ihdr,
    pub plte: Option<Vec<u8>>,
    pub trns: Option<Vec<u8>>,
    pub actl: Actl,
    /// One entry per animation frame.
    pub frames: Vec<ApngFrame>,
    /// True if the default image (IDAT) is also the first animation frame —
    /// i.e. there's an `fcTL` that came before `IDAT`.
    pub first_frame_is_default: bool,
}

#[derive(Debug)]
pub struct ApngFrame {
    pub fctl: Fctl,
    /// Concatenated compressed data: IDAT payload or fdAT payloads stripped
    /// of their 4-byte sequence number.
    pub compressed: Vec<u8>,
}

/// Parse an APNG file and return metadata + per-frame compressed segments.
/// Returns `Err` if the file is a plain PNG without `acTL`.
pub fn parse_apng(buf: &[u8]) -> Result<ApngInfo> {
    let chunks = parse_all_chunks(buf)?;
    let ihdr = Ihdr::parse(
        chunks
            .iter()
            .find(|c| c.is_type(b"IHDR"))
            .ok_or_else(|| Error::invalid("PNG: missing IHDR"))?
            .data,
    )?;
    let actl = chunks
        .iter()
        .find(|c| c.is_type(b"acTL"))
        .ok_or_else(|| Error::invalid("PNG: not animated (no acTL)"))?;
    let actl = Actl::parse(actl.data)?;

    let mut plte: Option<Vec<u8>> = None;
    let mut trns: Option<Vec<u8>> = None;

    let mut frames: Vec<ApngFrame> = Vec::new();
    let mut current_fctl: Option<Fctl> = None;
    let mut current_compressed: Vec<u8> = Vec::new();
    let mut saw_idat = false;
    let mut first_frame_is_default = false;

    for c in &chunks {
        match &c.chunk_type {
            b"PLTE" => plte = Some(c.data.to_vec()),
            b"tRNS" => trns = Some(c.data.to_vec()),
            b"fcTL" => {
                if let Some(prev_fctl) = current_fctl.take() {
                    frames.push(ApngFrame {
                        fctl: prev_fctl,
                        compressed: std::mem::take(&mut current_compressed),
                    });
                }
                let f = Fctl::parse(c.data)?;
                // An fcTL appearing before any IDAT means the default image
                // doubles as the first animation frame.
                if !saw_idat {
                    first_frame_is_default = true;
                }
                current_fctl = Some(f);
            }
            b"IDAT" => {
                // IDAT bytes only contribute to the animation if we've
                // already seen an fcTL claiming them; otherwise they belong
                // to the non-animated default image.
                saw_idat = true;
                if current_fctl.is_some() {
                    current_compressed.extend_from_slice(c.data);
                }
            }
            b"fdAT" => {
                let (_seq, payload) = parse_fdat(c.data)?;
                current_compressed.extend_from_slice(payload);
            }
            _ => {}
        }
    }
    if let Some(f) = current_fctl.take() {
        frames.push(ApngFrame {
            fctl: f,
            compressed: std::mem::take(&mut current_compressed),
        });
    }

    // RFC 2083 §4.2.9 tRNS validation also applies to the APNG default
    // image / per-frame plane: ct=0/2 fixed length, ct=4/6 prohibited,
    // ct=3 capped at PLTE entry count.
    validate_trns(&ihdr, trns.as_deref(), plte.as_deref())?;

    // Per APNG spec acTL.num_frames is an advisory frame count. The
    // spec does not require a hard reject when the value disagrees with
    // the actual fcTL count — the authoritative frame chain is the
    // fcTL/fdAT sequence we just walked. Generators in the wild emit
    // mismatched counts; we accept them rather than failing the parse.

    Ok(ApngInfo {
        ihdr,
        plte,
        trns,
        actl,
        frames,
        first_frame_is_default,
    })
}

/// Decode an entire APNG file into its composited per-frame canvases.
/// Standalone (no `oxideav-core`) entry point.
pub fn decode_apng(buf: &[u8]) -> Result<ApngImage> {
    let info = parse_apng(buf)?;
    decode_apng_info(&info)
}

/// Composite the parsed APNG `info` into per-frame canvases.
pub fn decode_apng_info(info: &ApngInfo) -> Result<ApngImage> {
    let canvas_w = info.ihdr.width;
    let canvas_h = info.ihdr.height;
    let canvas_fmt = info.ihdr.output_pixel_format()?;
    let bpp = canvas_fmt.bytes_per_pixel();
    let stride_canvas = canvas_w as usize * bpp;

    let mut canvas = vec![0u8; stride_canvas * canvas_h as usize];
    // For Disposal::Previous we snapshot the pre-draw state before writing
    // the new frame.
    let mut prev_snapshot: Option<Vec<u8>> = None;
    let mut out_frames: Vec<ApngFrameImage> = Vec::new();

    for frame in info.frames.iter() {
        // Build a synthetic IHDR-like block for the sub-frame dimensions.
        // Same colour_type / bit_depth / compression / filter / interlace.
        let sub_ihdr = Ihdr {
            width: frame.fctl.width,
            height: frame.fctl.height,
            bit_depth: info.ihdr.bit_depth,
            colour_type: info.ihdr.colour_type,
            compression: info.ihdr.compression,
            filter: info.ihdr.filter,
            interlace: info.ihdr.interlace,
        };
        let decompressed = decompress_to_vec_zlib(&frame.compressed)
            .map_err(|e| Error::invalid(format!("APNG: zlib failed: {e:?}")))?;
        let frame_raw = decode_image_pixels(&decompressed, &sub_ihdr)?;
        let sub_frame = build_png_image(
            &sub_ihdr,
            &frame_raw,
            info.plte.as_deref(),
            info.trns.as_deref(),
        )?;

        // Disposal: Previous → snapshot the region pre-draw.
        if frame.fctl.dispose_op == Disposal::Previous {
            prev_snapshot = Some(canvas.clone());
        }

        // Blend the sub_frame into the canvas.
        blit_sub_into_canvas(
            &mut canvas,
            stride_canvas,
            bpp,
            canvas_w as usize,
            canvas_h as usize,
            &sub_frame,
            sub_ihdr.width as usize,
            sub_ihdr.height as usize,
            canvas_fmt,
            frame.fctl.x_offset as usize,
            frame.fctl.y_offset as usize,
            frame.fctl.blend_op,
        );

        let img = PngImage {
            width: canvas_w,
            height: canvas_h,
            pixel_format: canvas_fmt,
            stride: stride_canvas,
            data: canvas.clone(),
            palette: Vec::new(),
        };
        let delay = frame.fctl.delay_centiseconds().max(1);
        out_frames.push(ApngFrameImage {
            image: img,
            delay_cs: delay,
        });

        // Apply disposal *after* emitting.
        match frame.fctl.dispose_op {
            Disposal::None => {}
            Disposal::Background => {
                // Clear the sub-region to zeros.
                clear_region(
                    &mut canvas,
                    stride_canvas,
                    bpp,
                    canvas_w as usize,
                    canvas_h as usize,
                    frame.fctl.x_offset as usize,
                    frame.fctl.y_offset as usize,
                    frame.fctl.width as usize,
                    frame.fctl.height as usize,
                );
            }
            Disposal::Previous => {
                if let Some(snap) = prev_snapshot.take() {
                    canvas = snap;
                }
            }
        }
    }

    Ok(ApngImage {
        width: canvas_w,
        height: canvas_h,
        pixel_format: canvas_fmt,
        frames: out_frames,
        num_plays: info.actl.num_plays,
    })
}

#[allow(clippy::too_many_arguments)]
fn blit_sub_into_canvas(
    canvas: &mut [u8],
    stride_canvas: usize,
    bpp: usize,
    canvas_w: usize,
    canvas_h: usize,
    sub: &PngImage,
    sub_w: usize,
    sub_h: usize,
    sub_fmt: PngPixelFormat,
    x_off: usize,
    y_off: usize,
    blend: Blend,
) {
    let sub_stride = sub.stride;
    for sy in 0..sub_h {
        let dy = y_off + sy;
        if dy >= canvas_h {
            break;
        }
        let row_cap = (canvas_w - x_off.min(canvas_w)).min(sub_w);
        for sx in 0..row_cap {
            let dx = x_off + sx;
            let src = &sub.data[sy * sub_stride + sx * bpp..sy * sub_stride + (sx + 1) * bpp];
            let dst_start = dy * stride_canvas + dx * bpp;
            let dst = &mut canvas[dst_start..dst_start + bpp];
            match blend {
                Blend::Source => {
                    dst.copy_from_slice(src);
                }
                Blend::Over => {
                    // Only meaningful for formats with alpha. For formats
                    // without alpha we fall back to Source.
                    if bpp == 4 {
                        // 8-bit RGBA alpha compositing.
                        let a = src[3] as u32;
                        if a == 255 {
                            dst.copy_from_slice(src);
                        } else if a == 0 {
                            // Leave canvas alone.
                        } else {
                            let inv = 255 - a;
                            for c in 0..3 {
                                let fg = src[c] as u32 * a;
                                let bg = dst[c] as u32 * inv;
                                dst[c] = ((fg + bg + 127) / 255) as u8;
                            }
                            // Alpha over: a_out = a_src + a_dst * (1 - a_src)
                            let a_dst = dst[3] as u32;
                            dst[3] = (a + ((a_dst * inv + 127) / 255)) as u8;
                        }
                    } else if bpp == 2 && matches!(sub_fmt, PngPixelFormat::Ya8) {
                        let a = src[1] as u32;
                        if a == 255 {
                            dst.copy_from_slice(src);
                        } else if a != 0 {
                            let inv = 255 - a;
                            let fg = src[0] as u32 * a;
                            let bg = dst[0] as u32 * inv;
                            dst[0] = ((fg + bg + 127) / 255) as u8;
                            let a_dst = dst[1] as u32;
                            dst[1] = (a + ((a_dst * inv + 127) / 255)) as u8;
                        }
                    } else {
                        dst.copy_from_slice(src);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn clear_region(
    canvas: &mut [u8],
    stride_canvas: usize,
    bpp: usize,
    canvas_w: usize,
    canvas_h: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
) {
    // An APNG `fcTL` carries an attacker-controlled `x_offset`/`y_offset`
    // (and width/height) that the spec says must lie within the canvas but
    // a malformed stream need not honour. Clamp the start column to the
    // canvas before turning it into a byte offset — otherwise `x` past
    // `canvas_w` produces a `row_start` beyond the buffer and the
    // (even empty) slice index panics. With `x >= canvas_w` the visible
    // width collapses to zero, so there is nothing to clear.
    if x >= canvas_w {
        return;
    }
    let visible_w = w.min(canvas_w - x);
    for dy in y..(y + h).min(canvas_h) {
        let row_start = dy * stride_canvas + x * bpp;
        let row_end = row_start + visible_w * bpp;
        for b in &mut canvas[row_start..row_end] {
            *b = 0;
        }
    }
}
