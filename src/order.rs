//! Caller-controlled ancillary chunk placement (W3C PNG3 §5.6).
//!
//! The encoder writes its ancillary chunks in a fixed default order
//! that satisfies W3C PNG3 §5.6 Table 7. A caller that needs another
//! encoder's layout (for byte-for-byte output, or to keep a file's
//! layout across a re-encode) passes a [`ChunkOrder`] in
//! [`crate::EncodeOptions::chunk_order`]: an ordered list of
//! [`ChunkSlot`]s naming chunk types, optionally one instance of a
//! multi-instance type, and the `PLTE` / `tRNS` / `IDAT` positions.
//! [`crate::parse_chunk_order`] reads the order of an existing file in
//! the same form.
//!
//! Every ordering is checked against Table 7 before the first byte is
//! written; an order that would put a chunk on the wrong side of
//! `PLTE` or `IDAT` is an encode error, never a nonconformant file.

use crate::error::{PngError as Error, Result};

/// One position in a [`ChunkOrder`]: a chunk type, and for the
/// multi-instance types (`sPLT`, `tEXt`, `zTXt`, `iTXt`, and
/// unrecognised ancillary chunks) optionally which instance.
///
/// * `instance: None` places every instance of the type not placed by an
///   earlier or later `Some(n)` slot, in the order the encoder holds
///   them (the `Vec` order of [`crate::PngMetadata`]).
/// * `instance: Some(n)` places the `n`-th (0-based) instance of the
///   type. For `iTXt`, the instances are
///   [`crate::PngMetadata::itxts`] followed by the image's XMP packet
///   when the encoder writes one. For an unrecognised chunk type, they
///   are the [`crate::PngMetadata::unknowns`] entries of that type. A
///   single-instance type has only instance `0`.
///
/// `PLTE`, `tRNS` and `IDAT` are accepted as markers: `IDAT` stands for
/// the whole pixel stream (with an APNG's frame chunks), `PLTE` /
/// `tRNS` for the palette chunks the image implies. `IHDR`, `IEND`,
/// `acTL`, `fcTL` and `fdAT` have fixed positions and are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ChunkSlot {
    /// The 4-byte chunk type, e.g. `*b"tEXt"`.
    pub chunk_type: [u8; 4],
    /// Which instance of the type, or `None` for every remaining one.
    pub instance: Option<usize>,
}

impl ChunkSlot {
    /// Every (remaining) instance of `chunk_type`.
    pub fn new(chunk_type: [u8; 4]) -> Self {
        Self {
            chunk_type,
            instance: None,
        }
    }

    /// The `instance`-th (0-based) instance of `chunk_type`.
    pub fn instance(chunk_type: [u8; 4], instance: usize) -> Self {
        Self {
            chunk_type,
            instance: Some(instance),
        }
    }
}

impl From<[u8; 4]> for ChunkSlot {
    fn from(chunk_type: [u8; 4]) -> Self {
        Self::new(chunk_type)
    }
}

impl From<&[u8; 4]> for ChunkSlot {
    fn from(chunk_type: &[u8; 4]) -> Self {
        Self::new(*chunk_type)
    }
}

impl std::fmt::Display for ChunkSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.chunk_type))?;
        if let Some(n) = self.instance {
            write!(f, "#{n}")?;
        }
        Ok(())
    }
}

