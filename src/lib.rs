//! Pure-Rust PNG + APNG decoder, encoder and container.
//!
//! # Standalone use
//!
//! The crate follows the OxideAV image-crate contract
//! (`IMAGE_CRATE_API`): a small root vocabulary that works with
//! `default-features = false` and returns pixels as plain `Vec<u8>`.
//!
//! ```no_run
//! # fn main() -> Result<(), oxideav_png::Error> {
//! let bytes = std::fs::read("in.png").map_err(oxideav_png::PngError::Io)?;
//! if oxideav_png::probe(&bytes) {
//!     let info = oxideav_png::info(&bytes)?;   // header only: width, height, format, frames
//!     let img = oxideav_png::decode(&bytes)?;  // PngImage, native layout
//!     let rgba: Vec<u8> = img.to_rgba8();      // tightly packed RGBA, 4 * width bytes per row
//!     let (w, h) = (img.width(), img.height());
//!     assert_eq!(info.width, w);
//!
//!     let opts = oxideav_png::EncodeOptions::default().with_level(2);
//!     let out: Vec<u8> = oxideav_png::encode_rgba8(w, h, &rgba, &opts)?;
//!     std::fs::write("out.png", out).map_err(oxideav_png::PngError::Io)?;
//! }
//! # Ok(()) }
//! ```
//!
//! * [`probe`] / [`info`] — signature sniff; header + chunk walk
//!   without decoding pixels ([`ImageInfo`]).
//! * [`decode`] / [`decode_with`] — the native layout ([`PngImage`]:
//!   `width`, `height`, [`PixelFormat`], one [`Plane`], [`ColorInfo`],
//!   [`Metadata`], [`Palette`], keyed [`Trns`] transparency), with
//!   [`DecodeOptions`] for limits and strictness.
//! * [`decode_rgb8`] / [`decode_rgba8`] — the one-call raw paths
//!   ([`RgbImage`] / [`RgbaImage`]); [`PngImage::to_rgb8`] /
//!   [`PngImage::to_rgba8`] do the same from a decoded image.
//! * [`decode_all`] — every frame of an APNG, composited
//!   ([`Frame`]); [`decode_from`] reads a `Read` to its end.
//! * [`encode`] / [`encode_rgb8`] / [`encode_rgba8`] / [`encode_to`]
//!   with [`EncodeOptions`] (level, filter, interlace, sub-byte depth,
//!   threads, extra chunks).
//! * [`PngError`] (alias [`Error`]): `InvalidData`, `Unsupported`,
//!   `LimitExceeded`, `Io`, …
//!
//! # Framework use
//!
//! With the default-on `registry` feature, [`register`] installs the
//! `png` codec and the PNG / APNG container into an
//! `oxideav_core::RuntimeContext`; [`make_decoder`] / [`make_encoder`]
//! are the factories, and `From<PngImage> for VideoFrame` /
//! [`PngImage::from_video_frame`] convert between the two worlds. The
//! trait-side `Decoder` / `Encoder` call the standalone functions.
//!
//! # Supported layouts
//!
//! Decode (IHDR colour type / bit depth → [`PixelFormat`]):
//!
//! | Colour type | Bit depth | Layout |
//! |---|---|---|
//! | 0 greyscale | 1 / 2 / 4 / 8 | `Gray8` (sub-byte scaled per §13.12: ×255 / ×85 / ×17) |
//! | 0 greyscale | 16 | `Gray16Le` |
//! | 2 truecolour | 8 / 16 | `Rgb24` / `Rgb48Le` |
//! | 3 indexed | 1 / 2 / 4 / 8 | `Pal8` (one index byte per pixel) + [`Palette`] |
//! | 4 grey + alpha | 8 / 16 | `Ya8` / `Rgba64Le` (grey replicated — no 16-bit grey+alpha layout) |
//! | 6 truecolour + alpha | 8 / 16 | `Rgba` / `Rgba64Le` |
//!
//! All five row filters, Adam7 interlacing, split `IDAT` runs, `tRNS`
//! keyed transparency on types 0 / 2 (carried as
//! [`PngImage::transparency`]), APNG (`acTL` / `fcTL` / `fdAT`, every
//! dispose / blend operator).
//!
//! Encode: every layout above at its natural depth (`Gray8` / `Pal8`
//! also at 1 / 2 / 4 bits via [`EncodeOptions::bit_depth`]), Adam7
//! opt-in, single `IDAT`, DEFLATE via `compcol` with per-row §12.8
//! heuristic or fixed filters; APNG via [`encode_apng`] (full-canvas
//! frames, one delay) or the region-aware [`encode_apng_frames`]
//! ([`ApngFrameSpec`]: sub-region, rational delay, dispose / blend,
//! optional separate default image). [`encode`] refuses what PNG
//! cannot carry with [`PngError::Unsupported`] (a non-identity colour
//! matrix) and never converts silently.
//!
//! # Options
//!
//! [`DecodeOptions`]: `max_width` / `max_height` / `max_pixels` /
//! `max_bytes` (checked against the header before any allocation;
//! default 1 GiB of decoded plane) and `strict` (ancillary-chunk
//! rules, see the type docs). [`EncodeOptions`]: `compression_level`
//! (`with_level`, 1..=9, default [`DEFAULT_COMPRESSION_LEVEL`]),
//! `filter_strategy`, `interlace`, `bit_depth`, `threads`, and
//! `metadata` ([`PngMetadata`], every ancillary chunk).
//!
//! # Metadata and colour
//!
//! [`PngImage::metadata`] carries the ICC profile (`iCCP`), Exif
//! (`eXIf`), XMP (the `XML:com.adobe.xmp` `iTXt`, [`XMP_KEYWORD`]) and
//! `gAMA` as `gamma`. [`PngImage::color`] is resolved by the W3C PNG3
//! §4.3 Table 1 precedence: `cICP` → its H.273 code points and range;
//! else `iCCP` → unspecified (the profile governs); else `sRGB` →
//! [`ColorInfo::srgb`]; else `cHRM` → `primaries` 1 (BT.709) or 9
//! (BT.2020) when the chromaticities match, `gAMA` never mapped to a
//! transfer code point; nothing → [`ColorInfo::png_default`]
//! (full-range RGB, unspecified primaries / transfer). Decoding
//! **never applies** gamma or colour management: `to_rgba8` is an
//! exact integer kernel per layout (16-bit samples drop the low byte,
//! as §13.12 permits — [`rescale_16bit_to_8bit`] is the linear
//! rescale). [`encode`] writes the chunks back: `gAMA` / `iCCP` /
//! `eXIf` / XMP from `metadata`, `sRGB` for exactly
//! [`ColorInfo::srgb`] (unless an ICC profile is present) or `cICP`
//! for any other specified colour, `tRNS` from `transparency` /
//! palette alpha; [`EncodeOptions::metadata`] wins where both name a
//! chunk. `decode(encode(img)) == img` for planes, colour, metadata,
//! palette and transparency.
//!
//! The full chunk set — `sBIT`, `pHYs`, `tIME`, `bKGD`, `hIST`,
//! `mDCV`, `cLLI`, `sPLT`, `tEXt`, `zTXt`, `iTXt`, unknown ancillary
//! chunks — is [`parse_metadata`] / [`PngMetadata`], round-tripped
//! through [`EncodeOptions::metadata`].
//!
//! # Limits
//!
//! Every function returns [`PngError`] on hostile input, never
//! panics. Beyond [`DecodeOptions`], the decoder bounds the inflate at
//! the exact filtered-stream size the header implies (decompression
//! bombs, §13.3), caps inflated metadata bodies at
//! [`MAX_INFLATED_METADATA_LEN`], and validates every CRC.
//!
//! # PNG specifics
//!
//! Opt-in colour transforms sit beside the codec: the [`gamma`] module
//! performs §13.13 decoder gamma handling, [`srgb`] the IEC 61966-2-1
//! transfer function and linear-light compositing
//! ([`decode_over_background`] is the §13.15 "display against a
//! background" path), [`depth`] the §12.4 / §13.12 sample-depth
//! scaling. [`decode_apng`] / [`parse_apng`] expose the APNG model
//! ([`ApngImage`] with `num_plays`, [`ApngInfo`] with the raw frame
//! chain) beyond what [`decode_all`] returns.

