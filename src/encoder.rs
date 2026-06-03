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
use crate::metadata::PngMetadata;

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
    /// Optional `sBIT` / `pHYs` / `tIME` / `bKGD` / `hIST` / `tRNS` /
    /// `eXIf` / `sRGB` / `cICP` / `iCCP` / `gAMA` / `cHRM` / `mDCV` /
    /// `cLLI` / `sPLT` / `tEXt` / `zTXt` / `iTXt` ancillary metadata
    /// to embed.
    /// Each `Some(_)` field (and each `sPLT` / `tEXt` / `zTXt` /
    /// `itxt` in its `Vec`) is written; chunk ordering follows
    /// RFC 2083 §4.3 / W3C PNG3 §5.6 Table 7:
    ///
    /// * `cICP` / `iCCP` / `sBIT` / `sRGB` / `gAMA` / `cHRM` / `mDCV` /
    ///   `cLLI` — before `PLTE` and `IDAT`. The colour chunks follow
    ///   §4.3 Table 1's "Color Chunk Priority" order (`cICP` `1` >
    ///   `iCCP` `2` > `sRGB` `3` > `cHRM`/`gAMA` `4`); `mDCV` and
    ///   `cLLI` are HDR-side supplemental colour-volume metadata that
    ///   the §4.3 table does not enumerate, emitted after the ranked
    ///   chunks so the basic colour-space signal leads the file
    ///   (§11.3.2.7 / §11.3.2.8).
    /// * `bKGD` / `hIST` — after `PLTE`, before `IDAT`. `tRNS` (the
    ///   ct=0/ct=2 keyed-sample form, or the ct=3 alpha table when the
    ///   caller routes it through `metadata.trns` instead of the legacy
    ///   `image.palette` tail) also rides in this bucket per RFC 2083
    ///   §4.2.9 "must precede the first IDAT chunk, and must follow the
    ///   PLTE chunk, if any."
    /// * `pHYs` — before `IDAT`.
    /// * `tIME` — unconstrained; we emit it before `IDAT` for
    ///   determinism.
    /// * `eXIf` — before `IDAT` (§5.6 Table 7).
    /// * `sPLT` — before `IDAT` (§5.6 Table 7). Multiple instances are
    ///   permitted; emitted in `Vec` order. An invalid palette name /
    ///   sample depth (or an 8-bit sample > 255) is an encode error.
    /// * `tEXt` — before `IDAT`; emitted after `sPLT` in `Vec` order.
    ///   Multiple instances with identical keywords are permitted
    ///   (§4.2.7 ¶3).
    /// * `zTXt` — before `IDAT`; emitted after `tEXt` so plain text
    ///   precedes compressed text in the chunk stream. Multiple
    ///   instances with identical keywords are permitted (§4.2.10 ¶6).
    ///   Body is zlib-compressed at the encoder default (level 6).
    /// * `iTXt` — before `IDAT`; emitted after `zTXt` so the
    ///   non-international text chunks lead the textual stream.
    ///   Multiple instances with identical keywords are permitted
    ///   (§11.3.3.4). Each entry's text is zlib-compressed when its
    ///   `compressed` flag is set, otherwise written verbatim.
    ///
    /// `None` (or an empty `splt` / `texts` / `ztxts` / `itxts` `Vec`)
    /// skips the chunk entirely.
    pub metadata: Option<PngMetadata>,
    /// Sub-byte bit depth for the on-wire IHDR. Only valid for
    /// `Gray8` (colour type 0) and `Pal8` (colour type 3) sources;
    /// any other source pixel format is an encode error.
    ///
    /// Permitted values are `1`, `2`, and `4`. The caller's
    /// `PngImage::data` continues to hold one byte per pixel: for
    /// grayscale, the byte is treated as the *unscaled* sample in
    /// `0..=(1 << bit_depth) - 1`; for indexed, the byte is the
    /// palette index in the same range. Anything outside the range
    /// is rejected so a malformed payload cannot reach the wire.
    ///
    /// Packing order matches the decode side and the PNG spec
    /// (RFC 2083 §2.3 / W3C PNG3 §11.1.2): "the leftmost pixel in
    /// the high-order bits of a byte, the rightmost in the low-order
    /// bits." Sub-byte rows whose pixel count does not divide
    /// `8 / bit_depth` are padded with zero bits in the trailing
    /// byte's low-order positions ("the contents of these wasted
    /// bits are unspecified" per §2.3).
    ///
    /// `None` (the default) emits the 8-bit-per-sample IHDR the
    /// matching `PngPixelFormat` implies. `Some(8)` is also accepted
    /// as a no-op equivalent to `None` for `Gray8` / `Pal8`. `16`
    /// is rejected — the 16-bit forms are reached via
    /// `Gray16Le` / `Rgb48Le` / `Rgba64Le` source formats instead.
    pub bit_depth: Option<u8>,
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
    let (mut ihdr, row_bytes, plte_bytes, trns_bytes) = ihdr_and_row_bytes(image, opts)?;
    if opts.interlace {
        if ihdr.bit_depth < 8 {
            return Err(Error::invalid(
                "PNG encoder: Adam7 interlaced encode with sub-byte \
                 bit_depth is not implemented (each pass would need its \
                 own sub-byte pack); split the request across two rounds",
            ));
        }
        ihdr.interlace = 1;
    }
    // Resolve the on-wire tRNS payload. Two sources can supply it: the
    // palette tail (`Pal8` only — `ihdr_and_row_bytes` splits
    // `image.palette` into `PLTE || tRNS`), and `metadata.trns` (the
    // ct=0 / ct=2 path; opt-in for ct=3 too). The two are mutually
    // exclusive — emitting both would put two `tRNS` chunks on the
    // wire, violating §5.6 Table 1 "Multiple OK? No" — so the resolver
    // errors if both are populated and otherwise picks whichever is
    // present.
    let trns_bytes = resolve_trns_bytes(&ihdr, trns_bytes.as_deref(), opts.metadata.as_ref())?;
    let raw_pixels = flatten_and_normalise_pixels(image, &ihdr, row_bytes)?;
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
    // sBIT must precede PLTE + IDAT (RFC 2083 §4.3 / §4.2.6).
    write_metadata_before_plte(&mut out, opts.metadata.as_ref())?;
    if let Some(p) = plte_bytes.as_deref() {
        write_chunk(&mut out, b"PLTE", p);
    }
    if let Some(t) = trns_bytes.as_deref() {
        write_chunk(&mut out, b"tRNS", t);
    }
    // pHYs + tIME go between PLTE/tRNS and IDAT (pHYs MUST be before
    // IDAT per RFC 2083 §4.2.5; tIME has no ordering constraint but we
    // bucket it here for determinism). sPLT also rides here.
    write_metadata_before_idat(&mut out, opts.metadata.as_ref())?;
    write_chunk(&mut out, b"IDAT", &idat);
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

