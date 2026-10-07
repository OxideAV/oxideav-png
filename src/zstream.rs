//! Streaming — and optionally multi-threaded — zlib framing for the
//! IDAT / fdAT pixel stream.
//!
//! The encoder used to materialise the whole filtered image (one byte
//! per row plus every filtered row, `(1 + row_bytes) × height`) and
//! hand it to a one-shot compressor. For a 12 MP RGB24 frame that is a
//! 36 MB intermediate on top of the caller's frame, and the DEFLATE
//! pass behind it is ≈ 95 % of the encode time. This module replaces
//! both halves:
//!
//! * **Rows stream straight into the compressor.** A
//!   [`SegmentSource`] hands uncompressed bytes to a sink one row at a
//!   time; nothing larger than a row ever sits between the caller's
//!   pixels and compcol's encoder.
//! * **The stream is cut into independent segments** so a thread
//!   budget above one can deflate them in parallel. Each segment is a
//!   complete run of RFC 1951 blocks that ends with a sync flush, so
//!   the segments concatenate into one deflate stream with no
//!   bit-level surgery: a sync flush closes the open block with
//!   `BFINAL = 0` and appends an empty stored block, which leaves the
//!   bitstream on a byte boundary (RFC 1951 §3.2.4 "Any bits of input
//!   up to the next byte boundary are ignored"). The next segment's
//!   first block header then starts exactly where a decoder expects
//!   it. Each segment starts with an empty LZ77 history, so no
//!   back-reference crosses a boundary (the decoder's window still
//!   spans it, which is harmless).
//!
//! The zlib (RFC 1950) container around the concatenation is written
//! here: the 2-byte `CMF` / `FLG` header (§2.2 — `CM = 8`, `CINFO = 7`
//! for the 32 KiB window, `FDICT = 0`, `FLEVEL` from the level, and
//! `FCHECK` so `CMF × 256 + FLG` is a multiple of 31), the stream
//! terminator — one empty stored block with `BFINAL = 1`, which packs
//! as `01 00 00 FF FF` (RFC 1951 §3.1.1 LSB-first header bits, §3.2.4
//! `LEN` / `NLEN`) — and the big-endian Adler-32 trailer over every
//! uncompressed byte (§2.2 / §8.2: `s1` = 1 + Σ bytes, `s2` = Σ `s1`,
//! both mod 65521, stored `s2 × 65536 + s1`). Each worker accumulates
//! its own segment's Adler-32 from the (1, 0) start state and the
//! assembler folds them: a segment of `n` bytes appended after a prefix
//! with sums `(s1_p, s2_p)` contributes `s1 = s1_p + s1_seg − 1` and
//! `s2 = s2_p + s2_seg + n × (s1_p − 1)`, which is the definition
//! evaluated with the prefix's running `s1` added to every per-byte
//! term of the segment.
//!
//! DEFLATE itself is compcol's: this module only drives its public
//! streaming [`Encoder`](compcol::Encoder) contract (`encode` /
//! `flush(Sync)` / `finish`) and frames the result.
//!
//! The segment grid is a function of the image alone
//! ([`SEGMENT_TARGET_BYTES`] of filtered bytes per segment, rounded to
//! whole rows by the caller), never of the thread budget, so the emitted
//! bytes are identical whether one thread or sixteen produced them. A
//! stream that fits in a single segment skips the custom framing
//! entirely and goes through compcol's zlib encoder with a normal
//! `finish`, so small images keep their historical byte layout.

use crate::error::{PngError as Error, Result};
use compcol::{Flush, Status};

/// Uncompressed bytes per segment the splitter aims for. Large enough
/// that the empty-history start of each segment costs well under a
/// percent of ratio on photographic rows, small enough that a 12 MP
/// frame yields dozens of work units for a many-core budget.
pub(crate) const SEGMENT_TARGET_BYTES: usize = 1 << 20;

