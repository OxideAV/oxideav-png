//! PNG ancillary chunks as format-native metadata blobs.
//!
//! The framework's well-known blob kinds cover three PNG chunks (`iCCP`
//! → `icc`, `eXIf` → `exif`, the XMP `iTXt` → `xmp`) and its colour
//! signal covers `cICP` / `sRGB`. Everything else a PNG file says about
//! its image travels as a blob whose kind is spelled by the framework's
//! `<producer>-<chunk>` rule — `png-` plus the lower-cased chunk type —
//! and whose payload is the chunk body exactly as stored:
//!
//! | kind | chunk | instances |
//! |---|---|---|
//! | `png-gama` | `gAMA` | one |
//! | `png-chrm` | `cHRM` | one |
//! | `png-srgb` | `sRGB` (the rendering intent) | one |
//! | `png-mdcv` | `mDCV` | one |
//! | `png-clli` | `cLLI` | one |
//! | `png-sbit` | `sBIT` | one |
//! | `png-phys` | `pHYs` | one |
//! | `png-time` | `tIME` | one |
//! | `png-bkgd` | `bKGD` | one |
//! | `png-hist` | `hIST` | one |
//! | `png-trns` | `tRNS` of a greyscale / truecolour image (the keyed sample) | one |
//! | `png-splt` | `sPLT` | any, distinct palette names |
//! | `png-text` | `tEXt` | any |
//! | `png-ztxt` | `zTXt` (body still compressed) | any |
//! | `png-itxt` | `iTXt` other than the XMP packet | any |
//! | `png-chunk` | an unrecognised ancillary chunk before `IDAT`: its four type bytes, then its body | any |
//! | `png-chunk-after-idat` | the same, for a chunk after `IDAT` | any |
//!
//! The decoder attaches them to every frame and the demuxer to the
//! stream; the encoder reads them back (a frame's records of a kind
//! before the stream's) and writes each chunk that is still valid for
//! the picture it is encoding: a record whose payload does not fit the
//! output layout (an `sBIT` of the wrong channel count after a pixel
//! format conversion, a `bKGD` palette index past the palette, a `gAMA`
//! that contradicts the frame's sRGB colour signal) is dropped, never an
//! error. An unrecognised chunk is written only when its type's
//! safe-to-copy bit is set (W3C PNG3 §14.2: an unsafe-to-copy chunk
//! depends on the image data, which the encoder cannot know to be
//! unchanged), on the side of `IDAT` it came from.

use oxideav_core::{BlobKind, MetadataBlob};

use crate::chunk::{ChunkRef, ChunkType};
use crate::decoder::Ihdr;
use crate::error::Result;
use crate::metadata::{
    Bkgd, Chrm, Clli, Gama, Hist, Itxt, Mdcv, Phys, PngMetadata, Sbit, Splt, Srgb, Text, Time,
    Trns, UnknownChunk, Ztxt,
};
use crate::sideinfo::XMP_KEYWORD;

/// The producer prefix of every kind this crate defines.
const PRODUCER: &str = "png";

/// Kind of an unrecognised ancillary chunk found before `IDAT`.
const UNKNOWN_BEFORE: &str = "png-chunk";
/// Kind of an unrecognised ancillary chunk found after `IDAT`.
const UNKNOWN_AFTER: &str = "png-chunk-after-idat";

/// The chunk types carried body-as-stored under `png-<type>`.
const NATIVE_TYPES: [&[u8; 4]; 15] = [
    b"gAMA", b"cHRM", b"sRGB", b"mDCV", b"cLLI", b"sBIT", b"pHYs", b"tIME", b"bKGD", b"hIST",
    b"tRNS", b"sPLT", b"tEXt", b"zTXt", b"iTXt",
];

/// The blob kind of chunk type `ty`: `png-` plus the lower-cased type.
fn kind_of(ty: &[u8; 4]) -> BlobKind {
    BlobKind::format_native(PRODUCER, std::str::from_utf8(ty).unwrap_or("chunk"))
}

