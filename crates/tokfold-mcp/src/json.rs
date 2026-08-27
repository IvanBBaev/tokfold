//! A dependency-free JSON reader and writer for the MCP wire format.
//!
//! # Why not `serde_json`
//!
//! Two reasons, in order of weight. The normative implementation spec forbids
//! `serde_json::Value` in the parse path, and nothing in `tokfold-core` implements
//! `serde::Serialize` — so a derive-based stack would buy no mapping code, only a
//! larger published dependency graph for a crate whose entire runtime dependency
//! budget is three crates. This module is the whole cost of that choice, and it is
//! bounded: a value tree, a depth-limited parser, and a writer.
//!
//! # Determinism
//!
//! [`Value::Object`] is an ordered `Vec` of pairs, never a hash map. The project's
//! prompt-cache invariant requires that the same logical content serialize to the
//! same bytes on every run, and hash-map iteration order is randomized per process.
//! Ordered pairs also let the server emit fields in a fixed, reviewable order.
//!
//! # Limits
//!
//! [`parse`] rejects nesting deeper than [`MAX_DEPTH`]. The parser recurses, so
//! without that bound a hostile message would abort the process on a stack
//! overflow — which a `catch_unwind` bulkhead cannot catch.
//!
//! [`parse`] also rejects a message holding more than [`MAX_NODES`] values. The
//! transport's byte cap does not bound the tree: a parsed value is much larger than
//! the text it came from, so bytes and nodes are two different quantities and only
//! one of them was bounded before. [`MAX_NODES`] carries the arithmetic.

use core::fmt;

/// Maximum container nesting [`parse`] accepts before returning an error.
///
/// MCP messages are shallow; the limit exists to bound recursion, not to express a
/// protocol rule. It is far below the depth at which the default stack overflows.
pub const MAX_DEPTH: usize = 128;

/// Maximum number of JSON values [`parse`] will materialise from one message.
///
/// # Why a second limit is needed at all
///
/// [`crate::MAX_MESSAGE_BYTES`] bounds the *text*. It does not bound the tree, because
/// a value costs far more in memory than on the wire: `Value` is 32 bytes and an object
/// member — a `(String, Value)` pair — is 56, while the cheapest array element, `0,`,
/// is two bytes of input. That is 16x in the steady state, and a `Vec` that doubles on
/// its last push holds the old buffer and the new one at once, so the transient cost is
/// three times the steady one.
///
/// Run that out for a line at the byte cap: 33,554,432 bytes of `[0,0,0,…]` is
/// 16,777,216 values — 512 MiB of `Value` at rest and about 1.5 GiB across the final
/// reallocation. That is worse than an ordinary flood, because a failed allocation in
/// Rust **aborts** the process; it does not unwind, so the `catch_unwind` bulkhead in
/// [`crate::stdio`] never sees it and the session dies with whatever it was holding.
///
/// # The number
///
/// `MAX_MESSAGE_BYTES / 8` = 4,194,304 values, i.e. the parser insists on an average of
/// at least eight input bytes per value. Two facts make that the right threshold:
///
/// * **No real request comes close.** The largest legitimate call this server takes is
///   `tokfold_decompress` carrying a ~22 MiB base64 archive, and that archive is a
///   single string — *one* value. The whole envelope is about eight. `tools/list` is
///   four, an `initialize` handshake about a dozen. The margin over real traffic is
///   five orders of magnitude, and it does not shrink as payloads grow, because payload
///   size and node count are independent here.
/// * **Not even the densest *legal* line reaches it.** The one shape that scales with
///   node count rather than payload size is a batch. A minimal well-formed member,
///   `{"jsonrpc":"2.0","method":"a"}`, is 30 bytes and 3 values, so 31 bytes with its
///   separator; a batch filling the byte cap holds about 1,082,000 of them, or about
///   3,247,000 values. That is 23% below the budget. Every well-formed message
///   therefore still parses, and the shapes that trip the limit — an array of bare
///   digits at 2 bytes per value, an array of tiny objects at about 6 — are ones no
///   request grammar produces.
///
/// At the budget the tree is capped at 128 MiB of `Value` (192 MiB across the last
/// doubling: the budget is 2^22, so the reallocation that reaches it happens at element
/// 2^21), or 224 MiB where every counted value is an object member. Bounded, and
/// bounded by arithmetic rather than by hope.
///
/// A message over the budget is refused as a [`ParseError`], which the server answers
/// with `-32700` and no id — the same treatment as any other unparseable line. There is
/// no tree to read an id out of, so inventing a second rule for this case would only
/// mean a client could not tell the two apart.
pub const MAX_NODES: usize = crate::MAX_MESSAGE_BYTES / 8;

/// A JSON value.
///
/// Numbers are split into [`Value::Int`] and [`Value::Float`] rather than kept as a
/// single `f64`: JSON-RPC request identifiers are integers and must round-trip
/// exactly, which `f64` cannot promise past 2^53.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number with no fraction or exponent, held exactly.
    Int(i64),
    /// A number with a fraction or exponent.
    Float(f64),
    /// A string, already unescaped.
    Str(String),
    /// An array.
    Array(Vec<Self>),
    /// An object, in insertion order. Order is preserved on both parse and write.
    Object(Vec<(String, Self)>),
}

