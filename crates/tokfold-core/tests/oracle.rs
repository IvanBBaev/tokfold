#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(
    missing_docs,
    clippy::panic,
    clippy::missing_docs_in_private_items,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::too_long_first_doc_paragraph,
    clippy::unreadable_literal,
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::items_after_statements,
    clippy::match_same_arms,
    clippy::single_match,
    clippy::format_push_string,
    clippy::literal_string_with_formatting_args,
    clippy::use_self
)]

//! Independent JSON semantic-equality oracle for the `tokfold-core` test suite.
//!
//! `json_semantically_eq(a, b)` decides whether two JSON documents are equal under
//! the crate's frozen reversibility contract, which this file states in full:
//!
//! * EQUAL requires the same tree shape; identical key order; duplicate keys
//!   preserved identically in order; identical array order; **byte-identical number
//!   lexemes** (`1e2` != `100.0`, `1E2` != `1e2`); strings equal **after unescaping**.
//! * IGNORED (canonicalized away): insignificant whitespace and escape style
//!   (`é` == `é` == `é`, `\/` == `/`).
//! * Lone surrogates cannot be unescaped to UTF-8, so any string containing one is
//!   compared by its **raw body bytes** (hex case significant): `"\uD834"` !=
//!   `"\ud834"`, even though both are unpaired high surrogates.
//! * NOT COMPARABLE (equal to nothing, not even itself): anything this parser cannot
//!   read. That is malformed input, trailing data -- and, the one place where the
//!   oracle's domain is narrower than JSON itself, nesting deeper than [`MAX_DEPTH`],
//!   which is valid JSON and still not reflexive here. So the contract above is a
//!   contract over the parseable subset, not over every valid document; the engine's
//!   own ceiling is half of `MAX_DEPTH`, so no input the engine accepts falls in the
//!   gap (pinned by `nesting_deeper_than_the_oracle_ceiling_is_not_comparable`).
//!
//! This is deliberately a **separate, hand-rolled recursive-descent parser**. It
//! shares no code with `tokfold_core::tape`, so a bug that made the tape and the
//! oracle agree by construction cannot hide: the two parsers were written
//! independently against the same grammar. The oracle imports nothing from the crate
//! under test.
//!
//! `tests/oracle.rs` (this file, which also unit-tests the oracle against hand-built
//! tricky pairs), `tests/roundtrip.rs` and `tests/hostile.rs` all use
//! `json_semantically_eq`; the latter two pull it in with
//! `#[path = "oracle.rs"] mod oracle;`. Because that is a module and not a shared
//! crate, this file's own tests are compiled into all three binaries and so are run
//! three times by one `cargo test`, under the `oracle::` prefix in the two that
//! import it. Any count of workspace tests that sums the per-binary `test result:`
//! lines therefore counts them three times; `grep -c '#\[test\]'` here gives the
//! number of distinct ones.

/// A parsed JSON value, normalized so that `==` means exactly the equality the module
/// docs state: order-preserving, duplicate-preserving, number lexemes compared
/// byte-for-byte, strings compared post-unescape (or raw when lone surrogates make
/// unescaping impossible).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// The `null` literal.
    Null,
    /// A `true` / `false` literal.
    Bool(bool),
    /// A number kept as its raw source lexeme; compared byte-for-byte.
    Number(String),
    /// A string, normalized per [`Str`].
    Str(Str),
    /// An array; element order is significant.
    Array(Vec<Value>),
    /// An object; member order and duplicate keys are significant.
    Object(Vec<(Str, Value)>),
}

/// A parsed JSON string, normalized for the comparison the module docs describe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Str {
    /// Fully unescaped scalar text. Escape style and `\u` hex case are canonicalized
    /// away, so `"A"`, `"A"` and `"A"`-with-any-hex-case all compare equal.
    Text(String),
    /// The raw body (bytes between the quotes) of a string that contains at least one
    /// lone surrogate. Such a string cannot be unescaped to UTF-8, so it is compared
    /// verbatim; hex case is therefore significant.
    Raw(String),
}