/// Rows per segment for a stream whose rows are `wire_row_len` bytes
/// (filter byte included): the smallest whole-row count that reaches
/// [`SEGMENT_TARGET_BYTES`], never less than one row.
pub(crate) fn rows_per_segment(wire_row_len: usize) -> usize {
    SEGMENT_TARGET_BYTES.div_ceil(wire_row_len.max(1)).max(1)
}

/// A producer of the uncompressed stream, addressable by segment so the
/// segments can be compressed independently (and concurrently — the
/// source is shared read-only across worker threads).
pub(crate) trait SegmentSource: Sync {
    /// Number of segments the stream is cut into (≥ 1).
    fn segment_count(&self) -> usize;
    /// Feed every uncompressed byte of segment `index`, in order, to
    /// `sink`. Called exactly once per segment.
    fn feed(&self, index: usize, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()>;
}

/// An already-materialised byte stream cut into `segment_len`-byte
/// pieces — used by the Adam7 / sub-byte paths that still build their
/// pass-concatenated stream up front.
pub(crate) struct BytesSource<'a> {
    pub bytes: &'a [u8],
    pub segment_len: usize,
}

impl SegmentSource for BytesSource<'_> {
    fn segment_count(&self) -> usize {
        self.bytes.len().div_ceil(self.segment_len.max(1)).max(1)
    }
    fn feed(&self, index: usize, sink: &mut dyn FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let seg = self.segment_len.max(1);
        let start = (index * seg).min(self.bytes.len());
        let end = ((index + 1) * seg).min(self.bytes.len());
        sink(&self.bytes[start..end])
    }
}

/// Running Adler-32 state (RFC 1950 §8.2): `s1` starts at 1, `s2` at 0.
#[derive(Clone, Copy)]
struct Adler32 {
    s1: u32,
    s2: u32,
    len: u64,
}

/// Bytes folded into the two sums between modular reductions. With
/// `s1, s2 < 65521` after a reduction, `n` more bytes push `s1` to at
/// most `65520 + 255 n` and `s2` to at most `65520 + n (65520 + 255 n)`,
/// which stays below `2^32` for `n ≤ 3800`.
const ADLER_CHUNK: usize = 3800;

impl Adler32 {
    const MODULUS: u32 = 65521;
    fn new() -> Self {
        Self {
            s1: 1,
            s2: 0,
            len: 0,
        }
    }
    fn update(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(ADLER_CHUNK) {
            let mut s1 = self.s1;
            let mut s2 = self.s2;
            for &b in chunk {
                s1 += b as u32;
                s2 += s1;
            }
            self.s1 = s1 % Self::MODULUS;
            self.s2 = s2 % Self::MODULUS;
        }
        self.len += bytes.len() as u64;
    }
    /// Append a checksum computed over the `other.len` bytes that
    /// follow this state's bytes, from the standard (1, 0) start.
    fn append(&mut self, other: &Adler32) {
        let m = Self::MODULUS as u64;
        let s1_p = self.s1 as u64;
        // (s1_p − 1) is the running `s1` the segment's bytes would have
        // seen added to each of their per-byte terms.
        let carry = (s1_p + m - 1) % m;
        let s1 = (s1_p + other.s1 as u64 + m - 1) % m;
        let s2 = (self.s2 as u64 + other.s2 as u64 + (other.len % m) * carry) % m;
        self.s1 = s1 as u32;
        self.s2 = s2 as u32;
        self.len += other.len;
    }
    fn value(&self) -> u32 {
        (self.s2 << 16) | self.s1
    }
}

/// Size of the scratch buffer each worker drains compcol's encoder
/// through.
const OUT_CHUNK: usize = 64 * 1024;

fn map_err(e: compcol::Error) -> Error {
    Error::invalid(format!("PNG: zlib compression failed: {e:?}"))
}