/// Every format-native kind the encoder embeds, in the module table's
/// order (for `CodecCapabilities::embedded_blob_kinds`).
pub(crate) fn native_kinds() -> Vec<BlobKind> {
    let mut kinds: Vec<BlobKind> = NATIVE_TYPES.iter().map(|t| kind_of(t)).collect();
    kinds.push(BlobKind::from_static(UNKNOWN_BEFORE));
    kinds.push(BlobKind::from_static(UNKNOWN_AFTER));
    kinds
}

/// `true` when `data` is an `iTXt` body carrying the XMP packet (which
/// travels as the well-known `xmp` blob instead).
fn is_xmp_itxt(data: &[u8]) -> bool {
    data.get(..XMP_KEYWORD.len()) == Some(XMP_KEYWORD.as_bytes())
        && data.get(XMP_KEYWORD.len()) == Some(&0)
}

/// A `zTXt` body up to its compressed text: a valid keyword, the `NUL`
/// and compression method 0. The text is not inflated.
fn ztxt_header_ok(data: &[u8]) -> bool {
    let Some(nul) = data.iter().position(|&b| b == 0) else {
        return false;
    };
    let keyword: String = data[..nul].iter().map(|&b| b as char).collect();
    crate::metadata::validate_keyword(&keyword, "zTXt").is_ok() && data.get(nul + 1) == Some(&0)
}

/// The sample depth `sBIT` is bounded by: 8 for indexed colour, the bit
/// depth otherwise (W3C PNG3 §11.3.2.4).
fn sample_depth(ihdr: &Ihdr) -> u8 {
    if ihdr.colour_type == 3 {
        8
    } else {
        ihdr.bit_depth
    }
}

