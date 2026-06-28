//! Decode-side enforcement of the W3C PNG3 §5.6 Table 7 chunk-ordering
//! rules.
//!
//! §5.6 is normative ("These lattice diagrams represent the constraints
//! on positioning imposed by this specification … Chunks higher up shall
//! appear before chunks lower down"). Table 7 sorts every ancillary
//! chunk this codec understands into one of four positional buckets
//! relative to the first `PLTE` and the first `IDAT`:
//!
//! * **Before PLTE and IDAT** — `cHRM`, `cICP`, `gAMA`, `iCCP`, `mDCV`,
//!   `cLLI`, `sBIT`, `sRGB`.
//! * **After PLTE; before IDAT** — `bKGD`, `hIST`, `tRNS`.
//! * **Before IDAT** (no `PLTE` relationship) — `eXIf`, `pHYs`, `sPLT`.
//! * **None** (anywhere) — `tIME`, `tEXt`, `zTXt`, `iTXt`.
//!
//! These tests build deliberately mis-ordered streams (re-CRC'd via the
//! production chunk writer so they pass the framing / CRC gate) and
//! assert `parse_metadata` / `decode_png` reject them, plus matching
//! positive cases that conformant ordering still parses.

use oxideav_png::{
    decode_png, encode_apng, encode_png_image, parse_apng, parse_metadata, PngImage, PngPixelFormat,
};

fn rgba_2x2() -> PngImage {
    PngImage {
        width: 2,
        height: 2,
        pixel_format: PngPixelFormat::Rgba,
        stride: 8,
        data: vec![
            255, 0, 0, 255, // (0,0)
            0, 255, 0, 255, // (1,0)
            0, 0, 255, 255, // (0,1)
            255, 255, 255, 255, // (1,1)
        ],
        palette: Vec::new(),
    }
}

/// A 2x2 indexed image so a stream carries a real `PLTE` chunk for the
/// "after PLTE" bucket tests.
fn pal_2x2() -> PngImage {
    PngImage {
        width: 2,
        height: 2,
        pixel_format: PngPixelFormat::Pal8,
        stride: 2,
        data: vec![0, 1, 2, 3],
        // 4 palette entries (RGB triples) so a bKGD/hIST index is valid.
        palette: vec![
            0, 0, 0, // 0
            255, 0, 0, // 1
            0, 255, 0, // 2
            0, 0, 255, // 3
        ],
    }
}

/// Splice `chunks` into an encoded PNG immediately *before* the first
/// `IDAT`, re-framing each with the production CRC writer.
fn splice_before_idat(bytes: &[u8], chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let idat = bytes
        .windows(4)
        .position(|w| w == b"IDAT")
        .expect("IDAT present");
    let inject = idat - 4;
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..inject]);
    for (ty, data) in chunks {
        oxideav_png::chunk::write_chunk(&mut out, ty, data);
    }
    out.extend_from_slice(&bytes[inject..]);
    out
}

/// Splice `chunks` in *after* the IDAT run, immediately before `IEND`.
fn splice_before_iend(bytes: &[u8], chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let iend = bytes
        .windows(4)
        .position(|w| w == b"IEND")
        .expect("IEND present");
    let inject = iend - 4;
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..inject]);
    for (ty, data) in chunks {
        oxideav_png::chunk::write_chunk(&mut out, ty, data);
    }
    out.extend_from_slice(&bytes[inject..]);
    out
}

/// Splice `chunks` in immediately *before* the first `PLTE`.
fn splice_before_plte(bytes: &[u8], chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let plte = bytes
        .windows(4)
        .position(|w| w == b"PLTE")
        .expect("PLTE present");
    let inject = plte - 4;
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..inject]);
    for (ty, data) in chunks {
        oxideav_png::chunk::write_chunk(&mut out, ty, data);
    }
    out.extend_from_slice(&bytes[inject..]);
    out
}

/// Splice `chunks` in immediately *after* the first `PLTE`.
fn splice_after_plte(bytes: &[u8], chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let plte = bytes
        .windows(4)
        .position(|w| w == b"PLTE")
        .expect("PLTE present");
    // PLTE chunk = 4 len + 4 type + N data + 4 CRC. The length is the
    // 4 bytes before the type code.
    let len_off = plte - 4;
    let data_len = u32::from_be_bytes([
        bytes[len_off],
        bytes[len_off + 1],
        bytes[len_off + 2],
        bytes[len_off + 3],
    ]) as usize;
    let inject = plte + 4 + data_len + 4; // past type + data + CRC
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..inject]);
    for (ty, data) in chunks {
        oxideav_png::chunk::write_chunk(&mut out, ty, data);
    }
    out.extend_from_slice(&bytes[inject..]);
    out
}