// When built without the `registry` feature, the `Decoder`/`Encoder`
// trait wrappers don't exist so a few standalone helpers go unused on
// that build. Suppress crate-wide rather than gating each individually.
#![cfg_attr(not(feature = "registry"), allow(dead_code))]

mod api;
pub mod apng;
pub mod chunk;
#[cfg(feature = "registry")]
pub mod container;
pub mod decoder;
pub mod depth;
pub mod encoder;
pub mod error;
pub mod filter;
pub mod gamma;
pub mod image;
pub mod metadata;
mod options;
#[cfg(feature = "registry")]
pub mod registry;
mod sideinfo;
pub mod srgb;
mod srgb_tables;
mod zlibvec;
mod zstream;

// ---- The image-crate contract (IMAGE_CRATE_API) ---------------------------
// Root vocabulary, identical across every oxideav image crate; works
// with `default-features = false`.
pub use api::{
    decode, decode_all, decode_all_with, decode_from, decode_rgb8, decode_rgba8, decode_with,
    encode, encode_rgb8, encode_rgba8, encode_to, info, probe,
};
#[allow(deprecated)]
pub use api::{decode_png, decode_png_over_background, decode_png_to_rgba};
pub use decoder::decode_over_background;
pub use encoder::EncodeOptions;
#[allow(deprecated)]
pub use encoder::PngEncoderOptions;
#[allow(deprecated)]
pub use encoder::{encode_png_image, encode_png_image_threaded, encode_png_image_with_options};
pub use error::{Error, PngError, Result};
#[allow(deprecated)]
pub use image::RgbaBitmap;
pub use image::{
    ColorInfo, ColorRange, Frame, ImageInfo, Metadata, Palette, PixelFormat, Plane, PngImage,
    PngPixelFormat, RgbImage, RgbaImage,
};
pub use options::DecodeOptions;
pub use sideinfo::XMP_KEYWORD;

