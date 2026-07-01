//! PNG per-row filters + CRC32 (used by chunks).
//!
//! PNG applies a single filter type byte at the start of each decoded row
//! ("filter type byte"), followed by the filtered pixel bytes. The filter
//! operates byte-wise; `bpp` (bytes per pixel, rounded up to at least 1) is
//! the stride used when subtracting a "left" or "upper-left" neighbour.

use crate::error::{PngError as Error, Result};

/// PNG filter type byte values. See RFC 2083 §6.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType {
    None = 0,
    Sub = 1,
    Up = 2,
    Average = 3,
    Paeth = 4,
}

impl FilterType {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0 => Self::None,
            1 => Self::Sub,
            2 => Self::Up,
            3 => Self::Average,
            4 => Self::Paeth,
            _ => return Err(Error::invalid(format!("PNG: unknown filter type {b}"))),
        })
    }
}

/// Reverse the filter on one row, writing the reconstructed bytes back into
/// `row`. `prev_row` is the previous row's already-reconstructed bytes (may
/// be an all-zero slice for the first row). `bpp` is the byte-distance to
/// the "left" pixel — at least 1, as specified by RFC 2083.
pub fn unfilter_row(filter: FilterType, row: &mut [u8], prev_row: &[u8], bpp: usize) -> Result<()> {
    if prev_row.len() != row.len() {
        return Err(Error::invalid(
            "PNG unfilter: prev_row length must match row length",
        ));
    }
    let len = row.len();
    // `bpp` is clamped to the row length so the head/body split below is
    // always valid even on degenerate sub-`bpp` rows (interlaced passes
    // can produce rows shorter than `bpp`).
    let bpp = bpp.min(len);
    match filter {
        FilterType::None => {}
        FilterType::Sub => {
            // First `bpp` bytes have no left neighbour (left == 0), so they
            // are unchanged. The body reconstructs each byte from the
            // already-reconstructed byte `bpp` positions earlier. Splitting
            // off the head lets the loop run with no per-iteration
            // `i >= bpp` branch.
            for i in bpp..len {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        FilterType::Up => {
            // Pure vertical add — no left dependency at all. Iterating in
            // lockstep over equal-length slices lets the optimiser drop the
            // bounds checks and vectorise.
            for (r, &p) in row.iter_mut().zip(prev_row.iter()) {
                *r = r.wrapping_add(p);
            }
        }
        FilterType::Average => {
            // Head: left == 0, so avg = up / 2.
            for (r, &up) in row[..bpp].iter_mut().zip(prev_row[..bpp].iter()) {
                *r = r.wrapping_add(up >> 1);
            }
            // Body: `left` is the byte `bpp` back (already reconstructed),
            // `up` is the byte directly above. Splitting `row` at `bpp`
            // gives `done` (the already-reconstructed bytes, where the
            // `left` neighbour lives) and `rest`, paired in lockstep so
            // index `j` of `rest` reads `done[j]` as its left — a single
            // forward-walking window with no `i - bpp` recomputation and
            // bounds the optimiser can hoist. `(left + up) / 2` widened to
            // u16, per RFC 2083 §6.5.
            let up_tail = &prev_row[bpp..];
            let (done, rest) = row.split_at_mut(bpp);
            // First `bpp` lefts come from `done`; subsequent lefts come
            // from `rest` itself `bpp` positions back. Process in chunks
            // so each source slice is a clean borrow.
            average_body(done, rest, up_tail, bpp);
        }
        FilterType::Paeth => {
            // Head: left == up_left == 0, so the predictor reduces to `up`
            // (paeth_predictor(0, b, 0) == b).
            for (r, &up) in row[..bpp].iter_mut().zip(prev_row[..bpp].iter()) {
                *r = r.wrapping_add(up);
            }
            // Body: all three neighbours present; same split-window trick
            // as Average. `up` and `up_left` come from `prev_row`
            // tails zipped in lockstep; `left` walks `done` then `rest`.
            let up_tail = &prev_row[bpp..];
            let up_left_tail = &prev_row[..len - bpp];
            let (done, rest) = row.split_at_mut(bpp);
            paeth_body(done, rest, up_tail, up_left_tail, bpp);
        }
    }
    Ok(())
}

/// Filter one row. `row` holds the raw pixel bytes; output is written to
/// `out` (must be same length as `row`). `prev_row` is the previous row's
/// *raw* bytes (zeros for first row — per the spec).
pub fn filter_row(filter: FilterType, row: &[u8], prev_row: &[u8], bpp: usize, out: &mut [u8]) {
    debug_assert_eq!(row.len(), out.len());
    debug_assert_eq!(row.len(), prev_row.len());
    let len = row.len();
    let bpp = bpp.min(len);
    match filter {
        FilterType::None => {
            out.copy_from_slice(row);
        }
        FilterType::Sub => {
            // Head: no left neighbour, so the output equals the raw byte.
            out[..bpp].copy_from_slice(&row[..bpp]);
            for i in bpp..len {
                out[i] = row[i].wrapping_sub(row[i - bpp]);
            }
        }
        FilterType::Up => {
            for ((o, &r), &p) in out.iter_mut().zip(row.iter()).zip(prev_row.iter()) {
                *o = r.wrapping_sub(p);
            }
        }
        FilterType::Average => {
            // Head: left == 0, so avg = up / 2.
            for ((o, &r), &up) in out[..bpp]
                .iter_mut()
                .zip(row[..bpp].iter())
                .zip(prev_row[..bpp].iter())
            {
                *o = r.wrapping_sub(up >> 1);
            }
            for i in bpp..len {
                let left = row[i - bpp] as u16;
                let up = prev_row[i] as u16;
                out[i] = row[i].wrapping_sub(((left + up) >> 1) as u8);
            }
        }
        FilterType::Paeth => {
            // Head: left == up_left == 0 → predictor == up.
            for ((o, &r), &up) in out[..bpp]
                .iter_mut()
                .zip(row[..bpp].iter())
                .zip(prev_row[..bpp].iter())
            {
                *o = r.wrapping_sub(up);
            }
            for i in bpp..len {
                let left = row[i - bpp] as i16;
                let up = prev_row[i] as i16;
                let up_left = prev_row[i - bpp] as i16;
                let p = paeth_predictor(left, up, up_left) as u8;
                out[i] = row[i].wrapping_sub(p);
            }
        }
    }
}

/// Reconstruct the Average-filter body in place. `done` holds the first
/// `bpp` already-reconstructed bytes; `rest` is the remainder of the row
/// (modified in place); `up_tail` is `prev_row[bpp..]`. For body index
/// `j`, the `left` neighbour is `done[j]` while `j < bpp`, then
/// `rest[j - bpp]` afterwards.
#[inline]
fn average_body(done: &[u8], rest: &mut [u8], up_tail: &[u8], bpp: usize) {
    let n = rest.len();
    // First `bpp` body bytes: left from `done`, no self-dependency.
    let split = bpp.min(n);
    for j in 0..split {
        let left = done[j] as u16;
        let up = up_tail[j] as u16;
        rest[j] = rest[j].wrapping_add(((left + up) >> 1) as u8);
    }
    // Remainder: left is `rest[j - bpp]` (already written this pass).
    for j in bpp..n {
        let left = rest[j - bpp] as u16;
        let up = up_tail[j] as u16;
        rest[j] = rest[j].wrapping_add(((left + up) >> 1) as u8);
    }
}

/// Reconstruct the Paeth-filter body in place. See [`average_body`] for
/// the `done` / `rest` split; `up_tail` is `prev_row[bpp..]` and
/// `up_left_tail` is `prev_row[..len - bpp]` so `up_left_tail[j]` is the
/// upper-left neighbour for body index `j`.
#[inline]
fn paeth_body(done: &[u8], rest: &mut [u8], up_tail: &[u8], up_left_tail: &[u8], bpp: usize) {
    let n = rest.len();
    let split = bpp.min(n);
    for j in 0..split {
        let left = done[j] as i16;
        let up = up_tail[j] as i16;
        let up_left = up_left_tail[j] as i16;
        let p = paeth_predictor(left, up, up_left) as u8;
        rest[j] = rest[j].wrapping_add(p);
    }
    for j in bpp..n {
        let left = rest[j - bpp] as i16;
        let up = up_tail[j] as i16;
        let up_left = up_left_tail[j] as i16;
        let p = paeth_predictor(left, up, up_left) as u8;
        rest[j] = rest[j].wrapping_add(p);
    }
}

fn paeth_predictor(a: i16, b: i16, c: i16) -> i16 {
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Pick a filter for `row` using the sum-of-absolute-deltas heuristic from
/// the PNG specification (§12.8). Tries all five filters and picks the one
/// whose filtered bytes have the lowest absolute sum (treating bytes as
/// signed i8).
pub fn choose_filter_heuristic(
    row: &[u8],
    prev_row: &[u8],
    bpp: usize,
    scratch: &mut [u8],
) -> FilterType {
    let mut best = FilterType::None;
    let mut best_sum = u64::MAX;
    for f in [
        FilterType::None,
        FilterType::Sub,
        FilterType::Up,
        FilterType::Average,
        FilterType::Paeth,
    ] {
        filter_row(f, row, prev_row, bpp, scratch);
        // Sum of absolute signed bytes (§12.8 min-sum-abs heuristic). The
        // per-byte term is at most 128 and a PNG scanline fits in a u32
        // length, so the running total stays far below u64::MAX — no
        // saturating add needed, which keeps the inner loop a plain
        // auto-vectorisable accumulate.
        let mut sum: u64 = 0;
        for &b in scratch.iter() {
            sum += (b as i8).unsigned_abs() as u64;
        }
        if sum < best_sum {
            best_sum = sum;
            best = f;
        }
    }
    best
}

/// Encoder-side filter-selection policy.
///
/// Maps to the recommendations in W3C PNG3 §12.7 ("Filter selection").
/// `Adaptive` is the per-row min-sum-abs-delta heuristic from §12.8 — the
/// spec's "general heuristic which may perform well enough" when an
/// exhaustive trial is unacceptable — and is the default for every
/// encode path.
///
/// `Fixed(f)` pins a single filter for every row in the image (each
/// Adam7 pass when interlaced). §12.7 advises that for an encoder which
/// chooses a fixed filter, the Paeth filter type is most likely to be
/// the best choice on truecolour and grayscale images; `Fixed(Paeth)`
/// is the recommended fixed-filter pick. For colour type 3 (indexed)
/// and bit depths below 8, §12.7 instead recommends filter type 0
/// (`Fixed(None)`); the encoder still emits exactly the filter the
/// caller selected, leaving the §12.7 mapping to the caller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterStrategy {
    /// Per-row §12.8 min-sum-abs-delta heuristic. Tries all five
    /// filter types on every row and keeps the row that minimises the
    /// signed-byte absolute sum. Highest compression at the cost of
    /// five filter evaluations per row.
    #[default]
    Adaptive,
    /// Apply the same filter type to every row of the image. Skips
    /// the per-row heuristic so the filter loop becomes a single
    /// pass (~5× faster than `Adaptive`) at the cost of compression
    /// for content the chosen filter does not suit.
    Fixed(FilterType),
    /// Exhaustive per-image filter search (W3C PNG3 §12.7: "An encoder
    /// could try every combination of filters to find what compresses
    /// best for a given image … if compression efficiency is valued
    /// over speed of compression"). Rather than the intractable
    /// `5^rows` per-row combinatorial search, `Brute` builds the
    /// whole-image filtered byte stream under each of the six candidate
    /// row-strategies — the §12.8 `Adaptive` heuristic plus each of the
    /// five `Fixed` filter types — compresses every candidate, and emits
    /// the one whose DEFLATE output is smallest. The §12.8 heuristic
    /// minimises a *proxy* for compressed size (signed-byte absolute
    /// sum); `Brute` instead measures the real compressed size, so it is
    /// always at least as small as `Adaptive` and never larger than any
    /// `Fixed` choice. It is the slowest strategy (six full-image
    /// deflate passes) and is opt-in only.
    Brute,
}

impl FilterStrategy {
    /// Resolve the strategy to a concrete filter type for one row.
    ///
    /// For `Adaptive` this runs the §12.8 heuristic; for `Fixed(f)`
    /// it returns `f` directly (no scratch evaluation). The caller is
    /// responsible for `filter_row` into the output slot afterwards;
    /// the `scratch` buffer here is only used when the heuristic
    /// branch runs.
    pub fn pick(self, row: &[u8], prev_row: &[u8], bpp: usize, scratch: &mut [u8]) -> FilterType {
        match self {
            // `Brute` is a per-*image* decision, not a per-row one, so it
            // has no single answer at this granularity. The whole-image
            // encoder paths special-case it before reaching here; this
            // arm is the defensive fallback for a direct `pick` caller,
            // and the §12.8 heuristic is the closest single-row analogue.
            FilterStrategy::Adaptive | FilterStrategy::Brute => {
                choose_filter_heuristic(row, prev_row, bpp, scratch)
            }
            FilterStrategy::Fixed(f) => f,
        }
    }

    /// The candidate row-strategies a [`FilterStrategy::Brute`] search
    /// compresses and compares: the §12.8 [`Adaptive`](FilterStrategy::Adaptive)
    /// heuristic followed by each of the five [`Fixed`](FilterStrategy::Fixed)
    /// filter types. A whole-image encoder filters the image once under
    /// each, deflates the result, and keeps the smallest.
    pub const BRUTE_CANDIDATES: [FilterStrategy; 6] = [
        FilterStrategy::Adaptive,
        FilterStrategy::Fixed(FilterType::None),
        FilterStrategy::Fixed(FilterType::Sub),
        FilterStrategy::Fixed(FilterType::Up),
        FilterStrategy::Fixed(FilterType::Average),
        FilterStrategy::Fixed(FilterType::Paeth),
    ];
}

// --- CRC32 ---------------------------------------------------------------

use std::sync::OnceLock;

/// Number of interleaved slices processed per iteration by the fast CRC-32
/// inner loop. Sixteen bytes at a time keeps sixteen independent table
/// lookups in flight so the CPU can overlap their latencies, while the
/// table set (`SLICES * 256` `u32` = 16 KiB) still fits comfortably in L1.
const CRC_SLICES: usize = 16;

/// The slice-by-N table set. `TABLES[0]` is the classic byte-at-a-time
/// CRC-32 table for the reflected `0xEDB88320` polynomial; each subsequent
/// table `TABLES[k]` is derived by advancing every `TABLES[k-1]` entry one
/// more byte position through the same polynomial. Feeding `CRC_SLICES`
/// consecutive input bytes through `TABLES[CRC_SLICES-1 - i]` and XOR-ing
/// the results reproduces exactly the byte-at-a-time recurrence — the
/// output is bit-identical, only the arithmetic is reorganised so more of
/// it runs in parallel.
type CrcTables = [[u32; 256]; CRC_SLICES];

static CRC_TABLES: OnceLock<CrcTables> = OnceLock::new();

fn crc_tables() -> &'static CrcTables {
    CRC_TABLES.get_or_init(|| {
        let mut t: CrcTables = [[0u32; 256]; CRC_SLICES];
        // Base table: the reflected polynomial recurrence, one byte.
        for (n, slot) in t[0].iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                if c & 1 != 0 {
                    c = 0xEDB8_8320 ^ (c >> 1);
                } else {
                    c >>= 1;
                }
            }
            *slot = c;
        }
        // Each higher table advances the previous one by one more byte:
        // TABLES[k][n] = TABLES[0][ TABLES[k-1][n] & 0xFF ] ^ (TABLES[k-1][n] >> 8).
        for k in 1..CRC_SLICES {
            for n in 0..256 {
                let prev = t[k - 1][n];
                t[k][n] = t[0][(prev & 0xFF) as usize] ^ (prev >> 8);
            }
        }
        t
    })
}