/// Decide whether two JSON documents are semantically equal under the contract the
/// module docs state.
///
/// Returns `false` if either side is not a single JSON document *this parser can
/// read* (such a document is equal to nothing, not even itself). On input it can read
/// the relation is reflexive, symmetric and transitive -- pinned by
/// [`reflexive_on_assorted_valid_documents`] and
/// [`the_relation_is_symmetric_and_transitive_on_parseable_input`].
///
/// "Can read" is narrower than "valid JSON" in exactly one way: nesting deeper than
/// [`MAX_DEPTH`] is rejected, so such a document is valid JSON and still not
/// reflexive here. [`nesting_deeper_than_the_oracle_ceiling_is_not_comparable`] pins
/// both sides of that boundary. Every other rejection this parser makes is a
/// rejection of malformed input.
#[must_use]
pub fn json_semantically_eq(a: &str, b: &str) -> bool {
    match (parse(a), parse(b)) {
        (Some(va), Some(vb)) => va == vb,
        _ => false,
    }
}

/// Parse one complete JSON document, or `None` if it is malformed or has trailing
/// data. The accepted grammar mirrors `tokfold_core::tape`: standard JSON, with lone
/// `\uXXXX` surrogate escapes accepted (kept raw) and leading zeros rejected.
///
/// [`json_semantically_eq`] claims that depth is the *only* way this parser is
/// narrower than valid JSON, and that every other rejection rejects malformed input.
/// That is a claim about every rejection site in `impl Parser` -- 36 of them on
/// 2026-10-03, 14 `return None` plus 22 `?`, and `parse`'s own trailing-data `if` on
/// top of those -- so here they are classified, to be re-checked by diffing this list
/// against the code rather than re-derived from scratch. Most of the 22 only
/// propagate a rejection raised elsewhere; the causes are what follow:
///
/// * `value`: `depth > MAX_DEPTH` -- **the one narrowing**; end of input; a byte that
///   cannot begin a JSON value.
/// * `object`: a member not opening with `"`; a missing `:`; a separator that is
///   neither `,` nor `}`. `array`: a separator that is neither `,` nor `]`.
/// * `string`: end of input before the closing quote; an escape character outside
///   `"\/bfnrt` and `u`; a non-hex digit in `\uXXXX`; an unescaped byte `<= 0x1F`,
///   which RFC 8259 forbids. Note what is *not* here: `\uXXXX` is validated for shape
///   only, so lone surrogates are accepted and `decode_string_body` keeps them raw.
/// * `number`: a leading zero followed by a digit; no digit at all; a `.` or an
///   `e`/`E` with no digit after it. `literal`: a byte that is not the one `true`,
///   `false` or `null` needs.
/// * `parse` itself: anything left after the value -- trailing data, which the module
///   docs list separately.
///
/// The two sites that are *not* grammar are the `self.src.get(start..j)?` slices in
/// `string` and `number`. `str::get` yields `None` off a char boundary, so each would
/// reject a valid document -- and neither is reachable. In `number` the slice is ASCII
/// digits and signs. In `string`, `start` sits just past the opening `"` and `j` stops
/// only at `"` (0x22) or `\` (0x5C); UTF-8 continuation bytes are 0x80..=0xBF, so
/// neither byte can occur inside a multi-byte character and both ends are always
/// boundaries. A multi-byte character is therefore walked one byte at a time by the
/// `_ => j += 1` arm and never split. The module docs' raw-vs-escaped accented cases
/// pin that both spellings reach the accepted arm.
fn parse(input: &str) -> Option<Value> {
    let mut p = Parser {
        src: input,
        s: input.as_bytes(),
        i: 0,
        depth: 0,
    };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.i == p.s.len() { Some(v) } else { None }
}

/// Recursion ceiling for the oracle parser. Comfortably above the engine's default
/// `max_depth` (512), so every document the engine accepts under the default config
/// parses here without overflowing the stack.
const MAX_DEPTH: usize = 1024;

