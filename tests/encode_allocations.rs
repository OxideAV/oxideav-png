//! Counts the bytes the encoder allocates, to pin that it reads the
//! caller's data in place.
//!
//! A counting `#[global_allocator]` records, per thread, every
//! allocation made while a measurement is running: the bytes allocated
//! (a `realloc` counts its new size), the largest single block, and the
//! peak of bytes live at once. Tests run on separate threads and the
//! encoder runs on the calling thread at `threads = 1`, so each
//! measurement sees only its own encode.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

/// Record a block of `new` bytes replacing one of `old` bytes (`old` is
/// 0 for a fresh allocation).
fn record(new: usize, old: usize) {
    let _ = ON.try_with(|on| {
        if !on.get() {
            return;
        }
        ALLOCATED.with(|c| c.set(c.get() + new));
        COUNT.with(|c| c.set(c.get() + 1));
        LARGEST.with(|c| c.set(c.get().max(new)));
        LIVE.with(|l| {
            let live = l.get() + new as isize - old as isize;
            l.set(live);
            PEAK.with(|p| p.set(p.get().max(live)));
        });
    });
}

fn release(size: usize) {
    let _ = ON.try_with(|on| {
        if on.get() {
            LIVE.with(|l| l.set(l.get() - size as isize));
        }
    });
}

// SAFETY: every method forwards to `System` with the caller's own
// arguments; the bookkeeping only touches thread-local counters, which
// never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), 0);
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), 0);
        System.alloc_zeroed(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        release(layout.size());
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size, layout.size());
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// What one measured call allocated.
#[derive(Debug)]
struct Stats {
    /// Bytes allocated (a `realloc` counts its new size).
    allocated: usize,
    /// Number of allocations and reallocations.
    count: usize,
    /// Largest single block.
    largest: usize,
    /// Most bytes live at once, counting only blocks made during the call.
    peak: usize,
}

fn measure<T>(f: impl FnOnce() -> T) -> (T, Stats) {
    ALLOCATED.with(|c| c.set(0));
    COUNT.with(|c| c.set(0));
    LARGEST.with(|c| c.set(0));
    LIVE.with(|c| c.set(0));
    PEAK.with(|c| c.set(0));
    ON.with(|on| on.set(true));
    let out = f();
    ON.with(|on| on.set(false));
    let stats = Stats {
        allocated: ALLOCATED.with(Cell::get),
        count: COUNT.with(Cell::get),
        largest: LARGEST.with(Cell::get),
        peak: PEAK.with(Cell::get).max(0) as usize,
    };
    (out, stats)
}

fn report(what: &str, s: &Stats) {
    eprintln!(
        "{what}: allocated {} bytes in {} blocks, largest {}, peak live {}",
        s.allocated, s.count, s.largest, s.peak
    );
}

/// A 512 x 512 RGBA plane from a caller's slice: beyond the
/// compressor's working memory and the output buffer, nothing
/// plane-sized is allocated.
mod planes {
    use super::*;
    use oxideav_png::{decode, encode_rgba8, EncodeOptions};

    const W: usize = 512;
    const H: usize = 512;
    /// The caller's plane: 512 x 512 RGBA.
    pub(crate) const PLANE: usize = W * H * 4;

    /// A smooth gradient: the compressed file is far smaller than the
    /// plane, so the output buffer never grows past its first
    /// reservation and every plane-sized block the count sees is a copy.
    pub(crate) fn gradient() -> Vec<u8> {
        let mut data = vec![0u8; PLANE];
        for y in 0..H {
            for x in 0..W {
                let p = (y * W + x) * 4;
                data[p..p + 4].copy_from_slice(&[(x / 2) as u8, (y / 2) as u8, 128, 255]);
            }
        }
        data
    }

    /// No block as large as the plane, and less than a plane live at
    /// once besides the output. (The bytes allocated over the whole call
    /// are reported but not bounded: the compressor allocates afresh for
    /// every 16 KiB block, so the total counts its churn, not what it
    /// holds.)
    pub(crate) fn assert_no_plane_copy(what: &str, s: &Stats, output_capacity: usize) {
        assert!(
            s.largest < PLANE,
            "{what}: a {}-byte block was allocated for a {PLANE}-byte plane",
            s.largest
        );
        let beside_output = s.peak.saturating_sub(output_capacity);
        assert!(
            beside_output < PLANE,
            "{what}: {beside_output} bytes were live at once besides the \
             {output_capacity}-byte output, at least a plane's worth"
        );
    }

    #[test]
    fn encode_rgba8_reads_the_slice_in_place() {
        let rgba = gradient();
        let opts = EncodeOptions::default();
        let (png, s) = measure(|| encode_rgba8(W as u32, H as u32, &rgba, &opts).unwrap());
        report("encode_rgba8 512x512", &s);
        assert_no_plane_copy("encode_rgba8", &s, png.capacity());
        assert_eq!(decode(&png).unwrap().as_bytes().unwrap(), &rgba[..]);
    }
}
