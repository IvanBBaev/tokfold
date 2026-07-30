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
    clippy::use_self,
    clippy::float_cmp
)]

//! Property-based conformance suite for `tokfold-core`, covering the eight
//! properties frozen in `docs/ai/impl-spec-v0.1.md` §12, plus the hostile and
//! "dirty reality" generators that section demands.
//!
//! # Case count
//!
//! Proptest reads `PROPTEST_CASES`; [`config`] makes the default **32** locally and
//! CI sets `PROPTEST_CASES=1024`. Any shrunk counterexample is persisted to
//! `tests/roundtrip.proptest-regressions`, which is committed (never git-ignored) so
//! a regression is replayed deterministically forever.
//!
//! # The oracle
//!
//! Semantic equality is decided by [`json_semantically_eq`], an independent parser
//! (`tests/oracle.rs`, pulled in below) that shares no code with the engine.
//!
//! # A note on Property 8 (anti-collision)
//!
//! Spec §12 states it as "for `a != b`, `compress(a).rendering != compress(b).rendering`".
//! Taken literally over *raw bytes* that invariant is **false by construction** for
//! this engine, and deliberately so: the crate's contract (`lib.rs`) is that
//! insignificant whitespace and escape style are canonicalized away, so two
//! byte-distinct inputs that differ only there are *supposed* to collapse to the same
//! rendering. Testing the literal statement would fail on the first benign
//! whitespace variant — that is the design, not a bug.
//!
//! The property's actual intent — no *semantically distinct* documents share a
//! rendering — is the contrapositive, which is both true and the strongest honest
//! form: `rendering(a) == rendering(b) => json_semantically_eq(a, b)`. That is what
//! [`prop08_anti_collision`] enforces; it still catches a real collision (two
//! genuinely different documents rendered identically) while not false-alarming on
//! the canonicalization the engine is paid to perform.

use proptest::prelude::*;
use tokfold_core::{
    CompressError, Compressor, Config, EncoderId, HeuristicEstimator, TokenEstimator,
};

#[path = "oracle.rs"]
mod oracle;
use oracle::json_semantically_eq;

/// Default 32 cases locally; `PROPTEST_CASES` overrides it (CI uses 1024). Reading
/// the env var ourselves guarantees an *unset* environment yields 32, not proptest's
/// built-in default of 256.
fn config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(32);
    ProptestConfig {
        cases,
        ..ProptestConfig::default()
    }
}

fn compressor() -> Compressor {
    Compressor::new(Config::default())
}

/// The sentinel line every passthrough rendering carries. 18 bytes.
const RAW_SENTINEL_LINE: &str = "\u{27E6}tkfd:v1:raw\u{27E7}\n";

/// What [`HeuristicEstimator`] charges for [`RAW_SENTINEL_LINE`] alone:
/// 2 (`\u{27E6}`) + 2 (`tkfd`) + 1 (`:`) + 1 (`v1`) + 1 (`:`) + 1 (`raw`) +
/// 2 (`\u{27E7}`) + 0 (the trailing newline, a one-character whitespace run) = 10.
///
/// Pinned as a literal on purpose: if the sentinel text or the estimator's cost
/// constants move, the tokens a passthrough result silently spends move with them,
/// and [`prop04_do_no_harm`] must say so out loud.
const RAW_FRAME_TOKENS: usize = 10;

// ===========================================================================
// JSON generators
// ===========================================================================

/// An abstract JSON node. Rendered to concrete JSON text by [`render`] /
/// [`render_pretty`], which always emit well-formed JSON.
#[derive(Debug, Clone)]
enum Node {
    Null,
    Bool(bool),
    /// A valid number lexeme (boundary values, 100-digit literals, floats, ...).
    Num(String),
    /// Decoded string content; [`push_json_string`] escapes it on render.
    Str(String),
    /// A raw JSON string body emitted verbatim between quotes (lone surrogates,
    /// specific escape spellings). Every value here is a valid string body.
    RawStr(String),
    Arr(Vec<Node>),
    Obj(Vec<(Key, Node)>),
}

#[derive(Debug, Clone)]
enum Key {
    Str(String),
    RawStr(String),
}

fn push_json_string(content: &str, out: &mut String) {
    out.push('"');
    for c in content.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_string(content: &str) -> String {
    let mut s = String::new();
    push_json_string(content, &mut s);
    s
}

fn render_key(key: &Key, out: &mut String) {
    match key {
        Key::Str(s) => push_json_string(s, out),
        Key::RawStr(b) => {
            out.push('"');
            out.push_str(b);
            out.push('"');
        }
    }
}

fn render(n: &Node, out: &mut String) {
    match n {
        Node::Null => out.push_str("null"),
        Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Node::Num(s) => out.push_str(s),
        Node::Str(s) => push_json_string(s, out),
        Node::RawStr(b) => {
            out.push('"');
            out.push_str(b);
            out.push('"');
        }
        Node::Arr(items) => {
            out.push('[');
            for (k, it) in items.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                render(it, out);
            }
            out.push(']');
        }
        Node::Obj(members) => {
            out.push('{');
            for (k, (key, val)) in members.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                render_key(key, out);
                out.push(':');
                render(val, out);
            }
            out.push('}');
        }
    }
}