// ---- valid payloads for the chunks under test ----
const SRGB_PERCEPTUAL: &[u8] = &[0];
const GAMA_45455: &[u8] = &[0, 0, 0xB1, 0x8F]; // 45455 = sRGB gamma
const SBIT_RGBA: &[u8] = &[8, 8, 8, 8];
const PHYS_72DPI: &[u8] = &[0, 0, 0x0B, 0x12, 0, 0, 0x0B, 0x12, 1];
const EXIF_LE_HEADER: &[u8] = &[0x49, 0x49, 0x2A, 0x00];
const BKGD_RGBA: &[u8] = &[0, 255, 0, 0, 0, 0]; // 3 BE u16 samples

fn base_rgba_png() -> Vec<u8> {
    encode_png_image(&rgba_2x2()).expect("encode rgba")
}
fn base_pal_png() -> Vec<u8> {
    encode_png_image(&pal_2x2()).expect("encode pal")
}

// =====================================================================
// Bucket 1 — "Before PLTE and IDAT": cHRM cICP gAMA iCCP mDCV cLLI sBIT sRGB
// =====================================================================

#[test]
fn srgb_after_idat_rejected() {
    let bytes = splice_before_iend(&base_rgba_png(), &[(b"sRGB", SRGB_PERCEPTUAL)]);
    assert!(
        parse_metadata(&bytes).is_err(),
        "sRGB after IDAT must be rejected (§5.6 Table 7)"
    );
}

#[test]
fn gama_after_idat_rejected() {
    let bytes = splice_before_iend(&base_rgba_png(), &[(b"gAMA", GAMA_45455)]);
    assert!(parse_metadata(&bytes).is_err());
}

#[test]
fn sbit_after_idat_rejected() {
    let bytes = splice_before_iend(&base_rgba_png(), &[(b"sBIT", SBIT_RGBA)]);
    assert!(parse_metadata(&bytes).is_err());
}

#[test]
fn srgb_after_plte_rejected() {
    // A colour-space chunk shall precede PLTE, not just IDAT.
    let bytes = splice_after_plte(&base_pal_png(), &[(b"sRGB", SRGB_PERCEPTUAL)]);
    assert!(
        parse_metadata(&bytes).is_err(),
        "sRGB after PLTE must be rejected (§5.6 \"Before PLTE and IDAT\")"
    );
}

#[test]
fn gama_after_plte_rejected() {
    let bytes = splice_after_plte(&base_pal_png(), &[(b"gAMA", GAMA_45455)]);
    assert!(parse_metadata(&bytes).is_err());
}

#[test]
fn sbit_before_plte_accepted() {
    // The conformant position for a "Before PLTE and IDAT" chunk.
    // sBIT on an indexed image is 3 bytes (one per palette RGB channel).
    let bytes = splice_before_plte(&base_pal_png(), &[(b"sBIT", &[8, 8, 8])]);
    let md = parse_metadata(&bytes).expect("sBIT before PLTE parses");
    assert!(md.sbit.is_some());
}

// =====================================================================
// Bucket 2 — "After PLTE; before IDAT": bKGD hIST tRNS
// =====================================================================

#[test]
fn bkgd_after_idat_rejected() {
    let bytes = splice_before_iend(&base_rgba_png(), &[(b"bKGD", BKGD_RGBA)]);
    assert!(
        parse_metadata(&bytes).is_err(),
        "bKGD after IDAT must be rejected (§5.6 \"After PLTE; before IDAT\")"
    );
}

#[test]
fn bkgd_before_idat_accepted() {
    let bytes = splice_before_idat(&base_rgba_png(), &[(b"bKGD", BKGD_RGBA)]);
    let md = parse_metadata(&bytes).expect("bKGD before IDAT parses");
    assert!(md.bkgd.is_some());
}

#[test]
fn trns_after_idat_rejected() {
    // tRNS for a truecolor (ct=2) image is 6 bytes (one BE RGB key).
    let rgb = PngImage {
        width: 2,
        height: 2,
        pixel_format: PngPixelFormat::Rgb24,
        stride: 6,
        data: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
        palette: Vec::new(),
    };
    let png = encode_png_image(&rgb).expect("encode rgb");
    let bytes = splice_before_iend(&png, &[(b"tRNS", &[0, 255, 0, 0, 0, 0])]);
    assert!(parse_metadata(&bytes).is_err());
}

