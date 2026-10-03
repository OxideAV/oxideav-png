//! Local error type used by `oxideav-png`'s standalone (no
//! `oxideav-core`) public API.
//!
//! When the `registry` feature is enabled, [`PngError`] gains a
//! `From<PngError> for oxideav_core::Error` impl (defined in
//! [`crate::registry`]) so the trait-side surface (`Decoder` /
//! `Encoder` / `Demuxer` / `Muxer`) can keep returning
//! `oxideav_core::Result<T>` while the underlying decode/encode
//! functions stay framework-free.

use std::fmt;

/// `Result` alias scoped to `oxideav-png`. Standalone (no
/// `oxideav-core`) callers see this; framework callers convert via
/// the gated `From<PngError> for oxideav_core::Error` impl.
pub type Result<T> = std::result::Result<T, PngError>;

/// The contract name for [`PngError`].
pub type Error = PngError;

/// Error variants returned by `oxideav-png`'s standalone API.
///
/// The variants mirror the subset of `oxideav_core::Error` the codec
/// can hit plus the two the image-crate contract requires
/// (`LimitExceeded`, `Io`). Framework-specific errors
/// (`FormatNotFound`, `CodecNotFound`) originate in callers that are
/// already linking `oxideav-core`.
#[derive(Debug)]
#[non_exhaustive]
pub enum PngError {
    /// The input bitstream / chunk stream is malformed (bad magic,
    /// truncated chunk, CRC mismatch, etc.).
    InvalidData(String),
    /// The bitstream uses a feature this decoder doesn't implement,
    /// or the encoder was asked to emit a frame format it doesn't
    /// support.
    Unsupported(String),
    /// A [`crate::DecodeOptions`] limit (dimensions / pixels / bytes)
    /// would be exceeded; nothing was allocated.
    LimitExceeded(String),
    /// A read / write on a caller-supplied stream failed
    /// ([`crate::decode_from`] / [`crate::encode_to`]).
    Io(std::io::Error),
    /// End of stream — no more packets / frames forthcoming.
    Eof,
    /// More input is required before another frame can be produced
    /// (decoder) or another packet can be flushed (encoder).
    NeedMore,
    /// Catch-all for everything else (e.g. caller protocol violations
    /// the trait surface needs to surface as `Other`).
    Other(String),
}

impl PngError {
    /// Construct a [`PngError::InvalidData`] from a stringy message.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidData(msg.into())
    }

    /// Construct a [`PngError::Unsupported`] from a stringy message.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }

    /// Construct a [`PngError::Other`] from a stringy message.
    pub fn other(msg: impl Into<String>) -> Self {
        Self::Other(msg.into())
    }

    /// Construct a [`PngError::LimitExceeded`] from a stringy message.
    pub fn limit(msg: impl Into<String>) -> Self {
        Self::LimitExceeded(msg.into())
    }
}

impl From<std::io::Error> for PngError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Display for PngError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(s) => write!(f, "invalid data: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported: {s}"),
            Self::LimitExceeded(s) => write!(f, "limit exceeded: {s}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Eof => write!(f, "end of stream"),
            Self::NeedMore => write!(f, "need more data"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for PngError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