fn indent(depth: usize, out: &mut String) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// Pretty-print with newlines and indentation, to exercise the whitespace-stripping
/// encoder path. Empty containers and scalars fall back to compact rendering.
fn render_pretty(n: &Node, depth: usize, out: &mut String) {
    match n {
        Node::Arr(items) if !items.is_empty() => {
            out.push('[');
            for (k, it) in items.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                out.push('\n');
                indent(depth + 1, out);
                render_pretty(it, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push(']');
        }
        Node::Obj(members) if !members.is_empty() => {
            out.push('{');
            for (k, (key, val)) in members.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                out.push('\n');
                indent(depth + 1, out);
                render_key(key, out);
                out.push_str(": ");
                render_pretty(val, depth + 1, out);
            }
            out.push('\n');
            indent(depth, out);
            out.push('}');
        }
        other => render(other, out),
    }
}

/// Number lexemes: boundary integers, a 100-digit literal, floats and exponents.
/// Spec §12 hostile generator: "numbers at i64/u64/f64 boundaries and 100-digit
/// literals".
fn arb_num() -> impl Strategy<Value = String> {
    prop_oneof![
        any::<i64>().prop_map(|n| n.to_string()),
        any::<u64>().prop_map(|n| n.to_string()),
        Just("0".to_string()),
        Just("-0".to_string()),
        Just(i64::MIN.to_string()),
        Just(i64::MAX.to_string()),
        Just(u64::MAX.to_string()),
        Just(format!("-{}", u64::MAX)),
        // 100-digit integer, no leading zero.
        Just(format!("9{}", "0".repeat(99))),
        Just("1".repeat(100)),
        // Fraction and exponent forms.
        (any::<i32>(), 0u32..1_000_000).prop_map(|(a, b)| format!("{a}.{b}")),
        (any::<i16>(), any::<i8>()).prop_map(|(m, e)| format!("{m}e{e}")),
    ]
}

/// String content including the specials spec §12 calls out: NUL, BOM, NFC/NFD
/// pairs, plus arbitrary Unicode (astral, control). `push_json_string` escapes
/// whatever needs escaping, so rendering is always valid JSON.
fn arb_str_content() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::collection::vec(any::<char>(), 0..16).prop_map(|v| v.into_iter().collect()),
        Just(String::new()),
        Just("\u{0}".to_string()),       // NUL
        Just("\u{feff}bom".to_string()), // BOM
        Just("é".to_string()),           // NFC (U+00E9)
        Just("e\u{301}".to_string()),    // NFD (e + combining acute)
        Just("\u{1f600}".to_string()),   // astral (emoji)
        Just("tab\there".to_string()),
        Just("new\nline".to_string()),
        Just("quote\"and\\slash/".to_string()),
    ]
}

/// A single `\uXXXX` escape for an arbitrary BMP scalar *outside* the surrogate
/// block, in either hex case.
///
/// The hand-written pool in [`arb_raw_body`] only ever reaches `A` and
/// `é`, so before this arm existed **no escape above `0x00FF` was reachable
/// anywhere in the suite** at any case count: the whole Greek / Cyrillic / CJK /
/// Hangul range of the escape decoder went untested. That was not a theoretical
/// gap — corrupting every escape in `0x0100..=0xD7FF` left the entire suite green.
///
/// The hex case is randomized on purpose: spec §4 makes it *significant* for a
/// string that also carries a lone surrogate (such a string is compared by its raw
/// body bytes) and *insignificant* for every other string, and the engine has to
/// get both halves of that right.
fn arb_bmp_escape() -> impl Strategy<Value = String> {
    (
        prop_oneof![0x0000u32..=0xD7FF, 0xE000u32..=0xFFFF],
        any::<bool>(),
    )
        .prop_map(|(cp, upper)| {
            if upper {
                format!("\\u{cp:04X}")
            } else {
                format!("\\u{cp:04x}")
            }
        })
}

