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

    /// The input exceeded a size limit — the configured one, or the format's
    /// structural ceiling.
    #[error("input {size} bytes exceeds limit {limit}")]
    InputTooLarge {
        /// Actual input length in bytes.
        size: usize,
        /// The limit that tripped, in bytes.
        ///
        /// Usually the configured
        /// [`ConfigBuilder::max_input_bytes`](crate::ConfigBuilder::max_input_bytes).
        /// The parser reports `u32::MAX` instead when the input cannot be addressed
        /// by the `u32` offsets a [`Span`](crate::tape::Span) holds — a structural
        /// ceiling no setter can raise, reachable by configuring `max_input_bytes`
        /// to 4 GiB or more or by calling [`tape::parse`](crate::tape::parse) directly. So
        /// this value is not always one the caller configured.
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
    #[error("input is not valid UTF-8 at byte {byte_offset}")]
    NotUtf8 {
        /// Byte offset of the first invalid sequence.
        ///
        /// This is [`Utf8Error::valid_up_to`], so everything before it did decode.
        /// It is the byte the caller has to look at, which the bare sentence this
        /// used to carry made them find for themselves -- on inputs where the
        /// answer is one byte somewhere in a megabyte of log capture.
        ///
        /// [`Utf8Error::valid_up_to`]: core::str::Utf8Error::valid_up_to
        byte_offset: usize,
    },
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

    /// The archive was structurally invalid.
    ///
    /// The word is "archive" rather than "payload" because every offset this
    /// reports lies in the header: the fixed fields at offsets 4, 5, 6 and 8 — each
    /// missing when the archive ends before it, or a
    /// version `0` (a higher version is `UnsupportedVersion` instead), and an encoder id, tokenizer id or flag word
    /// other than the one v0.0.1 writes (a flag bit this build names and never
    /// sets — bits 0 to 2 — is reported here, at 8; a bit in the reserved range 3
    /// to 15 is `ReservedBitsSet` instead); the ULEB128 length field, reported at
    /// the faulting byte (10 to 19) when a byte is missing, overlong or would
    /// overflow the `u64`, at 20 — one past the field — when the tenth byte is
    /// `0x80` or `0x81`, the only two values that keep the continuation bit without
    /// overflowing (overflow is checked first, so nine `0x80` bytes and a `0x82`
    /// say 19, not 20), and at 10 when a well-formed length disagrees with the
    /// payload that follows it; and a checksum the archive ends inside,
    /// reported at the offset the checksum should begin (11 to 20). The one site
    /// that would name a position in the payload region — `decompress` finding no
    /// bytes at `payload_start` — cannot fire, because `Header::decode` returns as
    /// `payload_start` the end of a checksum slice it has just required to exist.
    /// "Payload corrupted at byte 8" pointed a reader at the eighth byte of a
    /// payload that starts past the checksum.
    #[error("archive corrupted at byte {byte_offset}")]
    Corrupt {
        /// Byte offset at which decoding stopped, counted from the start of the
        /// archive — so every offset lands in the `TKFD` header, whose layout the
        /// CLI README documents.
        ///
        /// This is where the decoder *needed* a byte, which is not always a byte
        /// the archive has: an archive that ends early reports the offset it ran
        /// out at, so the value can equal or exceed `archive.len()`. Treat it as a
        /// position in the format, not as an index into the buffer — indexing an
        /// archive with it can panic.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use crate::{Compressor, Config};

    fn rejected(config: Config, input: &[u8]) -> String {
        Compressor::new(config)
            .compress(input)
            .unwrap_err()
            .to_string()
    }

    /// The whole line, numbers included, for every compression failure.
    ///
    /// The CLI prints these after `passing input through uncompressed:` from a
    /// `compress` that passed its input through, inside `the input was rejected (…) and
    /// passing it through failed` or `… and not passed through` from one that could
    /// not, and after `cannot compute stats:` from `stats`, and the MCP server returns
    /// them as the reason a call was declined, so the number in each is what a reader
    /// acts on. Tests elsewhere match the variant; a variant can keep its fields while
    /// its message drops or swaps them. Each input is one the engine really rejects — a
    /// hand-built value would only prove the format string.
    #[test]
    fn every_compress_error_prints_the_numbers_it_carries() {
        let limited = |bytes: usize, depth: usize| {
            Config::builder()
                .max_input_bytes(bytes)
                .max_depth(depth)
                .build()
        };
        assert_eq!(
            rejected(limited(4, 512), b"[1,2,3]"),
            "input 7 bytes exceeds limit 4"
        );
        assert_eq!(
            rejected(limited(1 << 24, 2), b"[[[1]]]"),
            "nesting depth 3 exceeds limit 2"
        );
        // The default limit, where the CLI README says 512 is accepted and 513 is not:
        // the depth reported is the first level past the limit, not how deep the input
        // goes.
        assert_eq!(
            rejected(Config::default(), "[".repeat(600).as_bytes()),
            "nesting depth 513 exceeds limit 512"
        );
        assert_eq!(
            rejected(Config::default(), b"[\"a\xff\"]"),
            "input is not valid UTF-8 at byte 3"
        );
        assert_eq!(
            rejected(Config::default(), b"[1,]"),
            "invalid JSON at byte 3: expected a JSON value"
        );
    }

    /// The same for decoding: one real archive, one header byte changed per case.
    #[test]
    fn every_decompress_error_prints_the_numbers_it_carries() {
        let compressor = Compressor::new(Config::default());
        let archive = compressor.compress(b"{\"a\": 1}").unwrap().archive;
        let with = |offset: usize, value: u8| {
            let mut bytes = archive.clone();
            bytes[offset] = value;
            compressor.decompress(&bytes).unwrap_err().to_string()
        };
        assert_eq!(with(0, b'X'), "not a recognized payload (bad magic)");
        assert_eq!(with(4, 2), "format version 2 > supported 1");
        assert_eq!(with(5, 9), "archive corrupted at byte 5");
        assert_eq!(with(8, 0x08), "reserved header bits are not zero");
        let last = archive.len() - 1;
        assert_eq!(with(last, archive[last] ^ 1), "integrity checksum mismatch");
    }
}
