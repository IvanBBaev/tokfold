//! The `TKFD` archive header: encode, decode, integrity checksum.
//!
//! The archive is a fixed-order header followed by an encoder-specific payload:
//!
//! ```text
//! magic[4] | version:u8 | encoder_id:u8 | tokenizer_id:u16 | flags:u16
//!          | original_len:uleb128 | checksum[32] | payload…
//! ```
//!
//! Invariants this module upholds:
//!
//! * **All multi-byte integers are little-endian.** `original_len` is an unsigned
//!   `LEB128` varint (max 10 bytes); overlong and overflowing encodings are rejected
//!   as [`DecompressError::Corrupt`], never silently normalized.
//! * **Encoding is deterministic and bit-stable.** The same [`Header`] produces the
//!   same bytes on every call and every build. Nondeterministic output would
//!   invalidate a provider's prompt cache.
//! * **The checksum is `SHA-256` of the *original* input**, not of the payload. It
//!   is verified against the reconstructed original after decoding, so a decoder
//!   never returns partially-recovered bytes.
//! * **Fail closed.** Bad magic, an unknown version, a set reserved bit, a
//!   malformed varint or a checksum mismatch each return an error rather than a
//!   best-effort guess.
//!
//! This module owns only the framing. Reconstructing the original from the payload
//! (and thus the final checksum verification via [`verify_checksum`]) belongs to the
//! decoder that knows the payload's encoder.

use crate::error::DecompressError;

/// Literal magic that opens every `TKFD` archive.
pub const MAGIC: [u8; 4] = *b"TKFD";

/// Format version this build writes. The decoder accepts every version ever
/// shipped; only version `1` exists.
pub const VERSION: u8 = 1;

/// Number of checksum bytes (`SHA-256` digest width).
const CHECKSUM_LEN: usize = 32;

/// Byte offset of the `version` field, derived from the frozen layout.
const VERSION_OFFSET: usize = MAGIC.len();
/// Byte offset of the `encoder_id` field.
const ENCODER_ID_OFFSET: usize = VERSION_OFFSET + 1;
/// Byte offset of the little-endian `tokenizer_id` field.
const TOKENIZER_ID_OFFSET: usize = ENCODER_ID_OFFSET + 1;
/// Byte offset of the little-endian `flags` field.
const FLAGS_OFFSET: usize = TOKENIZER_ID_OFFSET + 2;
/// Byte offset of the `original_len` varint.
const ORIGINAL_LEN_OFFSET: usize = FLAGS_OFFSET + 2;

/// Mask of `flags` bits 3..=15, all of which MUST be zero in this version.
const FLAGS_RESERVED_MASK: u16 = 0xFFF8;

/// Maximum bytes an unsigned `LEB128`-encoded `u64` may occupy.
const ULEB128_MAX_BYTES: usize = 10;

/// Value bits of a `LEB128` byte.
const LOW_SEVEN_BITS: u8 = 0x7F;

/// Continuation bit of a `LEB128` byte.
const CONTINUATION_BIT: u8 = 0x80;

/// Decoded `flags` field (bits 0..=2). Reserved bits 3..=15 are validated on
/// decode and are never represented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    /// Bit 0: a byte-exact sidecar accompanies the archive.
    ///
    /// Reserved in v0.0.1: nothing sets it. No encoder emits a sidecar, and the
    /// `tokfold` binary has no flag that asks for one — the bit exists so the
    /// opt-in byte-exact mode can arrive without a format version bump. It is
    /// still decoded and round-tripped, so an archive written by a future build
    /// reads back with the bit intact.
    pub has_sidecar: bool,
    /// Bit 1: the payload may be safely truncated by a size-limited consumer.
    pub truncation_tolerated: bool,
    /// Bit 2: at least one segment was encoded by a lossy codec.
    pub segment_lossy: bool,
}