/// Raw JSON string bodies (emitted verbatim between quotes). Includes unpaired
/// surrogates via raw escapes and a valid surrogate pair. Spec §12 hostile
/// generator: "unpaired surrogates via raw escapes".
///
/// Two thirds of the weight stays on the curated pool — those are the cases with a
/// known history — and one third goes to freely generated escape soup, so the
/// generator keeps searching instead of replaying fourteen fixed strings.
fn arb_raw_body() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => prop::sample::select(vec![
            "\\ud834".to_string(),        // lone high surrogate
            "\\udc00".to_string(),        // lone low surrogate
            "a\\udeadb".to_string(),      // lone surrogate embedded in text
            "\\uD834".to_string(),        // same lone surrogate, different hex case
            "\\ud83d\\ude00".to_string(), // valid surrogate pair (emoji)
            // A lone surrogate followed by canonicalizable content in the SAME string.
            // Spec §4 compares such a string by its raw body, so none of the trailing
            // content may be rewritten; these three catch a canonicalizer that decides
            // escape-by-escape instead of per-string.
            "\\ud834\\ud834\\udd1e".to_string(), // lone surrogate, then a foldable pair
            "\\ud834\\/".to_string(),            // lone surrogate, then a redundant solidus
            "\\ud834\\u0041".to_string(),        // lone surrogate, then an escaped 'A'
            "\\u0041".to_string(),               // escaped 'A'
            "\\u00e9".to_string(),               // escaped 'é'
            "\\n\\t\\r".to_string(),             // short escapes
            String::new(),
            "plain".to_string(),
        ]),
        1 => prop::collection::vec(
            prop_oneof![
                6 => arb_bmp_escape(),
                // Lone and paired surrogates, drawn independently so consecutive
                // draws sometimes form a valid pair and sometimes do not.
                2 => (0xD800u32..=0xDFFF, any::<bool>()).prop_map(|(cp, upper)| if upper {
                    format!("\\u{cp:04X}")
                } else {
                    format!("\\u{cp:04x}")
                }),
                // Two-character escapes and literal text, so escapes are exercised
                // adjacent to plain content rather than only in isolation.
                3 => prop::sample::select(vec![
                    "\\n", "\\t", "\\r", "\\b", "\\f", "\\\\", "\\\"", "\\/",
                    "a", "Z", "0", " ", "é", "\u{1f600}",
                ])
                .prop_map(String::from),
            ],
            1..6,
        )
        .prop_map(|parts| parts.concat()),
    ]
}

/// Object keys. A small shared pool makes duplicate keys occur naturally; arbitrary
/// and raw-surrogate keys widen coverage. Spec §12 hostile generator: "duplicate
/// keys at every level".
fn arb_key() -> impl Strategy<Value = Key> {
    prop_oneof![
        3 => prop::sample::select(vec!["a", "b", "c", "id", "name", "type", "x"])
            .prop_map(|s| Key::Str(s.to_string())),
        1 => arb_str_content().prop_map(Key::Str),
        1 => arb_raw_body().prop_map(Key::RawStr),
    ]
}

fn arb_leaf() -> impl Strategy<Value = Node> {
    prop_oneof![
        Just(Node::Null),
        any::<bool>().prop_map(Node::Bool),
        arb_num().prop_map(Node::Num),
        arb_str_content().prop_map(Node::Str),
        arb_raw_body().prop_map(Node::RawStr),
    ]
}

/// Recursive JSON AST, depth <= 8 as spec §12 requires.
fn arb_node() -> impl Strategy<Value = Node> {
    arb_leaf().prop_recursive(8, 48, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Node::Arr),
            prop::collection::vec((arb_key(), inner), 0..5).prop_map(Node::Obj),
        ]
    })
}

/// Structured JSON text (compact or pretty), capped at 4 KB as spec §12 requires.
fn arb_structured_json() -> impl Strategy<Value = String> {
    (arb_node(), any::<bool>())
        .prop_map(|(node, pretty)| {
            let mut out = String::new();
            if pretty {
                render_pretty(&node, 0, &mut out);
            } else {
                render(&node, &mut out);
            }
            out
        })
        .prop_filter("document <= 4 KB", |s| s.len() <= 4096)
}

/// Valid but deeply nested arrays: 129..=160 levels. Spec §12 hostile generator:
/// "nesting > 128". Kept under the engine's default `max_depth` (512) so the input is
/// *accepted* and must round-trip; the rejection path is covered by [`prop07_depth`].
fn arb_deep_valid() -> impl Strategy<Value = String> {
    (129usize..=160).prop_map(|d| format!("{}1{}", "[".repeat(d), "]".repeat(d)))
}

/// Documents with duplicate keys at several nesting levels.
fn arb_dupkey() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        r#"{"a":1,"a":2}"#.to_string(),
        r#"{"a":1,"a":2,"b":{"a":1,"a":2}}"#.to_string(),
        r#"[{"k":1,"k":2},{"k":3,"k":3}]"#.to_string(),
        r#"{"x":{"y":{"z":1,"z":2}},"x":9}"#.to_string(),
    ])
}

/// A one-element array wrapping a special string (NUL, BOM, NFC/NFD, astral, ...).
fn arb_special_string_doc() -> impl Strategy<Value = String> {
    arb_str_content().prop_map(|s| format!("[{}]", json_string(&s)))
}

/// Whole documents whose strings contain lone (or paired) surrogates via raw escapes.
fn arb_lone_surrogate_doc() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "\"\\ud834\"".to_string(),
        "[\"\\udc00\"]".to_string(),
        "{\"k\":\"a\\udeadb\"}".to_string(),
        "[\"\\ud834\",\"\\udc00\"]".to_string(),
        "{\"\\udead\":1}".to_string(),
        "\"\\ud83d\\ude00\"".to_string(), // valid pair
    ])
}