/// Push `input` through `enc`, handing compressed bytes to `out` as
/// they leave the encoder's `buf`.
fn drive_encode(
    enc: &mut impl compcol::Encoder,
    mut input: &[u8],
    buf: &mut [u8],
    out: &mut dyn FnMut(&[u8]),
) -> Result<()> {
    while !input.is_empty() {
        let (p, status) = enc.encode(input, buf).map_err(map_err)?;
        out(&buf[..p.written]);
        input = &input[p.consumed..];
        match status {
            Status::OutputFull => continue,
            Status::InputEmpty if input.is_empty() => {}
            // The contract says `InputEmpty` means every input byte was
            // consumed; anything else would spin forever, so fail.
            Status::InputEmpty | Status::StreamEnd => {
                return Err(Error::other(
                    "PNG: zlib encoder stopped consuming input before the end",
                ))
            }
        }
    }
    Ok(())
}

/// Drain the encoder at a sync boundary (segment end) into `out`.
fn drive_sync_flush(
    enc: &mut impl compcol::Encoder,
    buf: &mut [u8],
    out: &mut dyn FnMut(&[u8]),
) -> Result<()> {
    loop {
        let (p, status) = enc.flush(buf, Flush::Sync).map_err(map_err)?;
        out(&buf[..p.written]);
        match status {
            Status::OutputFull => continue,
            Status::InputEmpty | Status::StreamEnd => return Ok(()),
        }
    }
}

/// Finish the stream (single-segment zlib path) into `out`.
fn drive_finish(
    enc: &mut impl compcol::Encoder,
    buf: &mut [u8],
    out: &mut dyn FnMut(&[u8]),
) -> Result<()> {
    loop {
        let (p, status) = enc.finish(buf).map_err(map_err)?;
        out(&buf[..p.written]);
        match status {
            Status::StreamEnd => return Ok(()),
            Status::OutputFull | Status::InputEmpty => continue,
        }
    }
}

/// One compressed segment plus the Adler-32 of its uncompressed bytes.
struct Segment {
    bytes: Vec<u8>,
    adler: Adler32,
}

/// Compress segment `index` of `src` as a run of non-final deflate
/// blocks ending in a sync flush.
fn compress_segment(src: &dyn SegmentSource, index: usize, level: u8) -> Result<Segment> {
    use compcol::deflate::encoder::{Encoder, EncoderConfig};
    let mut enc = Encoder::with_config(EncoderConfig::new().with_level(level));
    let mut buf = vec![0u8; OUT_CHUNK];
    let mut bytes = Vec::new();
    let mut adler = Adler32::new();
    src.feed(index, &mut |chunk| {
        adler.update(chunk);
        drive_encode(&mut enc, chunk, &mut buf, &mut |b| {
            bytes.extend_from_slice(b)
        })
    })?;
    drive_sync_flush(&mut enc, &mut buf, &mut |b| bytes.extend_from_slice(b))?;
    Ok(Segment { bytes, adler })
}

/// Compress the whole of a single-segment `src` as one ordinary zlib
/// stream (compcol's own header / trailer, normal `finish`).
fn compress_single(src: &dyn SegmentSource, level: u8, out: &mut dyn FnMut(&[u8])) -> Result<()> {
    use compcol::zlib::{Encoder, EncoderConfig};
    let mut enc = Encoder::with_config(EncoderConfig { level });
    let mut buf = vec![0u8; OUT_CHUNK];
    let mut pending = Vec::new();
    src.feed(0, &mut |chunk| {
        drive_encode(&mut enc, chunk, &mut buf, &mut |b| {
            pending.extend_from_slice(b)
        })?;
        out(&pending);
        pending.clear();
        Ok(())
    })?;
    drive_finish(&mut enc, &mut buf, &mut |b| pending.extend_from_slice(b))?;
    out(&pending);
    Ok(())
}

