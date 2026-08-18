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
    // PNG's only legal byte-distances are 1..=8 (§7.2: at most 4
    // channels × 2 bytes); those take the register-lane kernels below.
    // Any other `bpp` (reachable through the public API only) falls
    // back to the index-walking reference loops.
    match filter {
        FilterType::None => {}
        FilterType::Sub => match bpp {
            1 => unfilter_sub_lanes::<1>(row),
            2 => unfilter_sub_lanes::<2>(row),
            3 => unfilter_sub_lanes::<3>(row),
            4 => unfilter_sub_lanes::<4>(row),
            5 => unfilter_sub_lanes::<5>(row),
            6 => unfilter_sub_lanes::<6>(row),
            7 => unfilter_sub_lanes::<7>(row),
            8 => unfilter_sub_lanes::<8>(row),
            _ => {
                let bpp = bpp.min(row.len());
                for i in bpp..row.len() {
                    row[i] = row[i].wrapping_add(row[i - bpp]);
                }
            }
        },
        FilterType::Up => {
            // Pure vertical add — no left dependency at all. Iterating in
            // lockstep over equal-length slices lets the optimiser drop the
            // bounds checks and vectorise.
            for (r, &p) in row.iter_mut().zip(prev_row.iter()) {
                *r = r.wrapping_add(p);
            }
        }
        FilterType::Average => match bpp {
            1 => unfilter_avg_lanes::<1>(row, prev_row),
            2 => unfilter_avg_lanes::<2>(row, prev_row),
            3 => unfilter_avg_lanes::<3>(row, prev_row),
            4 => unfilter_avg_lanes::<4>(row, prev_row),
            5 => unfilter_avg_lanes::<5>(row, prev_row),
            6 => unfilter_avg_lanes::<6>(row, prev_row),
            7 => unfilter_avg_lanes::<7>(row, prev_row),
            8 => unfilter_avg_lanes::<8>(row, prev_row),
            _ => {
                let len = row.len();
                let bpp = bpp.min(len);
                for (r, &up) in row[..bpp].iter_mut().zip(prev_row[..bpp].iter()) {
                    *r = r.wrapping_add(up >> 1);
                }
                let up_tail = &prev_row[bpp..];
                let (done, rest) = row.split_at_mut(bpp);
                average_body(done, rest, up_tail, bpp);
            }
        },
        FilterType::Paeth => match bpp {
            1 => unfilter_paeth_lanes::<1>(row, prev_row),
            2 => unfilter_paeth_lanes::<2>(row, prev_row),
            3 => unfilter_paeth_lanes::<3>(row, prev_row),
            4 => unfilter_paeth_lanes::<4>(row, prev_row),
            5 => unfilter_paeth_lanes::<5>(row, prev_row),
            6 => unfilter_paeth_lanes::<6>(row, prev_row),
            7 => unfilter_paeth_lanes::<7>(row, prev_row),
            8 => unfilter_paeth_lanes::<8>(row, prev_row),
            _ => {
                let len = row.len();
                let bpp = bpp.min(len);
                for (r, &up) in row[..bpp].iter_mut().zip(prev_row[..bpp].iter()) {
                    *r = r.wrapping_add(up);
                }
                let up_tail = &prev_row[bpp..];
                let up_left_tail = &prev_row[..len - bpp];
                let (done, rest) = row.split_at_mut(bpp);
                paeth_body(done, rest, up_tail, up_left_tail, bpp);
            }
        },
    }
    Ok(())
}