impl Value {
    /// Builds a string value from anything that converts into a `String`.
    #[must_use]
    pub fn string(value: impl Into<String>) -> Self {
        Self::Str(value.into())
    }

    /// Returns the string contents, or `None` if this is not a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the integer value, or `None` if this is not an integer.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Returns the boolean value, or `None` if this is not a boolean.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the object's entries, or `None` if this is not an object.
    #[must_use]
    pub fn as_object(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Object(entries) => Some(entries),
            _ => None,
        }
    }

    /// Returns the array's elements, or `None` if this is not an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Whether this value is `null`.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Looks up a member of an object.
    ///
    /// Returns `None` for a non-object or a missing key. A parsed object cannot
    /// contain a duplicate key — [`parse`] rejects those outright, see its docs — so
    /// the first-occurrence rule below only applies to values built in code.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.as_object()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

/// Builder for an object value, keeping fields in the order they are set.
#[derive(Debug, Clone, Default)]
pub struct Object(Vec<(String, Value)>);

impl Object {
    /// Creates an empty object.
    #[must_use]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Appends a field.
    #[must_use]
    pub fn set(mut self, key: &str, value: Value) -> Self {
        self.0.push((key.to_owned(), value));
        self
    }

    /// Appends a field only when it is present, so optional fields stay absent
    /// rather than becoming `null`.
    #[must_use]
    pub fn set_opt(self, key: &str, value: Option<Value>) -> Self {
        match value {
            Some(value) => self.set(key, value),
            None => self,
        }
    }

    /// Finishes the object.
    #[must_use]
    pub fn build(self) -> Value {
        Value::Object(self.0)
    }
}

impl fmt::Display for Value {
    /// Writes compact JSON: no spaces, no trailing newline, no embedded raw
    /// newlines. The stdio transport frames one message per line, so a raw newline
    /// anywhere in the output would split one message into two.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(true) => f.write_str("true"),
            Self::Bool(false) => f.write_str("false"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Float(x) => write_float(f, *x),
            Self::Str(s) => write_escaped(f, s),
            Self::Array(items) => write_array(f, items),
            Self::Object(entries) => write_object(f, entries),
        }
    }
}

/// Writes a float, mapping non-finite values to `null`.
///
/// JSON has no syntax for `NaN` or infinity. Emitting the bare Rust spelling would
/// produce a message no conformant client can parse, so a non-finite value degrades
/// to `null`. The parser never produces one — it rejects a literal that overflows to
/// infinity — so this is a guard on values built in code, not on anything that
/// arrives over the wire.
///
/// An integral float keeps a `.0` suffix. `Display for f64` renders `1.0` as `1`,
/// which this crate's own parser would read back as [`Value::Int`]; a ratio field
/// that changes JSON type depending on whether it happens to land on a whole number
/// is a needless differential for a client validating against a schema.
fn write_float(f: &mut fmt::Formatter<'_>, value: f64) -> fmt::Result {
    if !value.is_finite() {
        return f.write_str("null");
    }
    if value.fract() == 0.0 && value.abs() < 1e16 {
        write!(f, "{value:.1}")
    } else {
        write!(f, "{value}")
    }
}

fn write_array(f: &mut fmt::Formatter<'_>, items: &[Value]) -> fmt::Result {
    f.write_str("[")?;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            f.write_str(",")?;
        }
        write!(f, "{item}")?;
    }
    f.write_str("]")
}

fn write_object(f: &mut fmt::Formatter<'_>, entries: &[(String, Value)]) -> fmt::Result {
    f.write_str("{")?;
    for (i, (key, value)) in entries.iter().enumerate() {
        if i > 0 {
            f.write_str(",")?;
        }
        write_escaped(f, key)?;
        f.write_str(":")?;
        write!(f, "{value}")?;
    }
    f.write_str("}")
}

/// Writes a JSON string literal, escaping what RFC 8259 requires.
///
/// Every code point below `0x20` is escaped, which is what keeps `\n` and `\r` out
/// of the framed output. Characters above ASCII are emitted literally as UTF-8;
/// the transport is UTF-8, so `\u` escaping them would only inflate the message.
/// Runs of characters that need no escaping are written in one call rather than one
/// per character: a tool result can carry the whole compressed payload, and at that
/// size a per-character `write!` is the dominant cost of serializing a reply.
fn write_escaped(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_str("\"")?;
    let mut plain_from = 0;
    for (i, ch) in s.char_indices() {
        let escape = match ch {
            '"' => "\\\"",
            '\\' => "\\\\",
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            '\u{08}' => "\\b",
            '\u{0c}' => "\\f",
            c if c < '\u{20}' => {
                f.write_str(s.get(plain_from..i).unwrap_or_default())?;
                write!(f, "\\u{:04x}", u32::from(c))?;
                plain_from = i + 1;
                continue;
            }
            _ => continue,
        };
        f.write_str(s.get(plain_from..i).unwrap_or_default())?;
        f.write_str(escape)?;
        plain_from = i + ch.len_utf8();
    }
    f.write_str(s.get(plain_from..).unwrap_or_default())?;
    f.write_str("\"")
}