// =====================================================================
// Bucket 3 — "Before IDAT" (no PLTE relationship): eXIf pHYs sPLT
// =====================================================================

#[test]
fn phys_after_idat_rejected() {
    let bytes = splice_before_iend(&base_rgba_png(), &[(b"pHYs", PHYS_72DPI)]);
    assert!(
        parse_metadata(&bytes).is_err(),
        "pHYs after IDAT must be rejected (§5.6 \"Before IDAT\")"
    );
}

#[test]
fn phys_before_idat_accepted() {
    let bytes = splice_before_idat(&base_rgba_png(), &[(b"pHYs", PHYS_72DPI)]);
    let md = parse_metadata(&bytes).expect("pHYs before IDAT parses");
    assert!(md.phys.is_some());
}

#[test]
fn phys_after_plte_still_before_idat_accepted() {
    // "Before IDAT" carries no PLTE relationship: either side of PLTE is
    // fine so long as it precedes the pixel data.
    let bytes = splice_after_plte(&base_pal_png(), &[(b"pHYs", PHYS_72DPI)]);
    let md = parse_metadata(&bytes).expect("pHYs after PLTE before IDAT parses");
    assert!(md.phys.is_some());
}

#[test]
fn exif_after_idat_rejected() {
    let bytes = splice_before_iend(&base_rgba_png(), &[(b"eXIf", EXIF_LE_HEADER)]);
    assert!(parse_metadata(&bytes).is_err());
}

// =====================================================================
// Bucket 4 — "None": tIME / tEXt / zTXt / iTXt may appear anywhere
// =====================================================================

#[test]
fn time_after_idat_accepted() {
    // tIME has no ordering constraint; an after-IDAT placement is legal.
    // 7 bytes: year(BE u16) month day hour min sec.
    let time = &[0x07, 0xE8, 1, 1, 0, 0, 0]; // 2024-01-01 00:00:00
    let bytes = splice_before_iend(&base_rgba_png(), &[(b"tIME", time)]);
    let md = parse_metadata(&bytes).expect("tIME after IDAT parses (Ordering: None)");
    assert!(md.time.is_some());
}

// =====================================================================
// §5.6 / §11.2.3 — "Multiple IDAT chunks shall be consecutive"
// =====================================================================

/// Split a single-IDAT PNG so a *non*-IDAT chunk (tIME) lands between
/// the IDAT run and IEND — but with a second (empty) IDAT after it. The
/// decode path concatenates IDAT payloads, so a non-consecutive run must
/// be rejected rather than silently splicing two compressed segments.
#[test]
fn non_consecutive_idat_rejected() {
    let png = base_rgba_png();
    // Insert a tIME then a second (empty) IDAT before IEND. The original
    // IDAT run sits earlier; the new IDAT is now non-consecutive.
    let time: &[u8] = &[0x07, 0xE8, 1, 1, 0, 0, 0];
    let bytes = splice_before_iend(&png, &[(b"tIME", time), (b"IDAT", &[])]);
    assert!(
        decode_png(&bytes).is_err(),
        "non-consecutive IDAT must be rejected (§5.6)"
    );
}

/// A trailing *consecutive* extra IDAT (no intervening chunk) is legal —
/// the run is unbroken. (Here the extra IDAT is empty and abuts the
/// real one.)
#[test]
fn consecutive_extra_idat_accepted() {
    let png = base_rgba_png();
    // Inject an empty IDAT immediately *after* the existing IDAT run by
    // splicing it before IEND with nothing in between. Find the byte
    // just past the last IDAT's CRC by locating IEND and inserting the
    // empty IDAT right before it with no other chunk — still consecutive
    // because no non-IDAT chunk separates them.
    let bytes = splice_before_iend(&png, &[(b"IDAT", &[])]);
    // An empty trailing IDAT contributes no bytes to the zlib stream and
    // does not break the run, so the image still decodes.
    assert!(
        decode_png(&bytes).is_ok(),
        "consecutive extra (empty) IDAT must still decode (§5.6)"
    );
}

#[test]
fn text_after_idat_accepted() {
    // tEXt "Comment\0hi" — no ordering constraint.
    let mut payload = b"Comment".to_vec();
    payload.push(0);
    payload.extend_from_slice(b"hi");
    let bytes = splice_before_iend(&base_rgba_png(), &[(b"tEXt", &payload)]);
    let md = parse_metadata(&bytes).expect("tEXt after IDAT parses (Ordering: None)");
    assert_eq!(md.texts.len(), 1);
}