/// Emit the metadata chunks the PNG spec places "before PLTE and IDAT":
/// `cICP` (W3C PNG3 §11.3.2.6), `iCCP` (§11.3.2.3), `sBIT` (RFC 2083
/// §4.3), `sRGB` (W3C PNG3 §5.6 Table 1), the `gAMA` / `cHRM`
/// colour-management pair (RFC 2083 §4.2.2 / §4.2.3), and the HDR
/// supplemental colour-volume pair `mDCV` (§11.3.2.7) / `cLLI`
/// (§11.3.2.8). The colour chunks are emitted in §4.3 "Color Chunk
/// Priority" order (cICP `1` > iCCP `2` > sRGB `3` > cHRM/gAMA `4`),
/// with `sBIT` slotted after the highest-precedence pair for
/// deterministic output. `gAMA` precedes `cHRM` so the simpler scalar
/// gamma leads the chromaticity block. `mDCV` and `cLLI` trail the
/// §4.3-ranked chunks because they describe the *mastering display*
/// rather than the colour space itself — a viewer that walks the file
/// in order picks up the basic colour signal before the supplemental
/// HDR tone-mapping hints.
fn write_metadata_before_plte(out: &mut Vec<u8>, meta: Option<&PngMetadata>) -> Result<()> {
    let Some(meta) = meta else {
        return Ok(());
    };
    if let Some(cicp) = &meta.cicp {
        write_chunk(out, b"cICP", &cicp.to_bytes());
    }
    if let Some(iccp) = &meta.iccp {
        write_chunk(out, b"iCCP", &iccp.to_bytes()?);
    }
    if let Some(sbit) = &meta.sbit {
        write_chunk(out, b"sBIT", &sbit.to_bytes());
    }
    if let Some(srgb) = &meta.srgb {
        write_chunk(out, b"sRGB", &srgb.to_bytes());
    }
    if let Some(gama) = &meta.gama {
        write_chunk(out, b"gAMA", &gama.to_bytes());
    }
    if let Some(chrm) = &meta.chrm {
        write_chunk(out, b"cHRM", &chrm.to_bytes());
    }
    // mDCV + cLLI (W3C PNG3 §11.3.2.7 / §11.3.2.8) are HDR-side
    // supplemental colour-volume metadata. The §4.3 colour-priority
    // table does not enumerate them (they pair with cICP for
    // tone-mapping reads rather than picking a colour-space). §5.6
    // Table 1 only constrains them to "Before PLTE and IDAT"; we emit
    // them after the §4.3-ranked chunks so the basic colour-space
    // signal leads the file and HDR-display-volume metadata trails.
    if let Some(mdcv) = &meta.mdcv {
        write_chunk(out, b"mDCV", &mdcv.to_bytes());
    }
    if let Some(clli) = &meta.clli {
        write_chunk(out, b"cLLI", &clli.to_bytes());
    }
    Ok(())
}

