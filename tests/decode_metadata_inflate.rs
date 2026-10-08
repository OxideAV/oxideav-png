//! Counts the bytes a decode allocates, to pin that
//! `DecodeOptions::with_inflate_metadata(false)` decodes the pixels
//! without inflating the compressed metadata bodies.
//!
//! A counting `#[global_allocator]` records, per thread, every
//! allocation made while a measurement is running: the bytes allocated
//! (a `realloc` counts its new size) and the largest single block.
//! Tests run on separate threads, so each measurement sees only its own
//! decode.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::OnceLock;

use compcol::zlib::{EncoderConfig, Zlib};
use oxideav_png::chunk::write_chunk;
use oxideav_png::{
    decode_all_with, decode_with, encode, encode_all, ColorInfo, DecodeOptions, EncodeOptions,
    Frame, PngImage,
};

struct Counting;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn record(size: usize) {
    let _ = ON.try_with(|on| {
        if on.get() {
            ALLOCATED.with(|c| c.set(c.get() + size));
            LARGEST.with(|c| c.set(c.get().max(size)));
        }
    });
}

// SAFETY: every method forwards to `System` with the caller's own
// arguments; the bookkeeping only touches thread-local counters, which
// never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        System.alloc(layout)
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        System.alloc_zeroed(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size);
        System.realloc(ptr, layout, new_size)
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// What one measured call allocated: `(bytes, largest block)`.
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    ALLOCATED.with(|c| c.set(0));
    LARGEST.with(|c| c.set(0));
    ON.with(|on| on.set(true));
    let out = f();
    ON.with(|on| on.set(false));
    (out, ALLOCATED.with(Cell::get), LARGEST.with(Cell::get))
}

/// What each compressed body inflates to: half the 64 MiB bound, and
/// far more than anything a 16 x 16 decode needs.
const INFLATED: usize = 32 << 20;

/// What a decode under the option may allocate beyond decoding the same
/// image without the metadata chunks: the longer chunk list.
const SLACK: usize = 4 * 1024;

fn deflate(data: &[u8]) -> Vec<u8> {
    compcol::vec::compress_to_vec_with::<Zlib>(data, EncoderConfig { level: 1 })
        .expect("test-side zlib compression")
}

fn rgba_16x16() -> PngImage {
    let data = (0..16 * 16 * 4).map(|i| (i * 7 % 251) as u8).collect();
    PngImage::from_rgba8(16, 16, data).unwrap()
}

/// `file` with `chunks` inserted right after its `IHDR`.
fn with_chunks_after_ihdr(file: &[u8], chunks: &[u8]) -> Vec<u8> {
    // Signature (8) + IHDR (4 length + 4 type + 13 data + 4 CRC).
    let ihdr_end = 8 + 25;
    let mut out = file[..ihdr_end].to_vec();
    out.extend_from_slice(chunks);
    out.extend_from_slice(&file[ihdr_end..]);
    out
}

/// An `iCCP` chunk whose profile inflates to [`INFLATED`] zero bytes.
fn iccp_chunk(out: &mut Vec<u8>) {
    let mut payload = b"big profile\0\0".to_vec();
    payload.extend_from_slice(&deflate(&vec![0u8; INFLATED]));
    write_chunk(out, b"iCCP", &payload);
}

/// An XMP `iTXt` chunk whose packet inflates to [`INFLATED`] bytes.
fn compressed_xmp_chunk(out: &mut Vec<u8>) {
    // Keyword, NUL, flag 1 (compressed), method 0, empty language tag
    // and translated keyword, then the body.
    let mut payload = b"XML:com.adobe.xmp\0\x01\x00\0\0".to_vec();
    payload.extend_from_slice(&deflate(&vec![b'x'; INFLATED]));
    write_chunk(out, b"iTXt", &payload);
}

/// The 16 x 16 image with an `iCCP` and a compressed XMP `iTXt` that
/// each inflate to 32 MiB, and an `sRGB` chunk the `iCCP` outranks.
fn still_with_metadata() -> &'static [u8] {
    static FILE: OnceLock<Vec<u8>> = OnceLock::new();
    FILE.get_or_init(|| {
        let bare = encode(&rgba_16x16(), &EncodeOptions::default()).unwrap();
        let mut chunks = Vec::new();
        iccp_chunk(&mut chunks);
        write_chunk(&mut chunks, b"sRGB", &[0]);
        compressed_xmp_chunk(&mut chunks);
        with_chunks_after_ihdr(&bare, &chunks)
    })
}

fn left_compressed() -> DecodeOptions {
    DecodeOptions::default().with_inflate_metadata(false)
}