// ---- PNG-specific depth (the contract is a floor, not a ceiling) ----------
pub use apng::{Blend as ApngBlend, Disposal as ApngDisposal};
pub use chunk::{ChunkType, ColourType};
pub use decoder::CODEC_ID_STR;
pub use decoder::{
    decode_apng, decode_apng_info, parse_apng, parse_metadata, ApngInfo, Ihdr,
    DEFAULT_BACKGROUND_GREY,
};
pub use depth::{
    max_sample, recover_sbit, rescale_16bit_to_8bit, rescale_16bit_to_8bit_via_sbit,
    rescale_sample, scale_up_bit_replication, scale_up_zero_fill,
};
pub use encoder::{
    encode_apng, encode_apng_frames, encode_apng_frames_threaded, encode_apng_frames_with_options,
    encode_apng_threaded, encode_apng_with_options, ApngFrameSpec, DEFAULT_COMPRESSION_LEVEL,
};
pub use filter::{FilterStrategy, FilterType};
pub use gamma::{
    apply_gama_to_palette, apply_gama_to_png16, apply_gama_to_rgba,
    apply_to_palette as apply_gamma_to_palette, apply_to_png16 as apply_gamma_to_png16,
    apply_to_rgba as apply_gamma_to_rgba, GammaParams,
};
pub use image::{ApngFrameImage, ApngImage};
pub use metadata::{
    Bkgd, Chrm, Cicp, Clli, ColourSource, Exif, Gama, Hist, Iccp, Itxt, Mdcv, Phys, PhysUnit,
    PngMetadata, RenderingIntent, Sbit, Splt, SpltEntry, Srgb, Text, Time, Trns, UnknownChunk,
    Ztxt, MAX_INFLATED_METADATA_LEN,
};
pub use srgb::{
    composite_over_background, from_linear as srgb_from_linear, linearize_rgba,
    to_linear8 as srgb_to_linear8, to_scaled_linear8 as srgb_to_scaled_linear8,
};

// Public registry-gated API — keeps the framework integration surface
// (Decoder/Encoder/Demuxer/Muxer trait impls, `register*` helpers,
// `decode_png_to_frame` / `encode_single*` `VideoFrame` wrappers and
// the `PngImage` ⇄ `VideoFrame` conversions) behind the default-on
// `registry` feature so image-library callers can build the crate
// without dragging in `oxideav-core`.
#[cfg(feature = "registry")]
pub use registry::{
    __oxideav_entry, decode_png_to_frame, encode_single, encode_single_with_options,
    from_color_signal, make_decoder, make_encoder, register, register_codecs, register_containers,
    to_color_signal, to_core_pixel_format, PngDecoder, PngEncoder,
};