/// The full valid-JSON strategy fed to the round-trip properties: structured
/// documents plus every hostile-but-valid generator spec §12 lists.
fn arb_valid_json() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => arb_structured_json(),
        1 => arb_deep_valid(),
        2 => arb_num(),
        2 => arb_dupkey(),
        2 => arb_special_string_doc(),
        2 => arb_lone_surrogate_doc(),
    ]
}

/// Small valid documents, so the exhaustive bit-flip sweep in [`prop05_fail_closed`]
/// stays cheap while still covering surrogate/special payloads.
fn arb_small_valid_json() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop::sample::select(vec![
            "{}", "[]", "null", "true", "false", "0", "-0", "1e10",
            "{\"a\":1}", "[1,2,3]", "\"hi\"", "{\"a\":[1,2],\"b\":{\"c\":3}}",
        ])
        .prop_map(String::from),
        2 => arb_num(),
        1 => arb_lone_surrogate_doc(),
        1 => arb_special_string_doc(),
    ]
}

// ===========================================================================
// Related-document pairs (property 8)
// ===========================================================================

/// Inserts insignificant whitespace after every structural character that sits
/// outside a string literal.
///
/// The result is byte-distinct from `doc` (whenever `doc` has any structure at all)
/// yet semantically identical, which is exactly the input class whose renderings
/// *should* collide. Drawing the two sides of [`prop08_anti_collision`]
/// independently produced only byte-identical pairs in practice, which collapsed
/// the property to oracle reflexivity; this makes the collision branch fire on
/// genuinely different bytes.
fn inject_whitespace(doc: &str, filler: &str) -> String {
    let mut out = String::with_capacity(doc.len() * 2);
    let mut in_string = false;
    let mut escaped = false;
    for ch in doc.chars() {
        out.push(ch);
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' | '}' | '[' | ']' | ',' | ':' => out.push_str(filler),
            _ => {}
        }
    }
    out
}

/// Upper-cases the hex digits of every `\uXXXX` escape inside a string literal.
///
/// Spec §4 makes this rewrite semantics-*preserving* for an ordinary string and
/// semantics-*changing* for a string that carries a lone surrogate (compared by raw
/// body bytes, hex case significant). Feeding both sides of the same distinction to
/// property 8 forces the engine to canonicalize in the first case and refuse to in
/// the second — the precise line it once got wrong.
fn uppercase_unicode_escapes(doc: &str) -> String {
    let src: Vec<char> = doc.chars().collect();
    let mut out = String::with_capacity(doc.len());
    let mut in_string = false;
    let mut i = 0;
    while i < src.len() {
        let ch = src[i];
        if !in_string {
            out.push(ch);
            if ch == '"' {
                in_string = true;
            }
            i += 1;
            continue;
        }
        if ch == '\\' && i + 1 < src.len() {
            if src[i + 1] == 'u' && i + 5 < src.len() {
                out.push_str("\\u");
                for k in 0..4 {
                    out.push(src[i + 2 + k].to_ascii_uppercase());
                }
                i += 6;
                continue;
            }
            out.push(ch);
            out.push(src[i + 1]);
            i += 2;
            continue;
        }
        out.push(ch);
        if ch == '"' {
            in_string = false;
        }
        i += 1;
    }
    out
}

/// A document and a *related* one: same document reformatted, restyled, or wrapped.
/// Every mutation maps valid JSON to valid JSON, so both sides always compress.
fn arb_related_pair() -> impl Strategy<Value = (String, String)> {
    (arb_valid_json(), 0usize..6).prop_map(|(a, which)| {
        let b = match which {
            0 => inject_whitespace(&a, " "),
            1 => inject_whitespace(&a, "\n  "),
            2 => inject_whitespace(&a, "\t"),
            3 => uppercase_unicode_escapes(&a),
            4 => format!("[{a}]"),
            _ => format!("{{\"w\":{a}}}"),
        };
        (a, b)
    })
}

/// The pair strategy behind property 8: mostly related documents (so the collision
/// branch is actually reached) with independent draws still in the mix, since those
/// are the only way an unrelated cross-document collision could ever surface.
fn arb_json_pair() -> impl Strategy<Value = (String, String)> {
    prop_oneof![
        1 => (arb_valid_json(), arb_valid_json()),
        3 => arb_related_pair(),
    ]
}

/// Byte strings that are *plausibly* JSON.
///
/// [`prop06_no_panic_on_arbitrary_bytes`] used only `any::<u8>()` vectors, and over
/// 20 000 generated cases not one of them parsed — so its `Ok` arm, the half that
/// asserts the round-trip, was dead code. Mixing in valid documents and
/// lightly-corrupted valid documents keeps the "total on garbage" guarantee while
/// making both arms live, and the near-miss edits probe the parser's error paths far
/// closer to the boundary than random noise ever gets.
fn arb_hostile_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => prop::collection::vec(any::<u8>(), 0..2048),
        2 => arb_valid_json().prop_map(String::into_bytes),
        3 => (
            arb_valid_json(),
            prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..4),
        )
            .prop_map(|(s, edits)| {
                let mut v = s.into_bytes();
                if !v.is_empty() {
                    for (idx, byte) in edits {
                        let i = idx.index(v.len());
                        v[i] = byte;
                    }
                }
                v
            }),
        1 => arb_valid_json().prop_map(|s| {
            // Valid JSON with invalid UTF-8 welded on: must be rejected as NotUtf8
            // before the parser is ever reached.
            let mut v = s.into_bytes();
            v.extend_from_slice(&[0xff, 0xfe]);
            v
        }),
        1 => arb_valid_json().prop_map(|s| {
            // Truncation: the commonest real-world corruption of a captured tool
            // output, and the one most likely to leave the parser mid-state.
            let mut v = s.into_bytes();
            v.truncate(v.len() / 2);
            v
        }),
    ]
}