/// Compress `input` as one ordinary zlib stream at `level` (compcol's
/// own header and trailer, normal finish), handing the compressed bytes
/// to `out` as they leave the encoder's output buffer, so nothing
/// larger than that buffer is held whatever the input size. The
/// encoder sees the same calls as a one-shot compression of `input`,
/// so the bytes are the same. The input is one slice on purpose:
/// compcol cuts its blocks by how much input it holds, so feeding the
/// same bytes in pieces can change the stream.
pub(crate) fn compress_zlib_to(level: u8, input: &[u8], out: &mut dyn FnMut(&[u8])) -> Result<()> {
    use compcol::zlib::{Encoder, EncoderConfig};
    let mut enc = Encoder::with_config(EncoderConfig { level });
    let mut buf = vec![0u8; OUT_CHUNK];
    drive_encode(&mut enc, input, &mut buf, out)?;
    drive_finish(&mut enc, &mut buf, out)
}

/// RFC 1950 §2.2 `CMF` / `FLG` pair for a 32 KiB-window deflate stream
/// at `level`, `FLEVEL` mapped the way compcol's zlib encoder documents
/// it (1 → 0, 2..=5 → 1, 6 → 2, 7..=9 → 3).
fn zlib_header(level: u8) -> [u8; 2] {
    let cmf: u8 = 0x78; // CM = 8 (deflate), CINFO = 7 (32 KiB window)
    let flevel: u8 = match level {
        0..=1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let flg_base = flevel << 6; // FDICT = 0
    let rem = ((cmf as u16) * 256 + flg_base as u16) % 31;
    let fcheck = if rem == 0 { 0 } else { 31 - rem as u8 };
    [cmf, flg_base | fcheck]
}

/// Final empty stored block: `BFINAL = 1`, `BTYPE = 00`, padding to the
/// byte boundary, `LEN = 0`, `NLEN = 0xFFFF`.
const STREAM_TERMINATOR: [u8; 5] = [0x01, 0x00, 0x00, 0xFF, 0xFF];

/// Compress `src` into a zlib stream delivered to `out` in order, using
/// up to `threads` worker threads for the independent segments.
///
/// `threads ≤ 1` (or a single-segment stream) runs on the calling
/// thread. The emitted bytes do not depend on `threads`.
pub(crate) fn compress_zlib(
    src: &dyn SegmentSource,
    level: u8,
    threads: usize,
    out: &mut dyn FnMut(&[u8]),
) -> Result<()> {
    let n = src.segment_count();
    if n <= 1 {
        return compress_single(src, level, out);
    }
    out(&zlib_header(level));
    let mut total = Adler32::new();
    let workers = threads.min(n).max(1);
    if workers <= 1 {
        for i in 0..n {
            let seg = compress_segment(src, i, level)?;
            out(&seg.bytes);
            total.append(&seg.adler);
        }
    } else {
        // Static round-robin assignment: worker `w` compresses segments
        // `w, w + W, w + 2W, …`; each returns its results in index
        // order and the assembler interleaves them back.
        let results: Vec<Result<Vec<Segment>>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|w| {
                    scope.spawn(move || {
                        (w..n)
                            .step_by(workers)
                            .map(|i| compress_segment(src, i, level))
                            .collect::<Result<Vec<Segment>>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        Err(Error::other(
                            "PNG encoder: a deflate worker thread panicked",
                        ))
                    })
                })
                .collect()
        });
        let mut per_worker: Vec<std::vec::IntoIter<Segment>> = Vec::with_capacity(workers);
        for r in results {
            per_worker.push(r?.into_iter());
        }
        for i in 0..n {
            let seg = per_worker[i % workers].next().ok_or_else(|| {
                Error::other("PNG encoder: deflate worker returned too few segments")
            })?;
            out(&seg.bytes);
            total.append(&seg.adler);
        }
    }
    out(&STREAM_TERMINATOR);
    out(&total.value().to_be_bytes());
    Ok(())
}