#[test]
fn inflating_metadata_is_the_default() {
    assert!(DecodeOptions::default().inflate_metadata);
    let (image, _, largest) =
        measure(|| decode_with(still_with_metadata(), &DecodeOptions::default()));
    let image = image.unwrap();
    assert_eq!(image.metadata.icc.as_ref().map(Vec::len), Some(INFLATED));
    assert_eq!(image.metadata.xmp.as_ref().map(Vec::len), Some(INFLATED));
    assert!(largest >= INFLATED, "largest block {largest}");
}

#[test]
fn metadata_left_compressed_is_not_inflated() {
    let file = still_with_metadata();
    let bare = encode(&rgba_16x16(), &EncodeOptions::default()).unwrap();
    let (_, bare_bytes, _) = measure(|| decode_with(&bare, &left_compressed()).unwrap());
    let (image, bytes, largest) = measure(|| decode_with(file, &left_compressed()));
    let image = image.unwrap();
    eprintln!(
        "left compressed: {bytes} bytes allocated, largest {largest}; \
         the image without the chunks: {bare_bytes}"
    );
    assert!(
        bytes <= bare_bytes + SLACK,
        "the decode allocated {bytes} bytes, {bare_bytes} without the metadata chunks"
    );
    assert_eq!(image.metadata.icc, None);
    assert_eq!(image.metadata.xmp, None);
    // The pixels are the default decode's, and the `iCCP` chunk still
    // outranks the `sRGB` chunk.
    let inflated = decode_with(file, &DecodeOptions::default()).unwrap();
    assert_eq!(image.planes, inflated.planes);
    assert_eq!(image.color, inflated.color);
    assert_eq!(image.color, ColorInfo::png_default());
}

#[test]
fn decode_all_with_leaves_metadata_compressed_for_still_and_animated_files() {
    let frame = || Frame::new(rgba_16x16(), Some(std::time::Duration::from_millis(40)));
    let animated = encode_all(&[frame(), frame()], &EncodeOptions::default()).unwrap();
    let mut chunks = Vec::new();
    iccp_chunk(&mut chunks);
    let animated = with_chunks_after_ihdr(&animated, &chunks);
    for (what, file) in [
        ("still", still_with_metadata()),
        ("animated", &animated[..]),
    ] {
        let (frames, bytes, largest) = measure(|| decode_all_with(file, &left_compressed()));
        let frames = frames.unwrap();
        eprintln!("{what}: {bytes} bytes allocated, largest {largest}");
        assert!(largest < INFLATED / 16, "{what}: largest block {largest}");
        assert!(
            frames.iter().all(|f| f.image.metadata.icc.is_none()),
            "{what}"
        );
        let (inflated, _, largest) = measure(|| decode_all_with(file, &DecodeOptions::default()));
        assert!(inflated.unwrap()[0].image.metadata.icc.is_some(), "{what}");
        assert!(largest >= INFLATED, "{what}: largest block {largest}");
    }
}

#[test]
fn an_uncompressed_xmp_packet_is_still_read() {
    let bare = encode(&rgba_16x16(), &EncodeOptions::default()).unwrap();
    let mut chunks = Vec::new();
    write_chunk(
        &mut chunks,
        b"iTXt",
        b"XML:com.adobe.xmp\0\x00\x00\0\0<x:xmpmeta/>",
    );
    let file = with_chunks_after_ihdr(&bare, &chunks);
    let image = decode_with(&file, &left_compressed()).unwrap();
    assert_eq!(image.metadata.xmp.as_deref(), Some(&b"<x:xmpmeta/>"[..]));
}

#[test]
fn strict_mode_still_checks_what_comes_before_a_compressed_body() {
    let bare = encode(&rgba_16x16(), &EncodeOptions::default()).unwrap();
    let strict = left_compressed().with_strict(true);
    let file = |name: &[u8], method: u8, body: &[u8]| {
        let mut payload = name.to_vec();
        payload.extend_from_slice(&[0, method]);
        payload.extend_from_slice(body);
        let mut chunks = Vec::new();
        write_chunk(&mut chunks, b"iCCP", &payload);
        with_chunks_after_ihdr(&bare, &chunks)
    };
    let good_body = deflate(&[0u8; 64]);
    // A compression method other than 0 and an empty profile name are
    // still errors.
    assert!(decode_with(&file(b"p", 1, &good_body), &strict).is_err());
    assert!(decode_with(&file(b"", 0, &good_body), &strict).is_err());
    // A body that is not a zlib stream is not inflated, so it is not
    // found; inflating it fails as before.
    let corrupt = file(b"p", 0, b"not zlib");
    assert!(decode_with(&corrupt, &strict).is_ok());
    assert!(decode_with(&corrupt, &DecodeOptions::default().with_strict(true)).is_err());
    // A good one decodes either way.
    assert!(decode_with(&file(b"p", 0, &good_body), &strict).is_ok());
}
