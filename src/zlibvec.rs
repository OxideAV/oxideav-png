//! One-shot zlib helpers shared by the IDAT / fdAT pixel paths and the
//! `zTXt` / `iTXt` / `iCCP` metadata chunks.
//!
//! RFC 1950 (zlib) / RFC 1951 (DEFLATE) framing is delegated to
//! `compcol`, the workspace-wide compression collection. PNG-side code
//! only ever needs whole-buffer compress / decompress, so these two
//! thin `Vec<u8>` wrappers are the crate's entire compression surface.

use crate::error::{PngError, Result};
use compcol::zlib::{EncoderConfig, Zlib};

/// Compress `data` into a zlib (RFC 1950) stream at the given DEFLATE
/// level (1..=9). The crate uses level 6 — the zlib default — for every
/// stream it emits; PNG leaves the choice entirely to the encoder.
pub(crate) fn compress_to_vec_zlib(data: &[u8], level: u8) -> Result<Vec<u8>> {
    compcol::vec::compress_to_vec_with::<Zlib>(data, EncoderConfig { level })
        .map_err(|e| PngError::invalid(format!("PNG: zlib compression failed: {e:?}")))
}

/// Decompress a zlib (RFC 1950) stream. The error stays raw
/// (`compcol::Error`) so each call site can wrap it with
/// chunk-specific context (IDAT vs `zTXt` vs `iCCP` …).
pub(crate) fn decompress_to_vec_zlib(data: &[u8]) -> core::result::Result<Vec<u8>, compcol::Error> {
    compcol::vec::decompress_to_vec::<Zlib>(data)
}