/// Convenience: compress a materialised byte stream (segmented at
/// `segment_len`) into a fresh `Vec`.
pub(crate) fn compress_bytes_to_vec(
    bytes: &[u8],
    segment_len: usize,
    level: u8,
    threads: usize,
) -> Result<Vec<u8>> {
    let src = BytesSource { bytes, segment_len };
    let mut v = Vec::new();
    compress_zlib(&src, level, threads, &mut |b| v.extend_from_slice(b))?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zlibvec::decompress_to_vec_zlib_capped;

    /// Literal RFC 1950 §8.2 reference: per-byte modular sums.
    fn adler_reference(bytes: &[u8]) -> u32 {
        let (mut s1, mut s2) = (1u32, 0u32);
        for &b in bytes {
            s1 = (s1 + b as u32) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        (s2 << 16) | s1
    }

    fn noisy(len: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..len)
            .map(|i| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                ((i / 7) as u8).wrapping_add((s & 0x0f) as u8)
            })
            .collect()
    }

    #[test]
    fn adler_update_matches_reference_across_chunk_boundaries() {
        for len in [0usize, 1, 2, 3799, 3800, 3801, 7600, 20_000] {
            let data = noisy(len, 0x1234_5678);
            let mut a = Adler32::new();
            // Deliver in uneven pieces to exercise the chunked loop.
            let mut off = 0;
            let mut piece = 1;
            while off < data.len() {
                let end = (off + piece).min(data.len());
                a.update(&data[off..end]);
                off = end;
                piece = piece * 3 + 1;
            }
            assert_eq!(a.value(), adler_reference(&data), "len {len}");
        }
    }

    #[test]
    fn adler_append_folds_segment_sums() {
        let data = noisy(50_000, 0xC0FF_EE11);
        for split in [0usize, 1, 100, 3800, 25_000, 49_999, 50_000] {
            let mut head = Adler32::new();
            head.update(&data[..split]);
            let mut tail = Adler32::new();
            tail.update(&data[split..]);
            head.append(&tail);
            assert_eq!(head.value(), adler_reference(&data), "split {split}");
            assert_eq!(head.len, data.len() as u64);
        }
    }

    #[test]
    fn zlib_header_is_valid_for_every_level() {
        for level in 1..=9u8 {
            let [cmf, flg] = zlib_header(level);
            assert_eq!(cmf, 0x78);
            assert_eq!(flg & 0x20, 0, "FDICT must be clear");
            assert_eq!(
                (cmf as u16 * 256 + flg as u16) % 31,
                0,
                "FCHECK level {level}"
            );
        }
        assert_eq!(zlib_header(1)[1] >> 6, 0);
        assert_eq!(zlib_header(5)[1] >> 6, 1);
        assert_eq!(zlib_header(6)[1] >> 6, 2);
        assert_eq!(zlib_header(9)[1] >> 6, 3);
    }

    #[test]
    fn segmented_stream_inflates_to_the_input_at_every_thread_count() {
        let data = noisy(300_000, 0xBEEF_0001);
        let single = compress_bytes_to_vec(&data, usize::MAX, 6, 1).unwrap();
        assert_eq!(
            decompress_to_vec_zlib_capped(&single, data.len() as u64 + 1).unwrap(),
            data
        );
        let serial = compress_bytes_to_vec(&data, 40_000, 6, 1).unwrap();
        assert_eq!(
            decompress_to_vec_zlib_capped(&serial, data.len() as u64 + 1).unwrap(),
            data
        );
        for threads in [2usize, 3, 8, 64] {
            let par = compress_bytes_to_vec(&data, 40_000, 6, threads).unwrap();
            assert_eq!(
                par, serial,
                "output must not depend on the thread budget ({threads})"
            );
        }
        // Segment boundaries that do not divide the input evenly.
        let odd = compress_bytes_to_vec(&data, 12_345, 1, 4).unwrap();
        assert_eq!(
            decompress_to_vec_zlib_capped(&odd, data.len() as u64 + 1).unwrap(),
            data
        );
    }

    /// Feeding the single-segment path row by row yields the same bytes
    /// as the one-shot whole-buffer compressor, so the streaming rewrite
    /// leaves small images' IDAT byte-identical to the previous encoder.
    #[test]
    fn single_segment_streaming_matches_one_shot_compression() {
        use crate::zlibvec::compress_to_vec_zlib;
        let data = noisy(700_000, 0x5EED_0042);
        for level in [1u8, 2, 4, 6, 9] {
            let one_shot = compress_to_vec_zlib(&data, level).unwrap();
            // Deliver in 12 097-byte rows (a 4032-pixel RGB24 wire row).
            let src = BytesSource {
                bytes: &data,
                segment_len: 12_097,
            };
            let mut streamed = Vec::new();
            let mut enc =
                compcol::zlib::Encoder::with_config(compcol::zlib::EncoderConfig { level });
            let mut buf = vec![0u8; OUT_CHUNK];
            for i in 0..src.segment_count() {
                src.feed(i, &mut |chunk| {
                    drive_encode(&mut enc, chunk, &mut buf, &mut |b| {
                        streamed.extend_from_slice(b)
                    })
                })
                .unwrap();
            }
            drive_finish(&mut enc, &mut buf, &mut |b| streamed.extend_from_slice(b)).unwrap();
            assert_eq!(streamed, one_shot, "level {level}");
            // And the public single-segment entry point.
            let single = compress_bytes_to_vec(&data, usize::MAX, level, 4).unwrap();
            assert_eq!(single, one_shot, "level {level} via compress_bytes_to_vec");
        }
    }

    /// `compress_zlib_to` writes the one-shot compressor's bytes while
    /// its output passes through the 64 KiB buffer many times, so a
    /// metadata chunk streamed into the file matches the chunk the
    /// one-shot `to_bytes` path used to build.
    #[test]
    fn compress_zlib_to_matches_one_shot_compression() {
        use crate::zlibvec::compress_to_vec_zlib;
        for (data, level) in [
            (noisy(700_000, 0x5EED_0043), 6u8),
            (noisy(300_000, 0x5EED_0044), 1),
            (vec![b'a'; 200_000], 6),
            (vec![7], 6),
            (Vec::new(), 6),
        ] {
            let one_shot = compress_to_vec_zlib(&data, level).unwrap();
            let mut streamed = Vec::new();
            let mut pieces = 0usize;
            compress_zlib_to(level, &data, &mut |b| {
                streamed.extend_from_slice(b);
                pieces += 1;
            })
            .unwrap();
            assert_eq!(streamed, one_shot, "{} bytes at level {level}", data.len());
            assert!(pieces >= one_shot.len() / OUT_CHUNK);
        }
    }

    #[test]
    fn segmented_stream_ends_with_terminator_and_adler() {
        let data = noisy(100_000, 0x0BAD_F00D);
        let z = compress_bytes_to_vec(&data, 30_000, 4, 2).unwrap();
        let n = z.len();
        assert_eq!(&z[..2], &zlib_header(4));
        assert_eq!(&z[n - 9..n - 4], &STREAM_TERMINATOR);
        assert_eq!(&z[n - 4..], &adler_reference(&data).to_be_bytes());
    }

    #[test]
    fn empty_and_tiny_streams() {
        for len in [0usize, 1, 5] {
            let data = noisy(len, 7);
            let z = compress_bytes_to_vec(&data, 2, 6, 4).unwrap();
            assert_eq!(decompress_to_vec_zlib_capped(&z, 64).unwrap(), data);
        }
    }

    #[test]
    fn rows_per_segment_rounds_up_to_whole_rows() {
        assert_eq!(rows_per_segment(SEGMENT_TARGET_BYTES), 1);
        assert_eq!(rows_per_segment(SEGMENT_TARGET_BYTES + 1), 1);
        assert_eq!(rows_per_segment(SEGMENT_TARGET_BYTES / 2), 2);
        assert_eq!(rows_per_segment(1), SEGMENT_TARGET_BYTES);
        assert_eq!(rows_per_segment(0), SEGMENT_TARGET_BYTES);
        assert_eq!(rows_per_segment(12_097), 87); // 4032×3 + 1 → 87 rows ≈ 1 MiB
    }
}