/// The format-native blobs of a chunk list, in file order: one per
/// chunk of the module table that parses under `ihdr` (a malformed
/// chunk, or a second instance of a single-instance type, is left out;
/// compressed text is checked up to its compressed body, not inflated).
pub(crate) fn blobs_from_chunks(chunks: &[ChunkRef<'_>], ihdr: &Ihdr) -> Vec<MetadataBlob> {
    let plte_entries = chunks
        .iter()
        .find(|c| c.is_type(b"PLTE"))
        .filter(|c| c.data.len() % 3 == 0)
        .map(|c| c.data.len() / 3);
    let mut out: Vec<MetadataBlob> = Vec::new();
    let mut seen: Vec<[u8; 4]> = Vec::new();
    let mut splt_names: Vec<String> = Vec::new();
    let mut after_idat = false;
    for c in chunks {
        let d = c.data;
        let (valid, single) = match &c.chunk_type {
            b"gAMA" => (Gama::parse(d).is_ok(), true),
            b"cHRM" => (Chrm::parse(d).is_ok(), true),
            b"sRGB" => (Srgb::parse(d).is_ok(), true),
            b"mDCV" => (Mdcv::parse(d).is_ok(), true),
            b"cLLI" => (Clli::parse(d).is_ok(), true),
            b"sBIT" => (
                Sbit::parse(d, ihdr.colour_type, sample_depth(ihdr)).is_ok(),
                true,
            ),
            b"pHYs" => (Phys::parse(d).is_ok(), true),
            b"tIME" => (Time::parse(d).is_ok(), true),
            b"bKGD" => (
                match Bkgd::parse(d, ihdr.colour_type, ihdr.bit_depth) {
                    Ok(Bkgd::Palette(i)) => plte_entries.is_some_and(|n| usize::from(i) < n),
                    Ok(_) => true,
                    Err(_) => false,
                },
                true,
            ),
            b"hIST" => (
                plte_entries.is_some_and(|n| Hist::parse(d, n).is_ok()),
                true,
            ),
            // The palette's alpha table travels as the frame's
            // palette-alpha record.
            b"tRNS" => (
                matches!(ihdr.colour_type, 0 | 2)
                    && Trns::parse(d, ihdr.colour_type, ihdr.bit_depth, None).is_ok(),
                true,
            ),
            b"sPLT" => match Splt::parse(d) {
                Ok(s) if !splt_names.contains(&s.name) => {
                    splt_names.push(s.name);
                    (true, false)
                }
                _ => (false, false),
            },
            b"tEXt" => (Text::parse(d).is_ok(), false),
            b"zTXt" => (ztxt_header_ok(d), false),
            b"iTXt" => (!is_xmp_itxt(d) && Itxt::parse_header(d).is_ok(), false),
            b"IDAT" => {
                after_idat = true;
                continue;
            }
            b"IHDR" | b"PLTE" | b"IEND" | b"acTL" | b"fcTL" | b"fdAT" | b"cICP" | b"iCCP"
            | b"eXIf" => continue,
            other => {
                let ty = ChunkType::new(*other);
                if ty.is_ancillary() && ty.is_well_formed_name() {
                    let kind = if after_idat {
                        UNKNOWN_AFTER
                    } else {
                        UNKNOWN_BEFORE
                    };
                    let mut data = Vec::with_capacity(4 + d.len());
                    data.extend_from_slice(other);
                    data.extend_from_slice(d);
                    out.push(MetadataBlob::new(BlobKind::from_static(kind), data));
                }
                continue;
            }
        };
        if !valid || (single && seen.contains(&c.chunk_type)) {
            continue;
        }
        if single {
            seen.push(c.chunk_type);
        }
        out.push(MetadataBlob::new(kind_of(&c.chunk_type), d.to_vec()));
    }
    out
}

/// The format-native blobs of a [`PngMetadata`]: each record the module
/// table lists, serialised as its chunk body, in the table's order.
/// `cICP`, `iCCP` and `eXIf` are not listed (they are the colour signal
/// and the well-known `icc` / `exif` kinds), nor is an XMP `iTXt`, nor a
/// palette `tRNS` (the palette-alpha record). A record that cannot be
/// serialised (an invalid keyword, a non-Latin-1 `tEXt`) is the error.
pub(crate) fn blobs_from_metadata(meta: &PngMetadata) -> Result<Vec<MetadataBlob>> {
    let mut out: Vec<MetadataBlob> = Vec::new();
    let mut put = |ty: &[u8; 4], data: Vec<u8>| out.push(MetadataBlob::new(kind_of(ty), data));
    if let Some(v) = &meta.gama {
        put(b"gAMA", v.to_bytes().to_vec());
    }
    if let Some(v) = &meta.chrm {
        put(b"cHRM", v.to_bytes().to_vec());
    }
    if let Some(v) = &meta.srgb {
        put(b"sRGB", v.to_bytes().to_vec());
    }
    if let Some(v) = &meta.mdcv {
        put(b"mDCV", v.to_bytes().to_vec());
    }
    if let Some(v) = &meta.clli {
        put(b"cLLI", v.to_bytes().to_vec());
    }
    if let Some(v) = &meta.sbit {
        put(b"sBIT", v.to_bytes());
    }
    if let Some(v) = &meta.phys {
        put(b"pHYs", v.to_bytes().to_vec());
    }
    if let Some(v) = &meta.time {
        put(b"tIME", v.to_bytes().to_vec());
    }
    if let Some(v) = &meta.bkgd {
        put(b"bKGD", v.to_bytes());
    }
    if let Some(v) = &meta.hist {
        put(b"hIST", v.to_bytes());
    }
    if let Some(v @ (Trns::Grayscale(_) | Trns::Rgb(..))) = &meta.trns {
        put(b"tRNS", v.to_bytes());
    }
    for v in &meta.splt {
        put(b"sPLT", v.to_bytes()?);
    }
    for v in &meta.texts {
        put(b"tEXt", v.to_bytes()?);
    }
    for v in &meta.ztxts {
        put(b"zTXt", v.to_bytes()?);
    }
    for v in meta.itxts.iter().filter(|t| t.keyword != XMP_KEYWORD) {
        put(b"iTXt", v.to_bytes()?);
    }
    for u in &meta.unknowns {
        let kind = if u.after_idat {
            UNKNOWN_AFTER
        } else {
            UNKNOWN_BEFORE
        };
        let mut data = Vec::with_capacity(4 + u.data.len());
        data.extend_from_slice(&u.chunk_type);
        data.extend_from_slice(&u.data);
        out.push(MetadataBlob::new(BlobKind::from_static(kind), data));
    }
    Ok(out)
}

/// The chunks a set of blob records gives an encode whose header is
/// `ihdr` (the output colour type and bit depth) with `plte_entries`
/// palette entries.
///
/// A frame's records of a kind take the place of the stream's records
/// of that kind. Each payload is parsed against the output header and
/// dropped when it does not fit it; an unrecognised chunk is kept only
/// when it is ancillary, named by four letters, not a type this crate
/// writes itself, and safe to copy. The colour records (`gAMA`, `cHRM`,
/// `sRGB`) are returned as stored: the caller reconciles them with the
/// frame's colour signal.
pub(crate) fn metadata_from_blobs(
    frame_blobs: &[MetadataBlob],
    stream_blobs: &[MetadataBlob],
    ihdr: &Ihdr,
    plte_entries: Option<usize>,
) -> PngMetadata {
    // The records of `kind`: the frame's when it has any, else the
    // stream's.
    fn of<'a>(
        kind: &BlobKind,
        frame_blobs: &'a [MetadataBlob],
        stream_blobs: &'a [MetadataBlob],
    ) -> Vec<&'a [u8]> {
        let pick = |set: &'a [MetadataBlob]| -> Vec<&'a [u8]> {
            set.iter()
                .filter(|b| b.is(kind))
                .map(|b| b.data.as_slice())
                .collect()
        };
        let from_frame = pick(frame_blobs);
        if from_frame.is_empty() {
            pick(stream_blobs)
        } else {
            from_frame
        }
    }
    let all = |ty: &[u8; 4]| of(&kind_of(ty), frame_blobs, stream_blobs);
    let first = |ty: &[u8; 4]| all(ty).into_iter().next();

    let trns = if matches!(ihdr.colour_type, 0 | 2) {
        first(b"tRNS").and_then(|d| Trns::parse(d, ihdr.colour_type, ihdr.bit_depth, None).ok())
    } else {
        None
    };
    let mut meta = PngMetadata {
        gama: first(b"gAMA").and_then(|d| Gama::parse(d).ok()),
        chrm: first(b"cHRM").and_then(|d| Chrm::parse(d).ok()),
        srgb: first(b"sRGB").and_then(|d| Srgb::parse(d).ok()),
        mdcv: first(b"mDCV").and_then(|d| Mdcv::parse(d).ok()),
        clli: first(b"cLLI").and_then(|d| Clli::parse(d).ok()),
        sbit: first(b"sBIT")
            .and_then(|d| Sbit::parse(d, ihdr.colour_type, sample_depth(ihdr)).ok()),
        phys: first(b"pHYs").and_then(|d| Phys::parse(d).ok()),
        time: first(b"tIME").and_then(|d| Time::parse(d).ok()),
        bkgd: first(b"bKGD")
            .and_then(|d| Bkgd::parse(d, ihdr.colour_type, ihdr.bit_depth).ok())
            .filter(|b| match b {
                Bkgd::Palette(i) => plte_entries.is_some_and(|n| usize::from(*i) < n),
                _ => true,
            }),
        hist: first(b"hIST").and_then(|d| Hist::parse(d, plte_entries?).ok()),
        trns,
        texts: all(b"tEXt")
            .into_iter()
            .filter_map(|d| Text::parse(d).ok())
            .collect(),
        ztxts: all(b"zTXt")
            .into_iter()
            .filter_map(|d| Ztxt::parse(d).ok())
            .collect(),
        itxts: all(b"iTXt")
            .into_iter()
            .filter(|d| !is_xmp_itxt(d))
            .filter_map(|d| Itxt::parse(d).ok())
            .collect(),
        ..PngMetadata::default()
    };
    for d in all(b"sPLT") {
        if let Ok(s) = Splt::parse(d)
            && !meta.splt.iter().any(|have| have.name == s.name)
        {
            meta.splt.push(s);
        }
    }
    for (kind, after_idat) in [(UNKNOWN_BEFORE, false), (UNKNOWN_AFTER, true)] {
        for d in of(&BlobKind::from_static(kind), frame_blobs, stream_blobs) {
            if let Some(u) = unknown_chunk(d, after_idat) {
                meta.unknowns.push(u);
            }
        }
    }
    meta
}

