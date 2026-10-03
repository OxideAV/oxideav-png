//! The colour / metadata side of a decode: resolves [`ColorInfo`] and
//! [`Metadata`] from a chunk list without touching pixels. Shared by
//! [`crate::info`], [`crate::decode`] and [`crate::decode_all`].

use crate::chunk::ChunkRef;
use crate::decoder::validate_ancillary_ordering;
use crate::error::{PngError as Error, Result};
use crate::image::{ColorInfo, ColorRange, Metadata};
use crate::metadata::{Chrm, Cicp, Exif, Gama, Iccp, Itxt, Srgb};

/// The `iTXt` keyword that carries an XMP packet (W3C PNG3 §11.3.3.1).
pub const XMP_KEYWORD: &str = "XML:com.adobe.xmp";

/// What the chunk walk found.
#[derive(Clone, Debug, Default)]
pub(crate) struct SideInfo {
    pub color: ColorInfo,
    pub metadata: Metadata,
    pub has_icc: bool,
    pub has_exif: bool,
    pub has_xmp: bool,
}

/// Parse one chunk with `parse`; in strict mode a failure is the
/// error, otherwise the chunk is dropped (`None`). A second instance
/// of a single-instance chunk is an error in strict mode and ignored
/// otherwise (`seen`).
fn take<T>(
    name: &str,
    seen: bool,
    strict: bool,
    parse: impl FnOnce() -> Result<T>,
) -> Result<Option<T>> {
    if seen {
        if strict {
            return Err(Error::invalid(format!(
                "PNG: duplicate {name} chunk (W3C PNG3 §5.6 \"Multiple OK? No\")"
            )));
        }
        return Ok(None);
    }
    match parse() {
        Ok(v) => Ok(Some(v)),
        Err(e) if strict => Err(e),
        Err(_) => Ok(None),
    }
}

/// `cHRM` matches a set of H.273 primaries when every coordinate is
/// within ±0.0005 (50 in `cHRM`'s ×100 000 fixed point) of the table
/// value — the tolerance that absorbs the four-decimal rounding the
/// spec tables use.
fn chrm_matches(c: &Chrm, table: &[(u32, u32); 4]) -> bool {
    let near = |a: u32, b: u32| a.abs_diff(b) <= 50;
    let [(wx, wy), (rx, ry), (gx, gy), (bx, by)] = *table;
    near(c.white_point_x, wx)
        && near(c.white_point_y, wy)
        && near(c.red_x, rx)
        && near(c.red_y, ry)
        && near(c.green_x, gx)
        && near(c.green_y, gy)
        && near(c.blue_x, bx)
        && near(c.blue_y, by)
}

/// H.273 Table 2 value 1 (BT.709 / sRGB): white D65, R (0.640,
/// 0.330), G (0.300, 0.600), B (0.150, 0.060).
const BT709_CHRM: [(u32, u32); 4] = [
    (31270, 32900),
    (64000, 33000),
    (30000, 60000),
    (15000, 6000),
];
/// H.273 Table 2 value 9 (BT.2020): white D65, R (0.708, 0.292),
/// G (0.170, 0.797), B (0.131, 0.046).
const BT2020_CHRM: [(u32, u32); 4] = [
    (31270, 32900),
    (70800, 29200),
    (17000, 79700),
    (13100, 4600),
];