impl Flags {
    /// Packs the three flag bits into the on-wire `u16`. Reserved bits 3..=15 are
    /// zero by construction, so a round trip through [`Flags::from_bits`] is exact.
    #[must_use]
    pub fn to_bits(self) -> u16 {
        let mut bits = 0u16;
        if self.has_sidecar {
            bits |= 0b0001;
        }
        if self.truncation_tolerated {
            bits |= 0b0010;
        }
        if self.segment_lossy {
            bits |= 0b0100;
        }
        bits
    }

    /// Parses the on-wire `flags` field.
    ///
    /// Any reserved bit (3..=15) being set is a forward-compatibility hazard — a
    /// future flag this build cannot honour — and returns
    /// [`DecompressError::ReservedBitsSet`].
    pub fn from_bits(bits: u16) -> Result<Self, DecompressError> {
        if bits & FLAGS_RESERVED_MASK != 0 {
            return Err(DecompressError::ReservedBitsSet);
        }
        Ok(Self {
            has_sidecar: bits & 0b0001 != 0,
            truncation_tolerated: bits & 0b0010 != 0,
            segment_lossy: bits & 0b0100 != 0,
        })
    }
}

/// A decoded `TKFD` header. Fields are readable, but the struct is
/// `#[non_exhaustive]`: construct it through [`Header::new`] so future fields
/// cannot silently default to a wrong value.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Which sealed encoder produced the payload (`0` = Passthrough).
    pub encoder_id: u8,
    /// The slot the wire format reserves for the tokenizer under which the
    /// encoder-selection candidate rule was evaluated. v0.0.1 always writes `0` here and
    /// reports the configured cost model out-of-band on [`crate::Stats`]; `decompress`
    /// rejects any other value as `Corrupt`.
    pub tokenizer_id: u16,
    /// Optional-feature flags (bits 0..=2).
    pub flags: Flags,
    /// Byte length of the original input, cross-checked after reconstruction.
    pub original_len: u64,
    /// `SHA-256` digest of the original input bytes.
    pub checksum: [u8; CHECKSUM_LEN],
}

impl Header {
    /// Builds a header for encoding. `version` and `magic` are not parameters —
    /// this build always writes [`VERSION`] and [`MAGIC`].
    #[must_use]
    pub const fn new(
        encoder_id: u8,
        tokenizer_id: u16,
        flags: Flags,
        original_len: u64,
        checksum: [u8; CHECKSUM_LEN],
    ) -> Self {
        Self {
            encoder_id,
            tokenizer_id,
            flags,
            original_len,
            checksum,
        }
    }

