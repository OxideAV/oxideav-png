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
//!
//! Not implemented:
//! * Adam7 interlaced encode (decode only)
//! * Sub-byte encode (decode only — encoder always writes 8/16-bit)
//! * Colour management / metadata chunks (`cICP`, `sRGB`, `gAMA`, `cHRM`,
//!   `iCCP`, `tEXt`, `zTXt`, `iTXt`, `tIME`, `pHYs`, `sBIT`, `bKGD`, `hIST`,
//!   `sPLT`). CRC is verified on read and then they are dropped — they are
//!   not round-tripped through the container and not surfaced on decode.
//! * `tRNS` alpha application to decoded `Gray8` / `Gray16Le` / `Rgb24` /
//!   `Rgb48Le` pixels. For colour type 3 (palette), `tRNS` per-entry alpha
//!   is preserved verbatim in `CodecParameters::extradata` alongside `PLTE`
//!   so encoders can rewrite it, but the decoded `Pal8` plane itself
//!   carries no alpha.

pub mod apng;
pub mod chunk;
pub mod container;
pub mod decoder;
pub mod encoder;
pub mod filter;

pub use decoder::{decode_png_to_frame, CODEC_ID_STR};
pub use encoder::encode_single;

/// Register the PNG codec (both decoder and encoder).
pub fn register_codecs(reg: &mut oxideav_codec::CodecRegistry) {
    container::register_codecs(reg);
}

/// Register the PNG / APNG container (demuxer + muxer + extensions + probe).
pub fn register_containers(reg: &mut oxideav_container::ContainerRegistry) {
    container::register_containers(reg);
}

/// Combined registration: codecs + containers.
pub fn register(
    codecs: &mut oxideav_codec::CodecRegistry,
    containers: &mut oxideav_container::ContainerRegistry,
) {
    register_codecs(codecs);
    register_containers(containers);
}
