//! Archive decoding: on arbitrary bytes, and on archives `compress` really wrote.
//!
//! Must never panic. Beyond that, two invariants that a decoder is most likely to
//! break:
//!
//! * `Header::decode` is canonical: a header it accepts re-encodes to exactly the
//!   bytes it consumed, and never claims to have consumed more than it was given.
//! * `Compressor::decompress` is fail-closed: whatever it returns was checked against
//!   the header — the header decodes, the output is the payload verbatim, its length
//!   is the recorded length, its digest is the recorded digest, and the metadata is
//!   the only metadata v0.0.1 writes.
//!
//! On arbitrary bytes the second invariant is cheap to state and nearly unreachable
//! to falsify. `decompress` verifies the digest last, and rejects a nonzero
//! `encoder_id`, a nonzero `tokenizer_id`, non-default `flags` and a payload length
//! disagreeing with `original_len` before it — so every assertion `fail_closed` makes
//! after `Ok` restates a check `decompress` itself performed on the way to that `Ok`.
//! They guard a future refactor that drops one, not this build. Reaching `Ok` at all
//! asks the fuzzer for a correct SHA-256 of its own payload.
//!
//! So the same bytes are read a second time, as an edit to an archive `compress`
//! wrote: `data[0] >> 6` picks the operation (`0` and `1` overwrite, `2` truncate,
//! `3` insert), `data[0] & 0x3F` picks the document, `data[1..3]` is a big-endian
//! offset taken modulo the archive length, and `data[3..]` is the bytes to write or
//! insert. Only the overwrite arm truncates them, with
//! `replacement.len().min(patched.len() - offset)`, because an overwrite has to stay
//! inside the archive; the insert arm splices all of them in and the archive grows.
//! Inputs shorter than three bytes drive the first half only. A three-byte input does
//! reach the second half, with an empty `data[3..]`: under the overwrite arm and under
//! the insert arm that leaves the archive exactly as `compress` wrote it, which is the
//! input this half needs `decompress` to accept. Under the truncate arm it does not,
//! because the offset is taken modulo the archive length and so drops at least one
//! byte. The assertions read the bytes and not the arm, so a replacement that happens
//! to equal what it overwrites counts as unchanged too. What that buys is `Ok`.
//! Arbitrary bytes reach every *rejection*, `ChecksumMismatch` among them — measured
//! 2026-09-30 from a hand-built header over a zeroed digest, and from all 255
//! single-byte rewrites of a seed's last payload byte — but reaching an accepted
//! archive asks the fuzzer for a correct SHA-256 of its own payload. It also makes the
//! fail-closed invariant checkable against a known original in one direction: an edit
//! that changed no byte must be accepted, so a rejection proves the bytes differ from
//! the ones `compress` wrote.
//!
//! The converse does not hold, and the assertions no longer claim it. The digest is
//! inside the edited bytes too, so a valid archive of a *different* document is
//! accepted with no second preimage at all: `00 00 00` followed by the 45-byte archive
//! of `[]` overwrites the 45-byte archive of `{}` whole and decompresses to `[]`
//! (measured 2026-09-30, a 48-byte input). What a second preimage would buy is a
//! different payload under the *same* digest, which is the single-bit case
//! `decompress_rejects_every_single_bit_flip` pins in the crate's own suite, and
//! `a_valid_archive_of_another_document_is_accepted` in `tests/hostile.rs` pins the
//! substitution this paragraph describes.
//!
//! The reasoning that every byte of an archive is load-bearing — `read_uleb128`
//! rejects an overlong encoding, so `original_len` has one spelling per value; the
//! three metadata fields are compared against fixed values; the payload is covered by
//! the header's digest — bounds *corruption of this archive*, not substitution of
//! another. That is exactly enough for the one assertion that survives: an accepted
//! edit returning this document is byte-identical to the archive `compress` wrote.

#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use tokfold_core::format::{self, Flags, Header};
use tokfold_core::{Compressor, Config};

static COMPRESSOR: OnceLock<Compressor> = OnceLock::new();
static ARCHIVES: OnceLock<Vec<(Vec<u8>, Vec<u8>)>> = OnceLock::new();

fn compressor() -> &'static Compressor {
    COMPRESSOR.get_or_init(|| Compressor::new(Config::default()))
}