/// PNG CRC32 (IEEE 802.3 polynomial, start with 0xFFFFFFFF, invert result).
///
/// Uses the slice-by-[`CRC_SLICES`] algorithm: `CRC_SLICES` input bytes are
/// consumed per iteration through independent tables and combined with XOR,
/// which exposes far more instruction-level parallelism than the
/// byte-at-a-time recurrence while producing bit-identical output. Buffers
/// shorter than one slice block (or the trailing remainder of a longer one)
/// fall back to the classic single-byte loop.
pub fn crc32(bytes: &[u8]) -> u32 {
    let tables = crc_tables();
    let mut c: u32 = 0xFFFF_FFFF;

    let mut chunks = bytes.chunks_exact(CRC_SLICES);
    for block in &mut chunks {
        // Fold the running CRC into the first four input bytes (little-endian,
        // matching the reflected polynomial's bit order) before indexing.
        let a = c ^ u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
        c = tables[CRC_SLICES - 1][(a & 0xFF) as usize]
            ^ tables[CRC_SLICES - 2][((a >> 8) & 0xFF) as usize]
            ^ tables[CRC_SLICES - 3][((a >> 16) & 0xFF) as usize]
            ^ tables[CRC_SLICES - 4][((a >> 24) & 0xFF) as usize];
        // Remaining slice bytes fold in directly through their own tables.
        let mut i = 4;
        while i < CRC_SLICES {
            c ^= tables[CRC_SLICES - 1 - i][block[i] as usize];
            i += 1;
        }
    }

    // Tail: fewer than CRC_SLICES bytes left — classic byte-at-a-time.
    let tbl = &tables[0];
    for &b in chunks.remainder() {
        c = tbl[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// Same but with `once_cell` avoided — pure-loop crc32 for tiny use cases.
pub fn crc32_loop(bytes: &[u8]) -> u32 {
    let mut c: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            let mask = (c & 1).wrapping_neg();
            c = (c >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    c ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_table_matches_loop() {
        let a = crc32(b"IEND");
        let b = crc32_loop(b"IEND");
        assert_eq!(a, b);
        // Known value: CRC32 of "IEND" chunk type (empty payload) — well-known.
        assert_eq!(a, 0xAE42_6082);
    }

    #[test]
    fn crc_slice_matches_bitwise_across_lengths() {
        // The slice-by-N fast path only engages once the buffer reaches a
        // full CRC_SLICES block, and the tail loop handles the remainder;
        // sweep lengths across several block boundaries (0..=200) so both
        // the block loop and every remainder size are exercised against the
        // bit-serial reference `crc32_loop`.
        let mut state = 0x1234_5678u32;
        let mut buf = [0u8; 200];
        for b in buf.iter_mut() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *b = (state & 0xff) as u8;
        }
        for len in 0..=buf.len() {
            let fast = crc32(&buf[..len]);
            let bitwise = crc32_loop(&buf[..len]);
            assert_eq!(fast, bitwise, "CRC mismatch at length {len}");
        }
    }

    #[test]
    fn crc_slice_matches_bitwise_all_ones_and_zeros() {
        // Degenerate table-index distributions (all 0x00, all 0xFF) can
        // surface an off-by-one in the per-slice table selection that a
        // pseudo-random fill would mask. Check both across a full block
        // plus a partial one.
        for &byte in &[0x00u8, 0xFF] {
            for len in [15usize, 16, 17, 31, 32, 33, 48] {
                let buf = vec![byte; len];
                assert_eq!(
                    crc32(&buf),
                    crc32_loop(&buf),
                    "CRC mismatch: byte {byte:#04x} len {len}"
                );
            }
        }
    }

    #[test]
    fn strategy_default_is_adaptive() {
        assert_eq!(FilterStrategy::default(), FilterStrategy::Adaptive);
    }

    #[test]
    fn strategy_fixed_pick_returns_chosen_filter() {
        // `Fixed(_)` skips the heuristic; the returned type is the
        // exact filter the caller pinned, regardless of row content.
        let row = [10u8, 20, 30, 40];
        let prev = [5u8; 4];
        let mut scratch = [0u8; 4];
        for f in [
            FilterType::None,
            FilterType::Sub,
            FilterType::Up,
            FilterType::Average,
            FilterType::Paeth,
        ] {
            let got = FilterStrategy::Fixed(f).pick(&row, &prev, 1, &mut scratch);
            assert_eq!(got, f);
        }
    }

    #[test]
    fn strategy_adaptive_matches_heuristic() {
        // `Adaptive` is a thin wrapper around `choose_filter_heuristic`
        // — same input, same pick.
        let row = [10u8, 11, 12, 13, 14, 15, 16, 17];
        let prev = [5u8; 8];
        let mut s1 = [0u8; 8];
        let mut s2 = [0u8; 8];
        let a = FilterStrategy::Adaptive.pick(&row, &prev, 1, &mut s1);
        let b = choose_filter_heuristic(&row, &prev, 1, &mut s2);
        assert_eq!(a, b);
    }

    #[test]
    fn filter_roundtrip_all_types() {
        let row = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let prev = [5u8; 8];
        let bpp = 1;
        for f in [
            FilterType::None,
            FilterType::Sub,
            FilterType::Up,
            FilterType::Average,
            FilterType::Paeth,
        ] {
            let mut filtered = [0u8; 8];
            filter_row(f, &row, &prev, bpp, &mut filtered);
            let mut back = filtered;
            unfilter_row(f, &mut back, &prev, bpp).unwrap();
            assert_eq!(back, row, "filter {f:?} roundtrip mismatch");
        }
    }
}