/// A JSON syntax error, located at a byte offset in the input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid JSON at byte {offset}: {msg}")]
pub struct ParseError {
    /// Byte offset where the parser gave up.
    pub offset: usize,
    /// What the parser expected. A fixed string: it is safe to log and allocates
    /// nothing on the error path.
    pub msg: &'static str,
}

/// Parses one complete JSON value, rejecting trailing content.
///
/// # Errors
///
/// Returns [`ParseError`] on malformed syntax, nesting deeper than [`MAX_DEPTH`],
/// more than [`MAX_NODES`] values, an invalid `\u` escape, a duplicate key in an
/// object, or any non-whitespace byte after the value.
///
/// The number grammar is RFC 8259 exactly: no leading zeros, no bare `1.`, no bare
/// `.5`, and a literal that overflows to infinity is an error rather than a value
/// this crate could not write back out.
pub fn parse(input: &str) -> Result<Value, ParseError> {
    parse_bounded(input, MAX_NODES)
}

/// [`parse`], with the node budget supplied rather than taken from [`MAX_NODES`].
///
/// This exists so the budget's boundary can be pinned exactly — accepted at the limit,
/// refused one past it — without building the ~8 MB of input the production constant
/// would need. It is private and `parse` is its only non-test caller, which passes
/// [`MAX_NODES`] unconditionally; there is no path by which a test can relax the limit
/// a real message is held to.
fn parse_bounded(input: &str, max_nodes: usize) -> Result<Value, ParseError> {
    let mut parser = Parser {
        bytes: input.as_bytes(),
        pos: 0,
        depth: 0,
        nodes: 0,
        max_nodes,
    };
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.peek().is_some() {
        return Err(parser.error("trailing content after value"));
    }
    Ok(value)
}

/// Reports whether an object repeats a key.
///
/// RFC 8259 permits duplicate names but calls the resulting behaviour unpredictable,
/// and implementations duly disagree: `serde_json` and JavaScript keep the last
/// occurrence, [`Value::get`] would keep the first. That disagreement is a smuggling
/// primitive — one message that a proxy and its backend read differently — so the
/// input is refused instead of resolved. Sorting borrowed keys keeps this
/// `O(n log n)`; a linear scan per key would turn a wide object into a quadratic
/// denial of service.
fn has_duplicate_key(entries: &[(String, Value)]) -> bool {
    let mut keys: Vec<&str> = entries.iter().map(|(key, _)| key.as_str()).collect();
    let total = keys.len();
    keys.sort_unstable();
    keys.dedup();
    keys.len() != total
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
    /// Values materialised so far. See [`MAX_NODES`].
    nodes: usize,
    /// Ceiling for `nodes`. Always [`MAX_NODES`] outside this module's own tests.
    max_nodes: usize,
}

