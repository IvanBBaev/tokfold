//! Base64 for carrying binary archives inside JSON strings.
//!
//! A tokfold archive is arbitrary bytes — a header, a checksum, and a payload — and
//! JSON strings hold text. Base64 is the encoding chosen over hex because this
//! project exists to spend fewer tokens: hex doubles the payload, base64 adds a
//! third. Standard alphabet (RFC 4648 §4), padded, no line breaks.
//!
//! The decoder is **strict**: it rejects misplaced padding, unknown symbols, and
//! non-canonical trailing bits. Strictness is what makes the encoding a bijection,
//! and the crate's contract is that an archive handed back to `decompress` is the
//! archive that came out of `compress`.

/// Standard base64 alphabet, RFC 4648 §4.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Padding character.
const PAD: char = '=';

/// Padding character as a byte, for comparisons against input.
const PAD_BYTE: u8 = b'=';

/// Encodes bytes as padded base64.
#[must_use]
pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3).saturating_mul(4));
    for chunk in input.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let group = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(symbol(group, 18));
        out.push(symbol(group, 12));
        out.push(if chunk.len() > 1 {
            symbol(group, 6)
        } else {
            PAD
        });
        out.push(if chunk.len() > 2 {
            symbol(group, 0)
        } else {
            PAD
        });
    }
    out
}

/// Extracts the six bits at `shift` and maps them to an alphabet symbol.
fn symbol(group: u32, shift: u32) -> char {
    let index = usize::try_from((group >> shift) & 0x3F).unwrap_or(0);
    ALPHABET.get(index).copied().map_or(PAD, char::from)
}

/// Why a base64 string could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The input length is not a multiple of four, so the padding is missing or wrong.
    #[error("base64 length is not a multiple of 4")]
    BadLength,
    /// A character outside the standard alphabet appeared.
    #[error("invalid base64 character")]
    BadSymbol,
    /// Padding appeared anywhere other than the tail of the final quartet.
    #[error("misplaced base64 padding")]
    MisplacedPadding,
    /// The final quartet sets bits that the decoded length discards, so the input is
    /// not the canonical encoding of any byte string.
    #[error("non-canonical base64 trailing bits")]
    NonCanonical,
}

/// Decodes padded base64.
///
/// # Errors
///
/// Returns [`DecodeError`] for a bad length, an unknown character, misplaced
/// padding, or non-canonical trailing bits.
pub fn decode(input: &str) -> Result<Vec<u8>, DecodeError> {
    let bytes = input.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err(DecodeError::BadLength);
    }
    let quartets = bytes.len() / 4;
    // Capacity is capped rather than exact. A hostile string usually fails on its
    // first quartet, and committing three quarters of the input's length before a
    // single character has been validated turns a rejected message into a real
    // allocation; past the cap the vector simply grows.
    let mut out = Vec::with_capacity(quartets.saturating_mul(3).min(64 * 1024));
    for (index, chunk) in bytes.chunks_exact(4).enumerate() {
        let is_last = index.saturating_add(1) == quartets;
        decode_quartet(chunk, is_last, &mut out)?;
    }
    Ok(out)
}

/// Decodes one four-character group, appending one to three bytes.
fn decode_quartet(chunk: &[u8], is_last: bool, out: &mut Vec<u8>) -> Result<(), DecodeError> {
    let (a, b, c, d) = (
        chunk.first().copied().ok_or(DecodeError::BadLength)?,
        chunk.get(1).copied().ok_or(DecodeError::BadLength)?,
        chunk.get(2).copied().ok_or(DecodeError::BadLength)?,
        chunk.get(3).copied().ok_or(DecodeError::BadLength)?,
    );

    // Padding is legal only as the tail of the final quartet, and `=` may never
    // precede a non-`=`. Both checks are needed: "QQ==QQ==" would otherwise decode.
    let padding = usize::from(c == PAD_BYTE) + usize::from(d == PAD_BYTE);
    if padding > 0 && !is_last {
        return Err(DecodeError::MisplacedPadding);
    }
    if c == PAD_BYTE && d != PAD_BYTE {
        return Err(DecodeError::MisplacedPadding);
    }
    if a == PAD_BYTE || b == PAD_BYTE {
        return Err(DecodeError::MisplacedPadding);
    }

    let va = sextet(a)?;
    let vb = sextet(b)?;
    let mut group = (va << 18) | (vb << 12);
    match padding {
        // Two pads: one output byte, so the low four bits of `b` are discarded and
        // must therefore be zero in a canonical encoding.
        2 => {
            if vb & 0x0F != 0 {
                return Err(DecodeError::NonCanonical);
            }
        }
        // One pad: two output bytes, so the low two bits of `c` are discarded.
        1 => {
            let vc = sextet(c)?;
            if vc & 0x03 != 0 {
                return Err(DecodeError::NonCanonical);
            }
            group |= vc << 6;
        }
        _ => {
            group |= sextet(c)? << 6;
            group |= sextet(d)?;
        }
    }

    out.push(octet(group, 16));
    if padding < 2 {
        out.push(octet(group, 8));
    }
    if padding < 1 {
        out.push(octet(group, 0));
    }
    Ok(())
}