// ===========================================================================
// Properties 1-4, 8 and the hostile-valid round-trip (all over valid JSON)
// ===========================================================================

proptest! {
    #![proptest_config(config())]

    /// Property 1 — semantic round-trip: `json_semantically_eq(x, decompress(compress(x)))`.
    #[test]
    fn prop01_semantic_roundtrip(x in arb_valid_json()) {
        let c = compressor();
        let art = c.compress(x.as_bytes()).unwrap();
        let restored = c.decompress(&art.archive).unwrap();
        let restored_str = std::str::from_utf8(&restored)
            .expect("decompressed bytes must be valid UTF-8");
        prop_assert!(
            json_semantically_eq(&x, restored_str),
            "semantic round-trip failed\n  in  = {:?}\n  out = {:?}",
            x, restored_str
        );
    }

    /// Property 1b — the *rendering* is semantically faithful, not just the archive.
    ///
    /// [`prop01_semantic_roundtrip`] asserts on `decompress(compress(x))`, but the
    /// archive wraps the original bytes verbatim, so that assertion holds by
    /// construction and exercises no encoder rendering code at all. This covers the
    /// model-facing side: the rendering body — the rendering minus its sentinel line
    /// — must still stand for the input.
    ///
    /// The claim is per encoder: passthrough must reproduce the input byte for byte,
    /// and E1 minify must stay section-4-equal to it. E2's body is a header + row
    /// projection rather than a JSON document, so its fidelity is left to
    /// [`prop08_anti_collision`] and the E2 unit tests. Dispatching on the wire id
    /// with a plain `match` rather than `prop_assume!` matters: only a few percent of
    /// generated documents select E1, so assuming would exhaust proptest's global
    /// reject budget instead of testing anything.
    #[test]
    fn prop01b_rendering_semantic_fidelity(x in arb_valid_json()) {
        let c = compressor();
        let art = c.compress(x.as_bytes()).unwrap();
        let body = art.rendering
            .split_once('\n')
            .expect("a framed rendering always carries a sentinel line")
            .1;
        // Field access is allowed on the non_exhaustive id even from a test crate.
        match art.stats.encoder.0 {
            0 => prop_assert_eq!(
                body, x.as_str(),
                "passthrough rendering altered its input\n  in   = {:?}\n  body = {:?}",
                x, body
            ),
            1 => prop_assert!(
                json_semantically_eq(&x, body),
                "minify rendering is NOT semantically equal to its input\n  in   = {:?}\n  body = {:?}",
                x, body
            ),
            _ => {}
        }
    }

    /// Property 2 — idempotence: `compress(decompress(a))` is a fixed point.
    #[test]
    fn prop02_idempotence(x in arb_valid_json()) {
        let c = compressor();
        let art1 = c.compress(x.as_bytes()).unwrap();
        let decoded = c.decompress(&art1.archive).unwrap();
        let art2 = c.compress(&decoded).unwrap();
        prop_assert_eq!(&art2.archive, &art1.archive, "archive not a fixed point");
        prop_assert_eq!(&art2.rendering, &art1.rendering, "rendering not a fixed point");
        let decoded2 = c.decompress(&art2.archive).unwrap();
        prop_assert_eq!(decoded2, decoded, "second decompression diverged");
    }

    /// Property 3 — bit-determinism: same archive always decompresses to the same
    /// bytes, and compression itself is byte-identical across runs.
    #[test]
    fn prop03_bit_determinism(x in arb_valid_json()) {
        let c = compressor();
        let art = c.compress(x.as_bytes()).unwrap();
        let a = c.decompress(&art.archive).unwrap();
        let b = c.decompress(&art.archive).unwrap();
        prop_assert_eq!(&a, &b, "same archive decompressed to different bytes");
        prop_assert_eq!(a.as_slice(), x.as_bytes(), "v0.0.1 must reconstruct input exactly");

        let art2 = c.compress(x.as_bytes()).unwrap();
        prop_assert_eq!(&art2.archive, &art.archive, "compression not deterministic (archive)");
        prop_assert_eq!(&art2.rendering, &art.rendering, "compression not deterministic (rendering)");
    }

    /// Property 4 — do no harm, stated over the text that is actually shipped.
    ///
    /// # Why the obvious form is worthless
    ///
    /// `stats.est_tokens_after <= stats.est_tokens_before` — what this property
    /// asserted until 2026-07-30 — cannot fail. `encoder::select` has exactly two
    /// exits: it keeps a candidate only after `clears_margin` proved the candidate
    /// strictly *below* `est_before`, and on passthrough it **assigns**
    /// `est_tokens_after = est_before`. Both branches make the inequality true by
    /// construction, so the property ran 1024 cases per CI job and could never go red.
    /// It restated an assignment, not a guarantee.
    ///
    /// # What is asserted instead
    ///
    /// The rendering is re-measured with the same estimator the compressor used, and
    /// the reported numbers are checked against that measurement:
    ///
    /// * a winning encoder must report exactly what its own rendering estimates, and
    ///   that must be a strict win over the input — sentinel line included;
    /// * passthrough reports a clean no-op (`token_ratio() == 1.0`), but the text
    ///   handed to the model is the input *plus* [`RAW_SENTINEL_LINE`], which costs
    ///   [`RAW_FRAME_TOKENS`] every time. The gap between the reported `1.0` and the
    ///   tokens actually spent is pinned here as a measurement. Whether the reporting
    ///   should change is an open product question this test deliberately does not
    ///   answer — it fails if the *size* of the gap moves, not because the gap exists.
    #[test]
    fn prop04_do_no_harm(x in arb_valid_json()) {
        let c = compressor();
        let art = c.compress(x.as_bytes()).unwrap();
        let est = HeuristicEstimator; // what `Config::default()` selects with

        let measured_input = est.estimate(&x);
        let measured_rendering = est.estimate(&art.rendering);
        prop_assert_eq!(art.stats.est_tokens_before, measured_input);
        prop_assert_eq!(art.stats.bytes_before, x.len());

        if art.stats.encoder == EncoderId::PASSTHROUGH {
            // Reported as a no-op...
            prop_assert_eq!(art.stats.est_tokens_after, art.stats.est_tokens_before);
            prop_assert_eq!(art.stats.token_ratio(), 1.0);
            prop_assert_eq!(art.stats.byte_ratio(), 1.0);
            prop_assert_eq!(&art.rendering, &format!("{RAW_SENTINEL_LINE}{x}"));

            // ...but strictly more expensive than the input, by the sentinel line.
            // The `+ 1` applies only when `x` opens with exactly one ASCII whitespace
            // character: the sentinel's trailing newline then joins it into a
            // two-character run, which the heuristic charges where a one-character
            // run is free. (No generator here emits leading whitespace today, so that
            // arm is a correctness hedge; `compressor::tests` covers it directly.)
            let leading_ws = x.chars().take_while(char::is_ascii_whitespace).count();
            let expected = measured_input + RAW_FRAME_TOKENS + usize::from(leading_ws == 1);
            prop_assert_eq!(
                measured_rendering, expected,
                "passthrough framing cost moved: {} -> {} on {:?}",
                measured_input, measured_rendering, x
            );
        } else {
            // A winning encoder must report exactly what it shipped...
            prop_assert_eq!(art.stats.est_tokens_after, measured_rendering);
            // ...and that must be a real win over the input, framing included.
            prop_assert!(
                measured_rendering < measured_input,
                "encoder {:?} kept a rendering that is not a win: {} -> {} on {:?}",
                art.stats.encoder, measured_input, measured_rendering, x
            );
            prop_assert!(art.stats.token_ratio() < 1.0);
        }
    }

    /// Property 8 — anti-collision, as the honest contrapositive: identical
    /// renderings imply semantically identical documents. See the module docs.
    ///
    /// `b` is mostly *derived* from `a` rather than drawn independently
    /// ([`arb_json_pair`]). Two independent draws are almost never close enough to
    /// collide, so the `ra == rb` branch only ever fired on byte-identical pairs and
    /// the property degenerated into asserting that the oracle is reflexive.
    #[test]
    fn prop08_anti_collision((a, b) in arb_json_pair()) {
        let c = compressor();
        let ra = c.compress(a.as_bytes()).unwrap().rendering;
        let rb = c.compress(b.as_bytes()).unwrap().rendering;
        if ra == rb {
            prop_assert!(
                json_semantically_eq(&a, &b),
                "rendering collision between semantically DISTINCT inputs\n  a = {:?}\n  b = {:?}\n  rendering = {:?}",
                a, b, ra
            );
        }
    }
}

