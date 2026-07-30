//! E1 — token-aware JSON minification.
//!
//! Re-emits the parsed tape with every insignificant byte removed: inter-token
//! whitespace is stripped and indentation is normalized away, so a pretty-printed
//! document collapses to a single compact line. The transformation is purely
//! structural — it never folds, dedups or reorders — so the reversible contract
//! (§4) holds trivially: object key order and duplicate keys are re-emitted in
//! source order, and number lexemes are copied byte-for-byte (`1.0` stays `1.0`,
//! `1e3` stays `1e3`). String escapes are canonicalized (fidelity is
//! *semantic*, not byte-identical), with two carve-outs below.
//!
//! # Why this is a candidate, not a guarantee (CRITICAL)
//!
//! Minification is **not** uniformly a token win, even though it always removes
//! bytes. cl100k/o200k tokenize a multi-space indentation run as a single
//! indentation token, and a post-key `": "` as a single token, so stripping them
//! can leave the token count flat while the byte count drops. A byte objective
//! would therefore over-reward this encoder. That is exactly why
//! [`select`](super::select) applies the candidate rule against a *token* estimate
//! and falls back to passthrough when minification does not actually pay — this
//! module renders, it never decides.
//!
//! # Carve-outs
//!
//! * **Protected lines survive verbatim.** A string whose lexeme matches
//!   [`never_compress::is_protected`] (a compiler error, HTTP 4xx/5xx, `EACCES`, a
//!   panic trace, a denial/security line) is copied byte-for-byte, escapes and all,
//!   with its source position preserved. The only byte-changing step E1 applies to
//!   a string is escape canonicalization, so skipping it is what keeps such content
//!   exact (§5.2.1).
//! * **Lone surrogates pass through raw.** A `\uXXXX` escape that is an unpaired
//!   surrogate has no scalar value and cannot be emitted literally, so it is copied
//!   verbatim rather than canonicalized.

use crate::never_compress;
use crate::tape::{NodeKind, Span, Tape};

/// Minify `input` from its parsed `tape`, or `None` if a span cannot be resolved.
///
/// The returned body carries no sentinel; the caller frames it (see
/// [`super::render`]). Returning `None` degrades to passthrough rather than
/// emitting anything unsafe.
// `pub(crate)` is the contract even though this module is private: it is the exact
// visibility the sealed-encoder design specifies, so the visibility is stated, not
// inferred from the module being private.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn render(tape: &Tape, input: &str) -> Option<String> {
    let mut out = String::with_capacity(input.len());
    let mut frames: Vec<Frame> = Vec::new();

    for node in tape.nodes() {
        match node.kind {
            NodeKind::Null | NodeKind::Bool(_) | NodeKind::Number => {
                open_value(&mut frames, &mut out);
                out.push_str(span_str(input, node.span)?);
            }
            NodeKind::String => {
                open_value(&mut frames, &mut out);
                emit_string(span_str(input, node.span)?, &mut out);
            }
            NodeKind::Key => {
                open_key(&mut frames, &mut out);
                emit_string(span_str(input, node.span)?, &mut out);
                out.push(':');
            }
            NodeKind::ObjectStart { .. } => {
                open_value(&mut frames, &mut out);
                out.push('{');
                frames.push(Frame {
                    kind: FrameKind::Object,
                    seen: false,
                });
            }
            NodeKind::ObjectEnd => {
                out.push('}');
                frames.pop();
            }
            NodeKind::ArrayStart { .. } => {
                open_value(&mut frames, &mut out);
                out.push('[');
                frames.push(Frame {
                    kind: FrameKind::Array,
                    seen: false,
                });
            }
            NodeKind::ArrayEnd => {
                out.push(']');
                frames.pop();
            }
        }
    }

    Some(out)
}

/// Whether the current innermost container is an object or an array.
enum FrameKind {
    Object,
    Array,
}

/// One open container during emission. `seen` records whether a member/element has
/// already been written, which decides comma placement.
struct Frame {
    kind: FrameKind,
    seen: bool,
}