    /// Appends the encoded header to `out`. The payload, if any, is appended by the
    /// caller. Deterministic: identical headers always emit identical bytes.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(self.encoder_id);
        out.extend_from_slice(&self.tokenizer_id.to_le_bytes());
        out.extend_from_slice(&self.flags.to_bits().to_le_bytes());
        write_uleb128(self.original_len, out);
        out.extend_from_slice(&self.checksum);
    }

    /// Decodes the header prefix of `archive`, returning the header and the byte
    /// offset at which the payload begins.
    ///
    /// This validates framing only — magic, version, reserved flag bits and the
    /// varint. It does **not** verify the checksum, which requires the
    /// reconstructed original; the decoder calls [`verify_checksum`] after decoding
    /// the payload. Failures:
    ///
    /// * bytes do not start with [`MAGIC`] → [`DecompressError::BadMagic`];
    /// * `version` greater than [`VERSION`] → [`DecompressError::UnsupportedVersion`];
    /// * any other unshipped version, or a field truncated at its boundary →
    ///   [`DecompressError::Corrupt`];
    /// * a set reserved flag bit → [`DecompressError::ReservedBitsSet`];
    /// * an overlong or overflowing varint → [`DecompressError::Corrupt`].
    pub fn decode(archive: &[u8]) -> Result<(Self, usize), DecompressError> {
        if archive.get(0..MAGIC.len()) != Some(MAGIC.as_slice()) {
            return Err(DecompressError::BadMagic);
        }

        let Some(&version) = archive.get(VERSION_OFFSET) else {
            return Err(DecompressError::Corrupt {
                byte_offset: VERSION_OFFSET,
            });
        };
        if version > VERSION {
            return Err(DecompressError::UnsupportedVersion {
                found: u16::from(version),
                supported: u16::from(VERSION),
            });
        }
        if version != VERSION {
            // Only version 1 has ever shipped; a lower value is a malformed header.
            return Err(DecompressError::Corrupt {
                byte_offset: VERSION_OFFSET,
            });
        }

        let Some(&encoder_id) = archive.get(ENCODER_ID_OFFSET) else {
            return Err(DecompressError::Corrupt {
                byte_offset: ENCODER_ID_OFFSET,
            });
        };

        let tokenizer_id = match archive.get(TOKENIZER_ID_OFFSET..TOKENIZER_ID_OFFSET + 2) {
            Some(&[b0, b1]) => u16::from_le_bytes([b0, b1]),
            _ => {
                return Err(DecompressError::Corrupt {
                    byte_offset: TOKENIZER_ID_OFFSET,
                });
            }
        };

        let flag_bits = match archive.get(FLAGS_OFFSET..FLAGS_OFFSET + 2) {
            Some(&[b0, b1]) => u16::from_le_bytes([b0, b1]),
            _ => {
                return Err(DecompressError::Corrupt {
                    byte_offset: FLAGS_OFFSET,
                });
            }
        };
        let flags = Flags::from_bits(flag_bits)?;

        let (original_len, after_len) = read_uleb128(archive, ORIGINAL_LEN_OFFSET)?;

        let Some(checksum_end) = after_len.checked_add(CHECKSUM_LEN) else {
            return Err(DecompressError::Corrupt {
                byte_offset: after_len,
            });
        };
        let checksum = match archive.get(after_len..checksum_end) {
            Some(slice) => {
                let mut buf = [0u8; CHECKSUM_LEN];
                buf.copy_from_slice(slice);
                buf
            }
            None => {
                return Err(DecompressError::Corrupt {
                    byte_offset: after_len,
                });
            }
        };

        Ok((
            Self {
                encoder_id,
                tokenizer_id,
                flags,
                original_len,
                checksum,
            },
            checksum_end,
        ))
    }
}

/// Computes the `SHA-256` digest used as an archive's integrity checksum.
#[must_use]
pub fn sha256(input: &[u8]) -> [u8; CHECKSUM_LEN] {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(input);
    let digest = hasher.finalize();
    let mut out = [0u8; CHECKSUM_LEN];
    // `digest` is always exactly `CHECKSUM_LEN` bytes, so the lengths match.
    out.copy_from_slice(digest.as_slice());
    out
}

/// Verifies that `original` hashes to `expected`.
///
/// The decoder calls this against the *reconstructed* original, after decoding the
/// payload, before returning any bytes. A mismatch is
/// [`DecompressError::ChecksumMismatch`].
pub fn verify_checksum(
    expected: &[u8; CHECKSUM_LEN],
    original: &[u8],
) -> Result<(), DecompressError> {
    if sha256(original) == *expected {
        Ok(())
    } else {
        Err(DecompressError::ChecksumMismatch)
    }
}

/// Appends `value` as an unsigned `LEB128` varint. Emits the minimal (canonical)
/// encoding, so decode is a bijection over well-formed input.
fn write_uleb128(mut value: u64, out: &mut Vec<u8>) {
    loop {
        // `value & 0x7F` is at most 127 and always fits in a `u8`; the fallback is
        // unreachable and only present because the conversion is typed as fallible.
        let low = u8::try_from(value & u64::from(LOW_SEVEN_BITS)).unwrap_or(0);
        value >>= 7;
        if value == 0 {
            out.push(low);
            return;
        }
        out.push(low | CONTINUATION_BIT);
    }
}