impl Parser<'_> {
    fn error(&self, msg: &'static str) -> ParseError {
        ParseError {
            offset: self.pos,
            msg,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn bump(&mut self) {
        self.pos = self.pos.saturating_add(1);
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.bump();
        }
    }

    /// Consumes an expected byte, or fails.
    fn expect(&mut self, byte: u8, msg: &'static str) -> Result<(), ParseError> {
        if self.peek() == Some(byte) {
            self.bump();
            Ok(())
        } else {
            Err(self.error(msg))
        }
    }

    /// Consumes a bare literal such as `true`, checking every byte.
    fn expect_literal(&mut self, literal: &[u8], msg: &'static str) -> Result<(), ParseError> {
        for expected in literal {
            self.expect(*expected, msg)?;
        }
        Ok(())
    }

    fn parse_value(&mut self) -> Result<Value, ParseError> {
        // Every value in the tree passes through here exactly once — arrays and objects
        // reach their children by calling back into this function — so one integer
        // increment on this path bounds the whole tree. No allocation, no second walk.
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > self.max_nodes {
            return Err(self.error("too many values in one message"));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'n') => {
                self.expect_literal(b"null", "expected `null`")?;
                Ok(Value::Null)
            }
            Some(b't') => {
                self.expect_literal(b"true", "expected `true`")?;
                Ok(Value::Bool(true))
            }
            Some(b'f') => {
                self.expect_literal(b"false", "expected `false`")?;
                Ok(Value::Bool(false))
            }
            Some(b'"') => self.parse_string().map(Value::Str),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(self.error("expected a value")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    /// Runs `body` one nesting level deeper, enforcing [`MAX_DEPTH`].
    fn nested<T>(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        if self.depth >= MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        self.depth = self.depth.saturating_add(1);
        let result = body(self);
        self.depth = self.depth.saturating_sub(1);
        result
    }

    fn parse_array(&mut self) -> Result<Value, ParseError> {
        self.expect(b'[', "expected `[`")?;
        self.nested(|p| {
            let mut items = Vec::new();
            p.skip_ws();
            if p.peek() == Some(b']') {
                p.bump();
                return Ok(Value::Array(items));
            }
            loop {
                items.push(p.parse_value()?);
                p.skip_ws();
                match p.peek() {
                    Some(b',') => p.bump(),
                    Some(b']') => {
                        p.bump();
                        return Ok(Value::Array(items));
                    }
                    _ => return Err(p.error("expected `,` or `]`")),
                }
            }
        })
    }

    fn parse_object(&mut self) -> Result<Value, ParseError> {
        self.expect(b'{', "expected `{`")?;
        self.nested(|p| {
            let mut entries = Vec::new();
            p.skip_ws();
            if p.peek() == Some(b'}') {
                p.bump();
                return Ok(Value::Object(entries));
            }
            loop {
                p.skip_ws();
                let key = p.parse_string()?;
                p.skip_ws();
                p.expect(b':', "expected `:`")?;
                entries.push((key, p.parse_value()?));
                p.skip_ws();
                match p.peek() {
                    Some(b',') => p.bump(),
                    Some(b'}') => {
                        p.bump();
                        if has_duplicate_key(&entries) {
                            return Err(p.error("duplicate key in object"));
                        }
                        return Ok(Value::Object(entries));
                    }
                    _ => return Err(p.error("expected `,` or `}`")),
                }
            }
        })
    }

    /// Parses a number against the RFC 8259 grammar.
    ///
    /// The grammar is enforced rather than delegated to `str::parse`, which is far
    /// more permissive than JSON: it would accept `01`, `1.`, `+1`, `inf` and `NaN`.
    /// A parser that quietly takes input its peers reject is a message-smuggling
    /// primitive, so each production is checked here.
    fn parse_number(&mut self) -> Result<Value, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.bump();
        }
        // int = "0" / (digit1-9 *DIGIT). A leading zero stands alone.
        match self.peek() {
            Some(b'0') => {
                self.bump();
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.error("leading zeros are not allowed"));
                }
            }
            Some(b'1'..=b'9') => self.skip_digits(),
            _ => return Err(self.error("expected a digit")),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.bump();
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("a fraction needs at least one digit"));
            }
            self.skip_digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.bump();
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.bump();
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("an exponent needs at least one digit"));
            }
            self.skip_digits();
        }
        let text = self
            .bytes
            .get(start..self.pos)
            .and_then(|slice| core::str::from_utf8(slice).ok())
            .ok_or_else(|| self.error("malformed number"))?;
        if is_float {
            self.finite_float(text)
        } else {
            // An integer literal too large for `i64` is still valid JSON. Widening it
            // to `f64` keeps the message parseable; the only field this server reads
            // as an integer is the request id, which it echoes back unchanged.
            text.parse::<i64>()
                .map_or_else(|_| self.finite_float(text), |n| Ok(Value::Int(n)))
        }
    }

    /// Parses a syntactically valid literal, rejecting one that overflows to infinity.
    ///
    /// `"1e999".parse::<f64>()` succeeds and yields `inf`, which JSON cannot spell.
    /// Keeping it would put a value in the tree that [`Value::to_string`] has to
    /// degrade to `null`, so the input is refused instead of silently changing type.
    fn finite_float(&self, text: &str) -> Result<Value, ParseError> {
        match text.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Value::Float(n)),
            Ok(_) => Err(self.error("number is out of range")),
            Err(_) => Err(self.error("malformed number")),
        }
    }

    fn skip_digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.bump();
        }
    }

    fn parse_string(&mut self) -> Result<String, ParseError> {
        self.expect(b'"', "expected `\"`")?;
        // Bytes are collected raw so that multi-byte UTF-8 needs no special case;
        // escapes are decoded to `char` and appended in their UTF-8 form.
        let mut out: Vec<u8> = Vec::new();
        let mut buf = [0_u8; 4];
        loop {
            match self.peek() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.bump();
                    return String::from_utf8(out).map_err(|_| self.error("string is not UTF-8"));
                }
                Some(b'\\') => {
                    self.bump();
                    let ch = self.parse_escape()?;
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                Some(byte) if byte < 0x20 => {
                    return Err(self.error("unescaped control character in string"));
                }
                Some(byte) => {
                    out.push(byte);
                    self.bump();
                }
            }
        }
    }

    fn parse_escape(&mut self) -> Result<char, ParseError> {
        let escape = self.peek().ok_or_else(|| self.error("truncated escape"))?;
        self.bump();
        match escape {
            b'"' => Ok('"'),
            b'\\' => Ok('\\'),
            b'/' => Ok('/'),
            b'b' => Ok('\u{08}'),
            b'f' => Ok('\u{0c}'),
            b'n' => Ok('\n'),
            b'r' => Ok('\r'),
            b't' => Ok('\t'),
            b'u' => self.parse_unicode_escape(),
            _ => Err(self.error("unknown escape")),
        }
    }

    /// Decodes `\uXXXX`, joining a surrogate pair into one code point.
    ///
    /// An unpaired surrogate is rejected rather than replaced. A silent
    /// substitution would change the caller's text without saying so, and this
    /// crate's whole contract is that text survives a round trip unchanged.
    fn parse_unicode_escape(&mut self) -> Result<char, ParseError> {
        let high = self.read_hex4()?;
        if (0xD800..0xDC00).contains(&high) {
            self.expect(b'\\', "expected a low surrogate escape")?;
            self.expect(b'u', "expected a low surrogate escape")?;
            let low = self.read_hex4()?;
            if !(0xDC00..0xE000).contains(&low) {
                return Err(self.error("expected a low surrogate"));
            }
            let code = 0x1_0000 + ((high - 0xD800) << 10) + (low - 0xDC00);
            char::from_u32(code).ok_or_else(|| self.error("invalid code point"))
        } else if (0xDC00..0xE000).contains(&high) {
            Err(self.error("unpaired low surrogate"))
        } else {
            char::from_u32(high).ok_or_else(|| self.error("invalid code point"))
        }
    }

    fn read_hex4(&mut self) -> Result<u32, ParseError> {
        let mut value = 0_u32;
        for _ in 0..4 {
            let byte = self.peek().ok_or_else(|| self.error("truncated escape"))?;
            let digit = match byte {
                b'0'..=b'9' => u32::from(byte - b'0'),
                b'a'..=b'f' => u32::from(byte - b'a') + 10,
                b'A'..=b'F' => u32::from(byte - b'A') + 10,
                _ => return Err(self.error("invalid hex digit in escape")),
            };
            value = value * 16 + digit;
            self.bump();
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{MAX_DEPTH, MAX_NODES, Object, Value, parse, parse_bounded};

    fn roundtrip(text: &str) -> String {
        parse(text).unwrap().to_string()
    }

    #[test]
    fn scalars_round_trip() {
        assert_eq!(roundtrip("null"), "null");
        assert_eq!(roundtrip("true"), "true");
        assert_eq!(roundtrip("false"), "false");
        assert_eq!(roundtrip("42"), "42");
        assert_eq!(roundtrip("-7"), "-7");
        assert_eq!(roundtrip("\"hi\""), "\"hi\"");
    }

    #[test]
    fn integers_keep_full_precision() {
        // The reason Int and Float are separate: an f64 would round this.
        assert_eq!(roundtrip("9007199254740993"), "9007199254740993");
        assert_eq!(
            parse("9007199254740993").unwrap().as_i64(),
            Some(9_007_199_254_740_993)
        );
    }

    #[test]
    fn integers_too_large_for_i64_widen_instead_of_failing() {
        // Still valid JSON, so it must parse; precision loss is acceptable because
        // the server never interprets such a number.
        assert!(matches!(
            parse("123456789012345678901234567890").unwrap(),
            Value::Float(_)
        ));
    }

    #[test]
    fn containers_preserve_order() {
        assert_eq!(roundtrip(r#"{"b":1,"a":2}"#), r#"{"b":1,"a":2}"#);
        assert_eq!(roundtrip("[3,1,2]"), "[3,1,2]");
    }

    #[test]
    fn whitespace_is_ignored_between_tokens() {
        assert_eq!(roundtrip(" { \"a\" : [ 1 , 2 ] } "), r#"{"a":[1,2]}"#);
    }

    #[test]
    fn escapes_decode_and_re_encode() {
        assert_eq!(parse(r#""a\nb""#).unwrap().as_str(), Some("a\nb"));
        assert_eq!(parse(r#""\u0041""#).unwrap().as_str(), Some("A"));
        assert_eq!(parse(r#""\/""#).unwrap().as_str(), Some("/"));
        assert_eq!(roundtrip(r#""a\nb""#), r#""a\nb""#);
    }

    #[test]
    fn surrogate_pairs_join_into_one_code_point() {
        assert_eq!(parse(r#""\ud83d\ude00""#).unwrap().as_str(), Some("😀"));
    }

    #[test]
    fn unpaired_surrogates_are_rejected() {
        assert!(parse(r#""\ud83d""#).is_err());
        assert!(parse(r#""\ude00""#).is_err());
        assert!(parse(r#""\ud83dx""#).is_err());
    }

    #[test]
    fn non_ascii_survives_verbatim() {
        // The rendering sentinel is non-ASCII, so this is a load-bearing property.
        assert_eq!(roundtrip("\"⟦tkfd:v1:min⟧\""), "\"⟦tkfd:v1:min⟧\"");
    }

    #[test]
    fn output_never_contains_a_raw_newline() {
        // The stdio transport frames one message per line; a raw newline would
        // split one message into two.
        let value = Value::string("line1\nline2\r\ttab");
        let text = value.to_string();
        assert!(!text.contains('\n'), "{text}");
        assert!(!text.contains('\r'), "{text}");
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(Value::string("\u{1}").to_string(), r#""\u0001""#);
        assert_eq!(Value::string("\u{8}\u{c}").to_string(), r#""\b\f""#);
    }

    #[test]
    fn raw_control_characters_in_input_are_rejected() {
        assert!(parse("\"a\nb\"").is_err());
    }

    #[test]
    fn malformed_input_is_rejected() {
        for bad in [
            "",
            "{",
            "}",
            "[",
            "]",
            "[1,",
            "{\"a\"}",
            "{\"a\":}",
            "tru",
            "nul",
            "\"unterminated",
            "--1",
            "{'a':1}",
            "\"\\q\"",
        ] {
            assert!(parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn the_number_grammar_is_rfc_8259_and_not_rust_parse() {
        // `str::parse::<f64>` accepts every one of these; JSON does not. A parser
        // that is more permissive than its peers is a smuggling primitive, so each
        // production is refused rather than delegated.
        for bad in [
            "01", "00", "-01", "1.", "-1.", ".5", "+1", "1e", "1e+", "1E-", "inf", "NaN", "0x10",
            "1_000",
        ] {
            assert!(parse(bad).is_err(), "accepted {bad:?}");
        }
        // ... while everything the grammar does allow still parses.
        for good in [
            "0", "-0", "0.5", "-0.5", "1e3", "1E3", "1e+3", "1e-3", "0e0", "1.5e10",
        ] {
            assert!(parse(good).is_ok(), "rejected {good:?}");
        }
    }

    #[test]
    fn a_literal_that_overflows_to_infinity_is_rejected() {
        // `"1e999".parse::<f64>()` returns Ok(inf), which JSON cannot spell. Keeping
        // it would put a value in the tree that `to_string` degrades to `null`.
        assert!(parse("1e999").is_err());
        assert!(parse("-1e999").is_err());
        assert!(parse(&format!("1{}", "0".repeat(400))).is_err());
    }

    #[test]
    fn floats_keep_their_json_type_on_the_way_out() {
        // `Display for f64` renders 1.0 as "1", which this parser would read back as
        // an Int. A ratio field must not change JSON type just because it landed on
        // a whole number.
        assert_eq!(Value::Float(1.0).to_string(), "1.0");
        assert_eq!(Value::Float(0.0).to_string(), "0.0");
        assert_eq!(Value::Float(-2.0).to_string(), "-2.0");
        assert_eq!(Value::Float(0.5).to_string(), "0.5");
        assert!(matches!(parse("1.0").unwrap(), Value::Float(_)));
        assert_eq!(roundtrip("1.0"), "1.0");
    }

    #[test]
    fn fractional_floats_keep_every_digit_they_were_given() {
        // The `.0` suffix rule must never cost a client precision. A value such as a
        // `byteRatio` or `tokenRatio` is read to several decimal places; if the
        // fixed-one-decimal form leaked out to non-integral values, 0.25 would arrive
        // as 0.2 and the client would be told the wrong answer with no way to notice.
        // Each case below is one where one-decimal rendering visibly rounds, so a
        // widened condition in `write_float` cannot pass unseen.
        assert_eq!(Value::Float(0.25).to_string(), "0.25");
        assert_eq!(Value::Float(0.125).to_string(), "0.125");
        assert_eq!(Value::Float(1.0 / 3.0).to_string(), "0.3333333333333333");
        assert_eq!(Value::Float(-0.75).to_string(), "-0.75");
        assert_eq!(Value::Float(1.5).to_string(), "1.5");
        // And the digits survive the parser too, so the guarantee holds end to end.
        assert_eq!(roundtrip("0.25"), "0.25");
    }

    #[test]
    fn the_dot_zero_suffix_stops_at_the_magnitude_where_it_stops_paying() {
        // Why the `.0` rule has an upper bound at all: it is implemented with
        // one-decimal fixed notation, which spells a float out in full. At 1e300 that
        // is over three hundred digits of binary-expansion noise in a message framed
        // one per line. The type-stability the suffix buys — a field that is a JSON
        // float whether or not it lands on a whole number, so a schema-validating
        // client sees no needless differential — is not worth that, so above the
        // threshold the value is written plainly and may read back as an Int.
        //
        // The boundary is pinned in both directions: it is exactly 1e16, not 1e16
        // inclusive, so the comparison cannot drift by one value without failing here.
        assert_eq!(Value::Float(1e15).to_string(), "1000000000000000.0");
        assert_eq!(
            Value::Float(9_007_199_254_740_992.0).to_string(),
            "9007199254740992.0"
        );
        assert_eq!(Value::Float(1e16).to_string(), "10000000000000000");
        assert_eq!(Value::Float(-1e16).to_string(), "-10000000000000000");
        assert_eq!(Value::Float(1e17).to_string(), "100000000000000000");
        // The runaway the threshold exists to prevent: plainly written this is 301
        // characters, and one-decimal fixed notation would make it 303 characters of
        // digits that mean nothing to the reader.
        let huge = Value::Float(1e300).to_string();
        assert_eq!(huge.len(), 301);
        assert!(!huge.contains('.'), "{huge}");
    }

    #[test]
    fn every_escape_decodes_to_its_own_character() {
        // One assertion per arm: deleting any single arm of `parse_escape` must fail
        // a test, and a shared "escapes work" case would not do that.
        assert_eq!(parse(r#""\"""#).unwrap().as_str(), Some("\""));
        assert_eq!(parse(r#""\\""#).unwrap().as_str(), Some("\\"));
        assert_eq!(parse(r#""\/""#).unwrap().as_str(), Some("/"));
        assert_eq!(parse(r#""\b""#).unwrap().as_str(), Some("\u{8}"));
        assert_eq!(parse(r#""\f""#).unwrap().as_str(), Some("\u{c}"));
        assert_eq!(parse(r#""\n""#).unwrap().as_str(), Some("\n"));
        assert_eq!(parse(r#""\r""#).unwrap().as_str(), Some("\r"));
        assert_eq!(parse(r#""\t""#).unwrap().as_str(), Some("\t"));
    }

    #[test]
    fn hex_escapes_accept_both_cases_and_weight_digits_correctly() {
        // Exercises every branch of `read_hex4`: digits, lowercase a-f, uppercase
        // A-F, and a value whose four positions all differ so the base-16 weighting
        // cannot be wrong in a way that still yields the same code point.
        assert_eq!(parse(r#""\u0041""#).unwrap().as_str(), Some("A"));
        assert_eq!(parse(r#""\u00e9""#).unwrap().as_str(), Some("é"));
        assert_eq!(parse(r#""\u00C9""#).unwrap().as_str(), Some("É"));
        assert_eq!(parse(r#""\u00fF""#).unwrap().as_str(), Some("ÿ"));
        assert_eq!(parse(r#""\u1234""#).unwrap().as_str(), Some("\u{1234}"));
        assert_eq!(parse(r#""\uABCD""#).unwrap().as_str(), Some("\u{abcd}"));
        assert!(parse(r#""\u00g0""#).is_err());
        assert!(parse(r#""\u00""#).is_err());
    }

    #[test]
    fn every_whitespace_byte_separates_tokens() {
        // `skip_ws` accepts exactly these four; deleting any arm must fail here.
        assert_eq!(roundtrip("[1,\u{20}2]"), "[1,2]");
        assert_eq!(roundtrip("[1,\t2]"), "[1,2]");
        assert_eq!(roundtrip("[1,\n2]"), "[1,2]");
        assert_eq!(roundtrip("[1,\r2]"), "[1,2]");
        // ... and nothing else does. A BOM is not JSON whitespace.
        assert!(parse("\u{feff}1").is_err());
        assert!(parse("[1,\u{0b}2]").is_err());
    }

    #[test]
    fn i64_bounds_round_trip_without_widening() {
        assert_eq!(roundtrip("9223372036854775807"), "9223372036854775807");
        assert_eq!(roundtrip("-9223372036854775808"), "-9223372036854775808");
        assert_eq!(
            parse("9223372036854775807").unwrap().as_i64(),
            Some(i64::MAX)
        );
        assert_eq!(
            parse("-9223372036854775808").unwrap().as_i64(),
            Some(i64::MIN)
        );
    }

    #[test]
    fn trailing_content_is_rejected() {
        assert!(parse("1 2").is_err());
        assert!(parse("{} []").is_err());
    }

    #[test]
    fn depth_beyond_the_limit_is_an_error_not_a_stack_overflow() {
        fn nest(levels: usize) -> String {
            "[".repeat(levels) + &"]".repeat(levels)
        }
        // The boundary is pinned exactly: MAX_DEPTH levels of nesting are accepted
        // and MAX_DEPTH + 1 are not. Testing only "far above" and "far below" would
        // leave the comparison in `nested` free to be off by one.
        assert!(parse(&nest(MAX_DEPTH)).is_ok());
        assert!(parse(&nest(MAX_DEPTH + 1)).is_err());
        assert!(parse(&nest(MAX_DEPTH * 4)).is_err());
    }

    #[test]
    fn lookup_finds_fields_and_ignores_missing_ones() {
        let value = parse(r#"{"a":1,"b":"x"}"#).unwrap();
        assert_eq!(value.get("a").and_then(Value::as_i64), Some(1));
        assert_eq!(value.get("b").and_then(Value::as_str), Some("x"));
        assert!(value.get("c").is_none());
    }

    #[test]
    fn duplicate_keys_are_rejected_rather_than_resolved() {
        // serde_json and JavaScript would keep the last occurrence; `Value::get`
        // would keep the first. Accepting the input at all is what makes the two
        // disagree, so it is refused. The nesting case checks the guard is not
        // limited to the top level.
        assert!(parse(r#"{"a":1,"a":2}"#).is_err());
        assert!(parse(r#"{"a":1,"b":2,"a":3}"#).is_err());
        assert!(parse(r#"{"outer":{"a":1,"a":2}}"#).is_err());
        // Same key in *sibling* objects is not a duplicate.
        assert!(parse(r#"{"x":{"a":1},"y":{"a":2}}"#).is_ok());
    }

    #[test]
    fn first_wins_still_applies_to_values_built_in_code() {
        // `Object::set` does not deduplicate, so the accessor rule is still reachable
        // and still documented on `Value::get`.
        let value = Object::new()
            .set("a", Value::Int(1))
            .set("a", Value::Int(2))
            .build();
        assert_eq!(value.get("a").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn object_builder_keeps_insertion_order_and_skips_absent_fields() {
        let value = Object::new()
            .set("first", Value::Int(1))
            .set_opt("second", None)
            .set_opt("third", Some(Value::Bool(true)))
            .build();
        assert_eq!(value.to_string(), r#"{"first":1,"third":true}"#);
    }

    #[test]
    fn non_finite_floats_degrade_to_null() {
        // JSON has no syntax for any of these. Emitting the bare Rust spelling would
        // produce a line no conformant client can parse, which costs the client the
        // whole message rather than one field.
        assert_eq!(Value::Float(f64::NAN).to_string(), "null");
        assert_eq!(Value::Float(f64::INFINITY).to_string(), "null");
        assert_eq!(Value::Float(f64::NEG_INFINITY).to_string(), "null");
    }

    #[test]
    fn accessors_return_none_for_the_wrong_type() {
        let value = Value::Int(1);
        assert!(value.as_str().is_none());
        assert!(value.as_bool().is_none());
        assert!(value.as_array().is_none());
        assert!(value.as_object().is_none());
        assert!(value.get("a").is_none());
        assert!(!value.is_null());
    }

    /// An array of `count` zeroes: the densest node shape JSON admits, two bytes each.
    fn dense_array(count: usize) -> String {
        let mut text = String::with_capacity(count.saturating_mul(2).saturating_add(2));
        text.push('[');
        for index in 0..count {
            if index > 0 {
                text.push(',');
            }
            text.push('0');
        }
        text.push(']');
        text
    }

    #[test]
    fn the_node_budget_boundary_is_pinned_exactly() {
        // Exactly at the budget is accepted and one past it is not. The array itself is
        // a value, so `dense_array(n)` holds n + 1 of them.
        assert!(parse_bounded(&dense_array(7), 8).is_ok());
        assert!(parse_bounded(&dense_array(8), 8).is_err());
        // Off-by-one in the other direction: a budget of one admits a scalar and
        // nothing that contains anything.
        assert!(parse_bounded("0", 1).is_ok());
        assert!(parse_bounded("[]", 1).is_ok());
        assert!(parse_bounded("[0]", 1).is_err());
    }

    #[test]
    fn the_node_budget_counts_members_of_every_container() {
        // Nesting does not launder a value past the counter: five values here, whatever
        // shape they are arranged in.
        assert!(parse_bounded(r#"{"a":{"b":[0,1]}}"#, 5).is_ok());
        assert!(parse_bounded(r#"{"a":{"b":[0,1]}}"#, 4).is_err());
        assert!(parse_bounded("[[[[0]]]]", 5).is_ok());
        assert!(parse_bounded("[[[[0]]]]", 4).is_err());
    }

    #[test]
    fn an_over_budget_message_reports_the_node_limit() {
        let error = parse_bounded(&dense_array(8), 8).unwrap_err();
        assert_eq!(error.msg, "too many values in one message");
        // The message a caller logs names the limit rather than the syntax, so an
        // operator can tell a refused-because-huge line from a malformed one.
        assert!(error.to_string().contains("too many values"));
    }

    #[test]
    fn a_large_payload_is_not_a_large_node_count() {
        // The shape this bound must never punish: `tokfold_decompress` carrying a base64
        // archive. However many megabytes the archive is, it is one string and so one
        // value, and the envelope around it is a handful more. Parsed here under a
        // budget of ten to make the independence of the two quantities explicit.
        let archive = "QUJD".repeat(256 * 1024); // 1 MiB of base64
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"tokfold_decompress","arguments":{{"archive":"{archive}"}}}}}}"#
        );
        assert!(request.len() > 1024 * 1024);
        let value = parse_bounded(&request, 10).expect("a large payload is few values");
        assert_eq!(
            value
                .get("params")
                .and_then(|params| params.get("arguments"))
                .and_then(|arguments| arguments.get("archive"))
                .and_then(Value::as_str)
                .map(str::len),
            Some(archive.len())
        );
    }

    #[test]
    fn the_node_budget_arithmetic_matches_its_documentation() {
        // `MAX_NODES` is justified by numbers written into its doc comment; if the
        // layout of `Value` changes underneath them, the justification is wrong and this
        // fails rather than the doc quietly becoming fiction.
        assert_eq!(size_of::<Value>(), 32);
        assert_eq!(size_of::<(String, Value)>(), 56);
        assert_eq!(MAX_NODES, crate::MAX_MESSAGE_BYTES / 8);
        assert_eq!(MAX_NODES, 4_194_304);

        // The claim that no legal message reaches the budget rests on the densest one
        // being a batch of minimal members. Derive that count rather than quoting it.
        let member = r#"{"jsonrpc":"2.0","method":"a"}"#;
        assert_eq!(member.len(), 30);
        let members = crate::MAX_MESSAGE_BYTES / (member.len() + 1);
        let values = members * 3 + 1;
        assert!(
            values < MAX_NODES,
            "densest legal line is {values} values, budget is {MAX_NODES}"
        );
    }
}
