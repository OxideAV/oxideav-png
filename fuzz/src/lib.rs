//! Runtime libpng interop for the cross-decode fuzz harnesses.
//!
//! libpng is loaded via `dlopen` at first call — there is no
//! `png-sys`-style build-script dep that would pull libpng source
//! into the workspace's cargo dep tree. Each harness checks
//! [`libpng::available`] up front and `return`s early when the
//! shared library isn't installed, so fuzz binaries built on a host
//! without libpng simply do nothing instead of panicking.
//!
//! Install libpng with `brew install libpng` (macOS) or
//! `apt install libpng-dev` (Debian/Ubuntu). The loader probes the
//! conventional shared-object names for both platforms.
//!
//! Only the libpng 1.6+ "simplified" API is used (`png_image_*`
//! family) — it accepts a public, stable `png_image` POD struct and
//! never exposes the opaque `png_struct` / `png_info` internals, so
//! the FFI surface is small and ABI-stable across libpng minor
//! versions.

#![allow(unsafe_code)]

pub mod libpng {
    use libloading::{Library, Symbol};
    use std::sync::OnceLock;

    /// Conventional libpng shared-object names the loader will try
    /// in order. Covers macOS (`.dylib`), Linux (versioned + plain
    /// `.so`), and Windows (`.dll`).
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
                // SAFETY: `Library::new` is documented as unsafe because
                // the loaded library may run code at load time. We
                // accept that risk for fuzz tooling — libpng is a
                // well-behaved shared library.
                if let Ok(l) = unsafe { Library::new(name) } {
                    return Some(l);
                }
            }
            None
        })
        .as_ref()
    }

    /// True iff a libpng shared library was successfully loaded.
    /// Cross-decode fuzz harnesses early-return when this is false so
    /// the binary still runs without an oracle (the assertions just
    /// don't fire).
    pub fn available() -> bool {
        lib().is_some()
    }

    /// libpng's `PNG_IMAGE_VERSION` (the simplified-API ABI version).
    /// Currently 1 in libpng 1.6.x.
    const PNG_IMAGE_VERSION: u32 = 1;

    /// `PNG_FORMAT_RGBA` = `PNG_FORMAT_FLAG_COLOR | PNG_FORMAT_FLAG_ALPHA` = 0x03.
    /// 8-bit per channel, R G B A byte order.
    const PNG_FORMAT_RGBA: u32 = 0x03;

    /// Public POD struct from `png.h` (libpng ≥ 1.6). Layout must
    /// match exactly:
    /// ```c
    /// typedef struct {
    ///     png_controlp opaque;            // pointer
    ///     png_uint_32  version;           // u32
    ///     png_uint_32  width;             // u32
    ///     png_uint_32  height;            // u32
    ///     png_uint_32  format;            // u32
    ///     png_uint_32  flags;             // u32
    ///     png_uint_32  colormap_entries;  // u32
    ///     png_uint_32  warning_or_error;  // u32
    ///     char         message[64];
    /// } png_image;
    /// ```
    /// `png_uint_32` is `unsigned int` on all targets cargo supports
    /// where libpng is shipped (LP64 + LLP64 both).
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
            // SAFETY: `PngImage` is `#[repr(C)]` with only POD fields
            // (raw pointer + u32s + c_char array); zeros are a valid
            // bit pattern for every field, and libpng's read/write
            // entry points accept a zero-initialized struct (see
            // png.h §SIMPLIFIED_READ / SIMPLIFIED_WRITE comments).
            unsafe { std::mem::zeroed() }
        }
    }

    /// A PNG frame as decoded by libpng, normalised to 8-bit RGBA.
    pub struct DecodedRgba {
        pub width: u32,
        pub height: u32,
        /// Tightly packed RGBA, length `width * height * 4`.
        pub rgba: Vec<u8>,
    }

    /// Encode an 8-bit RGBA image to a PNG byte string via the
    /// libpng simplified-write API (`png_image_write_to_memory`,
    /// libpng ≥ 1.6). Returns `None` if libpng isn't available, the
    /// header probe rejects the image (e.g. width/height = 0), or
    /// the encoder reported a failure.
    pub fn encode_rgba(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
        // png.h:
        //   int png_image_write_to_memory(
        //       png_imagep image,
        //       void *memory,
        //       png_alloc_size_t *memory_bytes,   // size_t* on modern libpng
        //       int convert_to_8_bit,
        //       const void *buffer,
        //       png_int_32 row_stride,            // i32
        //       const void *colormap);
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

            // First call with `memory == NULL` to ask libpng for the
            // worst-case output size, then allocate exactly that and
            // write into it. This is the documented two-call pattern
            // (see png.h `png_image_write_get_memory_size`).
            let mut needed: usize = 0;
            let stride = (width as i32).checked_mul(4)?;
            let probe_ok = write(
                &mut img,
                std::ptr::null_mut(),
                &mut needed,
                0, // convert_to_8_bit: irrelevant for already-8-bit RGBA
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

    /// Decode a PNG byte string to 8-bit RGBA via the libpng
    /// simplified-read API (`png_image_begin_read_from_memory` +
    /// `png_image_finish_read`). Returns `None` on libpng
    /// unavailable, header parse failure, allocation overflow, or
    /// decode failure.
    pub fn decode_to_rgba(data: &[u8]) -> Option<DecodedRgba> {
        type BeginFn = unsafe extern "C" fn(*mut PngImage, *const std::ffi::c_void, usize) -> i32;
        type FinishFn = unsafe extern "C" fn(
            *mut PngImage,
            *const std::ffi::c_void, // background — NULL is fine for RGBA out
            *mut std::ffi::c_void,
            i32,                   // row_stride
            *mut std::ffi::c_void, // colormap
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

            // Force an 8-bit RGBA output regardless of source format
            // (libpng will composite alpha / expand palette for us).
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
