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

//! Property-based conformance suite for `tokfold-core`, covering the eight
//! properties frozen in `docs/ai/impl-spec-v0.1.md` §12, plus the hostile and
//! "dirty reality" generators that section demands.
//!
//! # Case count
//!
//! Proptest reads `PROPTEST_CASES`; [`config`] makes the default **32** locally and
//! CI sets `PROPTEST_CASES=1024`. Any shrunk counterexample is persisted under
//! `proptest-regressions/`, which is committed (never git-ignored) so a regression
//! is replayed deterministically forever.
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
use tokfold_core::{CompressError, Compressor, Config};

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

/// Raw JSON string bodies (emitted verbatim between quotes). Includes unpaired
/// surrogates via raw escapes and a valid surrogate pair. Spec §12 hostile
/// generator: "unpaired surrogates via raw escapes".
fn arb_raw_body() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "\\ud834".to_string(),        // lone high surrogate
        "\\udc00".to_string(),        // lone low surrogate
        "a\\udeadb".to_string(),      // lone surrogate embedded in text
        "\\uD834".to_string(),        // same lone surrogate, different hex case
        "\\ud83d\\ude00".to_string(), // valid surrogate pair (emoji)
        "\\u0041".to_string(),        // escaped 'A'
        "\\u00e9".to_string(),        // escaped 'é'
        "\\n\\t\\r".to_string(),      // short escapes
        String::new(),
        "plain".to_string(),
    ])
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

    /// Property 4 — do no harm: estimated tokens never grow.
    #[test]
    fn prop04_do_no_harm(x in arb_valid_json()) {
        let c = compressor();
        let art = c.compress(x.as_bytes()).unwrap();
        prop_assert!(
            art.stats.est_tokens_after <= art.stats.est_tokens_before,
            "tokens grew: {} -> {}",
            art.stats.est_tokens_before, art.stats.est_tokens_after
        );
        prop_assert!(art.stats.token_ratio() <= 1.0);
        prop_assert_eq!(art.stats.bytes_before, x.len());
    }

    /// Property 8 — anti-collision, as the honest contrapositive: identical
    /// renderings imply semantically identical documents. See the module docs.
    #[test]
    fn prop08_anti_collision(a in arb_valid_json(), b in arb_valid_json()) {
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
    #[test]
    fn prop06_no_panic_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
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