/// An unrecognised-chunk record (`type ++ body`) as the chunk the
/// encoder may write: ancillary, four letters, safe to copy, and not a
/// type the encoder writes from its own records.
fn unknown_chunk(data: &[u8], after_idat: bool) -> Option<UnknownChunk> {
    let (ty, body) = data.split_first_chunk::<4>()?;
    let t = ChunkType::new(*ty);
    let own = NATIVE_TYPES.contains(&ty)
        || matches!(
            ty,
            b"cICP" | b"iCCP" | b"eXIf" | b"acTL" | b"fcTL" | b"fdAT"
        );
    if !t.is_ancillary() || !t.is_well_formed_name() || !t.is_safe_to_copy() || own {
        return None;
    }
    Some(UnknownChunk::new(*ty, body.to_vec(), after_idat))
}

/// `explicit` with every record it leaves unset filled from `derived`
/// (a list from `derived` only when `explicit`'s is empty): the
/// caller's [`crate::EncodeOptions::metadata`] wins over what the blob
/// records give.
pub(crate) fn fill_unset(explicit: &PngMetadata, derived: PngMetadata) -> PngMetadata {
    let mut m = explicit.clone();
    m.sbit = m.sbit.or(derived.sbit);
    m.phys = m.phys.or(derived.phys);
    m.time = m.time.or(derived.time);
    m.bkgd = m.bkgd.or(derived.bkgd);
    m.hist = m.hist.or(derived.hist);
    m.trns = m.trns.or(derived.trns);
    m.srgb = m.srgb.or(derived.srgb);
    m.cicp = m.cicp.or(derived.cicp);
    m.gama = m.gama.or(derived.gama);
    m.chrm = m.chrm.or(derived.chrm);
    m.mdcv = m.mdcv.or(derived.mdcv);
    m.clli = m.clli.or(derived.clli);
    if m.splt.is_empty() {
        m.splt = derived.splt;
    }
    if m.texts.is_empty() {
        m.texts = derived.texts;
    }
    if m.ztxts.is_empty() {
        m.ztxts = derived.ztxts;
    }
    if m.itxts.is_empty() {
        m.itxts = derived.itxts;
    }
    if m.unknowns.is_empty() {
        m.unknowns = derived.unknowns;
    }
    m
}