/// Emit the separator that precedes a value in its container.
///
/// In an array, a comma precedes every element after the first. In an object the
/// comma belongs before the *key* (see [`open_key`]), so a value there emits
/// nothing. A top-level value has no enclosing frame and emits nothing.
fn open_value(frames: &mut [Frame], out: &mut String) {
    if let Some(frame) = frames.last_mut() {
        if matches!(frame.kind, FrameKind::Array) {
            if frame.seen {
                out.push(',');
            }
            frame.seen = true;
        }
    }
}

/// Emit the separator that precedes an object key: a comma before every member
/// after the first.
fn open_key(frames: &mut [Frame], out: &mut String) {
    if let Some(frame) = frames.last_mut() {
        if frame.seen {
            out.push(',');
        }
        frame.seen = true;
    }
}

/// Resolve a node span to its source slice, or `None` if it is out of bounds.
fn span_str(input: &str, span: Span) -> Option<&str> {
    input.get(span.start as usize..span.end as usize)
}

/// Emit a string lexeme (quotes included): verbatim if protected, else canonicalized.
fn emit_string(lexeme: &str, out: &mut String) {
    if never_compress::is_protected(lexeme).is_some() {
        // Protected content is reproduced byte-for-byte with position preserved.
        out.push_str(lexeme);
    } else {
        canonicalize(lexeme, out);
    }
}

/// Re-emit a JSON string lexeme with canonical escapes.
///
/// Redundant escapes are unwound (`\/` -> `/`, `\u0041` -> `A`), surrogate pairs are
/// folded into their scalar, control characters use the shortest legal escape, and
/// lone surrogates are copied verbatim. On any unexpected shape the lexeme is copied
/// verbatim rather than risk corrupting a value.
fn canonicalize(lexeme: &str, out: &mut String) {
    let bytes = lexeme.as_bytes();
    let inner = match lexeme.get(1..lexeme.len().saturating_sub(1)) {
        Some(s)
            if bytes.len() >= 2 && bytes.first() == Some(&b'"') && bytes.last() == Some(&b'"') =>
        {
            s
        }
        _ => {
            out.push_str(lexeme);
            return;
        }
    };

    out.push('"');
    let ib = inner.as_bytes();
    let mut i = 0usize;
    while let Some(&b) = ib.get(i) {
        if b == b'\\' {
            i = canonical_escape(inner, ib, i, out);
        } else if b < 0x80 {
            // A literal ASCII byte: guaranteed >= 0x20 and never a bare `"` (the
            // parser would have closed the string), so it is safe to copy as-is.
            out.push(char::from(b));
            i += 1;
        } else {
            // A multi-byte UTF-8 scalar: copy its whole byte sequence.
            let len = utf8_len(b);
            match inner.get(i..i.saturating_add(len)) {
                Some(s) => {
                    out.push_str(s);
                    i += len;
                }
                // Unreachable for a valid `&str`; advance one byte to stay finite.
                None => i += 1,
            }
        }
    }
    out.push('"');
}

/// Canonicalize the escape at `i` (where `ib[i] == b'\\'`). Returns the next index.
fn canonical_escape(inner: &str, ib: &[u8], i: usize, out: &mut String) -> usize {
    match ib.get(i + 1).copied() {
        Some(b'"') => push_return(out, "\\\"", i + 2),
        Some(b'\\') => push_return(out, "\\\\", i + 2),
        Some(b'/') => push_return(out, "/", i + 2), // canonical: solidus need not be escaped
        Some(b'b') => push_return(out, "\\b", i + 2),
        Some(b'f') => push_return(out, "\\f", i + 2),
        Some(b'n') => push_return(out, "\\n", i + 2),
        Some(b'r') => push_return(out, "\\r", i + 2),
        Some(b't') => push_return(out, "\\t", i + 2),
        Some(b'u') => canonical_unicode(inner, ib, i, out),
        // Unreachable for a parser-validated lexeme; copy the backslash to stay lossless.
        _ => push_return(out, "\\", i + 1),
    }
}

/// Push `s` and return `next`, keeping the escape arms to one line each.
fn push_return(out: &mut String, s: &str, next: usize) -> usize {
    out.push_str(s);
    next
}