// ===========================================================================
// Property 5 — fail closed
// ===========================================================================

proptest! {
    #![proptest_config(config())]

    /// Property 5 — flipping any single bit of a valid archive is rejected.
    #[test]
    fn prop05_fail_closed(x in arb_small_valid_json()) {
        let c = compressor();
        let art = c.compress(x.as_bytes()).unwrap();
        prop_assert!(c.decompress(&art.archive).is_ok(), "intact archive must decode");

        let n_bits = art.archive.len() * 8;
        // Keep the exhaustive sweep bounded; `arb_small_valid_json` stays well under.
        prop_assume!(n_bits <= 8192);
        for bit in 0..n_bits {
            let mut corrupt = art.archive.clone();
            corrupt[bit / 8] ^= 1 << (bit % 8);
            prop_assert!(
                c.decompress(&corrupt).is_err(),
                "flipping bit {} produced a valid decode\n  input = {:?}",
                bit, x
            );
        }
    }
}

// ===========================================================================
// Property 6 — no panic / no OOM on arbitrary bytes
// ===========================================================================

proptest! {
    #![proptest_config(config())]

    /// Property 6 — `compress`/`decompress` are total on arbitrary bytes: they return
    /// `Ok`/`Err`, never panic or run away. If arbitrary bytes happen to be valid
    /// JSON, the archive must still round-trip exactly.
    ///
    /// Fed by [`arb_hostile_bytes`], which deliberately keeps a large share of
    /// *parseable* inputs: uniform random bytes never parse, so on a pure
    /// `any::<u8>()` generator the `Ok` arm below never executed once.
    #[test]
    fn prop06_no_panic_on_arbitrary_bytes(bytes in arb_hostile_bytes()) {
        let c = compressor();
        match c.compress(&bytes) {
            Ok(art) => {
                let restored = c.decompress(&art.archive).unwrap();
                prop_assert_eq!(&restored, &bytes, "arbitrary valid JSON failed to round-trip");
            }
            Err(_) => {}
        }
        // Feeding arbitrary bytes straight to the decoder must also never panic.
        let _ = c.decompress(&bytes);
    }
}

