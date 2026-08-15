//! Lexeme-preserving pull parser producing a flat [`Tape`].
//!
//! The parser exists to satisfy one contract the standard `serde_json` path
//! cannot: decision D3 requires duplicate object keys and object key order to be
//! preserved, and number lexemes to survive byte-for-byte. `serde_json::Value`
//! collapses duplicate keys and round-trips numbers through `f64`, so it is
//! unusable here. This module never depends on `serde_json`.
//!
//! Invariants this module upholds:
//!
//! * **Iterative.** There is no recursion anywhere. Nesting is tracked with an
//!   explicit stack and an explicit depth counter, so a pathologically deep input
//!   returns [`CompressError::DepthExceeded`] instead of overflowing the stack.
//! * **Zero-copy.** Every node points at the original input by [`Span`]; no bytes
//!   are copied and no number is ever parsed through `f64`. A 100-digit integer,
//!   `1.0`, and `1e3` all survive as their original lexemes.
//! * **Lexeme-preserving.** String spans include the surrounding quotes and leave
//!   escapes uninterpreted, so lone-surrogate `\uXXXX` escapes survive verbatim.
//! * **Order-preserving.** Object key order, duplicate keys, and array order are
//!   emitted in source order and never collapsed.
//! * **No slicing.** The input is walked as `&[u8]` via `.get(..)`; the crate
//!   forbids `&s[a..b]` indexing.
//!
//! Because [`Node::depth`] is a `u16`, depths beyond `u16::MAX` saturate. That is
//! only reachable when `max_depth` is configured above `u16::MAX`, an atypical
//! setting; the default limit (512) keeps every depth exact.

use crate::error::CompressError;

/// Zero-copy span into the original input, as `[start, end)` byte offsets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
}

/// The kind of a tape node.
///
/// Container boundaries are explicit ([`NodeKind::ObjectStart`] /
/// [`NodeKind::ObjectEnd`], [`NodeKind::ArrayStart`] / [`NodeKind::ArrayEnd`]) so
/// the tape can be walked without recursion. The `Start` variants carry the exact
/// child count, back-patched when the matching `End` is reached, so a second pass
/// is never required.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeKind {
    /// The literal `null`.
    Null,
    /// A `true` or `false` literal.
    Bool(bool),
    /// A number kept as its original lexeme span. Never parsed through `f64`.
    Number,
    /// A string lexeme span, including the surrounding quotes; escapes are left
    /// uninterpreted so lone surrogates survive.
    String,
    /// The start of an object.
    ObjectStart {
        /// Number of member pairs in the object.
        members: u32,
    },
    /// The end of an object.
    ObjectEnd,
    /// The start of an array.
    ArrayStart {
        /// Number of elements in the array.
        elements: u32,
    },
    /// The end of an array.
    ArrayEnd,
    /// An object key. Always immediately precedes its value node.
    Key,
}

/// A single tape node: what it is, where it came from, and how deep it sits.
#[derive(Copy, Clone, Debug)]
pub struct Node {
    /// What the node represents.
    pub kind: NodeKind,
    /// The node's byte span into the original input.
    pub span: Span,
    /// Number of enclosing containers. The top-level value is depth `0`.
    pub depth: u16,
}

/// Flat, arena-backed tape. One `Vec`, no per-node boxing, no recursion.
#[derive(Debug, Default)]
pub struct Tape {
    nodes: Vec<Node>,
}

impl Tape {
    /// The nodes in source order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Whether the tape holds no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The number of nodes on the tape.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
}

/// Whether the current frame is an object or an array.
#[derive(Copy, Clone, PartialEq, Eq)]
enum FrameKind {
    Object,
    Array,
}

/// One open container on the explicit parse stack.
struct Frame {
    kind: FrameKind,
    /// Index of the `Start` node in the tape, patched with the final count.
    start_index: usize,
    /// Members (objects) or elements (arrays) seen so far.
    count: u32,
}

/// What token the state machine expects next.
#[derive(Copy, Clone)]
enum State {
    /// A JSON value is mandatory here.
    Value,
    /// Just opened `[`: a value or `]`.
    ArrayFirst,
    /// After an array element: `,` or `]`.
    ArrayNext,
    /// Just opened `{`: a key or `}`.
    ObjectFirst,
    /// After `,` in an object: a string key.
    ObjectKey,
    /// After a member value: `,` or `}`.
    ObjectNext,
    /// The top-level value is complete; only trailing whitespace may follow.
    End,
}