// =====================================================================
// APNG container — §5.6 ordering enforced on the animated path too
// =====================================================================

fn base_apng() -> Vec<u8> {
    // Two identical RGBA frames; the encoder paints both full-canvas.
    let f = rgba_2x2();
    encode_apng(&[f.clone(), f], 10, 0).expect("encode apng")
}

#[test]
fn apng_baseline_parses() {
    let png = base_apng();
    let info = parse_apng(&png).expect("baseline APNG parses");
    assert_eq!(info.frames.len(), 2);
}

#[test]
fn apng_srgb_after_idat_rejected() {
    // A colour-space chunk shall precede PLTE/IDAT even in an APNG.
    let png = base_apng();
    let bytes = splice_before_iend(&png, &[(b"sRGB", SRGB_PERCEPTUAL)]);
    assert!(
        parse_apng(&bytes).is_err(),
        "sRGB after IDAT must be rejected on the APNG path (§5.6)"
    );
}

#[test]
fn apng_phys_after_idat_rejected() {
    let png = base_apng();
    let bytes = splice_before_iend(&png, &[(b"pHYs", PHYS_72DPI)]);
    assert!(parse_apng(&bytes).is_err());
}

#[test]
fn apng_non_consecutive_default_idat_rejected() {
    // Break the default image's IDAT run with a tIME then a second IDAT.
    let png = base_apng();
    let time: &[u8] = &[0x07, 0xE8, 1, 1, 0, 0, 0];
    let bytes = splice_before_iend(&png, &[(b"tIME", time), (b"IDAT", &[])]);
    assert!(
        parse_apng(&bytes).is_err(),
        "non-consecutive default-image IDAT must be rejected on the APNG path (§5.6)"
    );
}

// =====================================================================
// Critical-chunk ordering (§5.1 / §5.6): IHDR-first, single IHDR,
// PLTE-before-IDAT.
// =====================================================================

/// Splice `chunks` immediately after the 8-byte PNG signature — i.e.
/// *before* the IHDR chunk.
fn splice_after_signature(bytes: &[u8], chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&bytes[..8]); // signature
    for (ty, data) in chunks {
        oxideav_png::chunk::write_chunk(&mut out, ty, data);
    }
    out.extend_from_slice(&bytes[8..]);
    out
}

#[test]
fn chunk_before_ihdr_rejected() {
    // A pHYs ahead of IHDR violates "signature immediately followed by
    // an IHDR chunk" (§5.1).
    let bytes = splice_after_signature(&base_rgba_png(), &[(b"pHYs", PHYS_72DPI)]);
    assert!(
        parse_metadata(&bytes).is_err(),
        "a chunk before IHDR must be rejected (§5.1)"
    );
    assert!(decode_png(&bytes).is_err());
}

#[test]
fn duplicate_ihdr_rejected() {
    // The IHDR payload of a 2x2 RGBA image, re-spliced after the
    // signature so two IHDRs precede everything else.
    let png = base_rgba_png();
    // IHDR data is the 13 bytes following the "IHDR" type code.
    let ihdr_pos = png.windows(4).position(|w| w == b"IHDR").expect("IHDR");
    let ihdr_data = &png[ihdr_pos + 4..ihdr_pos + 4 + 13];
    let bytes = splice_after_signature(&png, &[(b"IHDR", ihdr_data)]);
    assert!(
        parse_metadata(&bytes).is_err(),
        "a second IHDR must be rejected (§5.1: \"Only one IHDR chunk\")"
    );
}

#[test]
fn plte_after_idat_rejected() {
    // Splice a PLTE after the IDAT run of a truecolor image (which has
    // no original PLTE). §5.6 places PLTE "Before first IDAT"; a palette
    // following the pixel data is rejected.
    let png = base_rgba_png();
    let plte: &[u8] = &[0, 0, 0, 255, 255, 255];
    let bytes = splice_before_iend(&png, &[(b"PLTE", plte)]);
    assert!(
        decode_png(&bytes).is_err(),
        "PLTE after IDAT must be rejected (§5.6 Table 7)"
    );
}

#[test]
fn well_ordered_stream_still_parses() {
    // The encoder's own output is fully conformant on every gate.
    let png = base_rgba_png();
    assert!(decode_png(&png).is_ok());
    assert!(parse_metadata(&png).is_ok());
    let pal = base_pal_png();
    assert!(decode_png(&pal).is_ok());
}