struct Parser<'a> {
    src: &'a str,
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while let Some(&b) = self.s.get(self.i) {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => break,
            }
        }
    }

    fn value(&mut self) -> Option<Value> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        let b = *self.s.get(self.i)?;
        let out = match b {
            b'{' => self.object()?,
            b'[' => self.array()?,
            b'"' => Value::Str(self.string()?),
            b't' => {
                self.literal(b"true")?;
                Value::Bool(true)
            }
            b'f' => {
                self.literal(b"false")?;
                Value::Bool(false)
            }
            b'n' => {
                self.literal(b"null")?;
                Value::Null
            }
            b'-' | b'0'..=b'9' => Value::Number(self.number()?),
            _ => return None,
        };
        self.depth -= 1;
        Some(out)
    }

    fn object(&mut self) -> Option<Value> {
        self.i += 1; // consume '{'
        let mut members: Vec<(Str, Value)> = Vec::new();
        self.skip_ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Some(Value::Object(members));
        }
        loop {
            self.skip_ws();
            if *self.s.get(self.i)? != b'"' {
                return None;
            }
            let key = self.string()?;
            self.skip_ws();
            if *self.s.get(self.i)? != b':' {
                return None;
            }
            self.i += 1;
            let val = self.value()?;
            members.push((key, val));
            self.skip_ws();
            match *self.s.get(self.i)? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(Value::Object(members));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<Value> {
        self.i += 1; // consume '['
        let mut elems: Vec<Value> = Vec::new();
        self.skip_ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Some(Value::Array(elems));
        }
        loop {
            let v = self.value()?;
            elems.push(v);
            self.skip_ws();
            match *self.s.get(self.i)? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(Value::Array(elems));
                }
                _ => return None,
            }
        }
    }

    /// Scan a string token starting at the opening quote and return its normalized
    /// form. Escapes are validated for shape (as the tape parser does) but only
    /// interpreted by [`decode_string_body`].
    fn string(&mut self) -> Option<Str> {
        let start = self.i + 1; // first body byte
        let mut j = start;
        loop {
            match *self.s.get(j)? {
                b'"' => {
                    let body = self.src.get(start..j)?;
                    self.i = j + 1;
                    return Some(decode_string_body(body));
                }
                b'\\' => match *self.s.get(j + 1)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => j += 2,
                    b'u' => {
                        for k in 0..4 {
                            if !self.s.get(j + 2 + k)?.is_ascii_hexdigit() {
                                return None;
                            }
                        }
                        j += 6;
                    }
                    _ => return None,
                },
                c if c <= 0x1F => return None, // unescaped control character
                _ => j += 1,
            }
        }
    }

    /// Scan a JSON number and return its raw lexeme. Same grammar as the tape parser:
    /// optional sign, no leading zeros, optional fraction with at least one digit,
    /// optional exponent with at least one digit.
    fn number(&mut self) -> Option<String> {
        let start = self.i;
        let mut j = self.i;

        if self.s.get(j) == Some(&b'-') {
            j += 1;
        }

        match *self.s.get(j)? {
            b'0' => {
                j += 1;
                if let Some(&d) = self.s.get(j) {
                    if d.is_ascii_digit() {
                        return None; // leading zero
                    }
                }
            }
            d if d.is_ascii_digit() => {
                j += 1;
                while self.s.get(j).is_some_and(u8::is_ascii_digit) {
                    j += 1;
                }
            }
            _ => return None,
        }

        if self.s.get(j) == Some(&b'.') {
            j += 1;
            let mut any = false;
            while self.s.get(j).is_some_and(u8::is_ascii_digit) {
                j += 1;
                any = true;
            }
            if !any {
                return None;
            }
        }

        if matches!(self.s.get(j), Some(&b'e' | &b'E')) {
            j += 1;
            if matches!(self.s.get(j), Some(&b'+' | &b'-')) {
                j += 1;
            }
            let mut any = false;
            while self.s.get(j).is_some_and(u8::is_ascii_digit) {
                j += 1;
                any = true;
            }
            if !any {
                return None;
            }
        }

        let lexeme = self.src.get(start..j)?.to_string();
        self.i = j;
        Some(lexeme)
    }

    fn literal(&mut self, lit: &[u8]) -> Option<()> {
        for (k, &expected) in lit.iter().enumerate() {
            if *self.s.get(self.i + k)? != expected {
                return None;
            }
        }
        self.i += lit.len();
        Some(())
    }
}

