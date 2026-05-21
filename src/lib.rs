//! Pure-Rust PNG + APNG codec and container.
//!
//! Supports (decode):
//! * colour type 0 (grayscale) — 1/2/4/8/16-bit
//! * colour type 2 (RGB) — 8-bit, 16-bit
//! * colour type 3 (palette) — 1/2/4/8-bit
//! * colour type 4 (grayscale + alpha) — 8-bit, 16-bit
//! * colour type 6 (RGBA) — 8-bit, 16-bit
//! * all five PNG row filters (None / Sub / Up / Average / Paeth)
//! * Adam7 interlacing (seven-pass progressive)
//! * multiple IDAT chunks
//! * PLTE + tRNS palettes
//! * APNG animation: `acTL`, `fcTL`, `fdAT` with `None`/`Background`/`Previous`
//!   disposal and `Source`/`Over` blending.
//!
//! Sub-8-bit grayscale is expanded to `Gray8` (scaled per §13.12: ×255, ×85,
//! ×17 for 1/2/4-bit) on output. Sub-8-bit indexed is expanded to `Pal8`
//! (one palette index byte per pixel).
//!
//! Supports (encode):
//! * `Rgba` / `Rgb24` / `Gray8` / `Pal8` at 8-bit
//! * `Rgb48Le` / `Rgba64Le` / `Gray16Le` at 16-bit
//! * `Ya8` grayscale + alpha
//! * Single IDAT, DEFLATE via `miniz_oxide`, per-row heuristic filter
//!   selection (PNG §12.8 min-sum-abs-delta).
//! * APNG: `acTL` + per-frame `fcTL`/`fdAT` when `frame_rate` is set or
//!   more than one frame is submitted.
//! * Adam7 seven-pass interlaced encode, opt-in via
//!   [`encoder::PngEncoderOptions`]`::interlace` (or
//!   `CodecParameters::options` key `"interlace"`).
//!
//! Not implemented:
//! * Sub-byte encode (decode only — encoder always writes 8/16-bit)
//! * Round-tripped metadata: `sBIT`, `pHYs`, `tIME`, `bKGD`, `hIST` —
//!   surfaced via [`parse_metadata`] on decode and re-emitted by the
//!   encoder when [`PngEncoderOptions::metadata`] is populated.
//! * Colour management / metadata chunks (`cICP`, `sRGB`, `gAMA`, `cHRM`,
//!   `iCCP`, `tEXt`, `zTXt`, `iTXt`, `sPLT`). CRC is verified on read and
//!   then they are dropped — they are not round-tripped through the
//!   container and not surfaced on decode.
//! * `tRNS` alpha application to decoded `Gray8` / `Gray16Le` / `Rgb24` /
//!   `Rgb48Le` pixels. For colour type 3 (palette), `tRNS` per-entry alpha
//!   is preserved verbatim in `CodecParameters::extradata` alongside `PLTE`
//!   so encoders can rewrite it, but the decoded `Pal8` plane itself
//!   carries no alpha.
//!
//! ## Standalone (no `oxideav-core`) mode
//!
//! `oxideav-core` is gated behind the default-on `registry` feature. With
//! the feature off, the crate exposes a free-standing
//! [`decode_png`] / [`encode_png_image`] / [`decode_apng`] /
//! [`encode_apng`] API plus crate-local [`PngImage`] / [`PngError`]
//! types and never references `oxideav-core`. Image-library consumers
//! depend on this crate with `default-features = false` to skip the
//! framework dependency tree entirely.

// When built without the `registry` feature, the `Decoder`/`Encoder`
// trait wrappers don't exist so a few standalone helpers go unused on
// that build. Suppress crate-wide rather than gating each individually.
#![cfg_attr(not(feature = "registry"), allow(dead_code))]

pub mod apng;
pub mod chunk;
#[cfg(feature = "registry")]
pub mod container;
pub mod decoder;
pub mod encoder;
pub mod error;
pub mod filter;
pub mod image;
pub mod metadata;
#[cfg(feature = "registry")]
pub mod registry;

// Public unconditional API — works whether or not `registry` is enabled.
pub use decoder::CODEC_ID_STR;
pub use decoder::{
    decode_apng, decode_apng_info, decode_png, decode_png_to_rgba, parse_apng, parse_metadata,
    ApngInfo, Ihdr,
};
pub use encoder::{
    encode_apng, encode_apng_with_options, encode_png_image, encode_png_image_with_options,
    PngEncoderOptions,
};
pub use error::{PngError, Result};
pub use image::{ApngFrameImage, ApngImage, PngImage, PngPixelFormat, RgbaBitmap};
pub use metadata::{Bkgd, Hist, Phys, PhysUnit, PngMetadata, Sbit, Time};

// Public registry-gated API — keeps the framework integration surface
// (Decoder/Encoder/Demuxer/Muxer trait impls, `register*` helpers,
// `decode_png_to_frame` / `encode_single*` `VideoFrame` wrappers)
// behind the default-on `registry` feature so image-library callers can
// build the crate without dragging in `oxideav-core`.
#[cfg(feature = "registry")]
pub use registry::{
    __oxideav_entry, decode_png_to_frame, encode_single, encode_single_with_options, register,
    register_codecs, register_containers, PngDecoder, PngEncoder,
};
