//! End-to-end external-decoder roundtrip test:
//!
//!   oxideav encode → libpng decode → libpng encode → oxideav decode
//!
//! PNG is lossless, so every step is byte-equal in pixel space. The test
//! generates a deterministic 640×480 RGBA image (seeded xorshift) and
//! asserts the final RGBA matches the original.
//!
//! libpng is loaded at runtime via `dlopen` (no `*-sys` build dep, no
//! libpng source in the workspace). When libpng isn't installed the test
//! prints a skip message and returns early — CI hosts without
//! libpng-dev / libpng (homebrew) won't fail.
//!
//! The libpng shim mirrors `fuzz/src/lib.rs` (libpng 1.6+ "simplified"
//! API — `png_image_*` family) but is inlined here as a private module
//! so the main crate doesn't dev-depend on the fuzz crate (circular dep
//! risk).

#![allow(unsafe_code)]

use oxideav_core::{PixelFormat, VideoFrame, VideoPlane};

// ---------------------------------------------------------------------------
// libpng dlopen shim — mirror of fuzz/src/lib.rs (kept inline to avoid
// dev-depending on the fuzz crate). See that file for the long-form
// rationale comments.
// ---------------------------------------------------------------------------
mod libpng {
    use libloading::{Library, Symbol};
    use std::sync::OnceLock;

    const CANDIDATES: &[&str] = &[
        "libpng.dylib",
        "libpng16.16.dylib",
        "libpng16.so.16",
        "libpng.so.16",
        "libpng.so",
        "libpng16-16.dll",
    ];

