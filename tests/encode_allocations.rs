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

/// `encode_into` reserves its size estimate once when the caller's
/// buffer has less spare room than that, however little is spare.
mod reserve {
    use super::*;
    use oxideav_png::{encode_into, EncodeOptions, PngImage};

    #[test]
    fn a_buffer_with_a_few_spare_bytes_is_reserved_once() {
        let data: Vec<u8> = (0..64 * 64 * 3).map(|i| (i % 251) as u8).collect();
        let img = PngImage::from_rgb8(64, 64, data).unwrap();
        let opts = EncodeOptions::default();
        let mut full = Vec::with_capacity(10);
        full.extend_from_slice(&[7; 10]);
        let ((), no_spare) = measure(|| encode_into(&img, &opts, &mut full).unwrap());
        let mut spare = Vec::with_capacity(14);
        spare.extend_from_slice(&[7; 10]);
        let ((), four_spare) = measure(|| encode_into(&img, &opts, &mut spare).unwrap());
        report("encode_into, no spare room", &no_spare);
        report("encode_into, 4 spare bytes", &four_spare);
        assert_eq!(full, spare);
        assert_eq!(
            four_spare.count, no_spare.count,
            "the buffer with 4 spare bytes was grown more often than the full one"
        );
    }
}

/// Large metadata payloads (1 MiB each): measured through plain
/// `encode` and through `encode_into` into a buffer reserved for the
/// whole file. Through `encode_into` nothing payload-sized is
/// allocated at all; through `encode` the only payload-sized block is
/// the output itself, which must hold the payload.
mod chunks {
    use super::*;
    use oxideav_png::{
        encode, encode_into, EncodeOptions, Exif, Iccp, Itxt, Metadata, PngImage, PngMetadata,
        PngPixelFormat, Text, Ztxt,
    };

    /// One mebibyte: the payload size of every case below.
    const PAYLOAD: usize = 1 << 20;