/// Emit `bKGD`, `hIST`, `pHYs`, `tIME`, `eXIf`, `sPLT` — all go between
/// any `PLTE`/`tRNS` pair and the `IDAT` stream. `bKGD` + `hIST` are
/// emitted first per W3C PNG3 §5.6 ("After PLTE; before IDAT"); `eXIf`
/// and `sPLT` are "Before IDAT" (§5.6 Table 7) with no PLTE
/// relationship, so they trail this bucket. `sPLT` is the one chunk
/// permitting multiple instances; each entry of the `splt` `Vec` is
/// emitted in order. Returns an error if any `sPLT` payload is invalid
/// (bad palette name / sample depth, or an 8-bit sample > 255).
fn write_metadata_before_idat(out: &mut Vec<u8>, meta: Option<&PngMetadata>) -> Result<()> {
    let Some(meta) = meta else {
        return Ok(());
    };
    if let Some(bkgd) = &meta.bkgd {
        write_chunk(out, b"bKGD", &bkgd.to_bytes());
    }
    if let Some(hist) = &meta.hist {
        write_chunk(out, b"hIST", &hist.to_bytes());
    }
    if let Some(phys) = &meta.phys {
        write_chunk(out, b"pHYs", &phys.to_bytes());
    }
    if let Some(time) = &meta.time {
        write_chunk(out, b"tIME", &time.to_bytes());
    }
    if let Some(exif) = &meta.exif {
        write_chunk(out, b"eXIf", &exif.to_bytes());
    }
    for splt in &meta.splt {
        write_chunk(out, b"sPLT", &splt.to_bytes()?);
    }
    // `tEXt` (RFC 2083 §4.2.7) sits in the same "Before IDAT, no
    // ordering constraint" bucket as `sPLT` per Table 1's "Multiple OK?
    // Yes" / "Ordering constraints: None" row. Emitted after `sPLT` so
    // the more structured chunks (palette, transparency, timing,
    // suggested-palette) precede free-form textual annotations.
    for text in &meta.texts {
        write_chunk(out, b"tEXt", &text.to_bytes()?);
    }
    // `zTXt` (RFC 2083 §4.2.10) shares the "Before IDAT, no ordering
    // constraint" bucket with `tEXt`; "Any number of zTXt and tEXt
    // chunks can appear in the same file" (§4.2.10 ¶6). Emitted after
    // `tEXt` so the cheap-to-display plain-text chunks precede the
    // compressed bulk-text payloads — a reader streaming the file gets
    // the human-readable annotations first.
    for ztxt in &meta.ztxts {
        write_chunk(out, b"zTXt", &ztxt.to_bytes()?);
    }
    // `iTXt` (W3C PNG3 §11.3.3.4) shares the same "Before IDAT, no
    // ordering constraint" bucket as `tEXt` and `zTXt`. Emitted after
    // `zTXt` so the Latin-1 chunks (cheap to decode for callers that
    // only need byte-exact metadata) lead the internationalised UTF-8
    // chunks in the stream.
    for itxt in &meta.itxts {
        write_chunk(out, b"iTXt", &itxt.to_bytes()?);
    }
    Ok(())
}