/// Reads an unsigned `LEB128` varint starting at `start`, returning the value and
/// the offset just past it.
///
/// Rejects, all as [`DecompressError::Corrupt`]:
///
/// * a varint that runs off the end of `archive`;
/// * an overlong encoding (a trailing zero group that a canonical encoder omits);
/// * a value that does not fit in `u64` (overflow), including one that fails to
///   terminate within [`ULEB128_MAX_BYTES`] bytes.
fn read_uleb128(archive: &[u8], start: usize) -> Result<(u64, usize), DecompressError> {
    let mut result: u64 = 0;
    for i in 0..ULEB128_MAX_BYTES {
        let offset = start.saturating_add(i);
        let Some(&byte) = archive.get(offset) else {
            return Err(DecompressError::Corrupt {
                byte_offset: offset,
            });
        };
        let shift = match u32::try_from(i) {
            Ok(step) => step.saturating_mul(7),
            Err(_) => {
                return Err(DecompressError::Corrupt {
                    byte_offset: offset,
                });
            }
        };
        let payload = u64::from(byte & LOW_SEVEN_BITS);
        // `payload << shift` must not drop bits off the top of the `u64`.
        if payload > (u64::MAX >> shift) {
            return Err(DecompressError::Corrupt {
                byte_offset: offset,
            });
        }
        result |= payload << shift;
        if byte & CONTINUATION_BIT == 0 {
            // A terminating zero byte after the first position encodes a value that
            // fits in fewer bytes — a non-canonical (overlong) encoding.
            if i > 0 && byte == 0 {
                return Err(DecompressError::Corrupt {
                    byte_offset: offset,
                });
            }
            return Ok((result, offset.saturating_add(1)));
        }
    }
    // Continuation bit still set after the maximum number of bytes.
    Err(DecompressError::Corrupt {
        byte_offset: start.saturating_add(ULEB128_MAX_BYTES),
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::needless_range_loop
    )]

    use super::*;

    fn checksum_ramp() -> [u8; CHECKSUM_LEN] {
        let mut c = [0u8; CHECKSUM_LEN];
        for (i, b) in c.iter_mut().enumerate() {
            *b = u8::try_from(i).unwrap();
        }
        c
    }

    /// Header bytes only (no payload), for corrupting fixed fields in place.
    fn header_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        Header::new(0, 0, Flags::default(), 0, [0u8; CHECKSUM_LEN]).encode_into(&mut out);
        out
    }

    /// A complete Passthrough archive whose payload is the original bytes.
    fn passthrough_archive(original: &[u8]) -> Vec<u8> {
        let header = Header::new(
            0,
            0,
            Flags::default(),
            u64::try_from(original.len()).unwrap(),
            sha256(original),
        );
        let mut out = Vec::new();
        header.encode_into(&mut out);
        out.extend_from_slice(original);
        out
    }

    /// Assembles an archive with an arbitrary raw `original_len` field, for
    /// exercising the varint decoder directly.
    fn archive_with_len_bytes(len_bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(0);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(len_bytes);
        out.extend_from_slice(&[0u8; CHECKSUM_LEN]);
        out
    }

    /// The v0.0.1 fail-closed reader for a Passthrough archive: it accepts only the
    /// exact canonical header this build writes, then verifies the reconstructed
    /// original. Used by the single-bit-flip test.
    fn decode_full(archive: &[u8]) -> Result<Vec<u8>, DecompressError> {
        let (header, offset) = Header::decode(archive)?;
        // Passthrough is the only encoder framing this module can reconstruct, and
        // v0.0.1 writes no flags and the default tokenizer id. Anything else could
        // not have been produced by this build, so fail closed.
        if header.encoder_id != 0 {
            return Err(DecompressError::Corrupt {
                byte_offset: ENCODER_ID_OFFSET,
            });
        }
        if header.tokenizer_id != 0 {
            return Err(DecompressError::Corrupt {
                byte_offset: TOKENIZER_ID_OFFSET,
            });
        }
        if header.flags != Flags::default() {
            return Err(DecompressError::Corrupt {
                byte_offset: FLAGS_OFFSET,
            });
        }
        let Some(payload) = archive.get(offset..) else {
            return Err(DecompressError::Corrupt {
                byte_offset: offset,
            });
        };
        let expected_len =
            usize::try_from(header.original_len).map_err(|_| DecompressError::Corrupt {
                byte_offset: ORIGINAL_LEN_OFFSET,
            })?;
        if payload.len() != expected_len {
            return Err(DecompressError::ChecksumMismatch);
        }
        verify_checksum(&header.checksum, payload)?;
        Ok(payload.to_vec())
    }

    #[test]
    fn roundtrip_all_fields() {
        let checksum = checksum_ramp();
        let flags = Flags {
            has_sidecar: true,
            truncation_tolerated: true,
            segment_lossy: true,
        };
        let header = Header::new(2, 0xBEEF, flags, 300_000, checksum);

        let mut buf = Vec::new();
        header.encode_into(&mut buf);
        let (decoded, payload_start) = Header::decode(&buf).unwrap();

        assert_eq!(decoded, header);
        assert_eq!(payload_start, buf.len());
        assert_eq!(decoded.encoder_id, 2);
        assert_eq!(decoded.tokenizer_id, 0xBEEF);
        assert_eq!(decoded.flags, flags);
        assert_eq!(decoded.original_len, 300_000);
        assert_eq!(decoded.checksum, checksum);
    }

    #[test]
    fn encoding_is_bit_stable() {
        let header = Header::new(1, 7, Flags::default(), 42, checksum_ramp());
        let mut a = Vec::new();
        let mut b = Vec::new();
        header.encode_into(&mut a);
        header.encode_into(&mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn decode_returns_payload_offset() {
        let original = b"payload-bytes-here";
        let archive = passthrough_archive(original);
        let (header, offset) = Header::decode(&archive).unwrap();
        assert_eq!(header.original_len, u64::try_from(original.len()).unwrap());
        assert_eq!(&archive[offset..], original);
    }

    #[test]
    fn roundtrip_varint_boundaries() {
        for &len in &[
            0u64,
            1,
            127,
            128,
            16_383,
            16_384,
            2_097_151,
            2_097_152,
            u64::from(u32::MAX),
            u64::MAX,
        ] {
            let header = Header::new(1, 0, Flags::default(), len, [0u8; CHECKSUM_LEN]);
            let mut buf = Vec::new();
            header.encode_into(&mut buf);
            let (decoded, _) = Header::decode(&buf).unwrap();
            assert_eq!(decoded.original_len, len, "len {len} did not round-trip");
        }
    }

    #[test]
    fn bad_magic_rejected() {
        let mut corrupted = header_bytes();
        corrupted[0] ^= 0xFF;
        assert!(matches!(
            Header::decode(&corrupted),
            Err(DecompressError::BadMagic)
        ));
        assert!(matches!(
            Header::decode(b""),
            Err(DecompressError::BadMagic)
        ));
        assert!(matches!(
            Header::decode(b"TKF"),
            Err(DecompressError::BadMagic)
        ));
        assert!(matches!(
            Header::decode(b"XXXX"),
            Err(DecompressError::BadMagic)
        ));
    }

    #[test]
    fn version_too_new_rejected() {
        let mut two = header_bytes();
        two[VERSION_OFFSET] = 2;
        assert!(matches!(
            Header::decode(&two),
            Err(DecompressError::UnsupportedVersion {
                found: 2,
                supported: 1
            })
        ));

        let mut max = header_bytes();
        max[VERSION_OFFSET] = 255;
        assert!(matches!(
            Header::decode(&max),
            Err(DecompressError::UnsupportedVersion {
                found: 255,
                supported: 1
            })
        ));
    }

    #[test]
    fn version_zero_rejected() {
        let mut zero = header_bytes();
        zero[VERSION_OFFSET] = 0;
        assert!(Header::decode(&zero).is_err());
    }

    #[test]
    fn reserved_bits_rejected() {
        // Bit 3 lives in the low flags byte.
        let mut bit3 = header_bytes();
        bit3[FLAGS_OFFSET] |= 0b0000_1000;
        assert!(matches!(
            Header::decode(&bit3),
            Err(DecompressError::ReservedBitsSet)
        ));

        // Bit 15 lives in the high flags byte.
        let mut bit15 = header_bytes();
        bit15[FLAGS_OFFSET + 1] |= 0b1000_0000;
        assert!(matches!(
            Header::decode(&bit15),
            Err(DecompressError::ReservedBitsSet)
        ));

        // Bits 0..=2 are defined flags and must decode, not be rejected.
        let mut defined = header_bytes();
        defined[FLAGS_OFFSET] |= 0b0000_0111;
        let (header, _) = Header::decode(&defined).unwrap();
        assert!(header.flags.has_sidecar);
        assert!(header.flags.truncation_tolerated);
        assert!(header.flags.segment_lossy);
    }

    #[test]
    fn truncation_rejected_at_every_header_boundary() {
        let full = {
            let mut out = Vec::new();
            Header::new(1, 0x1234, Flags::default(), 0, [7u8; CHECKSUM_LEN]).encode_into(&mut out);
            out
        };
        for len in 0..full.len() {
            assert!(
                Header::decode(&full[..len]).is_err(),
                "prefix of length {len} decoded"
            );
        }
        let (_, offset) = Header::decode(&full).unwrap();
        assert_eq!(offset, full.len());
    }

    #[test]
    fn overlong_leb128_rejected() {
        // Value 0 and value 1 written in two bytes instead of one.
        for len_bytes in [[0x80u8, 0x00], [0x81, 0x00]] {
            let archive = archive_with_len_bytes(&len_bytes);
            assert!(matches!(
                Header::decode(&archive),
                Err(DecompressError::Corrupt { .. })
            ));
        }
    }

    #[test]
    fn leb128_overflow_rejected() {
        // Tenth byte carries value bits above bit 63.
        let overflow = [0x80u8, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02];
        assert!(matches!(
            Header::decode(&archive_with_len_bytes(&overflow)),
            Err(DecompressError::Corrupt { .. })
        ));

        // Ten bytes that never clear the continuation bit.
        let never_terminates = [0x80u8, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x81];
        assert!(matches!(
            Header::decode(&archive_with_len_bytes(&never_terminates)),
            Err(DecompressError::Corrupt { .. })
        ));

        // All-continuation bytes (also non-terminating / overflowing).
        let all_continue = [0xFFu8; ULEB128_MAX_BYTES];
        assert!(matches!(
            Header::decode(&archive_with_len_bytes(&all_continue)),
            Err(DecompressError::Corrupt { .. })
        ));
    }

    #[test]
    fn checksum_matches_and_mismatches() {
        let data = b"the original bytes";
        let digest = sha256(data);
        assert!(verify_checksum(&digest, data).is_ok());
        assert!(matches!(
            verify_checksum(&digest, b"the original byteS"),
            Err(DecompressError::ChecksumMismatch)
        ));
    }

    #[test]
    fn sha256_known_answer() {
        // NIST FIPS 180-2 test vector for "abc".
        let expected = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(sha256(b"abc"), expected);
    }

    #[test]
    fn empty_original_roundtrips_through_full_decode() {
        let archive = passthrough_archive(b"");
        assert_eq!(decode_full(&archive).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn any_single_bit_flip_is_rejected() {
        let original = b"hello world";
        let archive = passthrough_archive(original);
        assert_eq!(decode_full(&archive).unwrap(), original);

        for (byte_idx, _) in archive.iter().enumerate() {
            for bit in 0u32..8 {
                let mut flipped = archive.clone();
                flipped[byte_idx] ^= 1u8.wrapping_shl(bit);
                assert!(
                    decode_full(&flipped).is_err(),
                    "flip at byte {byte_idx} bit {bit} was not rejected"
                );
            }
        }
    }
}