    /// Deterministic xorshift32 bytes: incompressible.
    fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut s = seed | 1;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s >> 11) as u8
            })
            .collect()
    }

    fn tiny() -> PngImage {
        PngImage::packed(16, 16, PngPixelFormat::Gray8, 16, vec![0; 256]).unwrap()
    }

    /// `PAYLOAD` ASCII letters.
    fn ascii_text() -> String {
        noise(PAYLOAD, 0x7E57_0041)
            .iter()
            .map(|&b| (b'a' + b % 26) as char)
            .collect()
    }

    /// `PAYLOAD` Latin-1 characters, a quarter of them outside ASCII so
    /// the `String` holds more bytes than the chunk does.
    fn latin1_text() -> String {
        noise(PAYLOAD, 0x7E57_0042)
            .iter()
            .map(|&b| {
                if b < 64 {
                    'é'
                } else {
                    (b'a' + b % 26) as char
                }
            })
            .collect()
    }

    fn with(meta: PngMetadata) -> EncodeOptions {
        EncodeOptions::default().with_metadata(meta)
    }

    /// Measure `img` under `opts` through `encode` and through
    /// `encode_into` into a buffer reserved for the whole file; check
    /// both write the same bytes. Returns `(encode stats, output
    /// capacity, encode_into stats)`.
    fn measure_both(what: &str, img: &PngImage, opts: &EncodeOptions) -> (Stats, usize, Stats) {
        let (plain, plain_stats) = measure(|| encode(img, opts).unwrap());
        let mut out = Vec::with_capacity(plain.len());
        let ((), into_stats) = measure(|| encode_into(img, opts, &mut out).unwrap());
        report(&format!("{what}, encode"), &plain_stats);
        report(&format!("{what}, encode_into reserved"), &into_stats);
        assert_eq!(out, plain, "{what}: same bytes from both entries");
        assert_eq!(
            out.capacity(),
            plain.len(),
            "{what}: the reserved output was not regrown"
        );
        (plain_stats, plain.capacity(), into_stats)
    }

    /// No second payload-sized buffer. Through `encode_into` (output
    /// reserved, so not counted) no block reaches half the payload and
    /// less than a payload is live at once. Through `encode` no block
    /// outgrows the output, and less than a payload is live besides it.
    fn assert_no_payload_copy(what: &str, plain: &Stats, output_capacity: usize, into: &Stats) {
        assert!(
            into.largest < PAYLOAD / 2,
            "{what}: encode_into allocated a {}-byte block next to the {PAYLOAD}-byte payload",
            into.largest
        );
        assert!(
            into.peak < PAYLOAD,
            "{what}: encode_into held {} bytes at once, more than the {PAYLOAD}-byte payload",
            into.peak
        );
        assert!(
            plain.largest <= output_capacity,
            "{what}: encode allocated a {}-byte block, more than its {output_capacity}-byte output",
            plain.largest
        );
        let beside_output = plain.peak.saturating_sub(output_capacity);
        assert!(
            beside_output < PAYLOAD,
            "{what}: encode held {beside_output} bytes at once besides its \
             {output_capacity}-byte output"
        );
    }

    fn check(what: &str, img: &PngImage, opts: &EncodeOptions) {
        let (plain, cap, into) = measure_both(what, img, opts);
        assert_no_payload_copy(what, &plain, cap, &into);
    }

    #[test]
    fn iccp_from_options_has_no_second_profile_buffer() {
        let opts = with(PngMetadata::default().with_iccp(Iccp::new(
            "Big profile".to_string(),
            noise(PAYLOAD, 0x1CC0_0001),
        )));
        check("iCCP 1 MiB (options)", &tiny(), &opts);
    }

    #[test]
    fn iccp_from_image_has_no_second_profile_buffer() {
        let img = tiny().with_metadata(Metadata::new().with_icc(noise(PAYLOAD, 0x1CC0_0002)));
        check("iCCP 1 MiB (image)", &img, &EncodeOptions::default());
    }

    #[test]
    fn exif_has_no_second_payload_buffer() {
        let mut exif = vec![0x49, 0x49, 0x2A, 0x00];
        exif.extend_from_slice(&noise(PAYLOAD - 4, 0xE71F_0001));
        let opts = with(PngMetadata::default().with_exif(Exif::new(exif.clone())));
        check("eXIf 1 MiB (options)", &tiny(), &opts);
        let img = tiny().with_metadata(Metadata::new().with_exif(exif));
        check("eXIf 1 MiB (image)", &img, &EncodeOptions::default());
    }

    #[test]
    fn text_chunks_have_no_second_payload_buffer() {
        let text = latin1_text();
        let cases = [
            (
                "tEXt 1 MiB",
                PngMetadata::default().with_texts(vec![Text::new("Big".to_string(), text.clone())]),
            ),
            (
                "zTXt 1 MiB",
                PngMetadata::default().with_ztxts(vec![Ztxt::new("Big".to_string(), ascii_text())]),
            ),
            (
                "iTXt 1 MiB",
                PngMetadata::default().with_itxts(vec![Itxt::new("Big".to_string(), text.clone())]),
            ),
            (
                "iTXt 1 MiB compressed",
                PngMetadata::default().with_itxts(vec![
                    Itxt::new("Big".to_string(), text.clone()).with_compressed(true)
                ]),
            ),
        ];
        for (what, meta) in cases {
            check(what, &tiny(), &with(meta));
        }
    }

    #[test]
    fn xmp_packet_has_no_second_payload_buffer() {
        let xmp = latin1_text().into_bytes();
        let img = tiny().with_metadata(Metadata::new().with_xmp(xmp));
        check("XMP 1 MiB (image)", &img, &EncodeOptions::default());
    }

    /// `zTXt` text outside ASCII is converted to its Latin-1 bytes
    /// before it is compressed, because compcol's stream depends on how
    /// its input arrives and the bytes must match the one-shot
    /// compression. That conversion is the one payload-sized block; the
    /// compressed body still goes straight into the chunk.
    #[test]
    fn ztxt_with_non_ascii_text_converts_it_once() {
        let text = latin1_text();
        let meta =
            PngMetadata::default().with_ztxts(vec![Ztxt::new("Big".to_string(), text.clone())]);
        let (_, _, into) = measure_both("zTXt 1 MiB non-ASCII", &tiny(), &with(meta));
        assert!(
            into.largest <= text.len(),
            "the largest block ({}) is more than the converted text ({})",
            into.largest,
            text.len()
        );
        assert!(
            into.peak - into.largest < PAYLOAD,
            "{} bytes were live besides the converted text",
            into.peak - into.largest
        );
    }
}

/// The registry adapter's `encode_single` on a 512 x 512 RGBA frame:
/// beyond the compressor's working memory and the output buffer,
/// nothing plane-sized is allocated.
#[cfg(feature = "registry")]
mod registry {
    use super::planes::{assert_no_plane_copy, gradient};
    use super::*;
    use oxideav_core::{PixelFormat, VideoFrame, VideoPlane};

    pub(crate) fn frame() -> VideoFrame {
        VideoFrame {
            pts: Some(1),
            planes: vec![VideoPlane {
                stride: 512 * 4,
                data: gradient(),
            }],
        }
    }

    #[test]
    fn encode_single_reads_the_frame_in_place() {
        let frame = frame();
        let (png, s) = measure(|| {
            oxideav_png::encode_single(&frame, 512, 512, PixelFormat::Rgba, &[]).unwrap()
        });
        report("encode_single 512x512 RGBA", &s);
        assert_no_plane_copy("encode_single", &s, png.capacity());
        let img = oxideav_png::decode(&png).unwrap();
        assert_eq!(img.as_bytes().unwrap(), &frame.planes[0].data[..]);
    }
}
