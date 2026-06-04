#![no_main]

//! Drive the per-row filter codepath (`filter_row` + `unfilter_row`)
//! directly with fuzz-derived `(FilterType, bpp, row_size, prev_row,
//! row)` tuples. Bypasses the chunk-CRC / IDAT-inflate / IHDR-shape
//! gates so the fuzzer's mutation budget lands inside the filter
//! reconstruction arithmetic instead of being absorbed by upstream
//! framing rejections.
//!
//! Coverage:
//!   * All five filter types (RFC 2083 §6.2..§6.6 — None / Sub / Up /
//!     Average / Paeth) reached uniformly via a 0..=4 modulo on the
//!     fuzz-derived selector byte.
//!   * Every supported `bpp` value (1..=8 — 1 byte for sub-byte / Gray8
//!     / Pal8, 2 for Gray16Le / Ya8, 3 for Rgb24, 4 for Ya16Le / Rgba,
//!     6 for Rgb48Le, 8 for Rgba64Le). `bpp` is the byte-distance to
//!     the "left" pixel per §6.1; the bound matches the widest layout
//!     `Ihdr::bpp_for_filter` emits.
//!   * Row sizes from 0 (degenerate empty row — the spec doesn't
//!     forbid it; `unfilter_row` must not divide-by-zero or panic) up
//!     to a 2 KB cap (enough to cover the wrap-around arithmetic at
//!     `row[i - bpp]` boundaries without slowing the fuzzer down).
//!   * Arbitrary prior-row bytes (the §6.2..§6.6 reconstruction
//!     formulae take `prev_row[i]` as their "up" neighbour, so prior-
//!     row entropy widens the search space the same way the row
//!     entropy does).
//!
//! Asserts two properties:
//!
//! 1. **Liveness.** `unfilter_row` returns `Err` only when `prev_row.len
//!    != row.len()` (the only documented `Err` path) and otherwise
//!    returns `Ok(())` without panic / abort / wrapping-arithmetic
//!    overflow in debug builds. The fuzz harness only constructs
//!    same-length pairs, so every call must succeed.
//!
//! 2. **Round-trip identity.** For any `(filter_type, bpp, row,
//!    prev_row)` with `bpp >= 1`, `filter_row(filter_row(row)) == row`
//!    after applying `filter_row` followed by `unfilter_row` with the
//!    same parameters. This is the §6.1 invariant ("the filter is
//!    fully reversible") expressed as a fuzzable predicate.
//!
//! The §6 round-trip property is the reason a PNG encoder can pick any
//! filter per row without losing pixel data; a fuzzer-derived
//! counterexample to it would point at a bug in either the filter or
//! the unfilter path. Five hand-written cases in `filter.rs`'s unit
//! tests confirm the trivial case; this harness widens the search to
//! every realistic shape an in-the-wild PNG might present.

use libfuzzer_sys::fuzz_target;
use oxideav_png::filter::{filter_row, unfilter_row, FilterType};

/// `Ihdr::bpp_for_filter` returns at most 8 (16-bit RGBA = 2 * 4).
/// Match that cap so the harness mirrors the decoder's actual reach.
const MAX_BPP: usize = 8;

/// Two-KB row cap. Larger rows quadratically slow the fuzzer without
/// adding new branch coverage — the filter loops are byte-at-a-time
/// and the only state that grows with row length is the
/// `row[i - bpp]` lookback, which saturates well below 2 KB.
const MAX_ROW_LEN: usize = 2048;

fuzz_target!(|data: &[u8]| {
    // Need at least filter-selector + bpp + row-length-low + row-length-
    // high = 4 header bytes before the row + prev_row payload split.
    if data.len() < 4 {
        return;
    }

    let filter = match data[0] % 5 {
        0 => FilterType::None,
        1 => FilterType::Sub,
        2 => FilterType::Up,
        3 => FilterType::Average,
        4 => FilterType::Paeth,
        _ => unreachable!(),
    };

    // bpp ∈ 1..=8 (the `% MAX_BPP` would yield 0..=7; add 1 so the
    // minimum is 1 — `bpp = 0` is never legal per §6.1 "Filtering is
    // done on the byte values, not the pixel values").
    let bpp = ((data[1] as usize) % MAX_BPP) + 1;

    // 16-bit little-endian row length, masked to the cap so a wild
    // fuzz value can't request a 65 535-byte row.
    let row_len = (((data[2] as usize) | ((data[3] as usize) << 8)) % (MAX_ROW_LEN + 1)).min(
        // Leave enough bytes after the header for at least one
        // (row, prev_row) pair; otherwise the slice split below
        // returns an empty payload and the harness just exits.
        data.len().saturating_sub(4) / 2,
    );

    let body = &data[4..];
    // Split `body` into row + prev_row (each `row_len` bytes). If the
    // payload is short, the unused tail is dropped — the fuzzer doesn't
    // need a contiguous buffer to find a counterexample.
    if body.len() < row_len * 2 {
        return;
    }
    let row_src = &body[..row_len];
    let prev_row = &body[row_len..row_len * 2];

    // Working buffer the unfilter writes into.
    let mut row_buf: Vec<u8> = row_src.to_vec();

    // The single documented `Err` path is a prev_row / row length
    // mismatch. The harness constructs equal-length slices, so this
    // call must always succeed.
    let _ = unfilter_row(filter, &mut row_buf, prev_row, bpp);
    // Drop the result — the fuzzer is only checking the call doesn't
    // panic / overflow / OOM. A successful return is the expected
    // outcome; an `Err` would be the documented prev_row-mismatch path
    // (impossible by construction here) but still a valid `Result` so
    // not a fuzz failure.

    // Round-trip property. Pick a *fresh* row and check
    // `filter_row` ∘ `unfilter_row` is the identity for the chosen
    // (filter_type, bpp, prev_row) triple. This isolates the §6.1
    // reversibility guarantee from the prior `unfilter` call's
    // success or failure.
    let mut filtered = vec![0u8; row_len];
    filter_row(filter, row_src, prev_row, bpp, &mut filtered);
    let mut reconstructed = filtered.clone();
    let unfilter_result = unfilter_row(filter, &mut reconstructed, prev_row, bpp);
    assert!(
        unfilter_result.is_ok(),
        "unfilter_row({filter:?}, bpp={bpp}, row_len={row_len}) returned Err on equal-length slices"
    );
    assert_eq!(
        reconstructed, row_src,
        "filter→unfilter round-trip mismatch: filter={filter:?} bpp={bpp} row_len={row_len}"
    );
});