/// Build the `tRNS` keyed-sample record a decoded image's
/// [`crate::PngImage::transparency`] gives, for a caller that has an
/// image but no chunk list.
pub(crate) fn trns_blob(trns: &Trns) -> Option<MetadataBlob> {
    matches!(trns, Trns::Grayscale(_) | Trns::Rgb(..))
        .then(|| MetadataBlob::new(kind_of(b"tRNS"), trns.to_bytes()))
}

/// The `gAMA` record of a decoded image's `metadata.gamma`.
pub(crate) fn gama_blob(gamma: f32) -> Option<MetadataBlob> {
    (gamma.is_finite() && gamma >= 0.0).then(|| {
        let g = Gama::new((gamma * 100_000.0).round() as u32);
        MetadataBlob::new(kind_of(b"gAMA"), g.to_bytes().to_vec())
    })
}

/// The keyed-transparency record among `blobs` for an image of
/// `colour_type` / `bit_depth`, when one fits.
pub(crate) fn trns_of(
    frame_blobs: &[MetadataBlob],
    stream_blobs: &[MetadataBlob],
    colour_type: u8,
    bit_depth: u8,
) -> Option<Trns> {
    if !matches!(colour_type, 0 | 2) {
        return None;
    }
    let kind = kind_of(b"tRNS");
    frame_blobs
        .iter()
        .chain(stream_blobs)
        .find(|b| b.is(&kind))
        .and_then(|b| Trns::parse(&b.data, colour_type, bit_depth, None).ok())
}

/// The `gAMA` value among `blobs` as a gamma exponent.
pub(crate) fn gamma_of(frame_blobs: &[MetadataBlob], stream_blobs: &[MetadataBlob]) -> Option<f32> {
    let kind = kind_of(b"gAMA");
    frame_blobs
        .iter()
        .chain(stream_blobs)
        .find(|b| b.is(&kind))
        .and_then(|b| Gama::parse(&b.data).ok())
        .map(|g| g.gamma_times_100000 as f32 / 100_000.0)
}
