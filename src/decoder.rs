//! PNG + APNG decoder.
//!
//! High level flow:
//!
//! 1. Verify the 8-byte magic, then read chunks until IEND.
//! 2. `IHDR` gives width / height / bit depth / colour type.
//! 3. `PLTE` + `tRNS` feed the palette for colour type 3 (and alpha for 0/2).
//! 4. `acTL` + `fcTL` + `fdAT` carry animation frame metadata / data.
//! 5. Each frame's compressed stream = concatenation of `IDAT` (for default
//!    image) or `fdAT[4..]` (for animation frames) → `compcol` zlib
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
use crate::image::{ApngFrameImage, ApngImage, Palette, PngImage, PngPixelFormat, RgbaImage};
use crate::options::DecodeOptions;

// Backward-compat re-exports: existing callers reach for
// `oxideav_png::decoder::make_decoder` and
// `oxideav_png::decoder::decode_png_to_frame`. Keep both paths live
// by re-exporting the registry-side surface.
#[cfg(feature = "registry")]
pub use crate::registry::{decode_png_to_frame, make_decoder};
// The pre-contract entry points at their historical module path.
#[allow(deprecated)]
pub use crate::api::{decode_png, decode_png_over_background, decode_png_to_rgba};

use crate::zlibvec::decompress_to_vec_zlib_capped;
use compcol::{Decoder as _, Status};

use crate::apng::{Actl, Blend, Disposal, Fctl, parse_fdat};
use crate::chunk::{ChunkRef, PNG_MAGIC, read_chunk};
use crate::filter::{FilterType, unfilter_row};
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
                stride: f.image.stride(),
                data: f.image.data().to_vec(),
            }],
        });
        pts += f.delay_cs as i64;
    }
    Ok(out)
}

// ---- IHDR ---------------------------------------------------------------

/// Parsed IHDR chunk (13 bytes).
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
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
    /// Build an IHDR for a non-interlaced image with the only defined compression (0) and filter (0) methods; `with_interlace(1)` selects Adam7.
    pub fn new(width: u32, height: u32, bit_depth: u8, colour_type: u8) -> Self {
        Self {
            width,
            height,
            bit_depth,
            colour_type,
            compression: 0,
            filter: 0,
            interlace: 0,
        }
    }

    /// Set the compression method byte (only `0` is defined).
    pub fn with_compression(mut self, compression: u8) -> Self {
        self.compression = compression;
        self
    }

    /// Set the filter method byte (only `0` is defined).
    pub fn with_filter(mut self, filter: u8) -> Self {
        self.filter = filter;
        self
    }

    /// Set the interlace method: `0` none, `1` Adam7.
    pub fn with_interlace(mut self, interlace: u8) -> Self {
        self.interlace = interlace;
        self
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != 13 {
            return Err(Error::invalid(format!(
                "PNG IHDR: expected 13 bytes, got {}",
                data.len()
            )));
        }
        let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ihdr = Self {
            width,
            height,
            bit_depth: data[8],
            colour_type: data[9],
            compression: data[10],
            filter: data[11],
            interlace: data[12],
        };
        ihdr.validate()?;
        Ok(ihdr)
    }

    /// Reject a structurally-malformed IHDR per W3C PNG3 §11.2.1
    /// ("IHDR Image header"). Every field carries a wire constraint the
    /// spec states outright; a value outside those bounds is *invalid
    /// data*, not an unsupported feature, so each path returns
    /// [`Error::invalid`]. Run once at the wire-decode boundary (from
    /// [`Ihdr::parse`]) so `decode` / `parse_metadata` / `parse_apng`
    /// and the demuxer all share one gate rather than re-deriving the
    /// checks inline (or, worse, only catching some of them late in
    /// [`Ihdr::output_pixel_format`]).
    ///
    /// * **Width / height.** "Width and height give the image dimensions
    ///   in pixels. They are PNG four-byte unsigned integers. Zero is an
    ///   invalid value." A zero in either dimension would otherwise yield
    ///   a degenerate empty byte-plane rather than an error.
    /// * **Colour type + bit depth.** §11.2.1 Table 12 ("Allowed
    ///   combinations of color type and bit depth") is the single source
    ///   of truth: greyscale (0) → 1/2/4/8/16; truecolor (2) → 8/16;
    ///   indexed (3) → 1/2/4/8; greyscale-alpha (4) and truecolor-alpha
    ///   (6) → 8/16. [`Ihdr::is_allowed_combination`] decodes the table;
    ///   any other pairing (e.g. a 1-bit truecolor row, a 16-bit indexed
    ///   row, an invented colour type 1/5/7, or bit depth 0/3/…) is
    ///   rejected here. The colour-type byte itself is validated by
    ///   `colour_type_typed` inside that call.
    /// * **Compression method.** "Only compression method 0 (deflate
    ///   compression …) is defined in this specification."
    /// * **Filter method.** "Only filter method 0 is defined by this
    ///   specification."
    /// * **Interlace method.** §11.2.1 defines methods 0 (none) and 1
    ///   (Adam7); every other value is invalid.
    pub fn validate(&self) -> Result<()> {
        if self.width == 0 || self.height == 0 {
            return Err(Error::invalid(format!(
                "PNG IHDR: zero is an invalid dimension (width {}, height {})",
                self.width, self.height
            )));
        }
        if !self.is_allowed_combination()? {
            return Err(Error::invalid(format!(
                "PNG IHDR: colour type {} with bit depth {} is not an allowed \
                 combination (§11.2.1 Table 12)",
                self.colour_type, self.bit_depth
            )));
        }
        if self.compression != 0 {
            return Err(Error::invalid(format!(
                "PNG IHDR: unknown compression method {} (only 0 is defined)",
                self.compression
            )));
        }
        if self.filter != 0 {
            return Err(Error::invalid(format!(
                "PNG IHDR: unknown filter method {} (only 0 is defined)",
                self.filter
            )));
        }
        if self.interlace > 1 {
            return Err(Error::invalid(format!(
                "PNG IHDR: unknown interlace method {} (only 0 and 1 are defined)",
                self.interlace
            )));
        }
        Ok(())
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

    /// Exact byte length of the filtered scanline stream this IHDR
    /// implies — the size the IDAT (or per-frame fdAT) zlib stream must
    /// inflate to.
    ///
    /// Non-interlaced (W3C PNG3 §7.2 / §9.1): every scanline is one
    /// filter-type byte plus [`Self::row_bytes`], so the stream is
    /// `(1 + row_bytes) × height`. Adam7 (§8.1): each of the seven
    /// passes is serialized "as though it were a complete image of the
    /// appropriate dimensions", each pass row carrying its own
    /// filter-type byte; empty passes (zero width or height) contribute
    /// nothing. The sum is taken in saturating `u64` so a
    /// maximum-dimension IHDR cannot wrap — for any decodable image the
    /// value is exact.
    ///
    /// Knowing this size *before* inflating lets the decoder cap the
    /// zlib output at `expected + 1` bytes: a hostile stream that
    /// inflates past the IHDR-implied size (a decompression bomb behind
    /// a small declared canvas, W3C PNG3 §13.3) is cut off at the bound
    /// instead of being fully materialised and only then length-checked.
    pub fn expected_filtered_len(&self) -> Result<u64> {
        if self.interlace == 0 {
            let rb = self.row_bytes()? as u64;
            Ok((1u64.saturating_add(rb)).saturating_mul(self.height as u64))
        } else {
            let mut total = 0u64;
            for pass in 0..7 {
                let (pw, ph) = adam7_pass_dims(self.width as usize, self.height as usize, pass);
                if pw == 0 || ph == 0 {
                    continue;
                }
                let pass_ihdr = Ihdr {
                    width: pw as u32,
                    height: ph as u32,
                    ..*self
                };
                let rb = pass_ihdr.row_bytes()? as u64;
                total = total.saturating_add((1u64.saturating_add(rb)).saturating_mul(ph as u64));
            }
            Ok(total)
        }
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
                )));
            }
        })
    }

    /// Size in bytes of the decoded plane [`crate::decode`] would
    /// allocate for this header: `width × height × bytes_per_pixel` of
    /// [`Self::output_pixel_format`] (the widest buffer the decode
    /// holds — the ct 4 / 16-bit `Rgba64Le` widening included).
    pub fn decoded_plane_bytes(&self) -> Result<u64> {
        let bpp = self.output_pixel_format()?.bytes_per_pixel() as u64;
        Ok(u64::from(self.width) * u64::from(self.height) * bpp)
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
    validate_critical_ordering(&out)?;
    Ok(out)
}