/// Normalize a string body (the bytes between the quotes, already validated for
/// escape shape). Escapes are resolved to their scalar values, canonicalizing escape
/// style and `\u` hex case. If any lone surrogate is found, the string cannot be
/// unescaped to UTF-8, so its raw body is returned for verbatim comparison instead.
fn decode_string_body(body: &str) -> Str {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut idx = 0;

    while idx < chars.len() {
        let c = chars[idx];
        if c != '\\' {
            out.push(c);
            idx += 1;
            continue;
        }

        // An escape. The parser guaranteed a well-formed escape follows.
        let e = chars[idx + 1];
        match e {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{08}'),
            'f' => out.push('\u{0C}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let hi = hex4(&chars, idx + 2);
                idx += 6; // '\' 'u' + 4 hex
                if (0xD800..=0xDBFF).contains(&hi) {
                    // High surrogate: needs an immediately following low-surrogate escape.
                    let paired = chars.get(idx) == Some(&'\\')
                        && chars.get(idx + 1) == Some(&'u')
                        && idx + 6 <= chars.len()
                        && (0xDC00..=0xDFFF).contains(&hex4(&chars, idx + 2));
                    if paired {
                        let lo = hex4(&chars, idx + 2);
                        let scalar = 0x10000 + ((hi as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00);
                        // scalar is always a valid non-surrogate code point here.
                        out.push(char::from_u32(scalar).unwrap_or('\u{FFFD}'));
                        idx += 6;
                    } else {
                        return Str::Raw(body.to_string());
                    }
                    continue;
                } else if (0xDC00..=0xDFFF).contains(&hi) {
                    // Lone low surrogate.
                    return Str::Raw(body.to_string());
                }
                // Ordinary BMP scalar.
                out.push(char::from_u32(hi as u32).unwrap_or('\u{FFFD}'));
                continue;
            }
            _ => {}
        }
        idx += 2;
    }

    Str::Text(out)
}

fn hex4(chars: &[char], start: usize) -> u16 {
    let mut v: u32 = 0;
    for k in 0..4 {
        v = v * 16 + hex_val(chars[start + k]);
    }
    v as u16
}

fn hex_val(c: char) -> u32 {
    match c {
        '0'..='9' => c as u32 - '0' as u32,
        'a'..='f' => c as u32 - 'a' as u32 + 10,
        'A'..='F' => c as u32 - 'A' as u32 + 10,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// The oracle's own tests: hand-built tricky pairs. These pin the equality rules the
// oracle exists to enforce, independently of the compression engine.
// ---------------------------------------------------------------------------

fn eq(a: &str, b: &str) -> bool {
    json_semantically_eq(a, b)
}

#[test]
fn whitespace_is_ignored() {
    assert!(eq("{\"a\":1}", "{ \"a\" : 1 }"));
    assert!(eq("[1,2,3]", "[\n  1,\n  2,\n  3\n]"));
    assert!(eq("  42  ", "42"));
    assert!(eq("{}", "{ }"));
    assert!(eq("[]", "[ ]"));
    assert!(eq(
        "{\"x\":[1,{\"y\":2}]}",
        "{\n  \"x\": [\n    1,\n    { \"y\": 2 }\n  ]\n}"
    ));

    // One fixture per byte `Parser::skip_ws` accepts, because the name is a category
    // claim about all four of them: space, tab, newline and carriage return (RFC 8259
    // §2 ws). The fixtures above spell only space and newline, so dropping `b'\r'`
    // from `skip_ws` changed nothing any test here could see. Each byte is placed at
    // every position the grammar allows one -- before the document, between a key and
    // its colon, after the colon, around a separating comma and before the closing
    // bracket -- so a `skip_ws` that handled a byte in some call sites only would be
    // caught as well.
    for ws in ["\t", "\r", "\n", " "] {
        let doc = format!("{ws}{{{ws}\"a\"{ws}:{ws}[1{ws},{ws}2{ws}]{ws}}}{ws}");
        assert!(eq(&doc, "{\"a\":[1,2]}"), "whitespace {ws:?} not skipped");
    }
    // A whitespace byte INSIDE a string is data, not layout: the same four bytes must
    // not be skipped there. `\t` and `\r` have to be escaped -- the scanner rejects a
    // raw byte <= 0x1F -- so these also pin that the two escapes decode to something
    // non-empty (an escape arm that fell through to `_ => {}` would drop them and make
    // both sides equal to `""`).
    assert!(!eq("\" \"", "\"\""));
    assert!(!eq("\"\\t\"", "\"\""));
    assert!(!eq("\"\\r\"", "\"\""));
    assert!(!eq("\"\\n\"", "\"\""));
}

#[test]
fn escape_style_is_ignored_for_valid_scalars() {
    // Simple escape vs literal.
    assert!(eq("\"A\"", "\"\\u0041\""));
    assert!(eq("\"/\"", "\"\\/\""));
    // Unicode escape vs literal.
    assert!(eq("\"é\"", "\"\\u00e9\""));
    // Hex case is irrelevant for a valid (non-surrogate) code point.
    assert!(eq("\"\\u00E9\"", "\"\\u00e9\""));
    // A surrogate PAIR is valid text and equals the literal astral character.
    assert!(eq("\"\\uD834\\uDD1E\"", "\"𝄞\""));
    // A valid pair with mixed hex case still decodes to the same scalar.
    assert!(eq("\"\\ud834\\udd1e\"", "\"\\uD834\\uDD1E\""));
    // Escapes anywhere in a key are canonicalized too.
    assert!(eq("{\"\\u0041\":1}", "{\"A\":1}"));

    // One pair per two-character escape `decode_string_body` handles. The name is a
    // category claim about escape style, and until these fixtures existed only `\/`
    // and `\uXXXX` were compared across spellings: renaming the `'"' '\\' 'b' 'f' 'n'
    // 'r' 't'` arms to characters that cannot occur left all tests in the workspace
    // green, because `decode_string_body` runs on BOTH sides of every comparison and a
    // same-spelling fixture decodes identically however the arm is spelled.
    //
    // Derivation of the spellings: for these seven the literal character is not
    // available as a third spelling -- `Parser::string` rejects a raw byte <= 0x1F
    // (RFC 8259 §7) and a raw `"` closes the string while a raw `\` opens an escape --
    // so `\X` against `\u00XX` is the only cross-spelling comparison that reaches the
    // arm at all. The hex value is the code point the arm pushes.
    assert!(eq("\"\\\"\"", "\"\\u0022\"")); // quotation mark  U+0022
    assert!(eq("\"\\\\\"", "\"\\u005c\"")); // reverse solidus U+005C
    assert!(eq("\"\\b\"", "\"\\u0008\"")); // backspace       U+0008
    assert!(eq("\"\\f\"", "\"\\u000c\"")); // form feed       U+000C
    assert!(eq("\"\\n\"", "\"\\u000a\"")); // line feed       U+000A
    assert!(eq("\"\\r\"", "\"\\u000d\"")); // carriage return U+000D
    assert!(eq("\"\\t\"", "\"\\u0009\"")); // tab             U+0009
    assert!(eq("\"\\/\"", "\"\\u002f\"")); // solidus         U+002F
    // ... and the arms must not be interchangeable: each escape decodes to its OWN
    // code point, so no two of them compare equal. A single arm that pushed the wrong
    // character would satisfy the pairs above only if its `\uXXXX` twin were wrong the
    // same way, which the hex digits above forbid; this loop closes the remaining case
    // of two arms swapped with each other.
    let escapes = ["\\\"", "\\\\", "\\b", "\\f", "\\n", "\\r", "\\t", "\\/"];
    for (i, a) in escapes.iter().enumerate() {
        for b in &escapes[i + 1..] {
            assert!(
                !eq(&format!("\"{a}\""), &format!("\"{b}\"")),
                "{a} and {b} decode to the same character"
            );
        }
    }
}

#[test]
fn number_lexemes_are_compared_byte_for_byte() {
    assert!(!eq("1e2", "100"));
    assert!(!eq("100.0", "100"));
    assert!(!eq("1.0", "1"));
    assert!(!eq("1E2", "1e2")); // exponent case is significant
    assert!(!eq("-0", "0"));
    assert!(!eq("1e+2", "1e2"));
    // Identical lexemes (only whitespace differs) are equal.
    assert!(eq(" 1e2 ", "1e2"));
    assert!(eq(
        "123456789012345678901234567890",
        "123456789012345678901234567890"
    ));
}

#[test]
fn key_order_is_significant() {
    assert!(!eq("{\"a\":1,\"b\":2}", "{\"b\":2,\"a\":1}"));
    assert!(eq("{\"a\":1,\"b\":2}", "{ \"a\":1 , \"b\":2 }"));
}

#[test]
fn duplicate_keys_are_preserved_not_collapsed() {
    // Collapsing a duplicate changes the document.
    assert!(!eq("{\"a\":1,\"a\":2}", "{\"a\":2}"));
    assert!(!eq("{\"a\":1,\"a\":2}", "{\"a\":1}"));
    // Reordering the values under a duplicated key changes the document.
    assert!(!eq("{\"a\":1,\"a\":2}", "{\"a\":2,\"a\":1}"));
    // Same duplicates in the same order, only whitespace differing, are equal.
    assert!(eq("{\"a\":1,\"a\":2}", "{ \"a\":1, \"a\":2 }"));
    // Duplicate keys at a nested level.
    assert!(!eq(
        "{\"o\":{\"a\":1,\"a\":2}}",
        "{\"o\":{\"a\":2,\"a\":1}}"
    ));
}

#[test]
fn array_order_is_significant() {
    assert!(!eq("[1,2]", "[2,1]"));
    assert!(!eq("[1,2,3]", "[1,3,2]"));
    assert!(eq("[1,2,3]", "[ 1, 2, 3 ]"));
}

#[test]
fn type_and_shape_differences_are_not_equal() {
    assert!(!eq("1", "\"1\""));
    assert!(!eq("true", "1"));
    assert!(!eq("null", "false"));
    assert!(!eq("{}", "[]"));
    assert!(!eq("{\"a\":1}", "[1]"));
    assert!(!eq("{\"a\":1}", "{\"a\":1,\"b\":2}"));
    assert!(!eq("{\"a\":[1,2,3]}", "{\"a\":[1,2,4]}"));
    assert!(!eq("\"abc\"", "\"abd\""));
}

#[test]
fn lone_surrogates_compare_as_raw_lexemes() {
    // Reflexive: a lone surrogate equals itself.
    assert!(eq("\"\\ud834\"", "\"\\ud834\""));
    assert!(eq("[\"a\\udeadb\"]", "[\"a\\udeadb\"]"));
    // Different lone surrogates differ.
    assert!(!eq("\"\\ud834\"", "\"\\udc00\""));
    // Hex CASE is significant for lone surrogates (opposite of the valid-scalar rule).
    assert!(!eq("\"\\uD834\"", "\"\\ud834\""));
    // A lone surrogate is never equal to ordinary text.
    assert!(!eq("\"\\ud834\"", "\"a\""));
    // Whitespace outside the string is still ignored around a raw-compared string.
    assert!(eq("[ \"\\ud834\" ]", "[\"\\ud834\"]"));
    // A lone surrogate as an object key is compared raw as well.
    assert!(eq("{\"\\udead\":1}", "{\"\\udead\":1}"));
    assert!(!eq("{\"\\udead\":1}", "{\"\\udeAD\":1}"));

    // The fixtures above reach the raw path through only two of the four ways
    // `decode_string_body` can decide a high surrogate is unpaired. The `paired` test
    // is `chars[idx] == '\\' && chars[idx + 1] == 'u' && idx + 6 <= len &&
    // (0xDC00..=0xDFFF).contains(hex4(idx + 2))`, so each conjunct needs a fixture,
    // and hex case is the observable: a string that decodes to `Str::Raw` keeps the
    // body verbatim, while one that decodes to `Str::Text` has lost the spelling.
    //
    // (a) nothing follows -- `chars.get(idx) == None`. Covered by the first fixture.
    // (b) a following character that is not an escape.
    assert!(!eq("\"\\ud834a\"", "\"\\uD834a\""));
    // (c) a following escape that is not `\u`.
    assert!(!eq("\"\\ud834\\n\"", "\"\\uD834\\n\""));
    // (d) a following `\u` escape that is NOT a low surrogate. This is the only
    //     fixture that prices the UPPER bound of the low-surrogate range: `\ue000` is
    //     above it, so widening the test to `>= 0xDC00` would pair these two into the
    //     scalar 0x10000 + (0x034 << 10) + (0xE000 - 0xDC00) = U+1F400 -- a real
    //     `char`, decoded identically on both sides, so the raw spelling (and this
    //     inequality) would vanish. `\u0041` covers the same conjunct from below.
    assert!(!eq("\"\\ud834\\ue000\"", "\"\\uD834\\ue000\""));
    assert!(!eq("\"\\ud834\\u0041\"", "\"\\uD834\\u0041\""));
    // A raw-compared string is never equal to the text it would have decoded to, so
    // the two `Str` variants cannot be confused: `Str::Raw("\\ud834\\u0041")` against
    // `Str::Text("A")` and against the text of the pair that WOULD have been formed.
    assert!(!eq("\"\\ud834\\u0041\"", "\"A\""));
    assert!(!eq("\"\\ud834\\ue000\"", "\"\u{1f400}\""));
    // A valid pair is NOT raw: it decodes, so its hex case stops mattering -- the
    // boundary values of the low-surrogate range both pair.
    assert!(eq("\"\\ud834\\udc00\"", "\"\\uD834\\uDC00\""));
    assert!(eq("\"\\ud834\\udfff\"", "\"\\uD834\\uDFFF\""));
    // A lone LOW surrogate returns raw before the pairing test is ever reached, so a
    // following high surrogate does not rescue it.
    assert!(!eq("\"\\udc00\\ud834\"", "\"\\udc00\\uD834\""));
    // The high-surrogate range is bounded on both ends too: U+D7FF and U+E000 are
    // ordinary BMP scalars and decode, so their hex case is irrelevant.
    assert!(eq("\"\\ud7ff\"", "\"\\uD7FF\""));
    assert!(eq("\"\\ue000\"", "\"\\uE000\""));
    // ... while the ends of the surrogate block itself are raw.
    assert!(!eq("\"\\ud800\"", "\"\\uD800\""));
    assert!(!eq("\"\\udfff\"", "\"\\uDFFF\""));
}

/// The oracle parser is recursive with a fixed ceiling, so "valid JSON" and
/// "comparable by this oracle" are not the same set. Both sides of that boundary are
/// pinned here because [`json_semantically_eq`] documents the difference.
#[test]
fn nesting_deeper_than_the_oracle_ceiling_is_not_comparable() {
    // `Parser::value` increments `depth` on entry and restores it on the way out, so
    // `depth` is the nesting level of the value being parsed, and the refusal is at
    // the first level ABOVE the ceiling (`if self.depth > MAX_DEPTH`). An empty
    // container at the bottom costs no extra level -- `array()`/`object()` return
    // before calling `value()` when they see the closing bracket -- so N nested
    // arrays reach exactly depth N.
    let at = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    let over = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
    assert!(eq(&at, &at), "{MAX_DEPTH} nested arrays should parse");
    assert!(
        !eq(&over, &over),
        "{} nested arrays are valid JSON and must NOT be comparable",
        MAX_DEPTH + 1
    );
    // A scalar at the bottom is a value of its own, so it costs one level: N nested
    // arrays around a number reach depth N + 1.
    let scalar_at = format!(
        "{}1{}",
        "[".repeat(MAX_DEPTH - 1),
        "]".repeat(MAX_DEPTH - 1)
    );
    let scalar_over = format!("{}1{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    assert!(eq(&scalar_at, &scalar_at));
    assert!(!eq(&scalar_over, &scalar_over));
    // Objects count the same way: `object()` calls `value()` once per member value.
    let obj_at = format!(
        "{}{{}}{}",
        "{\"k\":".repeat(MAX_DEPTH - 1),
        "}".repeat(MAX_DEPTH - 1)
    );
    let obj_over = format!(
        "{}{{}}{}",
        "{\"k\":".repeat(MAX_DEPTH),
        "}".repeat(MAX_DEPTH)
    );
    assert!(eq(&obj_at, &obj_at));
    assert!(!eq(&obj_over, &obj_over));
    // The claim on `MAX_DEPTH` is that everything the engine accepts under its default
    // config parses here. The engine's default ceiling is 512 nested containers (named
    // as a literal on purpose: this file imports nothing from `tokfold_core`), and the
    // deepest document it accepts is 512 containers around a scalar, which is depth
    // 513 here -- so the margin is real and this is not a near miss.
    const ENGINE_DEFAULT_MAX_DEPTH: usize = 512;
    const _: () = assert!(ENGINE_DEFAULT_MAX_DEPTH + 1 < MAX_DEPTH);
    let deepest_the_engine_takes = format!(
        "{}1{}",
        "[".repeat(ENGINE_DEFAULT_MAX_DEPTH),
        "]".repeat(ENGINE_DEFAULT_MAX_DEPTH)
    );
    assert!(eq(&deepest_the_engine_takes, &deepest_the_engine_takes));
}

/// [`json_semantically_eq`] claims symmetry and transitivity; nothing read that back
/// until this test. It parses each side independently, so symmetry is not free, and
/// the derived `PartialEq` on [`Value`] is only as transitive as the normalization
/// that precedes it.
#[test]
fn the_relation_is_symmetric_and_transitive_on_parseable_input() {
    // Spellings grouped by the document they denote. Within a group every ORDERED
    // pair must be equal -- which is symmetry, and, for the three-member groups,
    // transitivity through a middle spelling that shares no bytes with the other two.
    // Across groups every ordered pair must differ, so a normalization that collapsed
    // two documents into one would show up here as well as a rule that lost one.
    let groups: &[&[&str]] = &[
        &["{\"a\":1}", "{ \"a\" : 1 }", "{\"\\u0061\":1}"],
        &["[1,2]", "[ 1 , 2 ]", "[\n\t1,\r2 ]"],
        // Three spellings of U+1D11E: literal, escaped pair, escaped pair upper-cased.
        &["\"\u{1d11e}\"", "\"\\ud834\\udd1e\"", "\"\\uD834\\uDD1E\""],
        &["\"\\t\"", "\"\\u0009\""],
        // Raw (lone surrogate): only the whitespace outside the string may vary.
        &["\"\\ud834\"", " \"\\ud834\" ", "\t\"\\ud834\"\r"],
        // ... and the upper-case spelling of the SAME lone surrogate is its own group.
        &["\"\\uD834\""],
        &["1e2", " 1e2", "1e2\r"],
        &["100"],
        &["1E2"],
        &["null"],
        &["true"],
        &["{}", "{ }"],
        &["[]", "[ ]"],
        &["0"],
        &["-0"],
    ];
    for (gi, g) in groups.iter().enumerate() {
        for a in *g {
            for b in *g {
                assert!(eq(a, b), "group {gi}: {a:?} should equal {b:?}");
            }
        }
        for (gj, h) in groups.iter().enumerate() {
            if gi == gj {
                continue;
            }
            for a in *g {
                for b in *h {
                    assert!(
                        !eq(a, b),
                        "groups {gi}/{gj}: {a:?} should differ from {b:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn empty_and_scalar_documents() {
    assert!(eq("\"\"", "\"\""));
    assert!(eq("null", "null"));
    assert!(eq("true", " true "));
    assert!(!eq("\"\"", "\" \""));
}

#[test]
fn invalid_documents_are_equal_to_nothing() {
    // Two identical INVALID inputs are still not "equal": the oracle is only defined
    // on well-formed JSON, and a parse failure means "not comparable".
    assert!(!eq("{", "{"));
    assert!(!eq("", ""));
    assert!(!eq("   ", "   "));
    assert!(!eq("{\"a\":}", "{\"a\":}"));
    assert!(!eq("01", "01")); // leading zero is invalid JSON
    assert!(!eq("NaN", "NaN"));
    assert!(!eq("[1,2,]", "[1,2,]")); // trailing comma
    assert!(!eq("{} {}", "{} {}")); // trailing data
    // A valid document is never equal to an invalid one.
    assert!(!eq("{\"a\":1}", "{\"a\":1"));
}

#[test]
fn reflexive_on_assorted_valid_documents() {
    for doc in [
        "null",
        "true",
        "false",
        "0",
        "-0",
        "1e10",
        "\"\"",
        "\"héllo\"",
        "\"\\ud834\"",
        "{}",
        "[]",
        "{\"a\":1,\"a\":2}",
        "[1,[2,[3,[4]]]]",
        "{\"x\":[1,2,{\"y\":3}],\"z\":{\"w\":[true,false,null]}}",
    ] {
        assert!(eq(doc, doc), "not reflexive on {doc:?}");
    }
}