// ===========================================================================
// Property 7 — depth
// ===========================================================================

proptest! {
    #![proptest_config(config())]

    /// Property 7 — nesting beyond `max_depth` yields `DepthExceeded`, never a stack
    /// overflow (the parser is iterative).
    #[test]
    fn prop07_depth(md in 1usize..=16, extra in 1usize..=64) {
        let cfg = Config::builder().max_depth(md).build();
        let c = Compressor::new(cfg);
        let depth = md + extra;
        let input = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        match c.compress(input.as_bytes()) {
            Err(CompressError::DepthExceeded { limit, .. }) => prop_assert_eq!(limit, md),
            other => prop_assert!(false, "expected DepthExceeded, got {:?}", other),
        }
    }
}

// ===========================================================================
// Dirty-reality inputs and other concrete cases (spec §12)
// ===========================================================================

/// Spec §12: `NaN`, `Infinity`, truncated JSON, ANSI escape codes and JSONL must all
/// yield `CompressError::InvalidJson`, never a panic.
#[test]
fn dirty_reality_inputs_are_invalid_json() {
    let c = compressor();
    let bad = [
        // NaN / Infinity (Python's json.dumps emits these; JSON does not define them).
        "NaN",
        "Infinity",
        "-Infinity",
        "[NaN]",
        "[Infinity]",
        "{\"x\":NaN}",
        // Truncated documents.
        "{",
        "[",
        "{\"a\":",
        "{\"a\":1",
        "[1,",
        "\"abc",
        "tru",
        "nul",
        // Trailing garbage / multiple documents (JSONL).
        "1 2",
        "{} {}",
        "[]x",
        "null null",
        "{\"a\":1}\n{\"b\":2}",
        // Leading zeros.
        "01",
        "-01",
        "{\"a\":00}",
        // ANSI escape codes (valid UTF-8, not JSON).
        "\u{1b}[31mred\u{1b}[0m",
        // Empty / whitespace-only.
        "",
        "   ",
        // Single quotes / unquoted keys / trailing comma.
        "'single'",
        "{a:1}",
        "[1,2,]",
    ];
    for input in bad {
        match c.compress(input.as_bytes()) {
            Err(CompressError::InvalidJson { .. }) => {}
            other => panic!("expected InvalidJson for {input:?}, got {other:?}"),
        }
    }
}

/// Non-UTF-8 input is rejected as `NotUtf8`, never a panic.
#[test]
fn non_utf8_input_is_rejected() {
    let c = compressor();
    for bytes in [
        vec![0xff, 0xfe],
        vec![0x80],
        b"{\"a\":\""
            .iter()
            .copied()
            .chain([0xff, 0x22, 0x7d])
            .collect::<Vec<u8>>(),
    ] {
        match c.compress(&bytes) {
            Err(CompressError::NotUtf8) => {}
            other => panic!("expected NotUtf8 for {bytes:?}, got {other:?}"),
        }
    }
}

/// Property 7, concrete extremes: hugely deep input errors cleanly with no stack
/// overflow, under both the unbalanced and balanced shapes.
#[test]
fn extremely_deep_input_errors_without_stack_overflow() {
    let c = compressor(); // default max_depth = 512
    let unbalanced = "[".repeat(100_000);
    match c.compress(unbalanced.as_bytes()) {
        Err(CompressError::DepthExceeded { limit, .. }) => assert_eq!(limit, 512),
        other => panic!("expected DepthExceeded, got {other:?}"),
    }
    let balanced = format!("{}1{}", "[".repeat(2000), "]".repeat(2000));
    assert!(matches!(
        c.compress(balanced.as_bytes()),
        Err(CompressError::DepthExceeded { .. })
    ));
}

