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
//! * PLTE + tRNS palettes; tRNS keyed transparency on grayscale +
//!   truecolor sources (8 / 16-bit) applied via [`decode_png_to_rgba`]
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
//! * Sub-byte encode (1, 2, 4-bit) for colour type 0 (grayscale) and
//!   colour type 3 (indexed), opt-in via
//!   [`encoder::PngEncoderOptions`]`::bit_depth`. Source is a `Gray8`
//!   or `Pal8` buffer with each byte already pre-quantized to
//!   `0..=(1 << bit_depth) - 1`; pixels pack MSB-first per PNG §2.3.
//!   The interlaced + sub-byte combination is not yet implemented and
//!   is rejected as an encode error rather than producing wrong bytes.
//!
//! Not implemented:
//! * Adam7 interlaced sub-byte encode (one of `interlace = true` and
//!   `bit_depth = Some(1 | 2 | 4)`, not both)
//! * Round-tripped metadata: `sBIT`, `pHYs`, `tIME`, `bKGD`, `hIST`,
//!   `tRNS`, `eXIf`, `sRGB`, `cICP`, `iCCP`, `mDCV`, `cLLI`, `sPLT`,
//!   `tEXt`, `zTXt`, `iTXt` —
//!   surfaced via [`parse_metadata`] on decode and re-emitted by the
//!   encoder when [`PngEncoderOptions::metadata`] is populated. `eXIf`
//!   is carried as an opaque (TIFF-header-validated) blob; `sRGB`
//!   carries the ICC rendering intent; `cICP` carries the H.273
//!   colour-primaries / transfer-function / video-range code points
//!   with `matrix_coefficients` pinned at 0 (PNG is RGB-only per
//!   §11.3.2.6); `iCCP` carries an opaque ICC.1 profile blob alongside
//!   its 1-79-byte Latin-1 name (deflate-framed on the wire, decoded
//!   to a byte vector in memory); `sPLT` carries one or more named
//!   suggested palettes (8- or 16-bit RGBA + frequency entries), the
//!   one metadata chunk PNG allows to repeat with distinct names.
//!   `tEXt` carries free-form Latin-1 keyword + text pairs (RFC 2083
//!   §4.2.7); `zTXt` carries the same with the body zlib-compressed
//!   (RFC 2083 §4.2.10); `iTXt` is the UTF-8 successor with a BCP47
//!   language tag and an optional zlib-compressed UTF-8 text body
//!   (W3C PNG3 §11.3.3.4). All three text-chunk types may repeat with
//!   identical keywords.
//!   `tRNS` is round-tripped through [`PngMetadata::trns`] for colour
//!   types 0 (grayscale) / 2 (truecolor) / 3 (indexed); the variant
//!   matches the IHDR colour type ([`Trns::Grayscale`] / [`Trns::Rgb`]
//!   / [`Trns::Palette`]), colour types 4 and 6 are rejected (a full
//!   alpha channel is already present per RFC 2083 §4.2.9 final
//!   paragraph), and the encoder errors if both [`PngMetadata::trns`]
//!   and the legacy `image.palette` `PLTE || tRNS` tail are present
//!   (W3C PNG3 §5.6 Table 1 "Multiple OK? No"). Decode-side keyed-
//!   transparency promotion still happens in [`decode_png_to_rgba`]
//!   per §4.2.9 (both bytes of a 16-bit sample compared before the
//!   8-bit truncation).
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
pub use metadata::{
    Bkgd, Chrm, Cicp, Clli, Exif, Gama, Hist, Iccp, Itxt, Mdcv, Phys, PhysUnit, PngMetadata,
    RenderingIntent, Sbit, Splt, SpltEntry, Srgb, Text, Time, Trns, Ztxt,
};

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
