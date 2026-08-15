//! E1 — token-aware JSON minification.
//!
//! Re-emits the parsed tape with every insignificant byte removed: inter-token
//! whitespace is stripped and indentation is normalized away, so a pretty-printed
//! document collapses to a single compact line. The transformation is purely
//! structural — it never folds, dedups or reorders — so the semantic-equality
//! contract holds trivially: object key order and duplicate keys are re-emitted in
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
//! * **Protected strings survive verbatim.** The unit E1 protects is a JSON *string
//!   lexeme*, not a line: [`never_compress::is_protected`] is applied to the whole
//!   lexeme, quotes and escapes included, and on a hit that lexeme is copied
//!   byte-for-byte, at its position in document order — E1 never folds, dedups or
//!   reorders, so nothing else can move it. The table covers compiler errors, HTTP
//!   4xx/5xx status lines, the `EACCES`/`EPERM`/`EROFS` errno symbols, panic and
//!   stack-trace markers, `denied`/`unauthorized`/`forbidden` lines,
//!   certificate-verification failures and `warning:`. The only byte-changing step E1
//!   applies to a string is escape canonicalization, so skipping it is what keeps such
//!   content exact. See [`emit_string`] for a known limitation: the match runs on the
//!   *escaped* spelling.
//! * **A string holding a lone surrogate passes through raw.** A `\uXXXX` escape that
//!   is an unpaired surrogate has no scalar value and cannot be emitted literally.
//!   The exemption is per *string*, not per escape: because semantic equality compares
//!   such a string by its raw body bytes, the whole lexeme is copied verbatim as soon
//!   as it contains one lone surrogate, so a foldable pair or a redundant `\/`
//!   elsewhere in the same string is left alone too (see [`canonicalize`]).

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
///
/// # Known limitation: the *escaped* spelling is what gets matched
///
/// [`never_compress::is_protected`] is a line-oriented literal-substring test, and what
/// it receives here is the **raw JSON lexeme** — quotes, backslashes and all — not the
/// unescaped text. A protected phrase written with `\u` escapes therefore evades the
/// carve-out: `"error:"` matches, while `"\u0065rror:"` does not, though both decode to
/// the same string. The escaped spelling is canonicalized like any other string instead
/// of being copied byte-for-byte.
///
/// This is a *fidelity* gap and only that. Canonicalization preserves the string's
/// meaning, so the escaped spelling still decodes to the same text, and the recovery
/// archive stores the original bytes either way. [`never_compress`] is a fidelity
/// safeguard — not a security control, not an injection filter — so what is missed here
/// is the byte-for-byte carve-out, never a safety property.
///
/// It is left as-is deliberately: unescaping before matching would change which lexemes
/// are copied verbatim, i.e. which bytes E1 emits, which makes it a format change rather
/// than a local fix.
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
/// folded into their scalar, and control characters use the shortest legal escape.
///
/// A string that contains **any** lone surrogate escape is exempt: semantic equality
/// compares such a string by its raw body bytes, so no part of it may be rewritten.
/// The whole lexeme is then copied verbatim, as it is on any unexpected shape.
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

    if has_lone_surrogate(inner) {
        // Semantic equality compares any string containing a lone surrogate by its
        // *raw body bytes*, because such a string cannot be unescaped to UTF-8.
        // Canonicalizing anything inside it -- folding a valid pair that appears
        // elsewhere in the same string, or unwinding `\/` -- can therefore make two
        // semantically distinct documents render identically. Copy the lexeme verbatim.
        out.push_str(lexeme);
        return;
    }

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