/// The documents whose archives the second half edits. Their lengths fall on both
/// sides of the one-byte ULEB128 boundary, so `original_len` is written in one byte
/// for some of them and in two for others; the asserts pin that.
fn documents() -> Vec<Vec<u8>> {
    let mut table = String::from("[");
    for row in 0..8 {
        if row > 0 {
            table.push(',');
        }
        table.push_str(&format!(r#"{{"id":{row},"name":"row-{row}","ok":true}}"#));
    }
    table.push(']');

    let docs = vec![
        b"{}".to_vec(),
        br#"{"a":1,"b":[1,2,3],"c":"x","d":null,"e":true}"#.to_vec(),
        table.into_bytes(),
    ];
    assert!(
        docs.iter().any(|doc| doc.len() < 128),
        "no document has a one-byte ULEB128 length"
    );
    assert!(
        docs.iter().any(|doc| (128..16_384).contains(&doc.len())),
        "no document has a two-byte ULEB128 length"
    );
    docs
}

/// Each document beside the archive `compress` wrote for it, built once.
fn archives() -> &'static [(Vec<u8>, Vec<u8>)] {
    ARCHIVES.get_or_init(|| {
        documents()
            .into_iter()
            .map(|doc| {
                let archive = compressor()
                    .compress(&doc)
                    .expect("compress rejected a fixed document")
                    .archive;
                (doc, archive)
            })
            .collect()
    })
}

/// Both invariants, on whatever bytes are handed in. Returns the output when
/// `decompress` accepted them.
fn fail_closed(bytes: &[u8]) -> Option<Vec<u8>> {
    let decoded = Header::decode(bytes);
    if let Ok((header, end)) = &decoded {
        assert!(
            *end <= bytes.len(),
            "decode consumed {end} of {} bytes",
            bytes.len()
        );
        let mut reencoded = Vec::new();
        header.encode_into(&mut reencoded);
        assert_eq!(
            reencoded.as_slice(),
            &bytes[..*end],
            "an accepted header does not re-encode to the bytes it was decoded from"
        );
    }

    let out = compressor().decompress(bytes).ok()?;
    let Ok((header, end)) = decoded else {
        panic!("decompress succeeded on bytes whose header does not decode");
    };
    assert_eq!(
        &bytes[end..],
        out.as_slice(),
        "output is not the payload verbatim"
    );
    assert_eq!(header.original_len, out.len() as u64);
    assert_eq!(header.checksum, format::sha256(&out));
    assert_eq!(header.encoder_id, 0);
    assert_eq!(header.tokenizer_id, 0);
    assert_eq!(header.flags, Flags::default());
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    fail_closed(data);

    // The seeds were written for the half above, so this half reads its selector off
    // their leading bytes. Nothing here depends on that being a chosen value.
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let (offset_bytes, replacement) = match rest {
        [high, low, tail @ ..] => ([*high, *low], tail),
        _ => return,
    };

    let all = archives();
    let (document, archive) = &all[usize::from(selector & 0x3F) % all.len()];
    let offset = usize::from(u16::from_be_bytes(offset_bytes)) % archive.len();

    let mut patched = archive.clone();
    match selector >> 6 {
        2 => patched.truncate(offset),
        3 => {
            let mut built = patched[..offset].to_vec();
            built.extend_from_slice(replacement);
            built.extend_from_slice(&patched[offset..]);
            patched = built;
        }
        _ => {
            let width = replacement.len().min(patched.len() - offset);
            patched[offset..offset + width].copy_from_slice(&replacement[..width]);
        }
    }

    match fail_closed(&patched) {
        // `out == document` pins every header field: the payload is the document, so
        // `original_len` and the digest are forced, and the three metadata values are
        // compared against fixed ones. That leaves only the length varint's spelling
        // free, and `read_uleb128` rejects an overlong encoding — so an accepted edit
        // that hands this document back must be the archive `compress` wrote, byte for
        // byte. This is the direction the format actually forces.
        Some(out) if out == *document => assert_eq!(
            patched, *archive,
            "an archive that differs from the one compress wrote returned the same document"
        ),
        // Any other document is a legitimate answer, not a finding: see the module doc.
        Some(_) => {}
        None => assert_ne!(
            patched, *archive,
            "decompress rejected the archive compress wrote, unedited"
        ),
    }
});