/// Maps an alphabet symbol back to its six-bit value.
fn sextet(byte: u8) -> Result<u32, DecodeError> {
    match byte {
        b'A'..=b'Z' => Ok(u32::from(byte - b'A')),
        b'a'..=b'z' => Ok(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Ok(u32::from(byte - b'0') + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(DecodeError::BadSymbol),
    }
}

/// Extracts the eight bits at `shift`.
fn octet(group: u32, shift: u32) -> u8 {
    u8::try_from((group >> shift) & 0xFF).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{DecodeError, decode, encode};

    #[test]
    fn matches_the_rfc_4648_test_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(plain.as_bytes()), encoded, "encoding {plain:?}");
            assert_eq!(
                decode(encoded).unwrap(),
                plain.as_bytes(),
                "decoding {encoded:?}"
            );
        }
    }

    #[test]
    fn round_trips_every_length_up_to_three_quartets() {
        // Covers all three padding cases several times over, plus the empty input.
        for len in 0..=12_usize {
            let bytes: Vec<u8> = (0..len)
                .map(|i| u8::try_from(i * 7 % 256).unwrap_or(0))
                .collect();
            assert_eq!(decode(&encode(&bytes)).unwrap(), bytes, "length {len}");
        }
    }

    #[test]
    fn round_trips_all_byte_values() {
        let bytes: Vec<u8> = (0..=255_u8).collect();
        assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn uses_the_full_alphabet_including_plus_and_slash() {
        // 0xFB 0xFF encodes to "+/", which the URL-safe alphabet would spell "-_".
        assert_eq!(encode(&[0xFB, 0xFF]), "+/8=");
        assert_eq!(decode("+/8=").unwrap(), vec![0xFB, 0xFF]);
    }

    #[test]
    fn bad_lengths_are_rejected() {
        for bad in ["A", "AB", "ABC", "ABCDE"] {
            assert_eq!(decode(bad), Err(DecodeError::BadLength), "accepted {bad:?}");
        }
    }

    #[test]
    fn unknown_symbols_are_rejected() {
        for bad in ["A-CD", "A_CD", "A CD", "AB\nD", "Zm9v!!!!"] {
            assert!(
                matches!(decode(bad), Err(DecodeError::BadSymbol)),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn misplaced_padding_is_rejected() {
        for bad in ["Zg==Zg==", "=AAA", "A=AA", "AB=A"] {
            assert_eq!(
                decode(bad),
                Err(DecodeError::MisplacedPadding),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn non_canonical_trailing_bits_are_rejected() {
        // "Zh==" and "Zg==" would both decode to "f"; only the latter is canonical.
        assert_eq!(decode("Zh=="), Err(DecodeError::NonCanonical));
        // "Zm9=" sets bits that a two-byte output discards.
        assert_eq!(decode("Zm9="), Err(DecodeError::NonCanonical));
        // The canonical spellings still decode.
        assert!(decode("Zg==").is_ok());
        assert!(decode("Zm8=").is_ok());
    }

    #[test]
    fn the_encoded_spelling_is_pinned_to_an_exact_string() {
        // Bit-stability: the output ends up in a prompt whose cache key is exact
        // bytes, so the spelling is part of the contract. Comparing two calls to
        // `encode` would hold for any pure function and prove nothing; only a literal
        // catches a changed alphabet, changed padding, or added line breaks.
        let bytes: &[u8] = b"tokfold archive bytes";
        assert_eq!(encode(bytes), "dG9rZm9sZCBhcmNoaXZlIGJ5dGVz");
        // Equal contents in two distinct buffers must agree, so nothing about the
        // encoding depends on where the bytes live.
        let copy = bytes.to_vec();
        assert_eq!(encode(&copy), encode(bytes));
    }
}
