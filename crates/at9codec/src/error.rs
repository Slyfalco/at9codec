//! Error types for the AT9 codec.

use core::fmt;

/// Result alias used throughout the crate.
pub type Result<T> = core::result::Result<T, Error>;

/// Errors that can occur parsing or coding an AT9 stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The buffer is not a RIFF/WAVE file or is truncated.
    NotRiff,
    /// A required chunk (`fmt `/`fact`/`data`) was missing.
    MissingChunk(&'static str),
    /// The `fmt ` chunk is not an ATRAC9 WAVE_FORMAT_EXTENSIBLE descriptor.
    NotAtrac9,
    /// A chunk was present but too short to parse.
    Truncated(&'static str),
    /// The ATRAC9 config data (4 bytes in the `fmt ` extra) is invalid.
    BadConfig,
    /// An explicit encode bitrate was requested that is not a standard ATRAC9
    /// rate for the stream's channel count and sample rate. Use 0 (auto) or one
    /// of [`crate::encode::encoder::standard_bitrates`].
    InvalidBitrate,
    /// A size limit was set but the audio is too long to fit it even at the lowest
    /// standard bitrate — shorten the track or raise the limit.
    ExceedsSizeLimit,
    /// Feature not yet implemented (e.g. decode/encode in progress).
    NotImplemented(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotRiff => write!(f, "not a RIFF/WAVE file"),
            Error::MissingChunk(c) => write!(f, "missing required chunk: {c}"),
            Error::NotAtrac9 => write!(f, "fmt chunk is not an ATRAC9 descriptor"),
            Error::Truncated(c) => write!(f, "chunk too short to parse: {c}"),
            Error::BadConfig => write!(f, "invalid ATRAC9 config data"),
            Error::InvalidBitrate => {
                write!(f, "bitrate is not a standard ATRAC9 rate for this stream")
            }
            Error::ExceedsSizeLimit => {
                write!(f, "audio is too long to fit the size limit at any standard bitrate")
            }
            Error::NotImplemented(w) => write!(f, "not implemented yet: {w}"),
        }
    }
}

impl std::error::Error for Error {}