/// Pick the on-wire `tRNS` payload bytes given the IHDR + the two
/// possible sources: the palette tail (`Pal8` only — derived from
/// `image.palette`'s `PLTE || tRNS` blob in [`ihdr_and_row_bytes`]) and
/// the optional `metadata.trns` field. The PNG spec allows at most one
/// `tRNS` chunk per file (W3C PNG3 §5.6 Table 1 "Multiple OK? No"),
/// so:
///
/// * Both sources `None` → no chunk on the wire.
/// * Exactly one source `Some` → that source's bytes.
/// * Both sources `Some` → encode error (the caller asked for two
///   `tRNS` chunks on the same file).
///
/// When `metadata.trns` is used, the variant must match the IHDR colour
/// type — a `Trns::Rgb` paired with a grayscale IHDR would put a
/// 6-byte payload on the wire that a strict decoder would reject.
fn resolve_trns_bytes(
    ihdr: &Ihdr,
    palette_trns: Option<&[u8]>,
    meta: Option<&PngMetadata>,
) -> Result<Option<Vec<u8>>> {
    let meta_trns = meta.and_then(|m| m.trns.as_ref());
    match (palette_trns, meta_trns) {
        (None, None) => Ok(None),
        (Some(p), None) => Ok(Some(p.to_vec())),
        (None, Some(t)) => {
            if !t.matches_colour_type(ihdr.colour_type) {
                return Err(Error::invalid(format!(
                    "PNG encoder: metadata.trns variant does not match \
                     IHDR colour type {} (RFC 2083 §4.2.9)",
                    ihdr.colour_type
                )));
            }
            // For ct=0 / ct=2 the keyed sample must fit in `bit_depth`
            // bits — same range check the parse path enforces (Trns::parse).
            match t {
                crate::metadata::Trns::Grayscale(v) => {
                    let max = (1u32 << ihdr.bit_depth) - 1;
                    if (*v as u32) > max {
                        return Err(Error::invalid(format!(
                            "PNG encoder: tRNS gray value {v} exceeds 2^{} - 1 ({max})",
                            ihdr.bit_depth
                        )));
                    }
                }
                crate::metadata::Trns::Rgb(r, g, b) => {
                    let max = (1u32 << ihdr.bit_depth) - 1;
                    for (name, v) in [("R", *r), ("G", *g), ("B", *b)] {
                        if (v as u32) > max {
                            return Err(Error::invalid(format!(
                                "PNG encoder: tRNS {name} value {v} \
                                 exceeds 2^{} - 1 ({max})",
                                ihdr.bit_depth
                            )));
                        }
                    }
                }
                crate::metadata::Trns::Palette(_) => {
                    // No bit-depth bound on indexed alpha tables — the
                    // chunk's length-vs-PLTE-entry-count constraint is
                    // enforced when the encoder splits image.palette,
                    // not here (since we landed in the "no palette tail"
                    // branch).
                }
            }
            Ok(Some(t.to_bytes()))
        }
        (Some(_), Some(_)) => Err(Error::invalid(
            "PNG encoder: both image.palette tail and metadata.trns supply a tRNS \
             chunk — only one source is allowed per file (W3C PNG3 §5.6 Table 1 \
             \"Multiple OK? No\")",
        )),
    }
}

/// IHDR + row byte count + optional PLTE / tRNS chunk payloads.
type IhdrAndRowInfo = (Ihdr, usize, Option<Vec<u8>>, Option<Vec<u8>>);