/// Canonicalize a `\uXXXX` escape at `i`, folding a surrogate pair when one follows.
/// Returns the next index.
fn canonical_unicode(inner: &str, ib: &[u8], i: usize, out: &mut String) -> usize {
    let Some(cp) = read_hex4(ib, i + 2) else {
        // Unreachable: the parser guarantees four hex digits. Copy `\u` defensively.
        return push_return(out, "\\u", i + 2);
    };

    // A high surrogate immediately followed by a low-surrogate escape folds into one
    // scalar and consumes all twelve bytes.
    if let Some(paired) = combine_surrogate_pair(cp, ib, i) {
        out.push(paired);
        return i + 12;
    }

    if is_surrogate(cp) {
        // Any unpaired surrogate has no scalar value: copy the six raw bytes verbatim.
        push_raw(inner, i, out);
    } else {
        emit_scalar(cp, out);
    }
    i + 6
}

/// Whether `cp` is a UTF-16 surrogate code unit (high or low).
const fn is_surrogate(cp: u16) -> bool {
    matches!(cp, 0xD800..=0xDFFF)
}

/// If `high` is a high surrogate and a low-surrogate `\uXXXX` escape follows at
/// `i + 6`, return their combined scalar; otherwise `None`.
fn combine_surrogate_pair(high: u16, ib: &[u8], i: usize) -> Option<char> {
    if !(0xD800..=0xDBFF).contains(&high) {
        return None;
    }
    if ib.get(i + 6) != Some(&b'\\') || ib.get(i + 7) != Some(&b'u') {
        return None;
    }
    let low = read_hex4(ib, i + 8)?;
    if !(0xDC00..=0xDFFF).contains(&low) {
        return None;
    }
    let scalar = 0x1_0000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
    char::from_u32(scalar)
}

/// Copy the six raw bytes of a `\uXXXX` escape (verbatim, preserving hex case).
fn push_raw(inner: &str, start: usize, out: &mut String) {
    if let Some(s) = inner.get(start..start.saturating_add(6)) {
        out.push_str(s);
    }
}

/// Emit a non-surrogate BMP code point canonically: shortest escape for the special
/// controls, `\u00xx` for other controls, `\"`/`\\` for the two reserved bytes, and
/// the literal character otherwise.
fn emit_scalar(cp: u16, out: &mut String) {
    match cp {
        0x08 => out.push_str("\\b"),
        0x09 => out.push_str("\\t"),
        0x0A => out.push_str("\\n"),
        0x0C => out.push_str("\\f"),
        0x0D => out.push_str("\\r"),
        0x22 => out.push_str("\\\""),
        0x5C => out.push_str("\\\\"),
        c if c < 0x20 => push_u_escape(c, out),
        c => {
            if let Some(ch) = char::from_u32(u32::from(c)) {
                out.push(ch);
            }
        }
    }
}

/// Write a `\u00xx`-style escape with four lowercase hex digits.
fn push_u_escape(c: u16, out: &mut String) {
    out.push_str("\\u");
    push_hex4(c, out);
}

/// Append `c` as four lowercase hex digits.
fn push_hex4(c: u16, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for shift in [12u32, 8, 4, 0] {
        let nibble = usize::from((c >> shift) & 0xF);
        if let Some(&h) = HEX.get(nibble) {
            out.push(char::from(h));
        }
    }
}

/// Parse four hex digits at `pos`, or `None` if any is missing or not hex.
fn read_hex4(ib: &[u8], pos: usize) -> Option<u16> {
    let mut value: u16 = 0;
    for k in 0..4 {
        let digit = hex_val(ib.get(pos + k).copied()?)?;
        value = (value << 4) | u16::from(digit);
    }
    Some(value)
}

/// The value of a single ASCII hex digit, or `None`.
fn hex_val(d: u8) -> Option<u8> {
    match d {
        b'0'..=b'9' => Some(d - b'0'),
        b'a'..=b'f' => Some(d - b'a' + 10),
        b'A'..=b'F' => Some(d - b'A' + 10),
        _ => None,
    }
}