/// Sub reconstruction with the previous pixel carried in an `N`-byte
/// register array instead of re-read from the row buffer. The serial
/// recurrence `Recon(x) = Filt(x) + Recon(x - bpp)` (RFC 2083 §6.3)
/// makes the loop a dependency chain per byte-lane; re-reading
/// `row[i - bpp]` right after writing it stalls on store-to-load
/// forwarding, while a register-carried lane array keeps the chain in
/// registers. The first (zero-initialised) iteration reproduces the
/// "no left neighbour → left == 0" head rule; a trailing sub-`N`
/// remainder (possible only on degenerate rows shorter than `bpp`)
/// keeps the same lane discipline.
#[inline]
fn unfilter_sub_lanes<const N: usize>(row: &mut [u8]) {
    let mut left = [0u8; N];
    let mut chunks = row.chunks_exact_mut(N);
    for px in &mut chunks {
        for k in 0..N {
            let v = px[k].wrapping_add(left[k]);
            px[k] = v;
            left[k] = v;
        }
    }
    for (k, b) in chunks.into_remainder().iter_mut().enumerate() {
        *b = b.wrapping_add(left[k]);
    }
}

/// Average reconstruction with register-carried left lanes (see
/// [`unfilter_sub_lanes`]). `(left + up) / 2` widened to u16 per
/// RFC 2083 §6.5; the zero-initialised first iteration reproduces the
/// head rule `avg = up / 2`.
#[inline]
fn unfilter_avg_lanes<const N: usize>(row: &mut [u8], prev_row: &[u8]) {
    let mut left = [0u8; N];
    let mut rc = row.chunks_exact_mut(N);
    let mut pc = prev_row.chunks_exact(N);
    for (px, up) in (&mut rc).zip(&mut pc) {
        for k in 0..N {
            let v = px[k].wrapping_add(((left[k] as u16 + up[k] as u16) >> 1) as u8);
            px[k] = v;
            left[k] = v;
        }
    }
    let rem = rc.into_remainder();
    let prem = pc.remainder();
    for k in 0..rem.len() {
        rem[k] = rem[k].wrapping_add(((left[k] as u16 + prem[k] as u16) >> 1) as u8);
    }
}