/// Validate and resolve the wire `bit_depth` from the source pixel
/// format's natural depth plus the optional `opts.bit_depth` override.
///
/// `Some(1 | 2 | 4)` requires `colour_type ∈ {0, 3}` (the only colour
/// types PNG allows sub-byte depths for — RFC 2083 §11.2.2 / Table 11.1
/// rejects RGB / Ya / RGBA sub-byte combinations). `Some(8)` is a
/// no-op for `Gray8` / `Pal8`. Anything else is an encode error.
fn resolve_bit_depth(base: u8, colour_type: u8, opts: &PngEncoderOptions) -> Result<u8> {
    let Some(requested) = opts.bit_depth else {
        return Ok(base);
    };
    match requested {
        // No-op overrides for the 8-bit Gray / indexed source formats.
        8 if base == 8 => Ok(8),
        1 | 2 | 4 if base == 8 && (colour_type == 0 || colour_type == 3) => Ok(requested),
        1 | 2 | 4 => Err(Error::invalid(format!(
            "PNG encoder: bit_depth {requested} requires a Gray8 or Pal8 \
             source (got colour type {colour_type} at native {base}-bit) — \
             the PNG allowed-combinations table (RFC 2083 §11.2.2 \
             Color/Bit-Depths) forbids sub-byte depths on colour types \
             2 / 4 / 6"
        ))),
        16 => Err(Error::invalid(
            "PNG encoder: bit_depth = Some(16) is unsupported — reach the \
             16-bit forms via Gray16Le / Rgb48Le / Rgba64Le source formats",
        )),
        other => Err(Error::invalid(format!(
            "PNG encoder: bit_depth = Some({other}) is not a permitted PNG \
             sample depth (allowed: 1, 2, 4 for Gray / indexed; 8 as no-op)"
        ))),
    }
}