/// The order the encoder writes its chunks in: a list of
/// [`ChunkSlot`]s (see [`crate::EncodeOptions::chunk_order`]).
///
/// The list need not be complete:
///
/// * a slot naming a chunk the encode does not write is skipped, so one
///   order can serve images with different metadata;
/// * a chunk the encode writes but the list does not name keeps a
///   default position: the chunks Table 7 places before `PLTE` go
///   immediately before `PLTE` (or before `IDAT` when there is no
///   `PLTE`), every other chunk written before `IDAT` goes immediately
///   before `IDAT`, and unrecognised chunks captured after `IDAT` go
///   at the end; each group keeps the encoder's default order;
/// * an unlisted `IDAT` goes after the last slot, an unlisted `PLTE`
///   before the first chunk that Table 7 places after it (or before
///   `IDAT`), and an unlisted `tRNS` right after `PLTE`.
///
/// The resulting order must satisfy W3C PNG3 §5.6 Table 7: the colour
/// chunks (`cHRM`, `cICP`, `gAMA`, `iCCP`, `mDCV`, `cLLI`, `sBIT`,
/// `sRGB`) before `PLTE` and `IDAT`; `bKGD`, `hIST` and `tRNS` after
/// `PLTE` and before `IDAT`; `eXIf`, `pHYs` and `sPLT` before `IDAT`;
/// `tIME`, `tEXt`, `zTXt` and `iTXt` anywhere; and an unrecognised
/// chunk on the side of `IDAT` it was captured on (§14.2). When the
/// image has no `PLTE`, only the `IDAT` constraints apply.
///
/// The text form ([`std::str::FromStr`] / [`std::fmt::Display`]) is the
/// slots separated by commas, `#n` selecting an instance:
/// `"iCCP,cHRM,bKGD,pHYs,tIME,zTXt,tEXt,IDAT,tEXt#3"`.
///
/// ```
/// use oxideav_png::{encode, ChunkOrder, EncodeOptions, PngImage, PngMetadata, Text, Time};
///
/// let image = PngImage::from_rgb8(1, 1, vec![0, 0, 0])?;
/// let meta = PngMetadata::default()
///     .with_time(Time::new(2026, 11, 8, 12, 0, 0))
///     .with_texts(vec![Text::new("Comment".into(), "hi".into())]);
/// let order: ChunkOrder = "IDAT,tIME,tEXt".parse()?;
/// let opts = EncodeOptions::new().with_metadata(meta).with_chunk_order(order.clone());
/// let png = encode(&image, &opts)?;
/// // IHDR, IDAT, tIME, tEXt, IEND
/// assert_eq!(oxideav_png::parse_chunk_order(&png)?.to_string(), "IDAT,tIME,tEXt#0");
/// # Ok::<(), oxideav_png::PngError>(())
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ChunkOrder {
    slots: Vec<ChunkSlot>,
}

impl ChunkOrder {
    /// An empty order: every chunk keeps its default position.
    pub fn new() -> Self {
        Self::default()
    }

    /// An order of whole chunk types (each slot `instance: None`).
    pub fn from_types<I, T>(types: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<ChunkSlot>,
    {
        Self {
            slots: types.into_iter().map(Into::into).collect(),
        }
    }

    /// Append `slot`.
    pub fn with(mut self, slot: impl Into<ChunkSlot>) -> Self {
        self.push(slot);
        self
    }

    /// Append `slot`.
    pub fn push(&mut self, slot: impl Into<ChunkSlot>) {
        self.slots.push(slot.into());
    }

    /// The slots, in order.
    pub fn slots(&self) -> &[ChunkSlot] {
        &self.slots
    }

    /// `true` when the order names no slot.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

impl<T: Into<ChunkSlot>> FromIterator<T> for ChunkOrder {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_types(iter)
    }
}

impl std::fmt::Display for ChunkOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, s) in self.slots.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{s}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for ChunkOrder {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let mut order = Self::new();
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (name, instance) = match part.split_once('#') {
                Some((name, n)) => {
                    let n = n.trim().parse::<usize>().map_err(|_| {
                        Error::invalid(format!("PNG chunk order: bad instance number in {part:?}"))
                    })?;
                    (name.trim(), Some(n))
                }
                None => (part, None),
            };
            let bytes = name.as_bytes();
            if bytes.len() != 4 || !bytes.iter().all(u8::is_ascii_alphabetic) {
                return Err(Error::invalid(format!(
                    "PNG chunk order: {name:?} is not a 4-letter chunk type (W3C PNG3 §5.4)"
                )));
            }
            order.push(ChunkSlot {
                chunk_type: [bytes[0], bytes[1], bytes[2], bytes[3]],
                instance,
            });
        }
        Ok(order)
    }
}

/// Where Table 7 lets a chunk go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// "Before PLTE and IDAT".
    BeforePlte,
    /// "After PLTE; before IDAT".
    AfterPlte,
    /// "Before IDAT".
    BeforeIdat,
    /// "None".
    Anywhere,
    /// An unrecognised chunk, on the recorded side of `IDAT` (§14.2):
    /// `true` = after.
    Side(bool),
    /// `PLTE` itself: "Before first IDAT".
    Plte,
    /// The pixel stream.
    Idat,
}