/// Resolve colour signalling and the contract metadata from a chunk
/// list. `strict` is [`crate::DecodeOptions::strict`].
///
/// Colour precedence is W3C PNG3 §4.3 Table 1: `cICP` (1) > `iCCP`
/// (2) > `sRGB` (3) > `cHRM` + `gAMA` (4). The mapping to H.273 code
/// points:
///
/// * `cICP` — the four fields verbatim (range from
///   `video_full_range_flag`);
/// * `iCCP` — [`ColorInfo::png_default`] (the profile, carried in
///   `metadata.icc`, governs; no code point describes it);
/// * `sRGB` — [`ColorInfo::srgb`];
/// * `cHRM` — `primaries` 1 when the chromaticities are BT.709's, 9
///   when BT.2020's, otherwise unspecified; `gAMA` is surfaced as
///   `metadata.gamma` and never mapped to a transfer code point;
/// * nothing — [`ColorInfo::png_default`].
///
/// Lower-precedence chunks still fill `metadata` (`gamma` from `gAMA`,
/// `icc` from `iCCP`) so nothing the file carries is lost.
pub(crate) fn extract(chunks: &[ChunkRef<'_>], strict: bool) -> Result<SideInfo> {
    if strict {
        validate_ancillary_ordering(chunks)?;
    }
    let mut gama: Option<Gama> = None;
    let mut chrm: Option<Chrm> = None;
    let mut srgb: Option<Srgb> = None;
    let mut cicp: Option<Cicp> = None;
    let mut iccp: Option<Iccp> = None;
    let mut exif: Option<Exif> = None;
    let mut xmp: Option<Vec<u8>> = None;
    let (mut s_gama, mut s_chrm, mut s_srgb, mut s_cicp, mut s_iccp, mut s_exif) =
        (false, false, false, false, false, false);
    let mut info = SideInfo::default();

    for c in chunks {
        match &c.chunk_type {
            b"gAMA" => {
                if let Some(v) = take("gAMA", s_gama, strict, || Gama::parse(c.data))? {
                    gama = Some(v);
                }
                s_gama = true;
            }
            b"cHRM" => {
                if let Some(v) = take("cHRM", s_chrm, strict, || Chrm::parse(c.data))? {
                    chrm = Some(v);
                }
                s_chrm = true;
            }
            b"sRGB" => {
                if let Some(v) = take("sRGB", s_srgb, strict, || Srgb::parse(c.data))? {
                    srgb = Some(v);
                }
                s_srgb = true;
            }
            b"cICP" => {
                if let Some(v) = take("cICP", s_cicp, strict, || Cicp::parse(c.data))? {
                    cicp = Some(v);
                }
                s_cicp = true;
            }
            b"iCCP" => {
                info.has_icc = true;
                if let Some(v) = take("iCCP", s_iccp, strict, || Iccp::parse(c.data))? {
                    iccp = Some(v);
                }
                s_iccp = true;
            }
            b"eXIf" => {
                info.has_exif = true;
                if let Some(v) = take("eXIf", s_exif, strict, || Exif::parse(c.data))? {
                    exif = Some(v);
                }
                s_exif = true;
            }
            b"iTXt" => {
                // Only the XMP keyword is of interest here; a quick
                // keyword peek avoids inflating unrelated text bodies.
                let is_xmp = c.data.get(..XMP_KEYWORD.len()) == Some(XMP_KEYWORD.as_bytes())
                    && c.data.get(XMP_KEYWORD.len()) == Some(&0);
                if is_xmp {
                    info.has_xmp = true;
                    if xmp.is_none() {
                        match Itxt::parse(c.data) {
                            Ok(t) => xmp = Some(t.text.into_bytes()),
                            Err(e) if strict => return Err(e),
                            Err(_) => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // §4.3 Table 1 precedence.
    info.color = if let Some(ci) = &cicp {
        ColorInfo::new(
            if ci.video_full_range_flag != 0 {
                ColorRange::Full
            } else {
                ColorRange::Limited
            },
            ci.color_primaries,
            ci.transfer_function,
            ci.matrix_coefficients,
        )
    } else if iccp.is_some() {
        ColorInfo::png_default()
    } else if srgb.is_some() {
        ColorInfo::srgb()
    } else {
        let mut c = ColorInfo::png_default();
        if let Some(ch) = &chrm {
            if chrm_matches(ch, &BT709_CHRM) {
                c.primaries = ColorInfo::PRIMARIES_BT709;
            } else if chrm_matches(ch, &BT2020_CHRM) {
                c.primaries = ColorInfo::PRIMARIES_BT2020;
            }
        }
        c
    };

    info.metadata = Metadata {
        icc: iccp.map(|p| p.profile),
        exif: exif.map(|e| e.data),
        xmp,
        gamma: gama.map(|g| g.gamma_times_100000 as f32 / 100_000.0),
    };
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bt709_chrm_matches_png3_table17_values() {
        // W3C PNG3 §11.3.2.5 Table 17: the cHRM an sRGB-compatible file
        // carries.
        let c = Chrm::SRGB;
        assert!(chrm_matches(&c, &BT709_CHRM));
        assert!(!chrm_matches(&c, &BT2020_CHRM));
        let off = Chrm::new(
            (31270, 32900),
            (64100, 33000),
            (30000, 60000),
            (15000, 6000),
        );
        assert!(!chrm_matches(&off, &BT709_CHRM));
    }
}
