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
//! the reversibility contract frozen in `docs/ai/impl-spec-v0.1.md` §4:
//!
//! * EQUAL requires the same tree shape; identical key order; duplicate keys
//!   preserved identically in order; identical array order; **byte-identical number
//!   lexemes** (`1e2` != `100.0`, `1E2` != `1e2`); strings equal **after unescaping**.
//! * IGNORED (canonicalized away): insignificant whitespace and escape style
//!   (`é` == `é` == `é`, `\/` == `/`).
//! * Lone surrogates cannot be unescaped to UTF-8, so any string containing one is
//!   compared by its **raw body bytes** (hex case significant): `"\uD834"` !=
//!   `"\ud834"`, even though both are unpaired high surrogates.
//!
//! This is deliberately a **separate, hand-rolled recursive-descent parser**. It
//! shares no code with `tokfold_core::tape`, so a bug that made the tape and the
//! oracle agree by construction cannot hide: the two parsers were written
//! independently against the same grammar. The oracle imports nothing from the crate
//! under test.
//!
//! Both `tests/oracle.rs` (this file, which also unit-tests the oracle against
//! hand-built tricky pairs) and `tests/roundtrip.rs` use `json_semantically_eq`;
//! `roundtrip.rs` pulls it in with `#[path = "oracle.rs"] mod oracle;`.

/// A parsed JSON value, normalized so that `==` means exactly the equality spec §4
/// mandates: order-preserving, duplicate-preserving, number lexemes compared
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

/// A parsed JSON string, normalized for the comparison spec §4 requires.
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

/// Decide whether two JSON documents are semantically equal per spec §4.
///
/// Returns `false` if either side is not a single well-formed JSON document (an
/// invalid document is equal to nothing, not even itself). On valid input the
/// relation is reflexive, symmetric and transitive.
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
// The oracle's own tests: hand-built tricky pairs. These pin the §4 rules the
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