/// Length in bytes of the UTF-8 scalar whose leading byte is `lead`.
const fn utf8_len(lead: u8) -> usize {
    if lead >= 0xF0 {
        4
    } else if lead >= 0xE0 {
        3
    } else if lead >= 0xC0 {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::render;
    use crate::tape;

    fn mini(input: &str) -> String {
        let t = tape::parse(input, 512).unwrap();
        render(&t, input).unwrap()
    }

    #[test]
    fn strips_inter_token_whitespace() {
        assert_eq!(
            mini("{ \"a\" : 1 , \"b\" : [ 2 , 3 ] }"),
            "{\"a\":1,\"b\":[2,3]}"
        );
        assert_eq!(mini("[\n  1,\n  2\n]"), "[1,2]");
    }

    #[test]
    fn preserves_key_order() {
        assert_eq!(mini("{\"b\":1,\"a\":2}"), "{\"b\":1,\"a\":2}");
    }

    #[test]
    fn preserves_duplicate_keys() {
        assert_eq!(mini("{\"a\":1,\"a\":2}"), "{\"a\":1,\"a\":2}");
    }

    #[test]
    fn preserves_number_lexemes_byte_for_byte() {
        assert_eq!(
            mini("[1.0,1e3,1E3,-0,2.5e10,0.5]"),
            "[1.0,1e3,1E3,-0,2.5e10,0.5]"
        );
        let big = "9".repeat(60);
        assert_eq!(mini(&format!("[{big}]")), format!("[{big}]"));
    }

    #[test]
    fn scalar_literals_survive() {
        assert_eq!(mini("[null,true,false]"), "[null,true,false]");
    }

    #[test]
    fn canonicalizes_redundant_escapes() {
        assert_eq!(mini("\"a\\/b\""), "\"a/b\""); // \/ -> /
        assert_eq!(mini("\"\\u0041\""), "\"A\""); // A -> A
        assert_eq!(mini("\"\\u00e9\""), "\"\u{e9}\""); // é -> é
        assert_eq!(mini("\"\\u0022\""), "\"\\\"\""); // " -> \"
        assert_eq!(mini("\"\\u005c\""), "\"\\\\\""); // \ -> \\
    }

    #[test]
    fn keeps_canonical_short_escapes() {
        assert_eq!(mini("\"a\\nb\\tc\""), "\"a\\nb\\tc\"");
    }

    #[test]
    fn combines_surrogate_pairs_into_scalars() {
        // 😀 is U+1F600 GRINNING FACE.
        assert_eq!(mini("\"\\uD83D\\uDE00\""), "\"\u{1F600}\"");
    }

    #[test]
    fn lone_surrogates_pass_through_verbatim() {
        assert_eq!(mini("\"\\ud834\""), "\"\\ud834\"");
        assert_eq!(mini("\"\\uDEAD\""), "\"\\uDEAD\"");
        assert_eq!(mini("\"a\\udeadb\""), "\"a\\udeadb\"");
    }

    #[test]
    fn control_chars_use_short_or_u_escapes() {
        assert_eq!(mini("\"\\u0000\""), "\"\\u0000\""); // NUL -> \u0000
        assert_eq!(mini("\"\\u0008\""), "\"\\b\""); // BS -> \b
        assert_eq!(mini("\"\\u001f\""), "\"\\u001f\""); // US -> \u001f
    }

    #[test]
    fn protected_strings_survive_verbatim() {
        // The value matches never_compress ("error["), so its bytes — including the
        // redundant \/ escape — are preserved exactly; the (unprotected) key is
        // canonicalized normally.
        let input = "{\"msg\":\"error[E0308] a\\/b\"}";
        assert_eq!(mini(input), "{\"msg\":\"error[E0308] a\\/b\"}");
    }

    #[test]
    fn nested_and_empty_containers() {
        assert_eq!(mini("{ }"), "{}");
        assert_eq!(mini("[ ]"), "[]");
        assert_eq!(mini("{\"a\":{},\"b\":[]}"), "{\"a\":{},\"b\":[]}");
        assert_eq!(mini("[{\"a\":[1,{\"b\":2}]}]"), "[{\"a\":[1,{\"b\":2}]}]");
    }

    #[test]
    fn top_level_scalar_and_string() {
        assert_eq!(mini("  42  "), "42");
        assert_eq!(mini("\"plain\""), "\"plain\"");
    }
}