/// Parse UTF-8 JSON into a lexeme-preserving tape.
///
/// - Duplicate keys are PRESERVED in order (never collapsed).
/// - Object key order is PRESERVED.
/// - Numbers are retained as raw lexeme spans (`1.0` stays `1.0`, `1e3` stays `1e3`).
/// - Strings are retained as raw lexeme spans; escape style is not interpreted here.
/// - Lone-surrogate `\uXXXX` escapes survive as raw lexemes.
/// - Iterative, explicit depth counter -> [`CompressError::DepthExceeded`]. MUST
///   NOT stack-overflow.
// The single-function state machine is intentionally flat: each arm is one token
// transition, and splitting it would scatter the control flow without simplifying it.
#[allow(clippy::too_many_lines)]
pub fn parse(input: &str, max_depth: usize) -> Result<Tape, CompressError> {
    let bytes = input.as_bytes();

    // Spans are `u32`, so an input that cannot be addressed by `u32` offsets
    // cannot be represented. This is reported as an input-size failure rather than
    // silently truncating an offset. In practice the compressor caps input length
    // far below this bound.
    if u32::try_from(bytes.len()).is_err() {
        return Err(CompressError::InputTooLarge {
            size: bytes.len(),
            limit: u32_max_as_usize(),
        });
    }

    let mut nodes: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut pos: usize = 0;
    let mut state = State::Value;

    loop {
        pos = skip_ws(bytes, pos);

        match state {
            State::End => {
                if pos < bytes.len() {
                    return Err(invalid(pos, "trailing data after top-level value"));
                }
                return Ok(Tape { nodes });
            }

            State::Value => match bytes.get(pos).copied() {
                None => return Err(invalid(pos, "unexpected end of input, expected a value")),
                Some(b'{') => {
                    open_container(&mut nodes, &mut stack, FrameKind::Object, pos, max_depth)?;
                    pos += 1;
                    state = State::ObjectFirst;
                }
                Some(b'[') => {
                    open_container(&mut nodes, &mut stack, FrameKind::Array, pos, max_depth)?;
                    pos += 1;
                    state = State::ArrayFirst;
                }
                Some(b'"') => {
                    let end = scan_string(bytes, pos)?;
                    push(&mut nodes, NodeKind::String, pos, end, stack.len());
                    pos = end;
                    state = after_value(&stack);
                }
                Some(b'n') => {
                    if !lit_matches(bytes, pos, b"null") {
                        return Err(invalid(pos, "invalid literal, expected 'null'"));
                    }
                    push(&mut nodes, NodeKind::Null, pos, pos + 4, stack.len());
                    pos += 4;
                    state = after_value(&stack);
                }
                Some(b't') => {
                    if !lit_matches(bytes, pos, b"true") {
                        return Err(invalid(pos, "invalid literal, expected 'true'"));
                    }
                    push(&mut nodes, NodeKind::Bool(true), pos, pos + 4, stack.len());
                    pos += 4;
                    state = after_value(&stack);
                }
                Some(b'f') => {
                    if !lit_matches(bytes, pos, b"false") {
                        return Err(invalid(pos, "invalid literal, expected 'false'"));
                    }
                    push(&mut nodes, NodeKind::Bool(false), pos, pos + 5, stack.len());
                    pos += 5;
                    state = after_value(&stack);
                }
                Some(b'-' | b'0'..=b'9') => {
                    let end = scan_number(bytes, pos)?;
                    push(&mut nodes, NodeKind::Number, pos, end, stack.len());
                    pos = end;
                    state = after_value(&stack);
                }
                Some(_) => return Err(invalid(pos, "expected a JSON value")),
            },

            State::ArrayFirst => match bytes.get(pos).copied() {
                None => {
                    return Err(invalid(
                        pos,
                        "unexpected end of input, expected value or ']'",
                    ));
                }
                Some(b']') => {
                    close_container(&mut nodes, &mut stack, pos);
                    pos += 1;
                    state = after_value(&stack);
                }
                Some(_) => {
                    if let Some(f) = stack.last_mut() {
                        f.count += 1;
                    }
                    state = State::Value;
                }
            },

            State::ArrayNext => match bytes.get(pos).copied() {
                None => return Err(invalid(pos, "unexpected end of input, expected ',' or ']'")),
                Some(b',') => {
                    pos += 1;
                    if let Some(f) = stack.last_mut() {
                        f.count += 1;
                    }
                    state = State::Value;
                }
                Some(b']') => {
                    close_container(&mut nodes, &mut stack, pos);
                    pos += 1;
                    state = after_value(&stack);
                }
                Some(_) => return Err(invalid(pos, "expected ',' or ']' in array")),
            },

            State::ObjectFirst => match bytes.get(pos).copied() {
                None => {
                    return Err(invalid(
                        pos,
                        "unexpected end of input, expected string key or '}'",
                    ));
                }
                Some(b'}') => {
                    close_container(&mut nodes, &mut stack, pos);
                    pos += 1;
                    state = after_value(&stack);
                }
                Some(b'"') => {
                    pos = read_key(bytes, &mut nodes, &mut stack, pos)?;
                    state = State::Value;
                }
                Some(_) => return Err(invalid(pos, "expected string key or '}'")),
            },

            State::ObjectKey => match bytes.get(pos).copied() {
                None => return Err(invalid(pos, "unexpected end of input, expected string key")),
                Some(b'"') => {
                    pos = read_key(bytes, &mut nodes, &mut stack, pos)?;
                    state = State::Value;
                }
                Some(_) => return Err(invalid(pos, "expected string key after ','")),
            },

            State::ObjectNext => match bytes.get(pos).copied() {
                None => return Err(invalid(pos, "unexpected end of input, expected ',' or '}'")),
                Some(b',') => {
                    pos += 1;
                    state = State::ObjectKey;
                }
                Some(b'}') => {
                    close_container(&mut nodes, &mut stack, pos);
                    pos += 1;
                    state = after_value(&stack);
                }
                Some(_) => return Err(invalid(pos, "expected ',' or '}' in object")),
            },
        }
    }
}