/// Paeth reconstruction with register-carried left / upper-left lanes
/// (see [`unfilter_sub_lanes`]). The zero-initialised first iteration
/// reproduces the head rule `paeth_predictor(0, up, 0) == up`.
#[inline]
fn unfilter_paeth_lanes<const N: usize>(row: &mut [u8], prev_row: &[u8]) {
    let mut left = [0u8; N];
    let mut up_left = [0u8; N];
    let mut rc = row.chunks_exact_mut(N);
    let mut pc = prev_row.chunks_exact(N);
    for (px, up) in (&mut rc).zip(&mut pc) {
        for k in 0..N {
            let b = up[k];
            let p = paeth_predictor(left[k] as i16, b as i16, up_left[k] as i16) as u8;
            let v = px[k].wrapping_add(p);
            px[k] = v;
            left[k] = v;
            up_left[k] = b;
        }
    }
    let rem = rc.into_remainder();
    let prem = pc.remainder();
    for k in 0..rem.len() {
        let p = paeth_predictor(left[k] as i16, prem[k] as i16, up_left[k] as i16) as u8;
        rem[k] = rem[k].wrapping_add(p);
    }
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
            // Body: unlike reconstruction, the filter direction has no
            // serial dependency — every output reads only the immutable
            // `row` — so lockstep zips over the shifted slices expose a
            // bounds-check-free, auto-vectorisable loop.
            for ((o, &r), &left) in out[bpp..]
                .iter_mut()
                .zip(row[bpp..].iter())
                .zip(row[..len - bpp].iter())
            {
                *o = r.wrapping_sub(left);
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
            // Body: same shifted-slice lockstep as Sub; `(left + up) / 2`
            // widened to u16 per RFC 2083 §6.5.
            for (((o, &r), &left), &up) in out[bpp..]
                .iter_mut()
                .zip(row[bpp..].iter())
                .zip(row[..len - bpp].iter())
                .zip(prev_row[bpp..].iter())
            {
                *o = r.wrapping_sub(((left as u16 + up as u16) >> 1) as u8);
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
            // Body: four shifted source slices in lockstep — all reads
            // from immutable inputs, no index arithmetic in the loop.
            for ((((o, &r), &left), &up), &up_left) in out[bpp..]
                .iter_mut()
                .zip(row[bpp..].iter())
                .zip(row[..len - bpp].iter())
                .zip(prev_row[bpp..].iter())
                .zip(prev_row[..len - bpp].iter())
            {
                let p = paeth_predictor(left as i16, up as i16, up_left as i16) as u8;
                *o = r.wrapping_sub(p);
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

#[inline(always)]
fn paeth_predictor(a: i16, b: i16, c: i16) -> i16 {
    // RFC 2083 §6.6 defines p = a + b - c and the distances
    // pa = |p - a|, pb = |p - b|, pc = |p - c|. Substituting p gives
    // the algebraically identical pa = |b - c|, pb = |a - c|,
    // pc = |a + b - 2c| — three independent subtract/abs chains with
    // no shared `p` intermediate, which shortens the dependency chain
    // in the serial reconstruction loop. Selection order (a, then b,
    // then c on ties) is exactly the §6.6 rule.
    let pa = (b - c).abs();
    let pb = (a - c).abs();
    let pc = (a + b - 2 * c).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Pick a filter for `row` using the sum-of-absolute-deltas heuristic from
/// the PNG specification (§12.8). Evaluates all five filters and picks the
/// one whose filtered bytes have the lowest absolute sum (treating bytes
/// as signed i8).
///
/// The five candidate sums are computed read-only, directly from `row` /
/// `prev_row` — the filtered bytes are never materialised, so the five
/// full-row scratch writes (and their re-reads) the trial used to cost
/// are gone. `scratch` is kept in the signature for call-site
/// compatibility but is no longer written; the winning filter's bytes are
/// produced by the caller's own [`filter_row`] pass.
pub fn choose_filter_heuristic(
    row: &[u8],
    prev_row: &[u8],
    bpp: usize,
    _scratch: &mut [u8],
) -> FilterType {
    let len = row.len();
    let bpp = bpp.min(len);

    // §12.8 min-sum-abs term: the filtered byte interpreted as signed
    // i8, absolute value. Each sum below reproduces exactly the bytes
    // `filter_row` would emit for that filter type (same wrapping
    // arithmetic, same head/body split) and folds them straight into
    // the accumulator. Per-byte terms are ≤ 128 and a PNG scanline
    // fits a u32 length, so u64 never saturates.
    #[inline(always)]
    fn abs_i8(b: u8) -> u64 {
        (b as i8).unsigned_abs() as u64
    }

    // None: the raw bytes.
    let sum_none: u64 = row.iter().map(|&b| abs_i8(b)).sum();

    // Sub: head = raw bytes, body = row[i] - row[i - bpp].
    let head_raw: u64 = row[..bpp].iter().map(|&b| abs_i8(b)).sum();
    let sum_sub: u64 = head_raw
        + row[bpp..]
            .iter()
            .zip(row[..len - bpp].iter())
            .map(|(&r, &left)| abs_i8(r.wrapping_sub(left)))
            .sum::<u64>();

    // Up: row[i] - prev_row[i] across the whole row.
    let sum_up: u64 = row
        .iter()
        .zip(prev_row.iter())
        .map(|(&r, &p)| abs_i8(r.wrapping_sub(p)))
        .sum();

    // Average: head = row[i] - (up >> 1), body widened to u16 (§6.5).
    let sum_avg: u64 = row[..bpp]
        .iter()
        .zip(prev_row[..bpp].iter())
        .map(|(&r, &up)| abs_i8(r.wrapping_sub(up >> 1)))
        .sum::<u64>()
        + row[bpp..]
            .iter()
            .zip(row[..len - bpp].iter())
            .zip(prev_row[bpp..].iter())
            .map(|((&r, &left), &up)| {
                abs_i8(r.wrapping_sub(((left as u16 + up as u16) >> 1) as u8))
            })
            .sum::<u64>();

    // Paeth: head = row[i] - up (predictor(0, b, 0) == b), body = §6.6.
    let sum_paeth: u64 = row[..bpp]
        .iter()
        .zip(prev_row[..bpp].iter())
        .map(|(&r, &up)| abs_i8(r.wrapping_sub(up)))
        .sum::<u64>()
        + row[bpp..]
            .iter()
            .zip(row[..len - bpp].iter())
            .zip(prev_row[bpp..].iter())
            .zip(prev_row[..len - bpp].iter())
            .map(|(((&r, &left), &up), &up_left)| {
                let p = paeth_predictor(left as i16, up as i16, up_left as i16) as u8;
                abs_i8(r.wrapping_sub(p))
            })
            .sum::<u64>();

    // Same candidate order and strict-less-than tie-breaking as the
    // materialising trial loop: on equal sums the earlier filter type
    // (None < Sub < Up < Average < Paeth) wins.
    let mut best = FilterType::None;
    let mut best_sum = sum_none;
    for (f, sum) in [
        (FilterType::Sub, sum_sub),
        (FilterType::Up, sum_up),
        (FilterType::Average, sum_avg),
        (FilterType::Paeth, sum_paeth),
    ] {
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
    crc32_update(CRC32_INIT, bytes) ^ 0xFFFF_FFFF
}

/// The pre-conditioning value a CRC-32 register starts from (all-ones),
/// before any input has been folded in. Pair with [`crc32_update`] and a
/// final `^ 0xFFFF_FFFF` to reproduce [`crc32`] incrementally — useful when
/// the CRC input is spread across two non-contiguous slices (chunk type then
/// chunk data) and materialising a concatenated buffer would waste an
/// allocation.
pub const CRC32_INIT: u32 = 0xFFFF_FFFF;

/// Fold `bytes` into a running (un-finalised) CRC-32 register and return the
/// updated register. Start from [`CRC32_INIT`], call this once per input
/// slice, then XOR the result with `0xFFFF_FFFF` to obtain the final CRC.
/// `crc32(a ++ b)` equals
/// `crc32_update(crc32_update(CRC32_INIT, a), b) ^ 0xFFFF_FFFF`.
///
/// Uses the slice-by-[`CRC_SLICES`] algorithm (see [`crc32`]); the register
/// is *not* inverted here so the state can be threaded across calls.
pub fn crc32_update(mut c: u32, bytes: &[u8]) -> u32 {
    let tables = crc_tables();

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
    c
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
    fn crc_incremental_matches_contiguous() {
        // The two-slice incremental path (chunk type then chunk data) must
        // reproduce crc32 over the concatenation exactly — this is the
        // property write_chunk relies on to skip the concat allocation.
        let mut state = 0x0bad_f00du32;
        let mut buf = [0u8; 300];
        for b in buf.iter_mut() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *b = (state & 0xff) as u8;
        }
        // Sweep the split point across block boundaries so both slices
        // exercise full-block and remainder cases.
        for split in [0usize, 1, 4, 15, 16, 17, 31, 100, 299, 300] {
            let (a, b) = buf.split_at(split);
            let incremental = crc32_update(crc32_update(CRC32_INIT, a), b) ^ 0xFFFF_FFFF;
            let contiguous = crc32(&buf);
            assert_eq!(incremental, contiguous, "split {split}");
        }
        // The canonical PNG use: 4-byte type + data.
        let ty = *b"IDAT";
        let data = &buf[..64];
        let mut concat = Vec::new();
        concat.extend_from_slice(&ty);
        concat.extend_from_slice(data);
        let incremental = crc32_update(crc32_update(CRC32_INIT, &ty), data) ^ 0xFFFF_FFFF;
        assert_eq!(incremental, crc32(&concat));
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

    /// Reference reconstruction: the RFC 2083 §6 recurrences written as
    /// the plainest possible index loops (the shape the lane kernels
    /// replaced). Used to prove the optimised `unfilter_row` bit-exact.
    fn unfilter_reference(filter: FilterType, row: &mut [u8], prev: &[u8], bpp: usize) {
        let len = row.len();
        for i in 0..len {
            let left = if i >= bpp { row[i - bpp] } else { 0 };
            let up = prev[i];
            let up_left = if i >= bpp { prev[i - bpp] } else { 0 };
            row[i] = match filter {
                FilterType::None => row[i],
                FilterType::Sub => row[i].wrapping_add(left),
                FilterType::Up => row[i].wrapping_add(up),
                FilterType::Average => row[i].wrapping_add(((left as u16 + up as u16) >> 1) as u8),
                FilterType::Paeth => {
                    // Literal §6.6 formulation with the shared p.
                    let (a, b, c) = (left as i16, up as i16, up_left as i16);
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    let pred = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    };
                    row[i].wrapping_add(pred as u8)
                }
            };
        }
    }

    #[test]
    fn unfilter_lane_kernels_match_reference() {
        // Sweep every filter type × bpp 1..=8 (plus an out-of-range 9
        // hitting the fallback arm) × row lengths crossing the lane
        // boundaries — including lengths shorter than bpp and lengths
        // that are not a multiple of bpp — on pseudo-random content.
        let mut state = 0x9e37_79b9u32;
        let mut fill = |buf: &mut [u8]| {
            for b in buf.iter_mut() {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *b = (state & 0xff) as u8;
            }
        };
        for f in [
            FilterType::None,
            FilterType::Sub,
            FilterType::Up,
            FilterType::Average,
            FilterType::Paeth,
        ] {
            for bpp in 1..=9usize {
                for len in [0usize, 1, 2, 3, 5, 7, 8, 9, 15, 16, 17, 33, 64, 100] {
                    let mut row = vec![0u8; len];
                    let mut prev = vec![0u8; len];
                    fill(&mut row);
                    fill(&mut prev);
                    let mut fast = row.clone();
                    unfilter_row(f, &mut fast, &prev, bpp).unwrap();
                    let mut reference = row;
                    unfilter_reference(f, &mut reference, &prev, bpp);
                    assert_eq!(fast, reference, "filter {f:?} bpp {bpp} len {len}");
                }
            }
        }
    }

    #[test]
    fn heuristic_matches_materialised_sums() {
        // The read-only sum evaluation must pick exactly the filter the
        // materialise-then-sum trial would (same §12.8 metric, same
        // first-wins tie-breaking on the None,Sub,Up,Average,Paeth order).
        let mut state = 0x243f_6a88u32;
        let mut fill = |buf: &mut [u8]| {
            for b in buf.iter_mut() {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *b = (state & 0xff) as u8;
            }
        };
        for bpp in 1..=8usize {
            for len in [1usize, 2, 4, 7, 8, 9, 31, 48, 96] {
                let mut row = vec![0u8; len];
                let mut prev = vec![0u8; len];
                fill(&mut row);
                fill(&mut prev);
                // Flat / gradient variants too — ties are where the
                // ordering rule matters.
                for variant in 0..3 {
                    let (row, prev) = match variant {
                        0 => (row.clone(), prev.clone()),
                        1 => (vec![0u8; len], vec![0u8; len]),
                        _ => (
                            (0..len).map(|i| (i * 3) as u8).collect(),
                            (0..len).map(|i| (i * 3) as u8).collect(),
                        ),
                    };
                    let mut scratch = vec![0u8; len];
                    let fast = choose_filter_heuristic(&row, &prev, bpp, &mut scratch);
                    // Materialising reference trial.
                    let mut best = FilterType::None;
                    let mut best_sum = u64::MAX;
                    for f in [
                        FilterType::None,
                        FilterType::Sub,
                        FilterType::Up,
                        FilterType::Average,
                        FilterType::Paeth,
                    ] {
                        filter_row(f, &row, &prev, bpp, &mut scratch);
                        let sum: u64 = scratch
                            .iter()
                            .map(|&b| (b as i8).unsigned_abs() as u64)
                            .sum();
                        if sum < best_sum {
                            best_sum = sum;
                            best = f;
                        }
                    }
                    assert_eq!(fast, best, "bpp {bpp} len {len} variant {variant}");
                }
            }
        }
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
