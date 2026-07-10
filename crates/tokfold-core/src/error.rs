//! Error taxonomy for the engine.
//!
//! Two rules govern this module:
//!
//! * **Compress errors are recoverable by design.** On any [`CompressError`] the
//!   caller passes the original bytes through unmodified. The tool must never eat
//!   an agent's tool output.
//! * **Decompress errors are contract violations.** They fail loudly and never
//!   return a best-effort guess.
//!
//! Core performs no I/O, so no variant wraps [`std::io::Error`].

/// Failure while compressing an input buffer.
///
/// Every variant is recoverable: the caller forwards the original input untouched.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CompressError {
    /// The input was not well-formed JSON.
    ///
    /// Also covers `NaN`/`Infinity` (which Python's `json.dumps` emits but JSON
    /// does not define), truncated documents and trailing garbage. Core never
    /// repairs input.
    #[error("invalid JSON at byte {byte_offset}: {msg}")]
    InvalidJson {
        /// Byte offset into the input where parsing failed.
        byte_offset: usize,
        /// Human-readable reason.
        msg: String,
    },

    /// The input exceeded the configured size limit.
    #[error("input {size} bytes exceeds limit {limit}")]
    InputTooLarge {
        /// Actual input length in bytes.
        size: usize,
        /// Configured limit in bytes.
        limit: usize,
    },

    /// The input nested deeper than the configured limit.
    ///
    /// Raised by an explicit depth counter; the parser is iterative and cannot
    /// overflow the stack.
    #[error("nesting depth {depth} exceeds limit {limit}")]
    DepthExceeded {
        /// Depth reached when the limit tripped.
        depth: usize,
        /// Configured maximum depth.
        limit: usize,
    },

    /// The input was not valid UTF-8.
    #[error("input is not valid UTF-8")]
    NotUtf8,
}

/// Failure while decoding a `TKFD` archive.
///
/// Decoding is fail-closed: any of these means the caller receives an error, never
/// partially recovered bytes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecompressError {
    /// The buffer did not start with the `TKFD` magic.
    #[error("not a recognized payload (bad magic)")]
    BadMagic,

    /// The archive was written by a newer format version than this build supports.
    #[error("format version {found} > supported {supported}")]
    UnsupportedVersion {
        /// Version read from the header.
        found: u16,
        /// Highest version this build can decode.
        supported: u16,
    },

    /// The payload was structurally invalid.
    #[error("payload corrupted at byte {byte_offset}")]
    Corrupt {
        /// Byte offset into the archive where decoding failed.
        byte_offset: usize,
    },

    /// The reconstructed original did not match the checksum in the header.
    #[error("integrity checksum mismatch")]
    ChecksumMismatch,

    /// A header bit documented as reserved-must-be-zero was set.
    ///
    /// Rejecting these keeps forward-compatible flags from being silently ignored.
    #[error("reserved header bits are not zero")]
    ReservedBitsSet,
}