/// Decide the continuation state after a value completes, from the enclosing frame.
fn after_value(stack: &[Frame]) -> State {
    stack.last().map_or(State::End, |f| match f.kind {
        FrameKind::Array => State::ArrayNext,
        FrameKind::Object => State::ObjectNext,
    })
}

/// Emit a `Start` node and push its frame, enforcing the depth limit first.
///
/// The `Start` node carries the enclosing depth and a placeholder count of `0`;
/// the count is back-patched by [`close_container`].
fn open_container(
    nodes: &mut Vec<Node>,
    stack: &mut Vec<Frame>,
    kind: FrameKind,
    pos: usize,
    max_depth: usize,
) -> Result<(), CompressError> {
    let new_depth = stack.len() + 1;
    if new_depth > max_depth {
        return Err(CompressError::DepthExceeded {
            depth: new_depth,
            limit: max_depth,
        });
    }

    let start_index = nodes.len();
    let start_kind = match kind {
        FrameKind::Object => NodeKind::ObjectStart { members: 0 },
        FrameKind::Array => NodeKind::ArrayStart { elements: 0 },
    };
    push(nodes, start_kind, pos, pos + 1, stack.len());
    stack.push(Frame {
        kind,
        start_index,
        count: 0,
    });
    Ok(())
}

/// Pop the current frame, back-patch its `Start` count, and emit the `End` node.
///
/// The caller's state machine guarantees a matching frame is on the stack; the
/// empty-stack branch is defensive and never taken on valid control flow.
fn close_container(nodes: &mut Vec<Node>, stack: &mut Vec<Frame>, pos: usize) {
    let Some(frame) = stack.pop() else {
        return;
    };

    let end_kind = match frame.kind {
        FrameKind::Object => {
            if let Some(n) = nodes.get_mut(frame.start_index) {
                n.kind = NodeKind::ObjectStart {
                    members: frame.count,
                };
            }
            NodeKind::ObjectEnd
        }
        FrameKind::Array => {
            if let Some(n) = nodes.get_mut(frame.start_index) {
                n.kind = NodeKind::ArrayStart {
                    elements: frame.count,
                };
            }
            NodeKind::ArrayEnd
        }
    };

    push(nodes, end_kind, pos, pos + 1, stack.len());
}

/// Scan a key string, emit its [`NodeKind::Key`] node, count the member, and
/// consume the `:` that must follow. Returns the position after the colon.
fn read_key(
    bytes: &[u8],
    nodes: &mut Vec<Node>,
    stack: &mut [Frame],
    pos: usize,
) -> Result<usize, CompressError> {
    let end = scan_string(bytes, pos)?;
    push(nodes, NodeKind::Key, pos, end, stack.len());
    if let Some(f) = stack.last_mut() {
        f.count += 1;
    }

    let colon = skip_ws(bytes, end);
    match bytes.get(colon).copied() {
        Some(b':') => Ok(colon + 1),
        Some(_) => Err(invalid(colon, "expected ':' after object key")),
        None => Err(invalid(colon, "unexpected end of input, expected ':'")),
    }
}