/// Given a [`PngImage`] (and the encoder options), produce an IHDR +
/// row byte count + optional PLTE / tRNS chunk payloads.
///
/// `opts.bit_depth = Some(1 | 2 | 4)` is honoured only when the source
/// `pixel_format` is `Gray8` (colour type 0) or `Pal8` (colour type 3);
/// any other combination is an encode error. The resulting `row_bytes`
/// is the *packed* on-wire row length (`(width * bit_depth + 7) / 8`)
/// rather than the source `image.data` row stride. The packing itself
/// happens in [`flatten_and_normalise_pixels`].
fn ihdr_and_row_bytes(image: &PngImage, opts: &PngEncoderOptions) -> Result<IhdrAndRowInfo> {
    let (base_bit_depth, colour_type, channels): (u8, u8, usize) = match image.pixel_format {
        PngPixelFormat::Gray8 => (8, 0, 1),
        PngPixelFormat::Gray16Le => (16, 0, 1),
        PngPixelFormat::Rgb24 => (8, 2, 3),
        PngPixelFormat::Rgb48Le => (16, 2, 3),
        PngPixelFormat::Pal8 => (8, 3, 1),
        PngPixelFormat::Ya8 => (8, 4, 2),
        PngPixelFormat::Rgba => (8, 6, 4),
        PngPixelFormat::Rgba64Le => (16, 6, 4),
    };
    let bit_depth = resolve_bit_depth(base_bit_depth, colour_type, opts)?;
    // For sub-byte (bit_depth < 8) Gray / indexed, the row's wire bytes
    // are `ceil(width * bit_depth / 8)` — pixels are packed MSB-first
    // per PNG §2.3. For 8-bit and 16-bit, the channels-and-byte-width
    // formula reduces to the historical `channels * (bit_depth / 8) *
    // width`.
    let row_bytes = if bit_depth < 8 {
        ((image.width as usize) * (bit_depth as usize)).div_ceil(8)
    } else {
        channels * (bit_depth as usize / 8) * image.width as usize
    };
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
/// expected byte count per row of the *wire* layout, which equals
/// `ceil(width * bit_depth / 8)` for the sub-byte Gray / indexed cases
/// and the source layout otherwise.
fn flatten_and_normalise_pixels(
    image: &PngImage,
    ihdr: &Ihdr,
    row_bytes: usize,
) -> Result<Vec<u8>> {
    let h = image.height as usize;
    let w = image.width as usize;
    let stride = image.stride;

    // Sub-byte: source is Gray8 / Pal8 (one byte per pixel in
    // image.data); pack into MSB-first sub-byte cells per PNG §2.3.
    if ihdr.bit_depth < 8 {
        return pack_subbyte_rows(image, ihdr.bit_depth, row_bytes);
    }

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

/// Pack a `Gray8` / `Pal8` source buffer into sub-byte rows for
/// PNG bit depths `1`, `2`, or `4`.
///
/// Each source byte (in `image.data`) is treated as the unscaled
/// sample value or palette index in `0..=(1 << bit_depth) - 1`. Pixels
/// are packed left-to-right with the leftmost pixel landing in the
/// high-order bits of each output byte (PNG §2.3 / W3C PNG3 §11.1.2:
/// "leftmost pixel in the high-order bits of a byte, the rightmost in
/// the low-order bits"). The last byte of each row is padded with
/// zero bits in its low-order positions when `width * bit_depth` is
/// not a multiple of 8 (the spec marks these bits unspecified; we
/// emit zeros for determinism).
///
/// A sample whose top bits exceed the `bit_depth` cap is rejected so
/// a malformed payload cannot reach the wire.
fn pack_subbyte_rows(image: &PngImage, bit_depth: u8, row_bytes: usize) -> Result<Vec<u8>> {
    debug_assert!(bit_depth == 1 || bit_depth == 2 || bit_depth == 4);
    let h = image.height as usize;
    let w = image.width as usize;
    let stride = image.stride;
    let bd = bit_depth as usize;
    let max: u8 = ((1u16 << bd) - 1) as u8;
    let pixels_per_byte = 8 / bd;

    let mut out = vec![0u8; row_bytes * h];
    for y in 0..h {
        let src_row = &image.data[y * stride..y * stride + w];
        let dst_row = &mut out[y * row_bytes..(y + 1) * row_bytes];
        for (x, &v) in src_row.iter().enumerate() {
            if v > max {
                return Err(Error::invalid(format!(
                    "PNG encoder: sub-byte sample {v} at pixel ({x},{y}) exceeds \
                     2^{bit_depth} - 1 ({max}) — callers supplying sub-byte input \
                     must pre-quantize source samples to the target bit depth"
                )));
            }
            let byte_idx = x / pixels_per_byte;
            let shift_in_byte = (pixels_per_byte - 1 - (x % pixels_per_byte)) * bd;
            dst_row[byte_idx] |= v << shift_in_byte;
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
    let (mut ihdr, row_bytes, plte, trns) = ihdr_and_row_bytes(&frames[0], opts)?;
    if opts.interlace {
        if ihdr.bit_depth < 8 {
            return Err(Error::invalid(
                "PNG encoder: APNG interlaced encode with sub-byte \
                 bit_depth is not implemented",
            ));
        }
        ihdr.interlace = 1;
    }
    // APNG shares the standalone path's tRNS resolution. The IHDR is
    // fixed across the whole APNG so a single resolve on the first-
    // frame palette + opts.metadata covers every frame.
    let trns = resolve_trns_bytes(&ihdr, trns.as_deref(), opts.metadata.as_ref())?;

    let actl = Actl {
        num_frames: frames.len() as u32,
        num_plays,
    };

    let mut out = Vec::new();
    out.extend_from_slice(&PNG_MAGIC);
    write_chunk(&mut out, b"IHDR", &ihdr.to_bytes());
    write_chunk(&mut out, b"acTL", &actl.to_bytes());
    // sBIT precedes PLTE + IDAT per RFC 2083 §4.3.
    write_metadata_before_plte(&mut out, opts.metadata.as_ref())?;
    if let Some(p) = plte.as_deref() {
        write_chunk(&mut out, b"PLTE", p);
    }
    if let Some(t) = trns.as_deref() {
        write_chunk(&mut out, b"tRNS", t);
    }
    // pHYs / tIME / sPLT precede IDAT (and APNG's fcTL/fdAT stream that
    // bracket subsequent frames).
    write_metadata_before_idat(&mut out, opts.metadata.as_ref())?;

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

        let raw = flatten_and_normalise_pixels(frame, &ihdr, row_bytes)?;
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