/// Property 5, concrete: an exhaustive single-bit sweep over a representative,
/// larger archive — every bit is load-bearing.
#[test]
fn fail_closed_full_bit_sweep() {
    let c = compressor();
    let input = r#"{"users":[{"id":1,"name":"alice"},{"id":2,"name":"bob"}],"n":42,"ok":true}"#;
    let art = c.compress(input.as_bytes()).unwrap();
    assert!(c.decompress(&art.archive).is_ok());
    for bit in 0..art.archive.len() * 8 {
        let mut corrupt = art.archive.clone();
        corrupt[bit / 8] ^= 1 << (bit % 8);
        assert!(
            c.decompress(&corrupt).is_err(),
            "flipping bit {bit} produced a valid decode"
        );
    }
}

/// True when `body` contains a `\uXXXX` escape for a scalar above `0x00FF`.
fn has_escape_above_00ff(body: &str) -> bool {
    let src: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i + 5 < src.len() {
        if src[i] == '\\' && src[i + 1] == 'u' && (src[i + 2] != '0' || src[i + 3] != '0') {
            return true;
        }
        i += 1;
    }
    false
}

/// Generator liveness — the properties above are only as strong as the inputs they
/// are handed, and a property that never reaches its interesting region still passes,
/// silently. Every assertion here corresponds to a region that was measurably
/// *unreachable* before the generators were widened:
///
/// - `arb_json_pair` never produced byte-distinct documents that render identically,
///   so [`prop08_anti_collision`]'s assertion only ever ran on `a == b` and reduced to
///   oracle reflexivity.
/// - `arb_hostile_bytes`' predecessor (uniform `any::<u8>()`) never once parsed, so
///   the round-trip arm of [`prop06_no_panic_on_arbitrary_bytes`] was dead code.
/// - `arb_raw_body` could not emit any escape above `0x00FF`, leaving the entire
///   Greek / Cyrillic / CJK / Hangul range of the escape decoder untested.
///
/// A fixed seed (`TestRunner::deterministic`) keeps this a hard assertion rather than
/// a flaky one.
#[test]
fn generators_reach_the_regions_the_properties_need() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    const DRAWS: usize = 1500;

    let mut runner = TestRunner::deterministic();
    let c = compressor();

    let mut distinct_collisions = 0usize;
    let mut hostile_ok = 0usize;
    let mut hostile_err = 0usize;
    let mut high_escapes = 0usize;

    for _ in 0..DRAWS {
        if let Ok(tree) = arb_json_pair().new_tree(&mut runner) {
            let (a, b) = tree.current();
            if a != b {
                let ra = c.compress(a.as_bytes()).expect("mutated pair stays valid");
                let rb = c.compress(b.as_bytes()).expect("mutated pair stays valid");
                if ra.rendering == rb.rendering {
                    distinct_collisions += 1;
                }
            }
        }
        if let Ok(tree) = arb_hostile_bytes().new_tree(&mut runner) {
            if c.compress(&tree.current()).is_ok() {
                hostile_ok += 1;
            } else {
                hostile_err += 1;
            }
        }
        if let Ok(tree) = arb_raw_body().new_tree(&mut runner) {
            if has_escape_above_00ff(&tree.current()) {
                high_escapes += 1;
            }
        }
    }

    assert!(
        distinct_collisions > 0,
        "prop08 is vacuous: {DRAWS} pairs produced no byte-distinct rendering collision"
    );
    assert!(
        hostile_ok > 0,
        "prop06's round-trip arm is dead: none of {DRAWS} hostile inputs compressed"
    );
    assert!(
        hostile_err > 0,
        "prop06's rejection arm is dead: all of {DRAWS} hostile inputs compressed"
    );
    assert!(
        high_escapes > 0,
        "arb_raw_body never emitted an escape above 0x00FF in {DRAWS} draws"
    );
}

/// Property 8, positive smoke test: hand-picked *semantically distinct* documents
/// (including canonicalization-prone ones) get distinct renderings.
#[test]
fn distinct_documents_get_distinct_renderings() {
    let c = compressor();
    let pairs = [
        (r#"{"a":1}"#, r#"{"a":2}"#),
        ("[1,2]", "[2,1]"),
        (r#"{"a":1,"b":2}"#, r#"{"b":2,"a":1}"#), // reordered keys
        ("[1e2]", "[100]"),                       // distinct number lexemes
        (r#"{"a":1,"a":2}"#, r#"{"a":2}"#),       // duplicate not collapsed
        ("1", "2"),
        ("\"x\"", "\"y\""),
        ("\"\\ud834\"", "\"\\udc00\""), // distinct lone surrogates
    ];
    for (a, b) in pairs {
        // Sanity: the oracle agrees these are distinct.
        assert!(!json_semantically_eq(a, b), "oracle thinks {a:?} == {b:?}");
        let ra = c.compress(a.as_bytes()).unwrap().rendering;
        let rb = c.compress(b.as_bytes()).unwrap().rendering;
        assert_ne!(ra, rb, "distinct documents collided: {a:?} vs {b:?}");
    }
}