/// Scan a string starting at its opening quote. Returns the index just past the
/// closing quote. Escapes are validated for shape but not interpreted; lone
/// surrogates are accepted so they survive as raw lexemes.
fn scan_string(bytes: &[u8], start: usize) -> Result<usize, CompressError> {
    let mut i = start + 1;
    loop {
        match bytes.get(i).copied() {
            None => return Err(invalid(i, "unterminated string")),
            Some(b'"') => return Ok(i + 1),
            Some(b'\\') => match bytes.get(i + 1).copied() {
                None => return Err(invalid(i + 1, "unterminated escape in string")),
                Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => i += 2,
                Some(b'u') => {
                    let mut k = 0;
                    while k < 4 {
                        match bytes.get(i + 2 + k).copied() {
                            None => {
                                return Err(invalid(i + 2 + k, "unterminated unicode escape"));
                            }
                            Some(h) if h.is_ascii_hexdigit() => k += 1,
                            Some(_) => {
                                return Err(invalid(
                                    i + 2 + k,
                                    "invalid hex digit in unicode escape",
                                ));
                            }
                        }
                    }
                    i += 6;
                }
                Some(_) => return Err(invalid(i + 1, "invalid escape character in string")),
            },
            Some(c) if c <= 0x1F => {
                return Err(invalid(i, "unescaped control character in string"));
            }
            Some(_) => i += 1,
        }
    }
}

/// Scan a number starting at `-` or a digit. Returns the index just past the last
/// character. Leading zeros are rejected; the lexeme is never parsed to `f64`.
fn scan_number(bytes: &[u8], start: usize) -> Result<usize, CompressError> {
    let mut i = start;

    if bytes.get(i).copied() == Some(b'-') {
        i += 1;
    }

    match bytes.get(i).copied() {
        Some(b'0') => {
            let zero_pos = i;
            i += 1;
            if let Some(d) = bytes.get(i).copied() {
                if d.is_ascii_digit() {
                    return Err(invalid(
                        zero_pos,
                        "leading zeros are not allowed in numbers",
                    ));
                }
            }
        }
        Some(d) if d.is_ascii_digit() => {
            i += 1;
            while let Some(d) = bytes.get(i).copied() {
                if d.is_ascii_digit() {
                    i += 1;
                } else {
                    break;
                }
            }
        }
        _ => return Err(invalid(i, "invalid number: expected a digit")),
    }

    if bytes.get(i).copied() == Some(b'.') {
        i += 1;
        let mut any = false;
        while let Some(d) = bytes.get(i).copied() {
            if d.is_ascii_digit() {
                i += 1;
                any = true;
            } else {
                break;
            }
        }
        if !any {
            return Err(invalid(i, "invalid number: expected a digit after '.'"));
        }
    }

    if let Some(e) = bytes.get(i).copied() {
        if e == b'e' || e == b'E' {
            i += 1;
            if let Some(s) = bytes.get(i).copied() {
                if s == b'+' || s == b'-' {
                    i += 1;
                }
            }
            let mut any = false;
            while let Some(d) = bytes.get(i).copied() {
                if d.is_ascii_digit() {
                    i += 1;
                    any = true;
                } else {
                    break;
                }
            }
            if !any {
                return Err(invalid(i, "invalid number: expected a digit in exponent"));
            }
        }
    }

    Ok(i)
}

/// Advance past JSON insignificant whitespace (space, tab, LF, CR).
fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while let Some(b) = bytes.get(i).copied() {
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            _ => break,
        }
    }
    i
}

/// Whether `lit` occurs at `start` in `bytes`.
fn lit_matches(bytes: &[u8], start: usize, lit: &[u8]) -> bool {
    for (k, expected) in lit.iter().enumerate() {
        match bytes.get(start + k) {
            Some(b) if b == expected => {}
            _ => return false,
        }
    }
    true
}

/// Append a node with the given span and depth.
fn push(nodes: &mut Vec<Node>, kind: NodeKind, start: usize, end: usize, depth: usize) {
    nodes.push(Node {
        kind,
        span: Span {
            start: to_u32(start),
            end: to_u32(end),
        },
        depth: to_u16(depth),
    });
}

/// Narrow a byte offset to `u32`. The caller guarantees the input fits `u32`, so
/// the saturating fallback is unreachable on the parse path.
fn to_u32(x: usize) -> u32 {
    u32::try_from(x).unwrap_or(u32::MAX)
}