    fn lib() -> Option<&'static Library> {
        static LIB: OnceLock<Option<Library>> = OnceLock::new();
        LIB.get_or_init(|| {
            for name in CANDIDATES {
                // SAFETY: loading a system shared library may execute init
                // code. libpng is well-behaved; we accept the risk for
                // test tooling.
                if let Ok(l) = unsafe { Library::new(name) } {
                    return Some(l);
                }
            }
            None
        })
        .as_ref()
    }

    pub fn available() -> bool {
        lib().is_some()
    }

    const PNG_IMAGE_VERSION: u32 = 1;
    /// PNG_FORMAT_FLAG_COLOR | PNG_FORMAT_FLAG_ALPHA = 0x03
    const PNG_FORMAT_RGBA: u32 = 0x03;

    /// Public POD struct from `png.h` (libpng ≥ 1.6). Layout must match
    /// exactly. `png_uint_32` is `unsigned int` on every target where
    /// libpng ships.
    #[repr(C)]
    struct PngImage {
        opaque: *mut std::ffi::c_void,
        version: u32,
        width: u32,
        height: u32,
        format: u32,
        flags: u32,
        colormap_entries: u32,
        warning_or_error: u32,
        message: [std::ffi::c_char; 64],
    }

    impl PngImage {
        fn zeroed() -> Self {
            // SAFETY: repr(C) POD; zero is a valid bit pattern for every
            // field, and libpng's read/write entry points accept a
            // zero-initialised struct.
            unsafe { std::mem::zeroed() }
        }
    }

    pub struct DecodedRgba {
        pub width: u32,
        pub height: u32,
        pub rgba: Vec<u8>,
    }

    /// Encode 8-bit RGBA → PNG bytes via `png_image_write_to_memory`.
    pub fn encode_rgba(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
        type WriteFn = unsafe extern "C" fn(
            *mut PngImage,
            *mut std::ffi::c_void,
            *mut usize,
            i32,
            *const std::ffi::c_void,
            i32,
            *const std::ffi::c_void,
        ) -> i32;
        type FreeFn = unsafe extern "C" fn(*mut PngImage);

        if width == 0 || height == 0 {
            return None;
        }
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        if rgba.len() < expected {
            return None;
        }

        let l = lib()?;
        unsafe {
            let write: Symbol<WriteFn> = l.get(b"png_image_write_to_memory").ok()?;
            let free: Symbol<FreeFn> = l.get(b"png_image_free").ok()?;

            let mut img = PngImage::zeroed();
            img.version = PNG_IMAGE_VERSION;
            img.width = width;
            img.height = height;
            img.format = PNG_FORMAT_RGBA;

            // Two-call pattern: first call with NULL memory asks libpng for
            // the worst-case size; allocate exactly that and write into it.
            let mut needed: usize = 0;
            let stride = (width as i32).checked_mul(4)?;
            let probe_ok = write(
                &mut img,
                std::ptr::null_mut(),
                &mut needed,
                0,
                rgba.as_ptr() as *const std::ffi::c_void,
                stride,
                std::ptr::null(),
            );
            if probe_ok == 0 || needed == 0 {
                free(&mut img);
                return None;
            }

            let mut buf = vec![0u8; needed];
            let mut written = needed;
            let ok = write(
                &mut img,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                &mut written,
                0,
                rgba.as_ptr() as *const std::ffi::c_void,
                stride,
                std::ptr::null(),
            );
            free(&mut img);
            if ok == 0 {
                return None;
            }
            buf.truncate(written);
            Some(buf)
        }
    }

    /// Decode PNG bytes → 8-bit RGBA via `png_image_begin_read_from_memory`
    /// + `png_image_finish_read`.
    pub fn decode_to_rgba(data: &[u8]) -> Option<DecodedRgba> {
        type BeginFn = unsafe extern "C" fn(*mut PngImage, *const std::ffi::c_void, usize) -> i32;
        type FinishFn = unsafe extern "C" fn(
            *mut PngImage,
            *const std::ffi::c_void,
            *mut std::ffi::c_void,
            i32,
            *mut std::ffi::c_void,
        ) -> i32;
        type FreeFn = unsafe extern "C" fn(*mut PngImage);

        let l = lib()?;
        unsafe {
            let begin: Symbol<BeginFn> = l.get(b"png_image_begin_read_from_memory").ok()?;
            let finish: Symbol<FinishFn> = l.get(b"png_image_finish_read").ok()?;
            let free: Symbol<FreeFn> = l.get(b"png_image_free").ok()?;

            let mut img = PngImage::zeroed();
            img.version = PNG_IMAGE_VERSION;

            if begin(
                &mut img,
                data.as_ptr() as *const std::ffi::c_void,
                data.len(),
            ) == 0
            {
                free(&mut img);
                return None;
            }

            // Force 8-bit RGBA out regardless of source format.
            img.format = PNG_FORMAT_RGBA;

            if img.width == 0 || img.height == 0 {
                free(&mut img);
                return None;
            }
            let stride = (img.width as usize).checked_mul(4)?;
            let size = stride.checked_mul(img.height as usize)?;
            let mut buf = vec![0u8; size];

            let ok = finish(
                &mut img,
                std::ptr::null(),
                buf.as_mut_ptr() as *mut std::ffi::c_void,
                stride as i32,
                std::ptr::null_mut(),
            );
            let w = img.width;
            let h = img.height;
            free(&mut img);
            if ok == 0 {
                return None;
            }
            Some(DecodedRgba {
                width: w,
                height: h,
                rgba: buf,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tiny xorshift64* PRNG. Inlined so the test doesn't need a `rand` dep.
// Period 2^64 - 1; sequence is fully determined by `seed`.
// ---------------------------------------------------------------------------
fn generate_random_rgba(width: u32, height: u32, seed: u64) -> Vec<u8> {
    let mut state = if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    };
    let n = (width as usize) * (height as usize) * 4;
    let mut out = vec![0u8; n];
    let mut i = 0;
    while i < n {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let v = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let bytes = v.to_le_bytes();
        let take = (n - i).min(8);
        out[i..i + take].copy_from_slice(&bytes[..take]);
        i += take;
    }
    out
}

fn rgba_video_frame(width: u32, height: u32, rgba: &[u8]) -> VideoFrame {
    VideoFrame {
        pts: Some(0),
        planes: vec![VideoPlane {
            stride: (width as usize) * 4,
            data: rgba.to_vec(),
        }],
    }
}

#[test]
fn external_roundtrip_640x480_rgba() {
    if !libpng::available() {
        eprintln!("skipping external_roundtrip_640x480_rgba: libpng not installed");
        return;
    }

    let (w, h) = (640u32, 480u32);
    let rgba = generate_random_rgba(w, h, 0xDEAD_BEEF);
    assert_eq!(rgba.len(), (w as usize) * (h as usize) * 4);

    // Step 1: oxideav encode → bytes.
    let frame = rgba_video_frame(w, h, &rgba);
    let png_bytes_1 = oxideav_png::encode_single(&frame, w, h, PixelFormat::Rgba, &[])
        .expect("oxideav-png encode_single failed");

    // Step 2: libpng decode → rgba. PNG is lossless so this must be
    // byte-equal to the source RGBA.
    let decoded_1 =
        libpng::decode_to_rgba(&png_bytes_1).expect("libpng failed to decode oxideav-encoded PNG");
    assert_eq!(decoded_1.width, w);
    assert_eq!(decoded_1.height, h);
    assert_eq!(
        decoded_1.rgba.len(),
        rgba.len(),
        "libpng decoded RGBA length mismatch"
    );
    assert_eq!(
        &decoded_1.rgba, &rgba,
        "oxideav-encoded PNG round-trips through libpng with byte-equal RGBA"
    );

    // Step 3: libpng encode → bytes.
    let png_bytes_2 = libpng::encode_rgba(&decoded_1.rgba, w, h).expect("libpng encode failed");

    // Step 4: oxideav decode → frame.
    let decoded_2 = oxideav_png::decode_png_to_frame(&png_bytes_2, None)
        .expect("oxideav-png failed to decode libpng-encoded PNG");

    // Sanity: stride must match a tightly-packed RGBA plane.
    assert_eq!(decoded_2.planes.len(), 1);
    assert_eq!(decoded_2.planes[0].stride, (w as usize) * 4);
    assert_eq!(decoded_2.planes[0].data.len(), rgba.len());

    // Final assertion: full e2e roundtrip preserves RGBA pixel-for-pixel.
    assert_eq!(
        &decoded_2.planes[0].data, &rgba,
        "full oxideav-encode → libpng-decode → libpng-encode → oxideav-decode preserves RGBA"
    );
}
