//! One-shot zlib helpers shared by the IDAT / fdAT pixel paths and the
//! `zTXt` / `iTXt` / `iCCP` metadata chunks.
//!
//! RFC 1950 (zlib) / RFC 1951 (DEFLATE) framing is delegated to
//! `compcol`, the workspace-wide compression collection. PNG-side code
//! only ever needs whole-buffer compress / decompress, so these two
//! thin `Vec<u8>` wrappers are the crate's entire compression surface.
//!
//! Every decompression in this crate is **output-bounded**: a PNG chunk
//! can be up to 2^31-1 bytes long (W3C PNG3 §13.3 "chunks can be
//! extremely large") and DEFLATE expands up to ~1000:1, so an
//! unbounded inflate would let a small hostile chunk allocate
//! gigabytes before any later length check runs. Each call site knows
//! a hard bound — the IHDR-implied filtered-stream size for IDAT /
//! fdAT, [`crate::metadata::MAX_INFLATED_METADATA_LEN`] for the
//! metadata chunks — and the decoder is cut off at that bound with
//! `compcol::Error::OutputLimitExceeded` instead of growing the output
//! without limit.

use crate::error::{PngError, Result};
use compcol::zlib::{EncoderConfig, Zlib};

/// Compress `data` into a zlib (RFC 1950) stream at the given DEFLATE
/// level (1..=9). The crate uses level 6 — the zlib default — for every
/// stream it emits; PNG leaves the choice entirely to the encoder.
pub(crate) fn compress_to_vec_zlib(data: &[u8], level: u8) -> Result<Vec<u8>> {
    compcol::vec::compress_to_vec_with::<Zlib>(data, EncoderConfig { level })
        .map_err(|e| PngError::invalid(format!("PNG: zlib compression failed: {e:?}")))
}

/// Decompress a zlib (RFC 1950) stream, refusing to produce more than
/// `max_output` bytes of plaintext. A stream that would inflate past
/// the bound fails with `compcol::Error::OutputLimitExceeded` — the
/// decompression-bomb defence. The error stays raw (`compcol::Error`)
/// so each call site can wrap it with chunk-specific context (IDAT vs
/// `zTXt` vs `iCCP` …) and distinguish the bomb case from a corrupt
/// stream.
pub(crate) fn decompress_to_vec_zlib_capped(
    data: &[u8],
    max_output: u64,
) -> core::result::Result<Vec<u8>, compcol::Error> {
    compcol::vec::decompress_to_vec_capped::<Zlib>(data, max_output)
}