/// Narrow a depth to `u16`, saturating (only reachable with `max_depth` above
/// `u16::MAX`).
fn to_u16(x: usize) -> u16 {
    u16::try_from(x).unwrap_or(u16::MAX)
}

/// `u32::MAX` as a `usize`, for the input-size limit report.
fn u32_max_as_usize() -> usize {
    // `usize` is at least 32 bits on every supported target, so this is exact.
    u32::MAX as usize
}

/// Build a [`CompressError::InvalidJson`] at a byte offset.
fn invalid(byte_offset: usize, msg: &str) -> CompressError {
    CompressError::InvalidJson {
        byte_offset,
        msg: msg.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::literal_string_with_formatting_args,
        clippy::too_many_lines
    )]

    use super::{NodeKind, Span, Tape, parse, u32_max_as_usize};
    use crate::error::CompressError;

    fn kinds(t: &Tape) -> Vec<NodeKind> {
        t.nodes().iter().map(|n| n.kind).collect()
    }

    fn lexeme(input: &str, span: Span) -> &str {
        input.get(span.start as usize..span.end as usize).unwrap()
    }

    fn assert_invalid(input: &str) {
        assert!(
            matches!(parse(input, 512), Err(CompressError::InvalidJson { .. })),
            "expected InvalidJson for {input:?}"
        );
    }

    fn assert_invalid_at(input: &str, offset: usize) {
        let result = parse(input, 512);
        assert!(
            matches!(&result, Err(CompressError::InvalidJson { byte_offset, .. }) if *byte_offset == offset),
            "expected InvalidJson at {offset} for {input:?}, got {result:?}"
        );
    }

    // ---- empty and nested-empty containers ----

    #[test]
    fn empty_object() {
        let t = parse("{}", 512).unwrap();
        assert_eq!(
            kinds(&t),
            vec![NodeKind::ObjectStart { members: 0 }, NodeKind::ObjectEnd]
        );
        assert_eq!(t.len(), 2);
        assert!(!t.is_empty());
    }

    #[test]
    fn empty_array() {
        let t = parse("[]", 512).unwrap();
        assert_eq!(
            kinds(&t),
            vec![NodeKind::ArrayStart { elements: 0 }, NodeKind::ArrayEnd]
        );
    }

    #[test]
    fn nested_empties() {
        let t = parse("[{}]", 512).unwrap();
        assert_eq!(
            kinds(&t),
            vec![
                NodeKind::ArrayStart { elements: 1 },
                NodeKind::ObjectStart { members: 0 },
                NodeKind::ObjectEnd,
                NodeKind::ArrayEnd,
            ]
        );

        let t = parse("{\"a\":[]}", 512).unwrap();
        assert_eq!(
            kinds(&t),
            vec![
                NodeKind::ObjectStart { members: 1 },
                NodeKind::Key,
                NodeKind::ArrayStart { elements: 0 },
                NodeKind::ArrayEnd,
                NodeKind::ObjectEnd,
            ]
        );

        let t = parse("[[],[]]", 512).unwrap();
        assert_eq!(
            kinds(&t),
            vec![
                NodeKind::ArrayStart { elements: 2 },
                NodeKind::ArrayStart { elements: 0 },
                NodeKind::ArrayEnd,
                NodeKind::ArrayStart { elements: 0 },
                NodeKind::ArrayEnd,
                NodeKind::ArrayEnd,
            ]
        );
    }

    // ---- container spans ----

    #[test]
    fn container_node_spans_cover_exactly_their_bracket_byte() {
        let input = "[{}]";
        let t = parse(input, 512).unwrap();
        let spans: Vec<Span> = t.nodes().iter().map(|n| n.span).collect();
        assert_eq!(
            spans,
            vec![
                Span { start: 0, end: 1 }, // ArrayStart  "["
                Span { start: 1, end: 2 }, // ObjectStart "{"
                Span { start: 2, end: 3 }, // ObjectEnd   "}"
                Span { start: 3, end: 4 }, // ArrayEnd    "]"
            ]
        );
        let lexemes: Vec<&str> = t.nodes().iter().map(|n| lexeme(input, n.span)).collect();
        assert_eq!(lexemes, vec!["[", "{", "}", "]"]);
    }

    #[test]
    fn container_spans_track_whitespace_offsets() {
        // Whitespace before each bracket moves both the start and the end offset,
        // so the span is genuinely derived from the bracket's own position.
        let input = "  [  ]  ";
        let t = parse(input, 512).unwrap();
        let spans: Vec<Span> = t.nodes().iter().map(|n| n.span).collect();
        assert_eq!(
            spans,
            vec![Span { start: 2, end: 3 }, Span { start: 5, end: 6 }]
        );
        assert_eq!(lexeme(input, spans[0]), "[");
        assert_eq!(lexeme(input, spans[1]), "]");
    }

    // ---- counts and ordering ----

    #[test]
    fn array_element_count() {
        let t = parse("[1,2,3]", 512).unwrap();
        assert_eq!(t.nodes()[0].kind, NodeKind::ArrayStart { elements: 3 });
    }

    #[test]
    fn object_member_count() {
        let t = parse("{\"a\":1,\"b\":2}", 512).unwrap();
        assert_eq!(t.nodes()[0].kind, NodeKind::ObjectStart { members: 2 });
    }

    #[test]
    fn key_order_preserved() {
        let input = "{\"b\":1,\"a\":2}";
        let t = parse(input, 512).unwrap();
        let keys: Vec<&str> = t
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Key)
            .map(|n| lexeme(input, n.span))
            .collect();
        assert_eq!(keys, vec!["\"b\"", "\"a\""]);
    }

    #[test]
    fn duplicate_keys_preserved_in_order() {
        let input = "{\"a\":1,\"a\":2}";
        let t = parse(input, 512).unwrap();
        assert_eq!(t.nodes()[0].kind, NodeKind::ObjectStart { members: 2 });
        let keys: Vec<&str> = t
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Key)
            .map(|n| lexeme(input, n.span))
            .collect();
        assert_eq!(keys, vec!["\"a\"", "\"a\""]);
        // Values are kept in order too.
        let numbers: Vec<&str> = t
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Number)
            .map(|n| lexeme(input, n.span))
            .collect();
        assert_eq!(numbers, vec!["1", "2"]);
    }

    #[test]
    fn duplicate_keys_at_nested_level() {
        let input = "{\"o\":{\"a\":1,\"a\":2}}";
        let t = parse(input, 512).unwrap();
        let keys: Vec<&str> = t
            .nodes()
            .iter()
            .filter(|n| n.kind == NodeKind::Key)
            .map(|n| lexeme(input, n.span))
            .collect();
        assert_eq!(keys, vec!["\"o\"", "\"a\"", "\"a\""]);
    }

    // ---- number lexeme preservation (never through f64) ----

    #[test]
    fn number_lexemes_survive_verbatim() {
        for raw in [
            "0",
            "-0",
            "1",
            "10",
            "1.0",
            "0.5",
            "-3.14",
            "1e3",
            "1E3",
            "1e+10",
            "1E-10",
            "2.5e10",
            "123456789012345678901234567890",
        ] {
            let t = parse(raw, 512).unwrap();
            assert_eq!(t.len(), 1, "unexpected node count for {raw}");
            assert_eq!(t.nodes()[0].kind, NodeKind::Number, "kind for {raw}");
            assert_eq!(lexeme(raw, t.nodes()[0].span), raw, "lexeme for {raw}");
        }
    }

    #[test]
    fn hundred_digit_integer_survives() {
        let big = "9".repeat(100);
        let t = parse(&big, 512).unwrap();
        assert_eq!(t.nodes()[0].kind, NodeKind::Number);
        assert_eq!(lexeme(&big, t.nodes()[0].span), big.as_str());
    }

    // ---- strings, escapes, surrogates, unicode ----

    #[test]
    fn empty_string_value() {
        let t = parse("\"\"", 512).unwrap();
        assert_eq!(t.nodes()[0].kind, NodeKind::String);
        assert_eq!(t.nodes()[0].span, Span { start: 0, end: 2 });
    }

    #[test]
    fn string_includes_quotes_in_span() {
        let input = "\"abc\"";
        let t = parse(input, 512).unwrap();
        assert_eq!(lexeme(input, t.nodes()[0].span), "\"abc\"");
    }

    #[test]
    fn escaped_quote_in_string() {
        let input = "\"a\\\"b\"";
        let t = parse(input, 512).unwrap();
        assert_eq!(t.nodes()[0].kind, NodeKind::String);
        assert_eq!(lexeme(input, t.nodes()[0].span), input);
    }

    #[test]
    fn unicode_string_and_escape() {
        let input = "\"héllo\"";
        let t = parse(input, 512).unwrap();
        assert_eq!(t.nodes()[0].kind, NodeKind::String);
        assert_eq!(lexeme(input, t.nodes()[0].span), input);

        let input = "\"\\u00e9\"";
        let t = parse(input, 512).unwrap();
        assert_eq!(t.nodes()[0].kind, NodeKind::String);
        assert_eq!(lexeme(input, t.nodes()[0].span), input);
    }

    #[test]
    fn lone_surrogates_survive_as_raw_lexemes() {
        for input in [
            "\"\\ud834\"",
            "\"\\udc00\"",
            "\"\\ud834x\"",
            "\"a\\udeadb\"",
        ] {
            let t = parse(input, 512).unwrap();
            assert_eq!(t.nodes()[0].kind, NodeKind::String, "kind for {input}");
            assert_eq!(
                lexeme(input, t.nodes()[0].span),
                input,
                "lexeme for {input}"
            );
        }
    }

    #[test]
    fn all_simple_escapes_accepted() {
        let input = "\"\\\"\\\\\\/\\b\\f\\n\\r\\t\"";
        let t = parse(input, 512).unwrap();
        assert_eq!(t.nodes()[0].kind, NodeKind::String);
    }

    // ---- scalars and whitespace ----

    #[test]
    fn scalar_literals() {
        assert_eq!(kinds(&parse("null", 512).unwrap()), vec![NodeKind::Null]);
        assert_eq!(
            kinds(&parse("true", 512).unwrap()),
            vec![NodeKind::Bool(true)]
        );
        assert_eq!(
            kinds(&parse("false", 512).unwrap()),
            vec![NodeKind::Bool(false)]
        );
    }

    #[test]
    fn whitespace_is_ignored() {
        let t = parse("  { \"a\" : 1 , \"b\" : [ 2 ] }  ", 512).unwrap();
        assert_eq!(
            kinds(&t),
            vec![
                NodeKind::ObjectStart { members: 2 },
                NodeKind::Key,
                NodeKind::Number,
                NodeKind::Key,
                NodeKind::ArrayStart { elements: 1 },
                NodeKind::Number,
                NodeKind::ArrayEnd,
                NodeKind::ObjectEnd,
            ]
        );
    }

    #[test]
    fn depth_field_reflects_nesting() {
        let t = parse("[[1]]", 512).unwrap();
        let nodes = t.nodes();
        assert_eq!(nodes[0].depth, 0); // outer ArrayStart
        assert_eq!(nodes[1].depth, 1); // inner ArrayStart
        assert_eq!(nodes[2].depth, 2); // number
        assert_eq!(nodes[3].depth, 1); // inner ArrayEnd
        assert_eq!(nodes[4].depth, 0); // outer ArrayEnd
    }

    // ---- depth limit ----

    #[test]
    fn nesting_at_limit_is_accepted() {
        let depth = 64;
        let input = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let t = parse(&input, depth).unwrap();
        assert_eq!(t.len(), depth * 2);
    }

    #[test]
    fn nesting_one_past_limit_is_rejected() {
        let depth = 64;
        let input = format!("{}{}", "[".repeat(depth + 1), "]".repeat(depth + 1));
        let err = parse(&input, depth).unwrap_err();
        assert!(matches!(
            err,
            CompressError::DepthExceeded {
                depth: 65,
                limit: 64
            }
        ));
    }

    #[test]
    fn deep_input_does_not_stack_overflow() {
        // Far past the limit: must return an error iteratively, never overflow.
        let input = "[".repeat(100_000);
        let err = parse(&input, 512).unwrap_err();
        assert!(matches!(err, CompressError::DepthExceeded { .. }));
    }

    #[test]
    fn deep_valid_input_parses_iteratively() {
        // A genuinely 100k-deep valid document under a generous limit proves the
        // parser recurses nowhere.
        let depth = 100_000;
        let input = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let t = parse(&input, depth).unwrap();
        assert_eq!(t.len(), depth * 2);
        assert_eq!(t.nodes()[0].kind, NodeKind::ArrayStart { elements: 1 });
    }

    // ---- rejections ----

    #[test]
    fn rejects_nan_and_infinity() {
        assert_invalid("NaN");
        assert_invalid("Infinity");
        assert_invalid("-Infinity");
        assert_invalid("[NaN]");
        assert_invalid("[Infinity]");
    }

    #[test]
    fn rejects_truncated_documents() {
        assert_invalid("");
        assert_invalid("   ");
        assert_invalid("{");
        assert_invalid("[");
        assert_invalid("{\"a\":");
        assert_invalid("{\"a\":1");
        assert_invalid("[1,");
        assert_invalid("[1");
        assert_invalid("\"abc");
        assert_invalid("tru");
        assert_invalid("nul");
        assert_invalid("{\"a\"");
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert_invalid_at("1 2", 2);
        assert_invalid_at("{} {}", 3);
        assert_invalid_at("[]x", 2);
        assert_invalid_at("null null", 5);
        assert_invalid_at("truefalse", 4);
    }

    #[test]
    fn rejects_leading_zeros() {
        assert_invalid_at("01", 0);
        assert_invalid_at("-01", 1);
        assert_invalid("{\"a\":00}");
        assert_invalid("[012]");
    }

    #[test]
    fn rejects_single_quoted_strings() {
        assert_invalid("'abc'");
        assert_invalid("{'a':1}");
        assert_invalid("['x']");
    }

    #[test]
    fn rejects_unquoted_keys() {
        assert_invalid("{a:1}");
        assert_invalid("{ a : 1 }");
    }

    #[test]
    fn rejects_trailing_commas() {
        assert_invalid("[1,]");
        assert_invalid("{\"a\":1,}");
        assert_invalid("[1,2,]");
        assert_invalid("[,]");
        assert_invalid("[,1]");
    }

    #[test]
    fn rejects_unescaped_control_characters() {
        assert_invalid("\"a\u{01}b\"");
        assert_invalid("\"a\nb\"");
        assert_invalid("\"\t\"");
        assert_invalid("\"\u{00}\"");
    }

    #[test]
    fn rejects_bad_escapes() {
        assert_invalid("\"\\x\"");
        assert_invalid("\"\\u12\"");
        assert_invalid("\"\\u12zz\"");
        assert_invalid("\"\\\"");
    }

    #[test]
    fn rejects_bad_numbers() {
        assert_invalid("-");
        assert_invalid("1.");
        assert_invalid("1e");
        assert_invalid("1e+");
        assert_invalid(".5");
        assert_invalid("--1");
    }

    #[test]
    fn rejects_missing_colon_and_value() {
        assert_invalid("{\"a\" 1}");
        assert_invalid("{\"a\":}");
        assert_invalid("{\"a\":,}");
    }

    #[test]
    fn rejects_same_length_wrong_literals() {
        // Every byte of a literal is compared, not just its length: these inputs
        // are exactly as long as `null` / `true` / `false` but differ in one byte.
        assert_invalid_at("nulx", 0);
        assert_invalid_at("trux", 0);
        assert_invalid_at("falsx", 0);
        assert_invalid_at("nxll", 0);
        assert_invalid_at("[nuxl]", 1);
        assert_invalid_at("{\"a\":truX}", 5);
    }

    // ---- exact error offsets inside string escapes ----

    #[test]
    fn escape_error_offsets_are_exact() {
        // Backslash at index 1 with nothing after it: the offset names the byte
        // that is missing (2), not the backslash (1) and not the quote (0).
        assert_invalid_at("\"\\", 2);
        // Unknown escape letter: the offset names the letter at index 2.
        assert_invalid_at("\"\\x\"", 2);
        // Same shape one byte further in, so the offset cannot be a constant.
        assert_invalid_at("\"a\\x\"", 3);
    }

    #[test]
    fn unicode_escape_error_offsets_are_exact() {
        // Backslash at 1, `u` at 2, one accepted hex digit at 3, then EOF: the
        // offset names the first missing byte, index 4.
        assert_invalid_at("\"\\uA", 4);
        // Same, but the byte at index 4 is present and is not a hex digit.
        assert_invalid_at("\"\\uAz\"", 4);
        // Three accepted hex digits: the reported offset moves with the digit
        // count, so it is `backslash + 2 + digits_seen`.
        assert_invalid_at("\"\\uABCz\"", 6);
        assert_invalid_at("\"\\uABC", 6);
    }

    // ---- tape emptiness ----

    #[test]
    fn default_tape_is_empty_and_parsed_tapes_are_not() {
        // `parse` never returns an empty tape (every valid document emits at least
        // one node), so the empty state is only observable via `Tape::default`.
        let empty = Tape::default();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(empty.nodes().is_empty());

        assert!(!parse("null", 512).unwrap().is_empty());
        assert!(!parse("{}", 512).unwrap().is_empty());
    }

    // ---- input-size limit constant ----

    #[test]
    fn u32_max_as_usize_reports_the_input_size_limit() {
        // The `InputTooLarge` path needs an input larger than 4 GiB, which a test
        // cannot allocate, so the reported limit is checked directly. It must be
        // the largest offset a `u32` span can address.
        assert_eq!(u32_max_as_usize(), 4_294_967_295_usize);
    }
}