/// Enforce the W3C PNG3 §5.6 / §5.1 critical-chunk ordering shared by
/// every decode entry point. §5.1: "A valid PNG datastream shall begin
/// with a PNG signature, immediately followed by an IHDR chunk … and
/// shall end with an IEND chunk. Only one IHDR chunk and one IEND chunk
/// are allowed." §5.6 Table 7 additionally places `PLTE` "Before first
/// IDAT".
///
/// Concretely this rejects: a chunk before the first `IHDR`; a second
/// `IHDR`; and a `PLTE` that does not precede the first `IDAT` (a `PLTE`
/// after `IDAT` would address a palette the pixel stream has already
/// been decoded against). The walker already guarantees the list ends
/// at the first `IEND`, so the "shall end with IEND" / "ends before
/// IEND" arms are handled by [`parse_all_chunks`] itself.
fn validate_critical_ordering(chunks: &[ChunkRef<'_>]) -> Result<()> {
    // IHDR shall be first.
    match chunks.first() {
        Some(c) if c.is_type(b"IHDR") => {}
        Some(_) => {
            return Err(Error::invalid(
                "PNG: first chunk is not IHDR \
                 (W3C PNG3 §5.1: signature \"immediately followed by an IHDR chunk\")",
            ));
        }
        None => return Err(Error::invalid("PNG: no chunks")),
    }
    // Exactly one IHDR (§5.1: "Only one IHDR chunk … allowed").
    if chunks.iter().filter(|c| c.is_type(b"IHDR")).count() > 1 {
        return Err(Error::invalid(
            "PNG: more than one IHDR chunk (W3C PNG3 §5.1: \"Only one IHDR chunk … allowed\")",
        ));
    }
    // PLTE shall precede the first IDAT (§5.6 Table 7: "Before first IDAT").
    let first_idat = chunks.iter().position(|c| c.is_type(b"IDAT"));
    let first_plte = chunks.iter().position(|c| c.is_type(b"PLTE"));
    if let (Some(plte), Some(idat)) = (first_plte, first_idat)
        && plte > idat
    {
        return Err(Error::invalid(
            "PNG: PLTE appears after IDAT \
                 (W3C PNG3 §5.6 Table 7: PLTE \"Before first IDAT\")",
        ));
    }
    Ok(())
}

/// Walk-position tracker for the W3C PNG3 §5.6 Table 7 chunk-ordering
/// rules. The ordering column of Table 7 places every ancillary chunk
/// the codec understands into one of four positional buckets relative to
/// the first `PLTE` and the first `IDAT`. §5.6 makes this normative:
/// "These lattice diagrams represent the constraints on positioning
/// imposed by this specification … Chunks higher up shall appear before
/// chunks lower down." A chunk that lands outside its bucket is a hard
/// `InvalidData` error rather than a silently-accepted out-of-spec
/// stream.
///
/// The tracker only flips forward (`PLTE` / `IDAT` are each seen at most
/// once on a conformant stream; the multiplicity gates elsewhere reject
/// a second `IHDR`/`IEND`, and `PLTE` duplication is policed by the
/// chunk walker). Each ancillary arm calls the bucket method matching
/// its Table 7 row *before* recording the parsed value, so an
/// out-of-position chunk fails ahead of any allocation its body parser
/// would do.
#[derive(Default)]
struct OrderTracker {
    seen_plte: bool,
    seen_idat: bool,
}

impl OrderTracker {
    /// Table 7 bucket "Before PLTE and IDAT" — `cHRM`, `cICP`, `gAMA`,
    /// `iCCP`, `mDCV`, `cLLI`, `sBIT`, `sRGB`. The colour-space and
    /// significant-bits chunks describe how the *samples* (including the
    /// palette, when present) are to be interpreted, so they shall lead
    /// both the palette and the pixel data.
    fn before_plte_and_idat(&self, name: &str) -> Result<()> {
        if self.seen_plte {
            return Err(Error::invalid(format!(
                "PNG {name}: chunk appears after PLTE \
                 (W3C PNG3 §5.6 Table 7: \"Before PLTE and IDAT\")",
            )));
        }
        if self.seen_idat {
            return Err(Error::invalid(format!(
                "PNG {name}: chunk appears after IDAT \
                 (W3C PNG3 §5.6 Table 7: \"Before PLTE and IDAT\")",
            )));
        }
        Ok(())
    }

    /// Table 7 bucket "After PLTE; before IDAT" — `bKGD`, `hIST`,
    /// `tRNS`. These reference palette entries (or, for the
    /// non-palette colour types that still carry `bKGD` / `tRNS`,
    /// describe the post-palette image), so they shall trail any `PLTE`
    /// and precede the pixel data. When a `PLTE` is present in the
    /// stream (`plte_present`) the chunk shall not precede it; in every
    /// case it shall precede `IDAT`. (A missing-`PLTE` requirement for
    /// the palette colour type is policed separately by the per-chunk
    /// parser.)
    fn after_plte_before_idat(&self, name: &str, plte_present: bool) -> Result<()> {
        if self.seen_idat {
            return Err(Error::invalid(format!(
                "PNG {name}: chunk appears after IDAT \
                 (W3C PNG3 §5.6 Table 7: \"After PLTE; before IDAT\")",
            )));
        }
        if plte_present && !self.seen_plte {
            return Err(Error::invalid(format!(
                "PNG {name}: chunk appears before PLTE \
                 (W3C PNG3 §5.6 Table 7: \"After PLTE; before IDAT\")",
            )));
        }
        Ok(())
    }

    /// Table 7 bucket "Before IDAT" with no `PLTE` relationship —
    /// `eXIf`, `pHYs`, `sPLT` (and the `acTL` animation-control chunk,
    /// policed on the APNG path). They may appear on either side of
    /// `PLTE` but shall precede the pixel data.
    fn before_idat(&self, name: &str) -> Result<()> {
        if self.seen_idat {
            return Err(Error::invalid(format!(
                "PNG {name}: chunk appears after IDAT \
                 (W3C PNG3 §5.6 Table 7: \"Before IDAT\")",
            )));
        }
        Ok(())
    }
}

/// Validate the W3C PNG3 §5.6 / §11.2.3 (RFC 2083 §4.1.3) rule that
/// "There may be multiple IDAT chunks; if so, they shall appear
/// consecutively with no other intervening chunks." A decoder
/// concatenates IDAT payloads into a single zlib stream, so a run broken
/// by an intervening non-IDAT chunk would silently splice two
/// logically-separate compressed segments. Tracks the run with a
/// two-state flag: once an IDAT has been seen and a non-IDAT chunk
/// follows, any *later* IDAT is non-consecutive.
fn validate_idat_consecutive(chunks: &[crate::chunk::ChunkRef<'_>]) -> Result<()> {
    let mut in_idat_run = false;
    let mut idat_run_closed = false;
    for c in chunks {
        if c.is_type(b"IDAT") {
            if idat_run_closed {
                return Err(Error::invalid(
                    "PNG: non-consecutive IDAT chunks \
                     (W3C PNG3 §5.6: \"Multiple IDAT chunks shall be consecutive\")",
                ));
            }
            in_idat_run = true;
        } else if in_idat_run {
            idat_run_closed = true;
        }
    }
    Ok(())
}

/// Validate the W3C PNG3 §5.6 Table 7 ancillary-chunk ordering buckets
/// over a parsed chunk list, without parsing any chunk body. Walks the
/// chunk *types* only, flipping the [`OrderTracker`] as the first `PLTE`
/// / `IDAT` are crossed, and rejects any recognised ancillary chunk that
/// lands outside its Table 7 bucket.
///
/// Shared by the metadata reader ([`parse_metadata`]) and the APNG
/// container parser ([`parse_apng`]) so a mis-ordered colour-space /
/// background / physical-dimension chunk is rejected uniformly on both
/// the static-image and the animated-image decode paths. (`tIME`,
/// `tEXt`, `zTXt`, `iTXt` carry no ordering constraint and are skipped;
/// the APNG control chunks `acTL` / `fcTL` / `fdAT` are policed by their
/// own §4.9 sequence rules.)
pub(crate) fn validate_ancillary_ordering(chunks: &[crate::chunk::ChunkRef<'_>]) -> Result<()> {
    // Whether the stream carries a PLTE at all: the "After PLTE" half of
    // the bKGD / hIST / tRNS bucket is only meaningful when a palette is
    // present (a truecolor image with no PLTE has nothing to trail).
    let plte_present = chunks.iter().any(|c| c.is_type(b"PLTE"));
    let mut order = OrderTracker::default();
    for c in chunks {
        match &c.chunk_type {
            b"cHRM" => order.before_plte_and_idat("cHRM")?,
            b"cICP" => order.before_plte_and_idat("cICP")?,
            b"gAMA" => order.before_plte_and_idat("gAMA")?,
            b"iCCP" => order.before_plte_and_idat("iCCP")?,
            b"mDCV" => order.before_plte_and_idat("mDCV")?,
            b"cLLI" => order.before_plte_and_idat("cLLI")?,
            b"sBIT" => order.before_plte_and_idat("sBIT")?,
            b"sRGB" => order.before_plte_and_idat("sRGB")?,
            b"bKGD" => order.after_plte_before_idat("bKGD", plte_present)?,
            b"hIST" => order.after_plte_before_idat("hIST", plte_present)?,
            b"tRNS" => order.after_plte_before_idat("tRNS", plte_present)?,
            b"eXIf" => order.before_idat("eXIf")?,
            b"pHYs" => order.before_idat("pHYs")?,
            b"sPLT" => order.before_idat("sPLT")?,
            b"PLTE" => order.seen_plte = true,
            b"IDAT" => order.seen_idat = true,
            _ => {}
        }
    }
    Ok(())
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
/// * W3C PNG3 §5.6 Table 7 ("Chunk ordering rules") — the per-chunk
///   placement constraints are normative ("Chunks higher up shall
///   appear before chunks lower down"). The four ordering buckets are
///   enforced by [`OrderTracker`] as the chunk walk crosses the first
///   `PLTE` and the first `IDAT`.
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

    // §5.6 Table 7 ancillary-chunk ordering (and the §5.6 "After PLTE"
    // half for bKGD / hIST / tRNS, which needs whole-stream PLTE
    // lookahead) is validated up front by the shared body-free walker
    // before any chunk body is parsed.
    validate_ancillary_ordering(&chunks)?;

    let mut out = PngMetadata::default();
    // Tracks whether the chunk walk has passed the first `IDAT` chunk
    // yet, so an unrecognised ancillary chunk can be recorded on the
    // correct side of `IDAT` (W3C PNG3 §14.2: a safe-to-copy chunk
    // "shall not be moved from before IDAT to after IDAT or vice versa").
    let mut after_idat = false;
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
                            ));
                        }
                        Some(n) if (idx as usize) >= n => {
                            return Err(Error::invalid(format!(
                                "PNG bKGD: palette index {idx} out of range (PLTE has {n} entries)",
                            )));
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
            // Critical chunks + the APNG control chunks are understood by
            // the codec elsewhere (decode / parse_apng); they are not
            // metadata and never become an `UnknownChunk`. `IDAT` flips
            // the before/after-IDAT tracker for any later unknown
            // ancillary chunk. (Positional §5.6 ordering was already
            // validated up front by `validate_ancillary_ordering`.)
            b"IHDR" | b"IEND" | b"acTL" | b"fcTL" | b"fdAT" | b"PLTE" => {}
            b"IDAT" => after_idat = true,
            // Any remaining 4-byte type is one the codec does not parse.
            // W3C PNG3 §14.2: an unrecognised *critical* chunk (§5.4
            // ancillary bit clear) is a hard error — "PNG editors shall
            // terminate on encountering an unrecognized critical chunk
            // type, because there is no way to be certain that a valid
            // datastream will result". An unrecognised *ancillary* chunk
            // with a §13.1 well-formed all-letter name is captured
            // verbatim for the editor round-trip; a name carrying a
            // non-letter byte is malformed and dropped (it is not a
            // conformant extension and re-emitting it would propagate the
            // malformation).
            other => {
                let ty = crate::chunk::ChunkType::new(*other);
                if ty.is_critical() {
                    return Err(Error::invalid(format!(
                        "PNG: unrecognised critical chunk {:?} (W3C PNG3 §14.2)",
                        ty.as_str()
                    )));
                }
                if ty.is_well_formed_name() {
                    out.unknowns.push(crate::metadata::UnknownChunk {
                        chunk_type: *other,
                        data: c.data.to_vec(),
                        after_idat,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Decode a single non-animated PNG file (or the "default image" of an
/// APNG) into a [`PngImage`] under `opts` — the engine behind
/// [`crate::decode`] / [`crate::decode_with`].
pub(crate) fn decode_image(buf: &[u8], opts: &DecodeOptions) -> Result<PngImage> {
    let chunks = parse_all_chunks(buf)?;
    decode_png_chunks(&chunks, opts)
}

/// [`decode_image`] over an already-walked chunk list. Factored out so
/// [`decode_over_background`], which needs its own look at the `bKGD`
/// chunk bytes, can share one chunk walk — the walk re-validates every
/// chunk CRC, so doing it once instead of twice halves the whole-file
/// CRC pass.
pub(crate) fn decode_png_chunks(chunks: &[ChunkRef<'_>], opts: &DecodeOptions) -> Result<PngImage> {
    let ihdr_chunk = chunks
        .iter()
        .find(|c| c.is_type(b"IHDR"))
        .ok_or_else(|| Error::invalid("PNG: missing IHDR"))?;
    // IHDR field validity (§11.2.1: non-zero dimensions, Table 12
    // colour-type/bit-depth combination, compression / filter / interlace
    // method) is enforced inside `Ihdr::parse` → `Ihdr::validate`, so the
    // checks formerly duplicated here are no longer needed.
    let ihdr = Ihdr::parse(ihdr_chunk.data)?;
    // Resource limits are checked against the header alone, before any
    // pixel buffer exists (IMAGE_CRATE_API: "limits are enforced before
    // allocation").
    opts.check(ihdr.width, ihdr.height, ihdr.decoded_plane_bytes()?)?;
    let side = crate::sideinfo::extract(chunks, opts.strict, opts.inflate_metadata)?;

    // W3C PNG3 §5.4 / §13.1: a decoder "encountering an unknown chunk in
    // which the ancillary bit is 0" — a critical chunk it cannot
    // interpret — "shall indicate to the user that the image contains
    // information it cannot safely interpret". We surface that as a hard
    // error rather than silently producing a possibly-wrong image. Every
    // critical chunk this codec understands (IHDR / PLTE / IDAT / IEND)
    // is in the allow-set; an unrecognised critical chunk type is
    // rejected. Unknown *ancillary* chunks are ignored here (the
    // standalone decode path produces pixels only); a PNG editor that
    // wants to carry them forward uses `parse_metadata`'s `unknowns`.
    for c in chunks {
        let ty = c.type_code();
        if ty.is_critical() && !matches!(&c.chunk_type, b"IHDR" | b"PLTE" | b"IDAT" | b"IEND") {
            return Err(Error::invalid(format!(
                "PNG: unrecognised critical chunk {:?} (W3C PNG3 §5.4)",
                ty.as_str()
            )));
        }
    }

    // W3C PNG3 §5.6 / §11.2.3: the IDAT run shall be consecutive (the
    // decoder concatenates IDAT payloads into one zlib stream).
    validate_idat_consecutive(chunks)?;

    let mut plte: Option<&[u8]> = None;
    let mut trns: Option<&[u8]> = None;
    let mut idat_slices: Vec<&[u8]> = Vec::new();
    let mut idat_total_len = 0usize;
    for c in chunks {
        if c.is_type(b"PLTE") {
            plte = Some(c.data);
        } else if c.is_type(b"tRNS") {
            trns = Some(c.data);
        } else if c.is_type(b"IDAT") {
            idat_total_len += c.data.len();
            idat_slices.push(c.data);
        }
    }
    validate_trns(&ihdr, trns, plte)?;

    if idat_total_len == 0 {
        return Err(Error::invalid("PNG: no IDAT chunks"));
    }
    // Single-chunk streams (the common case — this crate's own encoder
    // emits exactly one IDAT) borrow the payload straight out of the
    // source buffer; only a genuinely split IDAT run pays for the
    // concatenation copy.
    let idat_concat: Vec<u8>;
    let idat_stream: &[u8] = if idat_slices.len() == 1 {
        idat_slices[0]
    } else {
        let mut buf = Vec::with_capacity(idat_total_len);
        for s in &idat_slices {
            buf.extend_from_slice(s);
        }
        idat_concat = buf;
        &idat_concat
    };

    let frame_pixels = inflate_image_pixels(idat_stream, &ihdr, "PNG: IDAT", "IHDR")?;
    let img = build_png_image(&ihdr, frame_pixels, plte, trns)?;
    Ok(img.with_color(side.color).with_metadata(side.metadata))
}

/// Inflate a zlib pixel stream and reconstruct it into the decoded byte
/// plane (`decoded_bytes_per_pixel × width × height`, or the packed
/// sub-byte plane expanded as [`expand_byte_plane`] does).
///
/// Non-interlaced images stream: the inflater fills one wire row
/// (filter byte + `row_bytes`) at a time, which is reconstructed in
/// place into the output plane against the previous reconstructed row
/// — the whole filtered image is never materialised, so a decode holds
/// the compressed bytes and the output plane only. Interlaced images
/// keep the inflate-then-scatter path (the passes are strided
/// gathers, not output rows).
///
/// Both paths enforce the same bound: the stream must inflate to
/// exactly the filtered-stream size the header implies. A longer
/// stream — a bomb-shaped payload behind a small declared canvas — is
/// rejected the moment the first excess byte appears, never buffered
/// (W3C PNG3 §13.3); a shorter one is a truncation error. `what` /
/// `header` name the chunk and the dimension source in messages
/// ("PNG: IDAT" / "IHDR", "APNG: frame data" / "fcTL").
fn inflate_image_pixels(stream: &[u8], ihdr: &Ihdr, what: &str, header: &str) -> Result<Vec<u8>> {
    let expected = ihdr.expected_filtered_len()?;
    let bomb = || {
        Error::invalid(format!(
            "{what} inflates past the {expected}-byte filtered-stream size \
             implied by {header} (decompression bomb, W3C PNG3 §13.3)"
        ))
    };
    if ihdr.interlace != 0 {
        // Cap the inflate at the implied size (+1 so an overlong stream
        // is detected as such) instead of committing attacker-chosen
        // memory.
        let pixels = decompress_to_vec_zlib_capped(stream, expected.saturating_add(1)).map_err(
            |e| match e {
                compcol::Error::OutputLimitExceeded => bomb(),
                e => Error::invalid(format!("{what}: zlib decompress failed: {e:?}")),
            },
        )?;
        return decode_image_pixels(&pixels, ihdr);
    }

    let row_bytes = ihdr.row_bytes()?;
    let bpp = ihdr.bpp_for_filter()?;
    let height = ihdr.height as usize;
    let wire_row = 1 + row_bytes;
    let mut raw = vec![0u8; row_bytes * height];
    let mut wire = vec![0u8; wire_row];
    let zero_row = vec![0u8; row_bytes];
    let mut dec = compcol::zlib::Decoder::new();
    let mut consumed = 0usize;
    let zerr = |e: compcol::Error| Error::invalid(format!("{what}: zlib decompress failed: {e:?}"));
    let truncated = || {
        Error::invalid(format!(
            "{what}: zlib stream ends before the {expected}-byte filtered-stream \
             size implied by {header}"
        ))
    };
    for y in 0..height {
        // Fill one wire row.
        let mut filled = 0usize;
        while filled < wire_row {
            let (p, status) = dec
                .decode(&stream[consumed..], &mut wire[filled..])
                .map_err(zerr)?;
            consumed += p.consumed;
            filled += p.written;
            match status {
                Status::StreamEnd if filled < wire_row => return Err(truncated()),
                Status::InputEmpty if p.written == 0 && consumed >= stream.len() => {
                    return Err(truncated());
                }
                _ => {}
            }
        }
        let filter_type = FilterType::from_u8(wire[0])?;
        let dst_start = y * row_bytes;
        let (prev_rows, curr_rows) = raw.split_at_mut(dst_start);
        let curr = &mut curr_rows[..row_bytes];
        curr.copy_from_slice(&wire[1..]);
        // RFC 2083 §6.3: the prior scanline of the first row is all
        // zeros.
        let prev: &[u8] = if y == 0 {
            &zero_row
        } else {
            &prev_rows[(y - 1) * row_bytes..(y - 1) * row_bytes + row_bytes]
        };
        unfilter_row(filter_type, curr, prev, bpp)?;
    }
    // Everything the header implies has been produced. The stream must
    // now end: one more byte of output is a bomb, and a stream that
    // never reaches its trailer is truncated.
    let mut scratch = [0u8; 1];
    loop {
        let (p, status) = dec
            .decode(&stream[consumed..], &mut scratch)
            .map_err(zerr)?;
        consumed += p.consumed;
        if p.written > 0 {
            return Err(bomb());
        }
        match status {
            Status::StreamEnd => break,
            _ if p.consumed == 0 => return Err(truncated()),
            _ => {}
        }
    }
    expand_byte_plane(raw, ihdr, ihdr.width as usize, height)
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

/// Promote a decoded [`PngImage`] to RGBA, first checking that every
/// `Pal8` index addresses a palette entry — the fallible path behind
/// [`crate::decode_rgba8`] / [`crate::decode_rgb8`] (an index past the
/// `PLTE` is malformed data, not a black pixel).
pub(crate) fn check_palette_indices(img: &PngImage) -> Result<()> {
    if img.format != PngPixelFormat::Pal8 {
        return Ok(());
    }
    let entries = img.palette.as_ref().map(Palette::len).unwrap_or(0);
    if entries == 0 {
        return Err(Error::invalid(
            "PNG: Pal8 image missing PLTE chunk for RGBA promotion",
        ));
    }
    let w = img.width as usize;
    let stride = img.stride();
    for row in img.data().chunks(stride.max(1)).take(img.height as usize) {
        if let Some(&idx) = row.iter().take(w).find(|&&i| usize::from(i) >= entries) {
            return Err(Error::invalid(format!(
                "PNG: palette index {idx} out of bounds (PLTE has {entries} entries)"
            )));
        }
    }
    Ok(())
}

/// [`check_palette_indices`] then [`PngImage::to_rgba8`].
pub(crate) fn image_to_rgba_checked(img: &PngImage) -> Result<RgbaImage> {
    check_palette_indices(img)?;
    Ok(RgbaImage::new(img.width, img.height, img.to_rgba8()))
}

/// The §13.15 "reasonable choice" of background when a datastream carries
/// no `bKGD` chunk and the caller has no preferred background of its own:
/// a medium grey of `153` in the 8-bit sRGB colour space (W3C PNG3
/// §13.15 "When no other information is available, a medium grey such as
/// 153 in the 8-bit sRGB color space would be a reasonable choice").
pub const DEFAULT_BACKGROUND_GREY: [u8; 3] = [153, 153, 153];

/// Decode a PNG and composite it over a solid background colour, returning
/// an opaque 8-bit [`RgbaImage`] (every pixel `α = 255`).
///
/// This is the §13.15 / §13.16 "display the image against a background"
/// operation, the path a viewer that cannot show real transparency
/// (alpha-over-page) takes. Decoding proceeds exactly as
/// [`crate::decode_rgba8`] — palette / `tRNS` / grayscale-widening / 16→8
/// promotion all happen first — then every pixel's straight alpha is
/// composited over the background in **linear light** (§13.16 "This
/// computation should be performed with intensity samples, not
/// gamma-encoded samples"): `out = α·foreground + (1−α)·background`,
/// per channel, via [`crate::srgb::composite_over_background`].
///
/// The background colour is chosen, in order:
///
/// 1. `override_bg`, when the caller supplies one (a web browser
///    "should ignore the bKGD chunk … overriding bKGD with their
///    preferred background color", §13.15);
/// 2. otherwise the datastream's `bKGD` chunk, resolved to 8-bit sRGB via
///    [`Bkgd::resolve_rgb8`] (grayscale replicated, sub/super-8-bit
///    rescaled per §13.12, palette index looked up in `PLTE`);
/// 3. otherwise [`DEFAULT_BACKGROUND_GREY`] (the §13.15 medium-grey 153).
///
/// A fully-opaque image (no transparency anywhere) is returned unchanged
/// except that its already-`255` alpha is confirmed; a fully-transparent
/// pixel becomes the background colour exactly.
///
/// Standalone (no `oxideav-core`) entry point: works whether or not the
/// `registry` feature is enabled. Uses [`DecodeOptions::default`].
pub fn decode_over_background(buf: &[u8], override_bg: Option<[u8; 3]>) -> Result<RgbaImage> {
    let chunks = parse_all_chunks(buf)?;
    let ihdr_chunk = chunks
        .iter()
        .find(|c| c.is_type(b"IHDR"))
        .ok_or_else(|| Error::invalid("PNG: missing IHDR"))?;
    let ihdr = Ihdr::parse(ihdr_chunk.data)?;

    let mut plte: Option<&[u8]> = None;
    let mut bkgd_raw: Option<&[u8]> = None;
    for c in &chunks {
        if c.is_type(b"PLTE") {
            plte = Some(c.data);
        } else if c.is_type(b"bKGD") {
            bkgd_raw = Some(c.data);
        }
    }

    let mut bitmap = {
        // Shared chunk walk — one CRC-validating pass over the file,
        // not two.
        let img = decode_png_chunks(&chunks, &DecodeOptions::default())?;
        image_to_rgba_checked(&img)?
    };

    // §13.15: caller override > bKGD chunk > medium-grey default.
    let bg = match override_bg {
        Some(rgb) => rgb,
        None => match bkgd_raw {
            Some(raw) => {
                let bk = Bkgd::parse(raw, ihdr.colour_type, ihdr.bit_depth)?;
                bk.resolve_rgb8(ihdr.bit_depth, plte)?
            }
            None => DEFAULT_BACKGROUND_GREY,
        },
    };

    crate::srgb::composite_over_background(&mut bitmap, bg);
    Ok(bitmap)
}

/// Pack the raw decoded byte plane into a [`PngImage`] with the IHDR's
/// output pixel format. For 16-bit formats this swaps PNG's BE samples
/// to little-endian; for colour type 4 / 16-bit it expands `(gray,
/// alpha)` into `(gray, gray, gray, alpha)` because we have no native
/// Ya16 pixel format.
///
/// Takes the raw plane by value: the 8-bit formats pass the buffer
/// through untouched and the 16-bit endian swap happens in place, so
/// the decode pipeline hands one allocation from reconstruction to the
/// returned [`PngImage`] instead of re-copying the whole plane here.
/// Only the ct=4/16-bit `(G16, A16) → RGBA64` widening allocates.
fn build_png_image(
    ihdr: &Ihdr,
    raw: Vec<u8>,
    plte: Option<&[u8]>,
    trns: Option<&[u8]>,
) -> Result<PngImage> {
    let pf = ihdr.output_pixel_format()?;
    let w = ihdr.width as usize;
    let h = ihdr.height as usize;

    let (stride, data) = match pf {
        PngPixelFormat::Gray8 => (w, raw),
        PngPixelFormat::Pal8 => {
            let _plte =
                plte.ok_or_else(|| Error::invalid("PNG: colour type 3 requires PLTE chunk"))?;
            (w, raw)
        }
        PngPixelFormat::Gray16Le => (w * 2, swap16_in_place(raw)),
        PngPixelFormat::Rgb24 => (w * 3, raw),
        PngPixelFormat::Rgba => (w * 4, raw),
        PngPixelFormat::Rgb48Le => (w * 6, swap16_in_place(raw)),
        PngPixelFormat::Rgba64Le => {
            // Two cases: genuinely RGBA 16 (ct=6, bd=16) or gray+alpha 16 (ct=4, bd=16).
            if ihdr.colour_type == 6 {
                (w * 8, swap16_in_place(raw))
            } else {
                // colour type 4 + 16 bit → (G16, A16) in BE per sample.
                // Expand to (G,G,G,A) LE.
                let mut out = vec![0u8; w * h * 8];
                for (src, dst) in raw
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(out.as_chunks_mut::<8>().0)
                {
                    let (g_hi, g_lo, a_hi, a_lo) = (src[0], src[1], src[2], src[3]);
                    dst[0] = g_lo;
                    dst[1] = g_hi;
                    dst[2] = g_lo;
                    dst[3] = g_hi;
                    dst[4] = g_lo;
                    dst[5] = g_hi;
                    dst[6] = a_lo;
                    dst[7] = a_hi;
                }
                (w * 8, out)
            }
        }
        PngPixelFormat::Ya8 => (w * 2, raw),
        // Never an output layout (colour type 4 / 16-bit widens to
        // Rgba64Le above); kept for completeness.
        PngPixelFormat::Ya16Le => (w * 4, swap16_in_place(raw)),
    };

    let mut img = PngImage::packed(ihdr.width, ihdr.height, pf, stride, data)?;
    if pf == PngPixelFormat::Pal8 {
        img.palette = Some(Palette::from_rgb(plte.unwrap_or(&[]), trns));
    } else if let Some(t) = trns {
        // Keyed transparency for the alpha-less layouts (ct 0 / 2);
        // `validate_trns` has already checked the payload shape, so a
        // parse failure here can only be a ct 4 / 6 chunk the caller
        // let through — which `validate_trns` rejects first.
        if matches!(ihdr.colour_type, 0 | 2) {
            img.transparency = Some(Trns::parse(t, ihdr.colour_type, ihdr.bit_depth, None)?);
        }
    }
    Ok(img)
}

/// Swap every 2-byte sample of a network-byte-order plane to
/// little-endian in place and return the same buffer (PNG stores 16-bit
/// samples "most significant byte first", RFC 2083 §2.1; the decoder's
/// 16-bit output formats are LE). The paired-byte walk compiles to a
/// straight-line byte-shuffle loop and avoids allocating a second
/// full-size plane just to reorder bytes.
fn swap16_in_place(mut plane: Vec<u8>) -> Vec<u8> {
    for px in plane.as_chunks_mut::<2>().0 {
        px.swap(0, 1);
    }
    plane
}

/// Decompressed-zlib → unfiltered → (optionally expanded sub-byte, and for
/// Adam7 interlaced streams, scattered into the full canvas) byte plane.
///
/// The output layout is always "row-major, `decoded_bytes_per_pixel` bytes
/// per pixel, tightly packed (stride = width * bpp)".
pub(crate) fn decode_image_pixels(decompressed: &[u8], ihdr: &Ihdr) -> Result<Vec<u8>> {
    if ihdr.interlace == 0 {
        let raw = reconstruct_filtered(decompressed, ihdr)?;
        expand_byte_plane(raw, ihdr, ihdr.width as usize, ihdr.height as usize)
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
        let expanded = expand_byte_plane(raw, ihdr, pw, ph)?;

        // Scatter `expanded` (pw × ph, out_bpp bytes/pixel) into `canvas`.
        if cs == 1 {
            // Column-spacing 1 (pass 7): the pass row is a contiguous
            // run in the canvas row — one memcpy per row.
            let run = pw * out_bpp;
            for py in 0..ph {
                let dst_y = sr + py * rs;
                let dst_off = (dst_y * img_w + sc) * out_bpp;
                canvas[dst_off..dst_off + run].copy_from_slice(&expanded[py * run..(py + 1) * run]);
            }
        } else {
            // Strided scatter, specialised per pixel width so the
            // per-pixel copy is a fixed-size move instead of a
            // variable-length memcpy call.
            match out_bpp {
                1 => scatter_pass::<1>(&mut canvas, &expanded, img_w, pw, ph, sr, sc, rs, cs),
                2 => scatter_pass::<2>(&mut canvas, &expanded, img_w, pw, ph, sr, sc, rs, cs),
                3 => scatter_pass::<3>(&mut canvas, &expanded, img_w, pw, ph, sr, sc, rs, cs),
                4 => scatter_pass::<4>(&mut canvas, &expanded, img_w, pw, ph, sr, sc, rs, cs),
                6 => scatter_pass::<6>(&mut canvas, &expanded, img_w, pw, ph, sr, sc, rs, cs),
                8 => scatter_pass::<8>(&mut canvas, &expanded, img_w, pw, ph, sr, sc, rs, cs),
                _ => {
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

/// Scatter one Adam7 pass sub-image into the canvas with a const pixel
/// width: `expanded` is `pw × ph` pixels of `N` bytes each; the
/// destination pixel for pass cell `(px, py)` sits at column
/// `sc + px * cs` of canvas row `sr + py * rs` (§A.8 grid). The
/// fixed-size `N`-byte move compiles to a plain load/store pair per
/// pixel instead of a variable-length `memcpy` call.
#[allow(clippy::too_many_arguments)]
fn scatter_pass<const N: usize>(
    canvas: &mut [u8],
    expanded: &[u8],
    img_w: usize,
    pw: usize,
    ph: usize,
    sr: usize,
    sc: usize,
    rs: usize,
    cs: usize,
) {
    for py in 0..ph {
        let dst_y = sr + py * rs;
        let src_row = &expanded[py * pw * N..(py + 1) * pw * N];
        let dst_row = &mut canvas[dst_y * img_w * N..(dst_y * img_w + sc + (pw - 1) * cs + 1) * N];
        for (px, src) in src_row.as_chunks::<N>().0.iter().enumerate() {
            let dst_off = (sc + px * cs) * N;
            dst_row[dst_off..dst_off + N].copy_from_slice(src);
        }
    }
}

/// Given a raw (unfiltered) PNG byte plane at native bit depth, expand it to
/// the byte layout consumed by `build_png_image`. For sub-byte gray/pal,
/// this means unpacking 2/4/8 pixels per byte and (for grayscale) scaling
/// up to 8-bit. For ≥8-bit data this is a straight copy.
///
/// `w`/`h` are the logical pixel dimensions of the image the raw bytes
/// represent (the *pass* dimensions for an Adam7 pass, or the full image
/// dimensions otherwise).
fn expand_byte_plane(raw: Vec<u8>, ihdr: &Ihdr, w: usize, h: usize) -> Result<Vec<u8>> {
    if ihdr.bit_depth >= 8 {
        // Sanity check — caller passed us matching-sized data. The plane
        // is already in the output layout, so it passes through untouched
        // (no re-copy).
        let bpp = ihdr.decoded_bytes_per_pixel()?;
        let expected = w * h * bpp;
        if raw.len() != expected {
            return Err(Error::invalid(format!(
                "PNG: expand_byte_plane expected {expected} bytes, got {}",
                raw.len()
            )));
        }
        return Ok(raw);
    }

    // Sub-byte: only colour type 0 (grayscale) or 3 (indexed) allowed.
    let bd = ihdr.bit_depth as usize;
    if ihdr.colour_type != 0 && ihdr.colour_type != 3 {
        return Err(Error::invalid(format!(
            "PNG: colour type {} cannot have bit depth {}",
            ihdr.colour_type, bd
        )));
    }
    let row_bytes_packed = (w * bd).div_ceil(8);
    let expected = row_bytes_packed * h;
    if raw.len() != expected {
        return Err(Error::invalid(format!(
            "PNG: expand_byte_plane (sub-byte) expected {expected} bytes, got {}",
            raw.len()
        )));
    }

    // Scale table for grayscale: 1-bit → ×255, 2-bit → ×85, 4-bit → ×17.
    // (PNG spec §13.12.) Indexed leaves the raw index (×1).
    let scale = match (ihdr.colour_type, bd) {
        (0, 1) => 255,
        (0, 2) => 85,
        (0, 4) => 17,
        _ => 1,
    };

    let mut out = vec![0u8; w * h];
    // IHDR validation (§11.2.1 Table 12) admits exactly 1 / 2 / 4 here;
    // the const-depth instantiations let the per-byte unpack unroll to
    // straight shifts with no per-pixel division.
    match bd {
        1 => expand_subbyte_plane::<1>(&raw, &mut out, w, row_bytes_packed, scale),
        2 => expand_subbyte_plane::<2>(&raw, &mut out, w, row_bytes_packed, scale),
        4 => expand_subbyte_plane::<4>(&raw, &mut out, w, row_bytes_packed, scale),
        other => {
            return Err(Error::invalid(format!(
                "PNG: expand_byte_plane: unexpected sub-byte depth {other}"
            )));
        }
    }
    Ok(out)
}

/// Unpack one MSB-first sub-byte plane (`BD` ∈ {1, 2, 4} bits per
/// sample, RFC 2083 §2.3 packing order) into one byte per pixel,
/// multiplying by `scale` (§13.12 gray promotion, or ×1 for palette
/// indices). Walks each row byte-at-a-time and emits its
/// `8 / BD` pixels with compile-time shifts — no per-pixel `x /
/// pixels_per_byte` arithmetic — with the row's final partial byte
/// handled separately so padding bits (§2.3 "wasted bits") never reach
/// the output.
fn expand_subbyte_plane<const BD: usize>(
    raw: &[u8],
    out: &mut [u8],
    w: usize,
    row_bytes_packed: usize,
    scale: u8,
) {
    let ppb = 8 / BD; // pixels per packed byte
    let mask: u8 = ((1u16 << BD) - 1) as u8;
    for (src_row, dst_row) in raw
        .chunks_exact(row_bytes_packed)
        .zip(out.chunks_exact_mut(w))
    {
        let mut dst = dst_row.chunks_exact_mut(ppb);
        let mut src = src_row.iter();
        for chunk in &mut dst {
            // Full byte: emit all 8/BD samples, leftmost pixel in the
            // high-order bits.
            let byte = *src.next().expect("packed row shorter than pixel count");
            for (k, d) in chunk.iter_mut().enumerate() {
                *d = ((byte >> (8 - BD * (k + 1))) & mask).wrapping_mul(scale);
            }
        }
        let rem = dst.into_remainder();
        if !rem.is_empty() {
            let byte = *src.next().expect("packed row shorter than pixel count");
            for (k, d) in rem.iter_mut().enumerate() {
                *d = ((byte >> (8 - BD * (k + 1))) & mask).wrapping_mul(scale);
            }
        }
    }
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
#[non_exhaustive]
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
    /// Colour signalling of the stream (shared by every frame).
    pub color: crate::image::ColorInfo,
    /// ICC / Exif / XMP / gamma of the stream (shared by every frame).
    pub metadata: crate::image::Metadata,
}

impl ApngInfo {
    /// Assemble a parsed-APNG description from its parts.
    pub fn new(
        ihdr: Ihdr,
        plte: Option<Vec<u8>>,
        trns: Option<Vec<u8>>,
        actl: Actl,
        frames: Vec<ApngFrame>,
        first_frame_is_default: bool,
    ) -> Self {
        Self {
            ihdr,
            plte,
            trns,
            actl,
            frames,
            first_frame_is_default,
            color: crate::image::ColorInfo::png_default(),
            metadata: crate::image::Metadata::default(),
        }
    }

    /// Set the colour signalling.
    pub fn with_color(mut self, color: crate::image::ColorInfo) -> Self {
        self.color = color;
        self
    }

    /// Set the metadata.
    pub fn with_metadata(mut self, metadata: crate::image::Metadata) -> Self {
        self.metadata = metadata;
        self
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub struct ApngFrame {
    pub fctl: Fctl,
    /// Concatenated compressed data: IDAT payload or fdAT payloads stripped
    /// of their 4-byte sequence number.
    pub compressed: Vec<u8>,
}

impl ApngFrame {
    /// Pair a frame control record with its concatenated compressed pixel data.
    pub fn new(fctl: Fctl, compressed: Vec<u8>) -> Self {
        Self { fctl, compressed }
    }
}

/// Parse an APNG file and return metadata + per-frame compressed segments.
/// Returns `Err` if the file is a plain PNG without `acTL`.
pub fn parse_apng(buf: &[u8]) -> Result<ApngInfo> {
    parse_apng_with(buf, &DecodeOptions::default())
}

/// [`parse_apng`] under `opts`: the canvas is checked against the
/// limits before any frame is inflated, and `strict` governs the
/// colour / metadata chunk parse (the APNG control-chunk rules are
/// always enforced).
pub(crate) fn parse_apng_with(buf: &[u8], opts: &DecodeOptions) -> Result<ApngInfo> {
    let chunks = parse_all_chunks(buf)?;
    parse_apng_chunks(&chunks, opts)
}

/// [`parse_apng_with`] over an already-walked chunk list.
pub(crate) fn parse_apng_chunks(chunks: &[ChunkRef<'_>], opts: &DecodeOptions) -> Result<ApngInfo> {
    let ihdr = Ihdr::parse(
        chunks
            .iter()
            .find(|c| c.is_type(b"IHDR"))
            .ok_or_else(|| Error::invalid("PNG: missing IHDR"))?
            .data,
    )?;
    // W3C PNG3 §5.4 / §14.2: refuse an unrecognised critical chunk on the
    // APNG path too, matching `decode`. The APNG control chunks
    // (acTL / fcTL / fdAT) carry the ancillary bit, so the critical
    // allow-set is the same four core chunks.
    for c in chunks {
        let ty = c.type_code();
        if ty.is_critical() && !matches!(&c.chunk_type, b"IHDR" | b"PLTE" | b"IDAT" | b"IEND") {
            return Err(Error::invalid(format!(
                "PNG: unrecognised critical chunk {:?} (W3C PNG3 §5.4)",
                ty.as_str()
            )));
        }
    }

    // W3C PNG3 §5.6 Table 7 ancillary ordering applies to the animated
    // stream too — an APNG carrying a mis-placed colour-space /
    // background / physical-dimension chunk is as malformed as a static
    // PNG. Shared validator with `parse_metadata`.
    validate_ancillary_ordering(chunks)?;
    // §5.6 / §11.2.3: the default image's IDAT run shall be consecutive.
    validate_idat_consecutive(chunks)?;
    opts.check(ihdr.width, ihdr.height, ihdr.decoded_plane_bytes()?)?;
    let side = crate::sideinfo::extract(chunks, opts.strict, opts.inflate_metadata)?;

    let actl_index = chunks
        .iter()
        .position(|c| c.is_type(b"acTL"))
        .ok_or_else(|| Error::invalid("PNG: not animated (no acTL)"))?;
    // W3C PNG3 §5.6 Table: acTL "Multiple OK? No". A second acTL would
    // declare two conflicting animations in one stream; reject it.
    if chunks.iter().filter(|c| c.is_type(b"acTL")).count() > 1 {
        return Err(Error::invalid(
            "PNG acTL: appears more than once (W3C PNG3 §5.6: \"Multiple OK? No\")",
        ));
    }
    // W3C PNG3 §4.9 / §11.3.4: "The acTL chunk must appear before the first
    // IDAT chunk within a valid PNG stream." An acTL that follows IDAT names
    // an animation the renderer would have to retro-fit onto an already-shown
    // static image; reject it as malformed rather than silently animating.
    if let Some(first_idat) = chunks.iter().position(|c| c.is_type(b"IDAT"))
        && actl_index > first_idat
    {
        return Err(Error::invalid(
            "PNG acTL: appears after the first IDAT (W3C PNG3 §4.9: \
                 \"The acTL chunk must appear before the first IDAT chunk\")",
        ));
    }
    let actl = Actl::parse(chunks[actl_index].data)?;

    let mut plte: Option<Vec<u8>> = None;
    let mut trns: Option<Vec<u8>> = None;

    // W3C PNG3 §4.9: "num_frames ... 0 is not a valid value." Reject an
    // acTL that announces zero frames before walking the chain. (A mismatch
    // between num_frames and the actual fcTL count remains advisory — see the
    // note below — but the literal-zero floor is unambiguous.)
    if actl.num_frames == 0 {
        return Err(Error::invalid(
            "PNG acTL: num_frames is 0 (W3C PNG3 §4.9: \"0 is not a valid value\")",
        ));
    }

    let mut frames: Vec<ApngFrame> = Vec::new();
    let mut current_fctl: Option<Fctl> = None;
    let mut current_compressed: Vec<u8> = Vec::new();
    let mut saw_idat = false;
    let mut first_frame_is_default = false;
    // Shared fcTL/fdAT sequence stream (W3C PNG3 §4.9.2), validated below.
    let mut seq_stream: Vec<crate::apng::SeqChunk> = Vec::new();

    for c in chunks {
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
                seq_stream.push(crate::apng::SeqChunk {
                    kind: crate::apng::SeqKind::Fctl,
                    sequence_number: f.sequence_number,
                });
                // An fcTL appearing before any IDAT means the default image
                // doubles as the first animation frame.
                if !saw_idat {
                    // W3C PNG3 §11.3.5.1: "The fcTL chunk corresponding to the
                    // default image, if it exists, has these restrictions: the
                    // x_offset and y_offset fields must be 0. The width and
                    // height fields must equal the corresponding fields from
                    // the IHDR chunk." The default-image fcTL is precisely the
                    // one that precedes the first IDAT.
                    if !first_frame_is_default {
                        if f.x_offset != 0 || f.y_offset != 0 {
                            return Err(Error::invalid(format!(
                                "APNG: default-image fcTL must have x_offset=0 \
                                 y_offset=0, got ({}, {}) (W3C PNG3 §11.3.5.1)",
                                f.x_offset, f.y_offset
                            )));
                        }
                        if f.width != ihdr.width || f.height != ihdr.height {
                            return Err(Error::invalid(format!(
                                "APNG: default-image fcTL dimensions {}x{} must \
                                 equal IHDR {}x{} (W3C PNG3 §11.3.5.1)",
                                f.width, f.height, ihdr.width, ihdr.height
                            )));
                        }
                    }
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
                let (seq, payload) = parse_fdat(c.data)?;
                seq_stream.push(crate::apng::SeqChunk {
                    kind: crate::apng::SeqKind::Fdat,
                    sequence_number: seq,
                });
                current_compressed.extend_from_slice(payload);
            }
            _ => {}
        }
    }

    // §4.9.2: the first fcTL carries sequence number 0 and every subsequent
    // fcTL/fdAT is contiguous-ascending. An out-of-order, gapped, or duplicate
    // sequence is an error ("Decoders shall treat out-of-order APNG chunks as
    // an error").
    crate::apng::validate_apng_sequence(&seq_stream)?;
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

    Ok(
        ApngInfo::new(ihdr, plte, trns, actl, frames, first_frame_is_default)
            .with_color(side.color)
            .with_metadata(side.metadata),
    )
}

/// Decode an entire APNG file into its composited per-frame canvases.
/// Standalone (no `oxideav-core`) entry point.
pub fn decode_apng(buf: &[u8]) -> Result<ApngImage> {
    let info = parse_apng(buf)?;
    decode_apng_info(&info)
}

/// Per-pixel transparency key for `APNG_BLEND_OP_OVER` on canvas formats
/// that carry no alpha channel of their own (palette / grayscale / truecolour).
///
/// W3C PNG3 §11.3.6.2 says an OVER frame "should be composited onto the output
/// buffer based on its alpha". For colour types 0/2/3 the alpha is not in the
/// pixel — it comes from a `tRNS` chunk (RFC 2083 §4.2.9): a keyed gray/RGB
/// sample (types 0/2) or a per-palette-entry alpha tail (type 3). Because the
/// composited canvas is itself one of these alpha-less formats, an OVER pixel
/// can only be fully transparent (leave the canvas) or fully written — there
/// is no representable partial blend. This key identifies the transparent
/// source pixels so OVER respects them rather than degrading to a plain
/// overwrite. Without `tRNS` every pixel is opaque and OVER == SOURCE.
#[derive(Clone, Debug)]
enum TransparencyKey {
    /// Every pixel opaque (no `tRNS`) — OVER behaves as SOURCE.
    None,
    /// Colour type 3 (Pal8): palette index `i` is transparent iff
    /// `alpha[i] == 0`. Indices beyond the tail are opaque.
    Palette(Vec<u8>),
    /// Colour type 0 (Gray8): the 8-bit canvas value that is transparent.
    Gray8(u8),
    /// Colour type 0 (Gray16Le): the little-endian 16-bit value pair that
    /// is transparent.
    Gray16([u8; 2]),
    /// Colour type 2 (Rgb24): the transparent RGB triple.
    Rgb24([u8; 3]),
    /// Colour type 2 (Rgb48Le): the transparent little-endian 48-bit pixel.
    Rgb48([u8; 6]),
}

impl TransparencyKey {
    /// Derive the canvas-format transparency key from the IHDR + parsed
    /// `tRNS`. Returns [`TransparencyKey::None`] when the format carries its
    /// own alpha (types 4/6) or no `tRNS` is present. The keyed gray/RGB
    /// samples in `tRNS` are at the IHDR bit-depth's natural range and are
    /// scaled to the canvas byte layout: an 8-bit grayscale canvas scales a
    /// sub-8-bit keyed sample exactly as the pixel plane was scaled during
    /// decode (`v * 255 / max`), and 16-bit samples are stored low-byte-first.
    fn derive(ihdr: &Ihdr, fmt: PngPixelFormat, trns: Option<&[u8]>, plte_len: usize) -> Self {
        let Some(trns_bytes) = trns else {
            return Self::None;
        };
        let plte_entries = plte_len / 3;
        let Ok(parsed) = Trns::parse(
            trns_bytes,
            ihdr.colour_type,
            ihdr.bit_depth,
            Some(plte_entries),
        ) else {
            return Self::None;
        };
        match (fmt, parsed) {
            (PngPixelFormat::Pal8, Trns::Palette(alpha)) => Self::Palette(alpha),
            (PngPixelFormat::Gray8, Trns::Grayscale(v)) => {
                // Sub-8-bit grayscale is expanded to Gray8 by v * 255 / max.
                let max = (1u32 << ihdr.bit_depth) - 1;
                let scaled = ((v as u32 * 255 + max / 2) / max) as u8;
                Self::Gray8(scaled)
            }
            (PngPixelFormat::Gray16Le, Trns::Grayscale(v)) => Self::Gray16(v.to_le_bytes()),
            (PngPixelFormat::Rgb24, Trns::Rgb(r, g, b)) => Self::Rgb24([r as u8, g as u8, b as u8]),
            (PngPixelFormat::Rgb48Le, Trns::Rgb(r, g, b)) => {
                let mut px = [0u8; 6];
                px[0..2].copy_from_slice(&r.to_le_bytes());
                px[2..4].copy_from_slice(&g.to_le_bytes());
                px[4..6].copy_from_slice(&b.to_le_bytes());
                Self::Rgb48(px)
            }
            _ => Self::None,
        }
    }

    /// Whether the source pixel `src` (one canvas-format pixel) is fully
    /// transparent and should leave the canvas untouched under OVER.
    fn is_transparent(&self, src: &[u8]) -> bool {
        match self {
            Self::None => false,
            Self::Palette(alpha) => {
                let idx = src[0] as usize;
                alpha.get(idx).copied().unwrap_or(255) == 0
            }
            Self::Gray8(v) => src[0] == *v,
            Self::Gray16(v) => src[0] == v[0] && src[1] == v[1],
            Self::Rgb24(v) => src == v,
            Self::Rgb48(v) => src == v,
        }
    }
}

impl TransparencyKey {
    /// One canvas-format pixel that reads as "fully transparent black"
    /// (W3C PNG3 §11.3.6.2 `APNG_DISPOSE_OP_BACKGROUND`, and the §4.9.3
    /// output-buffer state at the start of each play). Formats with an
    /// alpha channel clear to all-zero bytes. An alpha-less canvas can
    /// only express transparency through its `tRNS` key: the first
    /// palette index whose alpha is 0, or the keyed gray / RGB sample.
    /// Without one the format cannot represent transparency at all and
    /// the closest it has — all-zero (black, palette index 0) — stands
    /// in.
    fn clear_pixel(&self, bpp: usize) -> Vec<u8> {
        match self {
            Self::None => vec![0; bpp],
            Self::Palette(alpha) => {
                vec![alpha.iter().position(|&a| a == 0).unwrap_or(0) as u8]
            }
            Self::Gray8(v) => vec![*v],
            Self::Gray16(v) => v.to_vec(),
            Self::Rgb24(v) => v.to_vec(),
            Self::Rgb48(v) => v.to_vec(),
        }
    }
}

/// The APNG output buffer (W3C PNG3 §4.9.3) and the frame-by-frame
/// rendering rules of §11.3.6.2 `fcTL`: each frame is drawn into its
/// `x_offset, y_offset, width × height` region by `blend_op`, the
/// buffer is read out as that frame's picture, then the region is
/// disposed of by `dispose_op` before the next frame is drawn.
///
/// The standalone [`decode_apng_info`] and the registry decoder (fed
/// one frame per packet by the container) both render through this,
/// so the two paths produce the same pictures.
#[derive(Clone, Debug)]
pub(crate) struct ApngCanvas {
    width: u32,
    height: u32,
    format: PngPixelFormat,
    bpp: usize,
    stride: usize,
    trns_key: TransparencyKey,
    /// One pixel of "fully transparent black" in the canvas format.
    clear: Vec<u8>,
    buffer: Vec<u8>,
    /// Frames drawn since the buffer was last cleared.
    drawn: usize,
}

impl ApngCanvas {
    /// A cleared `canvas.width × canvas.height` output buffer in the
    /// layout every frame of the stream decodes to (each frame inherits
    /// the IHDR colour type and bit depth, §11.3.6.3; `PLTE` / `tRNS`
    /// give the transparency key of the alpha-less formats).
    pub(crate) fn new(canvas: &Ihdr, plte: Option<&[u8]>, trns: Option<&[u8]>) -> Result<Self> {
        let format = canvas.output_pixel_format()?;
        let bpp = format.bytes_per_pixel();
        let stride = (canvas.width as usize)
            .checked_mul(bpp)
            .ok_or_else(|| Error::invalid("APNG: canvas row size overflows"))?;
        let len = stride
            .checked_mul(canvas.height as usize)
            .ok_or_else(|| Error::invalid("APNG: canvas size overflows"))?;
        let trns_key =
            TransparencyKey::derive(canvas, format, trns, plte.map(|p| p.len()).unwrap_or(0));
        let clear = trns_key.clear_pixel(bpp);
        let mut buffer = vec![0u8; len];
        fill_pixels(&mut buffer, &clear);
        Ok(Self {
            width: canvas.width,
            height: canvas.height,
            format,
            bpp,
            stride,
            trns_key,
            clear,
            buffer,
            drawn: 0,
        })
    }

    /// Canvas pixel layout.
    pub(crate) fn format(&self) -> PngPixelFormat {
        self.format
    }

    /// Draw `frame` (decoded at its `fctl.width × fctl.height`, in the
    /// canvas layout) into the buffer and return the buffer as shown
    /// for this frame; the frame's region is then disposed of so the
    /// buffer is ready for the next one.
    pub(crate) fn render(&mut self, fctl: &Fctl, frame: &PngImage) -> Result<Vec<u8>> {
        // §11.3.6.2: the region "may not fall outside of the default
        // image" — rejected rather than clipped.
        fctl.validate_within_canvas(self.width, self.height)?;
        if frame.format() != self.format
            || frame.width() != fctl.width
            || frame.height() != fctl.height
        {
            return Err(Error::invalid(format!(
                "APNG: frame is {}x{} {:?}, its fcTL region is {}x{} on a {:?} canvas",
                frame.width(),
                frame.height(),
                frame.format(),
                fctl.width,
                fctl.height,
                self.format
            )));
        }
        // "If the first fcTL chunk uses a dispose_op of
        // APNG_DISPOSE_OP_PREVIOUS it should be treated as
        // APNG_DISPOSE_OP_BACKGROUND."
        let dispose = if self.drawn == 0 && fctl.dispose_op == Disposal::Previous {
            Disposal::Background
        } else {
            fctl.dispose_op
        };
        let (x, y) = (fctl.x_offset as usize, fctl.y_offset as usize);
        let (w, h) = (fctl.width as usize, fctl.height as usize);
        let run = w * self.bpp;
        // PREVIOUS reverts the region to "the previous contents": keep
        // the region as it was before this frame was drawn.
        let saved: Option<Vec<u8>> = (dispose == Disposal::Previous).then(|| {
            let mut v = Vec::with_capacity(run * h);
            for row in y..y + h {
                let at = row * self.stride + x * self.bpp;
                v.extend_from_slice(&self.buffer[at..at + run]);
            }
            v
        });

        // "for the first frame, the two blend modes are functionally
        // equivalent due to the clearing of the output buffer": OVER onto
        // the cleared buffer differs from SOURCE only in the colour
        // stored under fully transparent pixels, so the first frame
        // drawn after a clear is copied, colour of its transparent
        // pixels included.
        let blend = if self.drawn == 0 {
            Blend::Source
        } else {
            fctl.blend_op
        };
        let src_stride = frame.stride();
        let src = frame.data();
        for row in 0..h {
            let s = &src[row * src_stride..row * src_stride + run];
            let at = (y + row) * self.stride + x * self.bpp;
            let d = &mut self.buffer[at..at + run];
            match blend {
                // "all color components of the frame, including alpha,
                // overwrite the current contents of the frame's output
                // buffer region."
                Blend::Source => d.copy_from_slice(s),
                Blend::Over => blend_over_row(d, s, self.format, &self.trns_key),
            }
        }
        let shown = self.buffer.clone();

        match dispose {
            // "the contents of the output buffer are left as is."
            Disposal::None => {}
            // "the frame's region of the output buffer is to be cleared
            // to fully transparent black."
            Disposal::Background => {
                for row in y..y + h {
                    let at = row * self.stride + x * self.bpp;
                    fill_pixels(&mut self.buffer[at..at + run], &self.clear);
                }
            }
            // "the frame's region of the output buffer is to be reverted
            // to the previous contents."
            Disposal::Previous => {
                if let Some(saved) = saved {
                    for (i, row) in (y..y + h).enumerate() {
                        let at = row * self.stride + x * self.bpp;
                        self.buffer[at..at + run].copy_from_slice(&saved[i * run..(i + 1) * run]);
                    }
                }
            }
        }
        self.drawn += 1;
        Ok(shown)
    }
}

/// Fill `buf` with repeats of the one-pixel pattern `px`.
fn fill_pixels(buf: &mut [u8], px: &[u8]) {
    if px.iter().all(|&b| b == px[0]) {
        buf.fill(px[0]);
    } else {
        for chunk in buf.chunks_exact_mut(px.len()) {
            chunk.copy_from_slice(px);
        }
    }
}

/// Non-premultiplied `src OVER dst` of one sample set at alpha scale
/// `max` (255 or 65535), rounded to nearest:
///
/// ```text
/// αo = αs + αd · (1 − αs)
/// Co = (Cs · αs + Cd · αd · (1 − αs)) / αo        (αo > 0; else 0)
/// ```
///
/// This is the "simple OVER operation" `APNG_BLEND_OP_OVER` refers to
/// (W3C PNG3 §11.3.6.2 → §13.16 `output = alpha * foreground + (1 -
/// alpha) * background`), carried through for an output buffer whose
/// own pixels may be translucent: over an opaque buffer pixel (αd = 1)
/// it is exactly the §13.16 equation, over a fully transparent one
/// (αd = 0) the frame pixel is copied unchanged, and αs = 0 / αs = 1
/// leave the buffer / copy the frame — the §13.16 special cases.
/// Samples are blended as stored (gamma-encoded): frame and buffer
/// share one sample depth and one gamma, the §13.16 variation that
/// needs no gamma round trip.
fn over_samples(src: &[u64], dst: &mut [u64], max: u64) {
    let n = src.len() - 1;
    let a_s = src[n];
    if a_s == max {
        dst.copy_from_slice(src);
        return;
    }
    if a_s == 0 {
        return;
    }
    let a_d = dst[n];
    // Everything at scale max²: αo·max² = αs·max + αd·(max − αs).
    let a_o2 = a_s * max + a_d * (max - a_s);
    for c in 0..n {
        let num = src[c] * a_s * max + dst[c] * a_d * (max - a_s);
        dst[c] = (2 * num + a_o2) / (2 * a_o2);
    }
    dst[n] = (2 * a_o2 + max) / (2 * max);
}

/// `APNG_BLEND_OP_OVER` of one frame row `src` onto the buffer row
/// `dst`, both in the canvas layout `format`.
fn blend_over_row(dst: &mut [u8], src: &[u8], format: PngPixelFormat, key: &TransparencyKey) {
    match format {
        PngPixelFormat::Rgba | PngPixelFormat::Ya8 => {
            let ch = if format == PngPixelFormat::Rgba { 4 } else { 2 };
            let (mut s, mut d) = ([0u64; 4], [0u64; 4]);
            for (dp, sp) in dst.chunks_exact_mut(ch).zip(src.chunks_exact(ch)) {
                for i in 0..ch {
                    s[i] = u64::from(sp[i]);
                    d[i] = u64::from(dp[i]);
                }
                over_samples(&s[..ch], &mut d[..ch], 255);
                for i in 0..ch {
                    dp[i] = d[i] as u8;
                }
            }
        }
        PngPixelFormat::Rgba64Le => {
            let (mut s, mut d) = ([0u64; 4], [0u64; 4]);
            for (dp, sp) in dst
                .as_chunks_mut::<8>()
                .0
                .iter_mut()
                .zip(src.as_chunks::<8>().0)
            {
                for i in 0..4 {
                    s[i] = u64::from(u16::from_le_bytes([sp[2 * i], sp[2 * i + 1]]));
                    d[i] = u64::from(u16::from_le_bytes([dp[2 * i], dp[2 * i + 1]]));
                }
                over_samples(&s, &mut d, 65535);
                for i in 0..4 {
                    dp[2 * i..2 * i + 2].copy_from_slice(&(d[i] as u16).to_le_bytes());
                }
            }
        }
        // Alpha-less canvas (palette / gray / RGB): every frame pixel is
        // either the tRNS key — fully transparent, the buffer stays —
        // or fully opaque and replaces it. No partial blend is
        // representable in a format without alpha.
        _ => {
            let bpp = format.bytes_per_pixel();
            for (dp, sp) in dst.chunks_exact_mut(bpp).zip(src.chunks_exact(bpp)) {
                if !key.is_transparent(sp) {
                    dp.copy_from_slice(sp);
                }
            }
        }
    }
}

/// Composite the parsed APNG `info` into per-frame canvases.
pub fn decode_apng_info(info: &ApngInfo) -> Result<ApngImage> {
    let mut canvas = ApngCanvas::new(&info.ihdr, info.plte.as_deref(), info.trns.as_deref())?;
    let mut out_frames: Vec<ApngFrameImage> = Vec::new();

    for frame in &info.frames {
        frame
            .fctl
            .validate_within_canvas(info.ihdr.width, info.ihdr.height)?;
        // Each frame decodes at its fcTL size with every other IHDR
        // property inherited (§11.3.6.3).
        let sub_ihdr = Ihdr {
            width: frame.fctl.width,
            height: frame.fctl.height,
            ..info.ihdr
        };
        // Same bomb defence as the still-image IDAT path: the fcTL
        // dimensions imply the exact filtered-stream size, so cap the
        // inflate there (+1 to detect overlong) rather than
        // materialising an unbounded hostile stream (W3C PNG3 §13.3).
        let frame_raw =
            inflate_image_pixels(&frame.compressed, &sub_ihdr, "APNG: frame data", "fcTL")?;
        let sub_frame = build_png_image(
            &sub_ihdr,
            frame_raw,
            info.plte.as_deref(),
            info.trns.as_deref(),
        )?;
        let shown = canvas.render(&frame.fctl, &sub_frame)?;
        let img = PngImage::packed(
            info.ihdr.width,
            info.ihdr.height,
            canvas.format(),
            canvas.stride,
            shown,
        )?
        .with_palette(sub_frame.palette.clone())
        .with_transparency(sub_frame.transparency.clone())
        .with_color(info.color)
        .with_metadata(info.metadata.clone());
        let delay = frame.fctl.delay_centiseconds().max(1);
        out_frames.push(ApngFrameImage::new(img, delay).with_delay(frame.fctl.delay_duration()));
    }

    Ok(ApngImage {
        width: info.ihdr.width,
        height: info.ihdr.height,
        pixel_format: canvas.format(),
        frames: out_frames,
        num_plays: info.actl.num_plays,
    })
}