/// Whether the string body `inner` contains a `\uXXXX` escape for an *unpaired*
/// surrogate.
///
/// Walks escapes exactly the way [`canonicalize`] does, so a `\\` immediately before
/// a `u` is never mistaken for a `\u` escape and a well-formed surrogate pair is
/// consumed whole rather than reported as two lone halves.
fn has_lone_surrogate(inner: &str) -> bool {
    let ib = inner.as_bytes();
    let mut i = 0usize;
    while let Some(&b) = ib.get(i) {
        if b != b'\\' {
            // A literal byte: skip the whole UTF-8 scalar it leads.
            i = i.saturating_add(if b < 0x80 { 1 } else { utf8_len(b) });
            continue;
        }
        match ib.get(i + 1) {
            // Only `\u` can carry a surrogate; every other escape is two bytes.
            Some(b'u') => {}
            Some(_) => {
                i = i.saturating_add(2);
                continue;
            }
            // Unreachable for a parser-validated lexeme; advance to stay finite.
            None => return false,
        }
        let Some(cp) = read_hex4(ib, i + 2) else {
            // Unreachable: the parser guarantees four hex digits.
            i = i.saturating_add(2);
            continue;
        };
        if combine_surrogate_pair(cp, ib, i).is_some() {
            i = i.saturating_add(12);
        } else if is_surrogate(cp) {
            return true;
        } else {
            i = i.saturating_add(6);
        }
    }
    false
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
        // Unreachable by construction: `canonicalize` returns early, copying the whole
        // lexeme, whenever `has_lone_surrogate` holds, so no *unpaired* surrogate ever
        // reaches this function -- and a *paired* one was already consumed by the branch
        // above. Kept as a fail-safe rather than deleted: an unpaired surrogate has no
        // scalar value, so the six raw bytes are the only lossless thing to emit, and
        // dropping this arm would send it to `emit_scalar`, where `char::from_u32`
        // returns `None` and the escape would be silently discarded.
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

    use super::{canonical_escape, canonical_unicode, canonicalize, render};
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

    #[test]
    fn literal_multi_byte_scalars_survive() {
        // The lead byte alone decides how many bytes the scalar occupies, so a
        // two-, three- and four-byte scalar must each be copied whole. Truncating
        // one would emit mojibake or drop it entirely.
        assert_eq!(mini("\"\u{e9}\""), "\"\u{e9}\""); // 2 bytes
        assert_eq!(mini("\"\u{65e5}\""), "\"\u{65e5}\""); // 3 bytes
        assert_eq!(mini("\"\u{1f600}\""), "\"\u{1f600}\""); // 4 bytes
        assert_eq!(
            mini("{\"k\u{e9}y\":\"a\u{65e5}b\u{1f600}c\"}"),
            "{\"k\u{e9}y\":\"a\u{65e5}b\u{1f600}c\"}"
        );
    }

    #[test]
    fn control_escapes_stop_at_the_space_boundary() {
        // 0x1F is the last control that needs an escape; 0x20 is a plain space.
        assert_eq!(mini("\"\\u001f\""), "\"\\u001f\"");
        assert_eq!(mini("\"\\u0020\""), "\" \"");
        assert_eq!(mini("\"\\u0021\""), "\"!\"");
    }

    #[test]
    fn escaped_backslash_shields_the_following_text() {
        // The body is a literal `u0041`, not the letter `A`: consuming both bytes of
        // the `\\` escape is what stops the rest from being re-read as an escape.
        assert_eq!(mini("\"\\\\u0041\""), "\"\\\\u0041\"");
        assert_eq!(mini("\"\\\\/\""), "\"\\\\/\"");
    }

    #[test]
    fn lone_surrogate_exempts_the_rest_of_the_string() {
        // The exemption is per string, not per escape: a redundant `\/` and a
        // foldable `\u0041` in the same lexeme are left alone as well.
        assert_eq!(mini("\"a\\/b\\udead\""), "\"a\\/b\\udead\"");
        assert_eq!(mini("\"\\u0041\\udead\""), "\"\\u0041\\udead\"");
    }

    #[test]
    fn surrogate_pair_folds_at_a_nonzero_offset() {
        // The pair does not start at the first body byte, so folding must be located
        // relative to the escape, not to the start of the string.
        assert_eq!(mini("\"a\\uD83D\\uDE00b\""), "\"a\u{1f600}b\"");
    }

    #[test]
    fn high_surrogate_followed_by_another_escape_is_not_a_pair() {
        // A high surrogate is only half a pair unless a `\u` escape follows it. Here
        // the next escape is `\n`, so the string holds a lone surrogate and is copied
        // verbatim -- it must not be folded with the hex digits that come later.
        assert_eq!(mini("\"\\uD83D\\ndc00\""), "\"\\uD83D\\ndc00\"");
        assert_eq!(mini("\"\\uD83Ddc00\""), "\"\\uD83Ddc00\"");
    }

    #[test]
    fn canonicalize_copies_an_unexpected_lexeme_verbatim() {
        // `canonicalize` renders a *quoted* lexeme. Anything else -- a bare word, a
        // half-quoted fragment, a single quote -- is copied byte for byte instead of
        // being re-quoted around a body that was never there.
        for lexeme in ["abc", "\"ab", "ab\"", "\"", "", "ab"] {
            let mut out = String::new();
            canonicalize(lexeme, &mut out);
            assert_eq!(out, lexeme, "lexeme {lexeme:?} must be copied verbatim");
        }
    }

    #[test]
    fn each_short_escape_renders_its_own_canonical_form() {
        // Every two-byte escape has its own canonical spelling and consumes both of
        // its bytes. Asserted here rather than through `mini` because the fallback
        // (copy the backslash, resume at the next byte) re-emits the very same bytes
        // for most of these, so an end-to-end round trip cannot tell them apart.
        const CASES: [(&str, &str); 8] = [
            ("\\\"", "\\\""),
            ("\\\\", "\\\\"),
            ("\\/", "/"), // canonical: the solidus need not be escaped
            ("\\b", "\\b"),
            ("\\f", "\\f"),
            ("\\n", "\\n"),
            ("\\r", "\\r"),
            ("\\t", "\\t"),
        ];
        for (input, expected) in CASES {
            let mut out = String::new();
            let next = canonical_escape(input, input.as_bytes(), 0, &mut out);
            assert_eq!(out, expected, "escape {input:?} renders wrong");
            assert_eq!(next, 2, "escape {input:?} must consume both bytes");
        }
    }

    #[test]
    fn unknown_escape_keeps_the_backslash_and_resumes_at_the_next_byte() {
        // Defensive arm: a parser-validated lexeme never gets here. Nothing may be
        // dropped, and the escaped byte must still be scanned.
        let body = "x\\z";
        let mut out = String::new();
        let next = canonical_escape(body, body.as_bytes(), 1, &mut out);
        assert_eq!(out, "\\");
        assert_eq!(next, 2);

        // A trailing backslash with nothing after it behaves the same.
        let tail = "x\\";
        let mut out = String::new();
        let next = canonical_escape(tail, tail.as_bytes(), 1, &mut out);
        assert_eq!(out, "\\");
        assert_eq!(next, 2);
    }

    #[test]
    fn truncated_unicode_escape_copies_the_prefix() {
        // Defensive arm: fewer than four hex digits. The `\u` is kept and scanning
        // resumes at the first byte after it, never inside the two copied bytes.
        let body = "abc\\uZ";
        let mut out = String::new();
        let next = canonical_unicode(body, body.as_bytes(), 3, &mut out);
        assert_eq!(out, "\\u");
        assert_eq!(next, 5);
    }

    #[test]
    fn unpaired_surrogate_escape_is_emitted_raw() {
        // Fail-safe arm: `canonicalize` copies the whole lexeme before an unpaired
        // surrogate can reach here, but if one did, its six bytes must be emitted as
        // written (hex case included) instead of being silently discarded.
        let body = "ab\\uDEAD";
        let mut out = String::new();
        let next = canonical_unicode(body, body.as_bytes(), 2, &mut out);
        assert_eq!(out, "\\uDEAD");
        assert_eq!(next, 8);
    }
}