/// One chunk (or marker) an encode writes, with the slot identity it
/// answers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry<K> {
    pub(crate) key: K,
    pub(crate) chunk_type: [u8; 4],
    /// 0-based rank among the entries of the same `chunk_type`.
    pub(crate) instance: usize,
    pub(crate) placement: Placement,
}

/// Chunk types whose position is fixed and which an order may not name.
const FIXED: [&[u8; 4]; 5] = [b"IHDR", b"IEND", b"acTL", b"fcTL", b"fdAT"];

/// Arrange `default` (every entry the encode writes, in the encoder's
/// default order, containing exactly one `Placement::Idat` entry and at
/// most one `Placement::Plte`) under `order`, and check the result
/// against W3C PNG3 §5.6 Table 7. See [`ChunkOrder`] for the rules.
pub(crate) fn arrange<K: Copy>(default: &[Entry<K>], order: &ChunkOrder) -> Result<Vec<Entry<K>>> {
    let mut placed = vec![false; default.len()];
    let mut seq: Vec<usize> = Vec::with_capacity(default.len());
    let mut seen: Vec<ChunkSlot> = Vec::new();
    for slot in order.slots() {
        if FIXED.contains(&&slot.chunk_type) {
            return Err(Error::invalid(format!(
                "PNG chunk order: {slot} has a fixed position and cannot be placed \
                 (W3C PNG3 §5.6 Table 7)"
            )));
        }
        if !slot.chunk_type.iter().all(u8::is_ascii_alphabetic) {
            return Err(Error::invalid(format!(
                "PNG chunk order: {:?} is not a 4-letter chunk type (W3C PNG3 §5.4)",
                slot.chunk_type
            )));
        }
        if seen.contains(slot) {
            return Err(Error::invalid(format!(
                "PNG chunk order: {slot} is listed twice"
            )));
        }
        seen.push(*slot);
        // A whole-type slot takes the remaining instances, minus any that
        // a later instance slot names.
        let later = |i: usize| {
            order.slots().iter().any(|s| {
                s.chunk_type == default[i].chunk_type && s.instance == Some(default[i].instance)
            })
        };
        for (i, e) in default.iter().enumerate() {
            if placed[i] || e.chunk_type != slot.chunk_type {
                continue;
            }
            let hit = match slot.instance {
                Some(n) => e.instance == n,
                None => !later(i),
            };
            if hit {
                placed[i] = true;
                seq.push(i);
            }
        }
    }

    let find = |seq: &[usize], p: Placement| seq.iter().position(|&i| default[i].placement == p);
    // IDAT: after the last slot when unlisted.
    if let Some(i) = default.iter().position(|e| e.placement == Placement::Idat)
        && !placed[i]
    {
        placed[i] = true;
        seq.push(i);
    }
    let idat_at = |seq: &[usize]| find(seq, Placement::Idat).unwrap_or(seq.len());
    // PLTE: before the first listed chunk that must follow it, else
    // before IDAT.
    if let Some(i) = default.iter().position(|e| e.placement == Placement::Plte)
        && !placed[i]
    {
        placed[i] = true;
        let at = seq
            .iter()
            .position(|&j| default[j].placement == Placement::AfterPlte)
            .unwrap_or_else(|| idat_at(&seq))
            .min(idat_at(&seq));
        seq.insert(at, i);
    }
    // tRNS: right after PLTE, or with the other before-IDAT chunks.
    if let Some(i) = default.iter().position(|e| &e.chunk_type == b"tRNS")
        && !placed[i]
        && let Some(p) = find(&seq, Placement::Plte)
    {
        placed[i] = true;
        seq.insert(p + 1, i);
    }
    // The unlisted rest, in default order, by group.
    let before_plte: Vec<usize> = (0..default.len())
        .filter(|&i| !placed[i] && default[i].placement == Placement::BeforePlte)
        .collect();
    let at = find(&seq, Placement::Plte).unwrap_or_else(|| idat_at(&seq));
    seq.splice(at..at, before_plte.iter().copied());
    for &i in &before_plte {
        placed[i] = true;
    }
    let tail: Vec<usize> = (0..default.len())
        .filter(|&i| !placed[i] && default[i].placement == Placement::Side(true))
        .collect();
    for &i in &tail {
        placed[i] = true;
    }
    let before_idat: Vec<usize> = (0..default.len()).filter(|&i| !placed[i]).collect();
    let at = idat_at(&seq);
    seq.splice(at..at, before_idat);
    seq.extend(tail);

    // Table 7.
    let plte = find(&seq, Placement::Plte);
    let idat = idat_at(&seq);
    for (pos, &i) in seq.iter().enumerate() {
        let e = &default[i];
        let name = String::from_utf8_lossy(&e.chunk_type);
        let before_idat = pos < idat;
        let ok = match e.placement {
            Placement::BeforePlte => before_idat && plte.is_none_or(|p| pos < p),
            Placement::AfterPlte => before_idat && plte.is_none_or(|p| pos > p),
            Placement::BeforeIdat | Placement::Plte => before_idat,
            Placement::Side(after) => before_idat != after,
            Placement::Anywhere | Placement::Idat => true,
        };
        if !ok {
            let rule = match e.placement {
                Placement::BeforePlte => "shall precede PLTE and IDAT (W3C PNG3 §5.6 Table 7)",
                Placement::AfterPlte => {
                    "shall follow PLTE and precede IDAT (W3C PNG3 §5.6 Table 7)"
                }
                Placement::Side(true) => {
                    "was captured after IDAT and shall stay after it (W3C PNG3 §14.2)"
                }
                Placement::Side(false) => {
                    "was captured before IDAT and shall stay before it (W3C PNG3 §14.2)"
                }
                _ => "shall precede IDAT (W3C PNG3 §5.6 Table 7)",
            };
            return Err(Error::invalid(format!(
                "PNG chunk order: {name} {rule}; order {order}"
            )));
        }
    }
    Ok(seq.into_iter().map(|i| default[i]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(t: &[u8; 4], instance: usize, placement: Placement) -> Entry<usize> {
        Entry {
            key: 0,
            chunk_type: *t,
            instance,
            placement,
        }
    }

    fn names(seq: &[Entry<usize>]) -> String {
        seq.iter()
            .map(|e| String::from_utf8_lossy(&e.chunk_type).into_owned())
            .collect::<Vec<_>>()
            .join(",")
    }

    fn set() -> Vec<Entry<usize>> {
        vec![
            entry(b"gAMA", 0, Placement::BeforePlte),
            entry(b"cHRM", 0, Placement::BeforePlte),
            entry(b"PLTE", 0, Placement::Plte),
            entry(b"tRNS", 0, Placement::AfterPlte),
            entry(b"bKGD", 0, Placement::AfterPlte),
            entry(b"pHYs", 0, Placement::BeforeIdat),
            entry(b"tIME", 0, Placement::Anywhere),
            entry(b"tEXt", 0, Placement::Anywhere),
            entry(b"tEXt", 1, Placement::Anywhere),
            entry(b"IDAT", 0, Placement::Idat),
        ]
    }

    #[test]
    fn empty_order_is_default() {
        let d = set();
        assert_eq!(arrange(&d, &ChunkOrder::new()).unwrap(), d);
    }

    #[test]
    fn partial_orders_fill_in_defaults() {
        let d = set();
        let o: ChunkOrder = "cHRM,gAMA,IDAT,tEXt#0,tIME".parse().unwrap();
        assert_eq!(
            names(&arrange(&d, &o).unwrap()),
            "cHRM,gAMA,PLTE,tRNS,bKGD,pHYs,tEXt,IDAT,tEXt,tIME"
        );
        let o: ChunkOrder = "bKGD,pHYs".parse().unwrap();
        assert_eq!(
            names(&arrange(&d, &o).unwrap()),
            "gAMA,cHRM,PLTE,tRNS,bKGD,pHYs,tIME,tEXt,tEXt,IDAT"
        );
    }

    #[test]
    fn table7_violations_are_errors() {
        let d = set();
        for bad in [
            "PLTE,gAMA",
            "IDAT,pHYs",
            "bKGD,PLTE",
            "IDAT,PLTE",
            "IHDR",
            "tEXt,tEXt",
        ] {
            let o: ChunkOrder = bad.parse().unwrap();
            assert!(arrange(&d, &o).is_err(), "{bad}");
        }
    }

    #[test]
    fn text_form_round_trips() {
        let o: ChunkOrder = "iCCP, cHRM,tEXt#2,IDAT".parse().unwrap();
        assert_eq!(o.to_string(), "iCCP,cHRM,tEXt#2,IDAT");
        assert!("gAM".parse::<ChunkOrder>().is_err());
        assert!("tEXt#x".parse::<ChunkOrder>().is_err());
    }
}
