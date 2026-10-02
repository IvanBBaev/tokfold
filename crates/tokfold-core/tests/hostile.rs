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

//! Hostile generators for the reversibility contract of `tokfold-core`.
//!
//! `tests/roundtrip.rs` holds the eight frozen properties over a broad valid-JSON
//! generator. This file adds the adversarial inputs that generator does not reach,
//! each aimed at one clause of the documented contract rather than at "JSON in
//! general":
//!
//! * nesting **at** the configured `max_depth` for every container kind (accepted)
//!   and one level past it (refused with the exact `depth` / `limit`), where
//!   `roundtrip.rs` only drives arrays and only past the limit;
//! * number lexemes from the full RFC 8259 grammar — 100+-digit integer and fraction
//!   parts, `e`/`E` with and without a sign, `-0` spellings, values past every `f64`
//!   and `u64` bound — plus malformed lexemes that must be refused;
//! * string bodies built from every control-character escape, lone and reversed
//!   surrogates, noncharacters, `U+2028`/`U+2029`, NFC/NFD pairs, the sentinel's own
//!   text and E2's row-marker punctuation — used as keys *and* values;
//! * raw control bytes, bad escapes and invalid UTF-8 spliced into a valid document
//!   at a generated position, refused at that exact byte offset;
//! * arrays of near-identical objects built to reach E2 (tabular), with rows that
//!   differ in one value, swap two keys, drop a key or repeat one — checked through
//!   the public API with an E2 body decoder written from the grammar in the
//!   `encoder::e2_tabular` rustdoc;
//! * every prefix of a hostile document of at most 256 bytes (the loop is quadratic
//!   in the document, so longer draws are filtered out), and the size cap at a
//!   generated limit.
//!
//! # The contract every accepted input is held to
//!
//! [`check_accepted`] applies the whole contract to one input: the crate docs say
//! decompression reproduces "a **semantically identical** document" and that "because
//! every archive this version writes holds the original bytes verbatim (see below),
//! `Compressor::decompress` is byte-exact today", so the archive must give back the
//! input byte for byte; they say "the same input bytes under the same `Config` produce
//! byte-identical output from one build", so a second `compress` must agree; property
//! 2 of the frozen spec says "`compress(decompress(a))` is a fixed point"; and the
//! rendering body must stand for the input — verbatim on passthrough, semantically
//! equal under E1, and semantically equal after table expansion under E2.
//!
//! # Case count
//!
//! As in `roundtrip.rs`: 32 cases unless `PROPTEST_CASES` says otherwise.
//!
//! The oracle is `tests/oracle.rs`, pulled in by path exactly as `roundtrip.rs` does,
//! so its own unit tests run once more in this binary.

use proptest::prelude::*;
use tokfold_core::{Artifact, CompressError, Compressor, Config, EncoderId};

#[path = "oracle.rs"]
mod oracle;
use oracle::json_semantically_eq;

/// Default 32 cases locally; `PROPTEST_CASES` overrides it, as in `roundtrip.rs`.
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

/// The documented default nesting ceiling (`ConfigBuilder::max_depth`: "Default: 512").
const DEFAULT_MAX_DEPTH: usize = 512;

/// The documented default input ceiling (16 MiB).
const DEFAULT_MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;

// ===========================================================================
// The contract, applied to one input
// ===========================================================================

/// The sentinel line an encoder's rendering opens with, without its newline. The
/// tags are the ones `encoder::Encoder` documents: `raw`, `min` and `tbl`.
fn sentinel_for(encoder: EncoderId) -> Option<&'static str> {
    match encoder {
        EncoderId::PASSTHROUGH => Some("\u{27E6}tkfd:v1:raw\u{27E7}"),
        EncoderId::E1_MINIFY => Some("\u{27E6}tkfd:v1:min\u{27E7}"),
        EncoderId::E2_TABULAR => Some("\u{27E6}tkfd:v1:tbl\u{27E7}"),
        _ => None,
    }
}

/// Compresses `input`, which the caller expects to be accepted, and holds the result
/// to the whole contract described in the module docs. Returns the artifact so a
/// property can make further, input-specific claims about it.
fn check_accepted(c: &Compressor, input: &str) -> Result<Artifact, TestCaseError> {
    let art = match c.compress(input.as_bytes()) {
        Ok(art) => art,
        Err(e) => {
            return Err(TestCaseError::fail(format!(
                "valid input refused: {e:?}\n  input = {input:?}"
            )));
        }
    };

    // Byte-exact recovery (v0.0.1 archives hold the original verbatim).
    let decoded = c.decompress(&art.archive).unwrap();
    prop_assert_eq!(decoded.as_slice(), input.as_bytes(), "archive lost bytes");

    // Determinism: the same bytes under the same config give the same output.
    let again = c.compress(input.as_bytes()).unwrap();
    prop_assert_eq!(
        &again.rendering,
        &art.rendering,
        "rendering not deterministic"
    );
    prop_assert_eq!(&again.archive, &art.archive, "archive not deterministic");

    // Fixed point: compressing the expansion reproduces the artifact.
    let refolded = c.compress(&decoded).unwrap();
    prop_assert_eq!(
        &refolded.rendering,
        &art.rendering,
        "rendering not a fixed point"
    );
    prop_assert_eq!(&refolded.archive, &art.archive, "archive not a fixed point");

    // The rendering: sentinel line first, then a body that stands for the input.
    let (sentinel, body) = art
        .rendering
        .split_once('\n')
        .expect("a framed rendering carries a sentinel line");
    let expected_sentinel = sentinel_for(art.stats.encoder);
    prop_assert_eq!(
        Some(sentinel),
        expected_sentinel,
        "sentinel does not name the reported encoder {:?}",
        art.stats.encoder
    );
    match art.stats.encoder {
        EncoderId::PASSTHROUGH => {
            prop_assert_eq!(body, input, "passthrough body altered its input");
        }
        EncoderId::E1_MINIFY => prop_assert!(
            json_semantically_eq(input, body),
            "E1 body is not semantically equal to its input\n  in   = {:?}\n  body = {:?}",
            input,
            body
        ),
        EncoderId::E2_TABULAR => {
            let expanded = expand_e2_body(body);
            prop_assert!(
                expanded.is_some(),
                "E2 body does not follow the documented grammar\n  in   = {:?}\n  body = {:?}",
                input,
                body
            );
            let expanded = expanded.unwrap();
            prop_assert!(
                json_semantically_eq(input, &expanded),
                "E2 body does not expand to its input\n  in       = {:?}\n  body     = {:?}\n  expanded = {:?}",
                input,
                body,
                expanded
            );
        }
        other => prop_assert!(false, "unknown encoder {:?}", other),
    }
    Ok(art)
}

// ===========================================================================
// An E2 body decoder, written from the `e2_tabular` rustdoc grammar
// ===========================================================================

/// Splits a minified JSON array or object that fills `s` exactly (it opens with `[`
/// or `{` and its matching bracket is the last byte) into its top-level element
/// slices. Returns `None` if the bracket closes early, never closes, or `s` does
/// not open with one — the decoder treats all three as a grammar violation.
fn split_top_level(s: &str) -> Option<Vec<&str>> {
    let bytes = s.as_bytes();
    // Closers are matched to their opener by kind, not merely counted. A plain depth
    // counter accepted `+[1}` as the one-cell row `["1"]`, so the decoder let a
    // mismatched closer through and `the_e2_decoder_rejects_bodies_that_break_the_grammar`
    // never noticed: its only early-closing fixture, `+[1]]`, fails on the length
    // check instead.
    let outer_close = match bytes.first() {
        Some(b'[') => b']',
        Some(b'{') => b'}',
        _ => return None,
    };
    let mut elems = Vec::new();
    let mut want: Vec<u8> = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    let mut start = 1usize;
    for (i, &c) in bytes.iter().enumerate().skip(1) {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'[' => want.push(b']'),
            b'{' => want.push(b'}'),
            b']' | b'}' => match want.pop() {
                Some(expected) if c == expected => {}
                Some(_) => return None,
                None => {
                    if c != outer_close || i + 1 != bytes.len() {
                        return None;
                    }
                    if i > start {
                        elems.push(&s[start..i]);
                    }
                    return Some(elems);
                }
            },
            b',' if want.is_empty() => {
                elems.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    None
}

/// Expands a `tbl` body back into minified JSON, following the grammar the
/// `encoder::e2_tabular` rustdoc publishes: every table block is a `#<N>[keys]`
/// header line followed by exactly `N` rows, each either `+[values]` (paired with
/// the header keys in order) or `*{object}` (written out in full); every other line
/// is minified JSON copied through. The count is the block's only terminator, so the
/// decoder holds the body to it. All of these return `None`: too few lines; a line
/// that is not a row inside the block; a plain row whose cell count differs from the
/// header's; a row marker on the line after the block; a closer of the wrong kind
/// anywhere in a header, row or cell (`+[1}`); a count that is not written as plain
/// digits (`#+1[…]`, which `usize::from_str` would otherwise accept); a count below
/// 2; and an empty header key list. The last two are not arbitrary tightenings — the
/// encoder cannot emit either. `finish_array` refuses an array with `elem_count < 2`
/// and refuses a dominant shape whose `header_keys.is_empty()`, so `N >= 2` and
/// `H >= 1` hold for every block `emit_table` writes, and a body claiming otherwise
/// did not come from this encoder. Before these four checks the decoder accepted
/// `#1["a"]\n+[1}`, `#+1["a"]\n+[1]`, `#1[]\n+[]` and `#0[]`.
fn expand_e2_body(body: &str) -> Option<String> {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut out = String::with_capacity(body.len() * 2);
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        idx += 1;
        let Some(header) = line.strip_prefix('#') else {
            out.push_str(line);
            continue;
        };
        let bracket = header.find('[')?;
        let digits = &header[..bracket];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let count: usize = digits.parse().ok()?;
        let keys = split_top_level(&header[bracket..])?;
        if count < 2 || keys.is_empty() || idx + count > lines.len() {
            return None;
        }
        out.push('[');
        for r in 0..count {
            if r > 0 {
                out.push(',');
            }
            let row = lines[idx];
            idx += 1;
            if let Some(cells) = row.strip_prefix('+') {
                let vals = split_top_level(cells)?;
                if !cells.starts_with('[') || vals.len() != keys.len() {
                    return None;
                }
                out.push('{');
                for (k, (key, val)) in keys.iter().zip(&vals).enumerate() {
                    if k > 0 {
                        out.push(',');
                    }
                    out.push_str(key);
                    out.push(':');
                    out.push_str(val);
                }
                out.push('}');
            } else {
                let obj = row.strip_prefix('*')?;
                if !obj.starts_with('{') {
                    return None;
                }
                split_top_level(obj)?;
                out.push_str(obj);
            }
        }
        out.push(']');
        if lines.get(idx).is_some_and(|l| l.starts_with(['+', '*'])) {
            return None;
        }
    }
    Some(out)
}

// ===========================================================================
// Generators
// ===========================================================================

/// Inserts `filler` after every structural character outside a string literal, so a
/// compact document becomes a pretty one with the same value tree — the input class
/// the whitespace-stripping encoders exist for.
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

/// How many shapes [`number_lexemes_survive_every_encoder`] draws over. Both that
/// test and [`the_number_lexeme_shapes_reach_every_encoder`] read this one constant,
/// so narrowing the property test's range without touching the coverage test — which
/// is exactly how this test came to run over two encoders while claiming three — no
/// longer type-checks against a coverage table of a different length.
const NUMBER_LEXEME_SHAPES: u8 = 5;

/// The five document shapes [`number_lexemes_survive_every_encoder`] wraps a number
/// lexeme in, shared with the coverage test that pins which encoder each one reaches.
fn number_lexeme_doc(lex: &str, shape: u8) -> String {
    match shape {
        0 => lex.to_string(),
        1 => format!("[{lex}]"),
        2 => format!("{{\"n\":{lex},\"n\":{lex}}}"),
        3 => {
            format!("[{{\"id\":1,\"v\":{lex}}},{{\"id\":2,\"v\":{lex}}},{{\"id\":3,\"v\":{lex}}}]")
        }
        _ => format!("[{}]", vec![lex; 20].join(",")),
    }
}

fn digits(len: std::ops::RangeInclusive<usize>) -> impl Strategy<Value = String> {
    prop::collection::vec(0u8..10, len)
        .prop_map(|v| v.into_iter().map(|d| char::from(b'0' + d)).collect())
}

/// An integer part as the grammar allows it: `0`, or a non-zero digit followed by up
/// to 120 more digits.
fn arb_int_part() -> impl Strategy<Value = String> {
    prop_oneof![
        1 => Just("0".to_string()),
        3 => (1u8..10, digits(0..=6)).prop_map(|(d, rest)| format!("{d}{rest}")),
        1 => (1u8..10, digits(99..=120)).prop_map(|(d, rest)| format!("{d}{rest}")),
    ]
}

/// A valid number lexeme drawn from the whole RFC 8259 number grammar — sign,
/// integer part, optional fraction of up to 120 digits, optional exponent in either
/// case, with or without its sign and with leading zeros in the exponent — plus
/// fixed boundary spellings that no `f64`, `i64` or `u64` holds exactly.
fn arb_number_lexeme() -> impl Strategy<Value = String> {
    let generated = (
        any::<bool>(),
        arb_int_part(),
        prop::option::of(prop_oneof![3 => digits(1..=6), 1 => digits(100..=120)]),
        prop::option::of((
            prop::sample::select(vec!["e", "E"]),
            prop::sample::select(vec!["", "+", "-"]),
            digits(1..=5),
        )),
    )
        .prop_map(|(neg, int, frac, exp)| {
            let mut s = String::new();
            if neg {
                s.push('-');
            }
            s.push_str(&int);
            if let Some(f) = frac {
                s.push('.');
                s.push_str(&f);
            }
            if let Some((e, sign, d)) = exp {
                s.push_str(e);
                s.push_str(sign);
                s.push_str(&d);
            }
            s
        });
    prop_oneof![
        3 => generated,
        1 => prop::sample::select(vec![
            "-0".to_string(),
            "-0.0".to_string(),
            "-0e0".to_string(),
            "-0.0E-0".to_string(),
            "0e0".to_string(),
            "0E+00000".to_string(),
            // One past the i64 and u64 ranges, both signs.
            "9223372036854775808".to_string(),
            "-9223372036854775809".to_string(),
            "18446744073709551616".to_string(),
            "-18446744073709551616".to_string(),
            // 2^53 + 1: the first integer an f64 cannot hold.
            "9007199254740993".to_string(),
            // Below the smallest subnormal (rounds to 0) and past f64::MAX (inf).
            "2.4703282292062327e-324".to_string(),
            "1e-400".to_string(),
            "-1E400".to_string(),
            "1.7976931348623159e308".to_string(),
            // 1 000 digits.
            format!("1{}", "0".repeat(999)),
            format!("0.{}1", "0".repeat(998)),
        ]),
    ]
}

/// One piece of a hostile string body (the text between the quotes, escapes
/// spelled out). Every piece is a valid body on its own, so any concatenation is
/// one too.
fn arb_string_piece() -> impl Strategy<Value = String> {
    prop_oneof![
        // Every control character, escaped with `\u`, in either hex case.
        3 => (0u32..0x20, any::<bool>()).prop_map(|(cp, upper)| if upper {
            format!("\\u{cp:04X}")
        } else {
            format!("\\u{cp:04x}")
        }),
        // Lone, reversed and split surrogate escapes.
        2 => prop::sample::select(vec![
            "\\ud800", "\\uDBFF", "\\udc00", "\\uDFFF",
            "\\udc00\\ud800",       // low before high: two lone surrogates
            "\\ud800x\\udc00",      // a pair split by text
            "\\uD83D\\uDE00",       // a valid pair
            "\\ud83d\\uDE00",       // a valid pair, mixed hex case
        ])
        .prop_map(String::from),
        // Escapes that spell a character with a short form too.
        2 => prop::sample::select(vec![
            "\\u0022", "\\u005c", "\\u005C", "\\u002f", "\\/", "\\\"", "\\\\",
            "\\u0000", "\\u007f", "\\ufeff", "\\uFFFF", "\\u2028", "\\u2029",
            "e\\u0301",
        ])
        .prop_map(String::from),
        // Raw characters JSON allows unescaped but that are easy to mishandle.
        3 => prop::sample::select(vec![
            "\u{7f}", "\u{feff}", "\u{2028}", "\u{2029}", "\u{fffe}", "\u{ffff}",
            "\u{10ffff}", "\u{e9}", "e\u{301}", "\u{d55c}", "\u{1112}\u{1161}\u{11ab}",
            "\u{1f600}",
        ])
        .prop_map(String::from),
        // Text that imitates the rendering's own framing: all three sentinels
        // `sentinel_for` names — this list held two of them, so a string that spells
        // the E1 sentinel was never generated — E2's header and row markers, and a
        // whole escaped table block.
        2 => prop::sample::select(vec![
            "\u{27E6}tkfd:v1:raw\u{27E7}",
            "\u{27E6}tkfd:v1:min\u{27E7}",
            "\u{27E6}tkfd:v1:tbl\u{27E7}",
            "+[", "*{", "#2[", ",:[]{}",
            "\\n#1[\\\"a\\\"]\\n+[1]\\n",
        ])
        .prop_map(String::from),
        2 => "[a-z0-9 ]{0,6}",
    ]
}

/// A hostile string body: 0..=4 pieces.
fn arb_string_body() -> impl Strategy<Value = String> {
    prop::collection::vec(arb_string_piece(), 0..=4).prop_map(|p| p.concat())
}

fn arb_string_lexeme() -> impl Strategy<Value = String> {
    arb_string_body().prop_map(|b| format!("\"{b}\""))
}

/// A value lexeme for an object member: hostile numbers and strings, the literals,
/// and fixed containers that stress E2's cell handling (an array of objects that
/// must stay compact inside a cell, duplicate keys, nesting).
fn arb_value_lexeme() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => arb_number_lexeme(),
        3 => arb_string_lexeme(),
        1 => prop::sample::select(vec!["null", "true", "false"]).prop_map(String::from),
        1 => prop::sample::select(vec![
            "[]", "{}", "[[[[1]]]]",
            r#"{"d":1,"d":2}"#,
            r#"[{"x":1},{"x":2}]"#,
            r#"{"t":[{"a":"\ud800"},{"a":-0}]}"#,
        ])
        .prop_map(String::from),
    ]
}

/// A container kind for the nesting generator. `DupObj` opens an object whose key
/// already occurred once at that level, so a nested document carries a duplicate key
/// at every level it uses that kind.
#[derive(Debug, Clone, Copy)]
enum Opener {
    Arr,
    Obj,
    DupObj,
}

fn arb_opener() -> impl Strategy<Value = Opener> {
    prop::sample::select(vec![Opener::Arr, Opener::Obj, Opener::DupObj])
}

/// Nests `leaf` inside one container per entry of `kinds`, outermost first. The
/// result is exactly `kinds.len()` levels deep.
fn nest(kinds: &[Opener], leaf: &str) -> String {
    let mut s = String::new();
    for k in kinds {
        s.push_str(match k {
            Opener::Arr => "[",
            Opener::Obj => "{\"k\":",
            Opener::DupObj => "{\"k\":0,\"k\":",
        });
    }
    s.push_str(leaf);
    for k in kinds.iter().rev() {
        s.push(match k {
            Opener::Arr => ']',
            Opener::Obj | Opener::DupObj => '}',
        });
    }
    s
}

/// A scalar leaf for the nesting generator (a container would add a level).
fn arb_scalar_lexeme() -> impl Strategy<Value = String> {
    prop_oneof![
        arb_number_lexeme(),
        arb_string_lexeme(),
        Just("null".to_string()),
    ]
}

/// How a row of the near-identical table deviates from the base row.
#[derive(Debug, Clone, Copy)]
enum RowEdit {
    /// The base row, unchanged.
    Same,
    /// The base row with one value replaced — still the header's shape.
    OneValue,
    /// The first two members swapped — a different key order.
    Swap,
    /// One member removed.
    Drop,
    /// One member written a second time — a duplicate key.
    Repeat,
}

fn arb_row_edit() -> impl Strategy<Value = RowEdit> {
    prop_oneof![
        3 => Just(RowEdit::Same),
        3 => Just(RowEdit::OneValue),
        1 => Just(RowEdit::Swap),
        1 => Just(RowEdit::Drop),
        1 => Just(RowEdit::Repeat),
    ]
}

/// An array of 2..=10 objects that start from one base row — 1..=4 hostile keys
/// with hostile values — and each apply one [`RowEdit`], placed bare, under a key,
/// twice in an outer array, or two levels deep, and optionally pretty-printed. Most
/// rows keep the base shape, so the array qualifies for E2 whenever at least two do.
fn arb_table_doc() -> impl Strategy<Value = String> {
    arb_table_doc_with_edits().prop_map(|(doc, _)| doc)
}

/// [`arb_table_doc`] with the [`RowEdit`]s it applied. A rendered document cannot be
/// read back into its edits — a `Drop` on a one-key base row and a `Same` row on a
/// shorter base render alike — so the generator hands them over and
/// [`hostile_generators_reach_every_encoder_and_row_kind`] counts them from here.
/// `arb_table_doc` discarded them, which is why "every row edit is drawn" stood in
/// that test's doc comment unasserted.
fn arb_table_doc_with_edits() -> impl Strategy<Value = (String, Vec<RowEdit>)> {
    (
        prop::collection::vec((arb_string_lexeme(), arb_value_lexeme()), 1..=4),
        prop::collection::vec(
            (
                arb_row_edit(),
                any::<prop::sample::Index>(),
                arb_value_lexeme(),
            ),
            2..=10,
        ),
        0u8..4,
        any::<bool>(),
    )
        .prop_map(|(base, edits, wrap, pretty)| {
            let mut arr = String::from("[");
            for (r, (edit, at, value)) in edits.iter().enumerate() {
                let mut row = base.clone();
                let i = at.index(row.len());
                match edit {
                    RowEdit::Same => {}
                    RowEdit::OneValue => row[i].1.clone_from(value),
                    RowEdit::Swap if row.len() >= 2 => row.swap(0, 1),
                    RowEdit::Swap => row[0].1.clone_from(value),
                    RowEdit::Drop => {
                        row.remove(i);
                    }
                    RowEdit::Repeat => row.push(row[i].clone()),
                }
                if r > 0 {
                    arr.push(',');
                }
                arr.push('{');
                for (m, (k, v)) in row.iter().enumerate() {
                    if m > 0 {
                        arr.push(',');
                    }
                    arr.push_str(k);
                    arr.push(':');
                    arr.push_str(v);
                }
                arr.push('}');
            }
            arr.push(']');
            let doc = match wrap {
                0 => arr,
                1 => format!(r#"{{"results":{arr},"ok":true}}"#),
                2 => format!("[{arr},{arr}]"),
                _ => format!(r#"{{"a":{{"b":[{arr}]}}}}"#),
            };
            let doc = if pretty {
                inject_whitespace(&doc, "\n  ")
            } else {
                doc
            };
            (doc, edits.iter().map(|(edit, _, _)| *edit).collect())
        })
}

/// Small documents that put a hostile number or string lexeme at the top level, in
/// an array, as an object value, as an object key, and inside a two-row table.
fn arb_scalar_doc() -> impl Strategy<Value = String> {
    (
        prop_oneof![arb_number_lexeme(), arb_string_lexeme()],
        arb_string_lexeme(),
        0u8..5,
        any::<bool>(),
    )
        .prop_map(|(v, key, shape, pretty)| {
            let doc = match shape {
                0 => v,
                1 => format!("[{v},{v}]"),
                2 => format!("{{{key}:{v},{key}:{v}}}"),
                3 => format!("{{{key}:[{v}]}}"),
                _ => format!("[{{{key}:{v},\"n\":1}},{{{key}:{v},\"n\":2}}]"),
            };
            if pretty {
                inject_whitespace(&doc, "\n\t")
            } else {
                doc
            }
        })
}

/// Every hostile valid-document generator in this file.
fn arb_hostile_valid() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => arb_table_doc(),
        2 => arb_scalar_doc(),
        1 => (prop::collection::vec(arb_opener(), 0..=24), arb_scalar_lexeme())
            .prop_map(|(kinds, leaf)| nest(&kinds, &leaf)),
    ]
}

// ===========================================================================
// Properties over valid hostile input
// ===========================================================================

proptest! {
    #![proptest_config(config())]

    /// Nesting exactly `max_depth` levels deep — arrays, objects and objects with a
    /// duplicate key, mixed — is accepted and held to the whole contract; one level
    /// more is refused with `DepthExceeded { depth: max_depth + 1, limit: max_depth }`,
    /// however far past the limit the input goes.
    ///
    /// `ConfigBuilder::max_depth` documents "Deeper input yields
    /// `CompressError::DepthExceeded`", so the boundary is: `max_depth` levels are not
    /// deeper. `roundtrip.rs`'s property 7 drives arrays only, and only past the limit.
    #[test]
    fn nesting_at_the_limit_is_accepted_and_past_it_refused(
        md in 0usize..=48,
        kinds in prop::collection::vec(arb_opener(), 56),
        extra in 1usize..=8,
        leaf in arb_scalar_lexeme(),
    ) {
        let c = Compressor::new(Config::builder().max_depth(md).build());
        let at = nest(&kinds[..md], &leaf);
        check_accepted(&c, &at)?;

        let past = nest(&kinds[..md + extra], &leaf);
        match c.compress(past.as_bytes()) {
            Err(CompressError::DepthExceeded { depth, limit }) => {
                prop_assert_eq!(limit, md);
                prop_assert_eq!(depth, md + 1, "depth must name the first level past the limit");
            }
            other => prop_assert!(false, "expected DepthExceeded for {:?}, got {:?}", past, other),
        }
    }

    /// Every number lexeme of the grammar — including ones no `f64`/`i64`/`u64`
    /// holds — survives every encoder byte for byte. The crate docs: "number lexemes
    /// are preserved exactly". Beyond [`check_accepted`] (whose oracle already
    /// compares lexemes byte for byte), each lexeme must appear verbatim in the
    /// rendering body, whichever encoder won.
    ///
    /// The five shapes are what makes "every encoder" true, and each is here for a
    /// different arm of the selection. Shapes 0-2 are passthrough: they are scalars
    /// or two-member objects, and `pretty` does not move them, because the filler
    /// `inject_whitespace` adds to a document that small is worth less than the
    /// `⟦tkfd:v1:min⟧` framing plus the do-no-harm margin. Shape 3 is the array of
    /// three like objects E2 tabularizes. Shape 4 — twenty copies of the lexeme in
    /// one array, always pretty-printed — is the only one E1 wins, and it is here
    /// because without it this test never reached E1 at all: measured over 4 000
    /// draws of each of the first four shapes, E1 was selected 0 times, so the name
    /// of the test was a claim about an encoder its inputs never ran. Shape 4 on a
    /// 121-digit lexeme measures 2 541 bytes in, 2 459 out, encoder `e1-minify`;
    /// [`the_number_lexeme_shapes_reach_every_encoder`] pins that all three appear
    /// rather than leaving it to this comment.
    #[test]
    fn number_lexemes_survive_every_encoder(
        lex in arb_number_lexeme(),
        shape in 0u8..NUMBER_LEXEME_SHAPES,
        pretty in any::<bool>(),
    ) {
        let doc = number_lexeme_doc(&lex, shape);
        // Shape 4 needs its whitespace to beat passthrough, so it is pretty whatever
        // `pretty` says; on the others the flag is free.
        let doc = if pretty || shape == 4 { inject_whitespace(&doc, "\n    ") } else { doc };
        let art = check_accepted(&compressor(), &doc)?;
        let body = art.rendering.split_once('\n').unwrap().1;
        prop_assert!(
            body.contains(lex.as_str()),
            "lexeme {:?} missing from the {:?} body {:?}",
            lex, art.stats.encoder, body
        );
    }

    /// Strings built from control-character escapes, lone/reversed surrogates,
    /// noncharacters, NFC/NFD pairs and text imitating the rendering's own framing are
    /// held to the whole contract as keys and as values, at the top level and inside
    /// a table.
    #[test]
    fn hostile_scalars_hold_the_contract(doc in arb_scalar_doc()) {
        check_accepted(&compressor(), &doc)?;
    }

    /// Arrays of near-identical objects — rows equal to the base, differing in one
    /// value, with two keys swapped, a key dropped or a key repeated — hold the whole
    /// contract; when E2 wins, its body must expand, by the published grammar, to a
    /// document semantically equal to the input.
    #[test]
    fn near_identical_object_arrays_hold_the_contract(doc in arb_table_doc()) {
        check_accepted(&compressor(), &doc)?;
    }

    /// Two compressors built independently from equal configs, and one compressor
    /// shared by two threads, give byte-identical renderings, archives and stats.
    /// `Compressor` documents "one instance is safely shared across threads", and the
    /// crate docs promise the same output "from one build" for the same input and
    /// config — not only from one instance.
    #[test]
    fn compression_is_deterministic_across_instances_and_threads(doc in arb_hostile_valid()) {
        let a = Compressor::new(Config::default());
        let b = Compressor::new(Config::builder().build());
        let ra = a.compress(doc.as_bytes()).unwrap();
        let rb = b.compress(doc.as_bytes()).unwrap();
        let (t1, t2) = std::thread::scope(|s| {
            let h1 = s.spawn(|| a.compress(doc.as_bytes()).unwrap());
            let h2 = s.spawn(|| a.compress(doc.as_bytes()).unwrap());
            (h1.join().unwrap(), h2.join().unwrap())
        });
        for other in [&rb, &t1, &t2] {
            prop_assert_eq!(&other.rendering, &ra.rendering);
            prop_assert_eq!(&other.archive, &ra.archive);
            prop_assert_eq!(format!("{:?}", other.stats), format!("{:?}", ra.stats));
        }
    }
}

// ===========================================================================
// Properties over refused input
// ===========================================================================

/// How a valid number lexeme is broken. Each form is outside the RFC 8259 grammar.
#[derive(Debug, Clone, Copy)]
enum NumberBreak {
    /// A zero in front of the integer part: `01`, `-00.5`.
    LeadingZero,
    /// A `+` sign.
    PlusSign,
    /// A fraction with no integer part: `.5`.
    BareFraction,
    /// A dot with no fraction digits: `1.`, `1e5.`.
    TrailingDot,
    /// An exponent marker with no digits: `1e`, `1E+`.
    EmptyExponent,
    /// A lone minus in front of a non-digit: `-.5`, `-e1`.
    MinusNonDigit,
}

/// Breaks `lex` the way `how` says, using `exp` as the digitless exponent's marker.
/// [`malformed_number_lexemes_name_their_offset_and_rule`] calls this too, so its
/// fixtures are the spellings the property test actually drives rather than a
/// hand-written copy of them.
fn break_number(lex: &str, how: NumberBreak, exp: &str) -> String {
    let unsigned = lex.strip_prefix('-').unwrap_or(lex);
    let bare = || unsigned.replace(['.', 'e', 'E', '+', '-'], "");
    match how {
        NumberBreak::LeadingZero => format!("0{unsigned}"),
        NumberBreak::PlusSign => format!("+{unsigned}"),
        NumberBreak::BareFraction => format!(".{}", bare()),
        NumberBreak::TrailingDot => format!("{lex}."),
        NumberBreak::EmptyExponent => format!("{lex}{exp}"),
        NumberBreak::MinusNonDigit => format!("-.{}", bare()),
    }
}

/// How many shapes [`malformed_number_lexemes_are_refused`] wraps a broken lexeme in.
/// Both that test and [`malformed_number_lexemes_name_their_offset_and_rule`] read this
/// constant, so neither can narrow its range without the other following.
const NUMBER_BREAK_SHAPES: u8 = 3;

/// The document shapes [`malformed_number_lexemes_are_refused`] wraps a broken lexeme
/// in, with the byte the lexeme starts at in each — the prefix that turns an offset
/// inside the lexeme into an offset into the document.
fn number_break_doc(broken: &str, shape: u8) -> (String, usize) {
    match shape {
        0 => (broken.to_string(), 0),
        1 => (format!("[1,{broken}]"), 3),
        _ => (format!("[{{\"v\":1}},{{\"v\":{broken}}}]"), 14),
    }
}

proptest! {
    #![proptest_config(config())]

    /// Malformed number lexemes, derived from valid ones, are refused as
    /// `InvalidJson` wherever they sit — core "never repairs input" (the
    /// `CompressError::InvalidJson` docs). Only the variant is asserted here: the
    /// offset and the message a break reports are pinned on named fixtures by
    /// `malformed_number_lexemes_name_their_offset_and_rule`, because over generated
    /// lexemes a break does not have one message — `1e5` with `EmptyExponent` and `e`
    /// appended is a parsed number followed by trailing input, not a digitless
    /// exponent.
    #[test]
    fn malformed_number_lexemes_are_refused(
        lex in arb_number_lexeme(),
        how in prop::sample::select(vec![
            NumberBreak::LeadingZero, NumberBreak::PlusSign, NumberBreak::BareFraction,
            NumberBreak::TrailingDot, NumberBreak::EmptyExponent, NumberBreak::MinusNonDigit,
        ]),
        exp in prop::sample::select(vec!["e", "E", "e+", "E-"]),
        shape in 0u8..NUMBER_BREAK_SHAPES,
    ) {
        let broken = break_number(&lex, how, exp);
        let (doc, _) = number_break_doc(&broken, shape);
        match compressor().compress(doc.as_bytes()) {
            Err(CompressError::InvalidJson { .. }) => {}
            other => prop_assert!(false, "expected InvalidJson for {:?}, got {:?}", doc, other),
        }
    }

    /// A raw control byte (`0x00..=0x1F`) inside a string literal is refused as
    /// `InvalidJson` at the byte's own offset — RFC 8259 requires those characters
    /// escaped, and `InvalidJson::byte_offset` is documented as the "byte offset into
    /// the input where parsing failed". A backslash in front of it does not rescue the
    /// byte, but it does change the rule that refuses it: the control byte is then read
    /// as the escape's character, so the message is "invalid escape character in string"
    /// instead of "unescaped control character in string". The offset names the control
    /// byte either way. Both messages are asserted, because a message is a claim apart
    /// from the offset behind it and this test read only the offset; the two were
    /// measured on `0x00`, `0x09`, `0x0A`, `0x0D` and `0x1F`, each with and without the
    /// backslash.
    #[test]
    fn raw_control_bytes_in_strings_are_refused_at_their_offset(
        doc in arb_table_doc(),
        ctrl in 0u8..0x20,
        backslash in any::<bool>(),
        at in any::<prop::sample::Index>(),
    ) {
        // Candidate splice points: just after an opening quote of a string, i.e.
        // any quote outside a string. The prefix up to it is then valid so far.
        let bytes = doc.as_bytes();
        let mut opens = Vec::new();
        let mut in_str = false;
        let mut esc = false;
        for (i, &b) in bytes.iter().enumerate() {
            if in_str {
                if esc { esc = false; } else if b == b'\\' { esc = true; } else if b == b'"' { in_str = false; }
            } else if b == b'"' {
                in_str = true;
                opens.push(i + 1);
            }
        }
        prop_assume!(!opens.is_empty());
        let pos = opens[at.index(opens.len())];
        let mut spliced = bytes[..pos].to_vec();
        if backslash {
            spliced.push(b'\\');
        }
        let ctrl_at = spliced.len();
        spliced.push(ctrl);
        spliced.extend_from_slice(&bytes[pos..]);
        let want_msg = if backslash {
            "invalid escape character in string"
        } else {
            "unescaped control character in string"
        };
        match compressor().compress(&spliced) {
            Err(CompressError::InvalidJson { byte_offset, msg }) => {
                prop_assert_eq!(byte_offset, ctrl_at, "offset does not name the control byte");
                prop_assert_eq!(msg.as_str(), want_msg, "wrong rule for the control byte");
            }
            other => prop_assert!(false, "expected InvalidJson at {}, got {:?}", ctrl_at, other),
        }
    }

    /// A byte that can never begin a UTF-8 sequence (a continuation byte, `0xC0`,
    /// `0xC1`, or `0xF5..=0xFF`), inserted at any character boundary of a valid
    /// document, is refused as `NotUtf8` at exactly that offset — the variant documents
    /// its offset as `Utf8Error::valid_up_to`, "so everything before it did decode".
    #[test]
    fn invalid_utf8_is_refused_at_the_inserted_byte(
        doc in arb_hostile_valid(),
        bad in prop_oneof![0x80u8..=0xBF, 0xC0u8..=0xC1, 0xF5u8..=0xFF],
        at in any::<prop::sample::Index>(),
    ) {
        let boundaries: Vec<usize> = doc
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(doc.len()))
            .collect();
        let pos = boundaries[at.index(boundaries.len())];
        let mut bytes = doc.as_bytes()[..pos].to_vec();
        bytes.push(bad);
        bytes.extend_from_slice(&doc.as_bytes()[pos..]);
        match compressor().compress(&bytes) {
            Err(CompressError::NotUtf8 { byte_offset }) => prop_assert_eq!(byte_offset, pos),
            other => prop_assert!(false, "expected NotUtf8 at {}, got {:?}", pos, other),
        }
    }

    /// Every prefix of a hostile document **of at most 256 bytes** is either refused
    /// as `InvalidJson` / `NotUtf8` (a cut inside a multi-byte character) or — when
    /// the prefix is itself valid JSON, as `12` is of `123` — accepted and held to
    /// the whole contract. The size bound is not decoration: this loop compresses
    /// once per byte, so it is quadratic in the document, and `arb_hostile_valid`
    /// draws documents well past 256 bytes that the `prop_filter` below discards
    /// outright. Prefixes of a longer hostile document are therefore *not* covered
    /// here. Truncation is the commonest corruption of a captured tool output;
    /// `roundtrip.rs` cuts only at the midpoint.
    #[test]
    fn every_prefix_is_refused_or_holds_the_contract(
        doc in arb_hostile_valid().prop_filter("<= 256 bytes", |d| d.len() <= 256),
    ) {
        let c = compressor();
        let bytes = doc.as_bytes();
        for cut in 0..bytes.len() {
            let prefix = &bytes[..cut];
            match c.compress(prefix) {
                Ok(_) => {
                    check_accepted(&c, std::str::from_utf8(prefix).unwrap())?;
                }
                Err(CompressError::InvalidJson { byte_offset, .. }) => {
                    prop_assert!(byte_offset <= cut, "offset {} past a {}-byte prefix", byte_offset, cut);
                    // The offset is a value, not just a bound, and the two cases below
                    // are exhaustive. Measured over 800 draws under
                    // `TestRunner::deterministic()`: every cut whose refusal is *not*
                    // reported at `cut` is a cut inside one of the three JSON literals,
                    // reported at that literal's first byte — gaps of 1..=3 bytes for
                    // `null` (114 draws each) and `true` (49 each) and 1..=4 for `false`
                    // (17 each), and no other first byte ever occurred. A cut inside a
                    // number or a string reports at `cut`, because the tape commits to
                    // those lexemes byte by byte, while `true`/`false`/`null` are matched
                    // whole; that asymmetry is the subject of
                    // [`broken_literals_name_the_literal_only_once_the_input_commits_to_it`].
                    if byte_offset < cut {
                        let tail = &bytes[byte_offset..cut];
                        prop_assert!(
                            [&b"true"[..], &b"false"[..], &b"null"[..]]
                                .iter()
                                .any(|lit| lit.starts_with(tail)),
                            "offset {} is {} bytes before the cut and {:?} is no cut-short literal",
                            byte_offset,
                            cut - byte_offset,
                            tail
                        );
                    }
                }
                Err(CompressError::NotUtf8 { byte_offset }) => {
                    prop_assert!(!doc.is_char_boundary(cut), "NotUtf8 on a char boundary cut");
                    prop_assert!(doc.is_char_boundary(byte_offset) && byte_offset < cut);
                }
                Err(other) => prop_assert!(false, "unexpected {:?} for prefix {:?}", other, prefix),
            }
        }
    }

    /// The size guard runs "before any work is done" (the `DEFAULT_MAX_INPUT_BYTES`
    /// docs): any input one byte or more over a configured limit is `InputTooLarge`
    /// with its real size and that limit, whatever its bytes are — invalid UTF-8
    /// included — while a valid document of exactly the limit is accepted.
    ///
    /// "Whatever its bytes are" is a claim about every input, and this test reaches two
    /// kinds: over 4 000 draws under `TestRunner::deterministic()` the appended garbage
    /// left 3 917 inputs invalid UTF-8 and the other 83 valid UTF-8 that is not valid
    /// JSON. It reaches no input the depth check would refuse, because
    /// `arb_hostile_valid` nests at most 24 levels against a limit of 512 — that
    /// precedence is pinned by `the_size_guard_precedes_the_depth_check`.
    #[test]
    fn the_size_guard_precedes_every_other_check(
        doc in arb_hostile_valid(),
        garbage in prop::collection::vec(any::<u8>(), 1..64),
    ) {
        let len = doc.len();
        let at = Compressor::new(Config::builder().max_input_bytes(len).build());
        check_accepted(&at, &doc)?;

        let under = Compressor::new(Config::builder().max_input_bytes(len - 1).build());
        let mut over = doc.as_bytes().to_vec();
        over.extend_from_slice(&garbage);
        for (input, size) in [(doc.as_bytes(), len), (over.as_slice(), len + garbage.len())] {
            match under.compress(input) {
                Err(CompressError::InputTooLarge { size: s, limit }) => {
                    prop_assert_eq!(s, size);
                    prop_assert_eq!(limit, len - 1);
                }
                other => prop_assert!(false, "expected InputTooLarge, got {:?}", other),
            }
        }
    }
}

// ===========================================================================
// Concrete boundaries at the default limits
// ===========================================================================

/// At the default `max_depth` of 512, a document mixing all three container kinds
/// 512 levels deep is accepted and holds the contract, 513 levels is refused as
/// `DepthExceeded { depth: 513, limit: 512 }`, and a million unclosed object openers
/// are refused the same way rather than overflowing the stack (`DepthExceeded`
/// documents "the parser is iterative and cannot overflow the stack";
/// `roundtrip.rs` drives that with arrays only).
#[test]
fn the_default_depth_limit_holds_for_every_container_kind() {
    let c = compressor();
    let kinds: Vec<Opener> = [Opener::Arr, Opener::Obj, Opener::DupObj]
        .into_iter()
        .cycle()
        .take(DEFAULT_MAX_DEPTH + 1)
        .collect();

    let at = nest(&kinds[..DEFAULT_MAX_DEPTH], "\"\\ud800\"");
    check_accepted(&c, &at).unwrap();

    let past = nest(&kinds, "1");
    match c.compress(past.as_bytes()) {
        Err(CompressError::DepthExceeded { depth, limit }) => {
            assert_eq!((depth, limit), (DEFAULT_MAX_DEPTH + 1, DEFAULT_MAX_DEPTH));
        }
        other => panic!("expected DepthExceeded, got {other:?}"),
    }

    let unclosed = "{\"a\":".repeat(1_000_000);
    match c.compress(unclosed.as_bytes()) {
        Err(CompressError::DepthExceeded { depth, limit }) => {
            assert_eq!((depth, limit), (DEFAULT_MAX_DEPTH + 1, DEFAULT_MAX_DEPTH));
        }
        other => panic!("expected DepthExceeded, got {other:?}"),
    }
}

/// One byte over the default 16 MiB cap is refused as `InputTooLarge` with the
/// documented default as its limit, even when every byte is invalid UTF-8 (the guard
/// runs first).
#[test]
fn one_byte_over_the_default_cap_is_refused_first() {
    let input = vec![0xFFu8; DEFAULT_MAX_INPUT_BYTES + 1];
    match compressor().compress(&input) {
        Err(CompressError::InputTooLarge { size, limit }) => {
            assert_eq!(
                (size, limit),
                (DEFAULT_MAX_INPUT_BYTES + 1, DEFAULT_MAX_INPUT_BYTES)
            );
        }
        other => panic!("expected InputTooLarge, got {other:?}"),
    }
}

/// [`malformed_number_lexemes_are_refused`] asserts only the error variant, so the
/// offset and the rule each [`NumberBreak`] reports are pinned here, on the narrowest
/// fixture per kind — `break_number("1", how, "e")`, which spells `01`, `+1`, `.1`,
/// `1.`, `1e` and `-.1`. The offset is the prefix [`number_break_doc`] returns plus a
/// per-kind offset inside the lexeme, so one column of six numbers covers all three
/// shapes instead of eighteen literals.
#[test]
fn malformed_number_lexemes_name_their_offset_and_rule() {
    // (kind, offset within the broken lexeme, the rule serde_json names)
    let cases: [(NumberBreak, usize, &str); 6] = [
        (
            NumberBreak::LeadingZero,
            0,
            "leading zeros are not allowed in numbers",
        ),
        (NumberBreak::PlusSign, 0, "expected a JSON value"),
        (NumberBreak::BareFraction, 0, "expected a JSON value"),
        (
            NumberBreak::TrailingDot,
            2,
            "invalid number: expected a digit after '.'",
        ),
        (
            NumberBreak::EmptyExponent,
            2,
            "invalid number: expected a digit in exponent",
        ),
        (
            NumberBreak::MinusNonDigit,
            1,
            "invalid number: expected a digit",
        ),
    ];
    let c = compressor();
    for (how, inner, want) in cases {
        let broken = break_number("1", how, "e");
        for shape in 0..NUMBER_BREAK_SHAPES {
            let (doc, prefix) = number_break_doc(&broken, shape);
            match c.compress(doc.as_bytes()) {
                Err(CompressError::InvalidJson { byte_offset, msg }) => {
                    assert_eq!(byte_offset, prefix + inner, "offset for {doc:?}");
                    assert_eq!(msg, want, "rule for {doc:?}");
                }
                other => panic!("expected InvalidJson for {doc:?}, got {other:?}"),
            }
        }
    }
}

/// The size guard precedes the depth check as well, which
/// [`the_size_guard_precedes_every_other_check`] cannot reach: its generator nests at
/// most 24 levels against a limit of 512. A document 600 levels deep is
/// `DepthExceeded { depth: 513, limit: 512 }` under the default cap, which is wide
/// enough to hold it, and `InputTooLarge` under a cap one byte short of it — so for an
/// input the parser would refuse for its shape, the guard still answers first. A
/// thousand unclosed openers are neither valid JSON nor within the depth limit, and the
/// guard answers first there too.
#[test]
fn the_size_guard_precedes_the_depth_check() {
    let deep = format!("{}1{}", "[".repeat(600), "]".repeat(600));
    match compressor().compress(deep.as_bytes()) {
        Err(CompressError::DepthExceeded { depth, limit }) => {
            assert_eq!((depth, limit), (513, 512));
        }
        other => panic!("expected DepthExceeded, got {other:?}"),
    }
    let unclosed = "[".repeat(1000);
    for input in [deep.as_str(), unclosed.as_str()] {
        let cap = input.len() - 1;
        let c = Compressor::new(Config::builder().max_input_bytes(cap).build());
        match c.compress(input.as_bytes()) {
            Err(CompressError::InputTooLarge { size, limit }) => {
                assert_eq!((size, limit), (input.len(), cap));
            }
            other => panic!("expected InputTooLarge, got {other:?}"),
        }
    }
}

/// Builds a JSON array of near-identical objects of exactly `len` bytes: rows
/// `{"id":<n>,"s":"<pad>"}` with the last row's padding sized to hit `len`.
fn table_of_exact_len(len: usize) -> String {
    let mut s = String::with_capacity(len);
    s.push('[');
    let mut n = 0usize;
    loop {
        let row = format!("{{\"id\":{n},\"s\":\"x\"}}");
        // Room for this row, a comma, one more row of at least this size, and `]`.
        if s.len() + row.len() + 1 + row.len() + 16 > len {
            break;
        }
        s.push_str(&row);
        s.push(',');
        n += 1;
    }
    let head = format!("{{\"id\":{n},\"s\":\"");
    let pad = len - s.len() - head.len() - "\"}]".len();
    s.push_str(&head);
    s.push_str(&"x".repeat(pad));
    s.push_str("\"}]");
    assert_eq!(s.len(), len);
    s
}

/// A 16 MiB array of near-identical objects — exactly the default cap — is accepted
/// and holds the whole contract, E2 expansion included.
///
/// It carried `#[ignore = "slow"]` until it was timed, and in this repo an ignored
/// test is an absent one: no gate leg passes `--ignored`, so the only case that
/// reaches the default cap exactly never ran. Re-timed 2026-10-02 (macOS 27.0.0
/// arm64, stable 1.97, 10 cores, warm target directory, two runs per figure; one
/// 16 MiB document through every encoder and the oracle). Run on its own — `cargo
/// test -p tokfold-core --all-features --test hostile <this name> -- --exact` — it
/// takes 10.64 s in a debug build and 1.03 s under `--release`; the first run of
/// either profile is inflated (12.09 s and 3.94 s), so the second is the steady
/// state. Those are single-target wall clocks, which is not what it costs a gate
/// leg. Its marginal cost inside the parallel workspace leg is the difference
/// against `--skip <this name>`: 10.11–17.42 s in debug (leg 26.64–33.19 s, leg
/// minus it 15.77–16.53 s) and 0.91–1.14 s under `--release` (leg 2.85–3.16 s,
/// minus it 1.94–2.02 s). The debug spread is the leg's own run-to-run noise,
/// wider than the gap between its two figures. `--report-time` would answer this
/// directly but is nightly-only on a stable toolchain, so the difference is the
/// measurement. Both legs run it now.
/// Re-time it before adding a second document of this size — the cost is linear in
/// the document — and if it ever has to be skipped again, skip it behind a `cfg` a
/// gate leg still exercises, not behind `#[ignore]`.
#[test]
fn a_table_at_exactly_the_default_cap_holds_the_contract() {
    let doc = table_of_exact_len(DEFAULT_MAX_INPUT_BYTES);
    let art = check_accepted(&compressor(), &doc).unwrap();
    assert_eq!(art.stats.bytes_before, DEFAULT_MAX_INPUT_BYTES);
    // "E2 expansion included" is a claim about which encoder wins at this size, and the
    // E2 arm is the only one of `check_accepted`'s three that expands a table. Without
    // this the test stayed green if selection fell back to passthrough and the expansion
    // it is named for stopped running altogether.
    assert_eq!(art.stats.encoder, EncoderId::E2_TABULAR);
    assert!(art.stats.bytes_after < art.stats.bytes_before);
}

// ===========================================================================
// Generator liveness
// ===========================================================================

/// The properties above are only as strong as the regions their generators reach.
/// With a fixed seed, this asserts that the hostile generators put every encoder in
/// play, that E2 bodies carry both plain and deviating rows and hostile (escaped)
/// header keys, that each of the three sentinels `sentinel_for` names is spelled by
/// some drawn input, and that each of the five [`RowEdit`] variants is drawn.
///
/// The encoder and row-kind counts come from [`arb_hostile_valid`], the strategy the
/// properties above draw from. The edit counts come from [`arb_table_doc_with_edits`]
/// directly, because a table document no longer says which edits made it. Both loops
/// share one deterministic runner and the edit loop runs second, so it cannot move the
/// counts above it.
#[test]
fn hostile_generators_reach_every_encoder_and_row_kind() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    const DRAWS: usize = 600;
    let mut runner = TestRunner::deterministic();
    let c = compressor();

    let (mut raw, mut min, mut tbl) = (0usize, 0usize, 0usize);
    let (mut plain_rows, mut deviating_rows, mut escaped_header_keys) = (0usize, 0usize, 0usize);
    // One counter per sentinel `sentinel_for` names, over the *input*: the framing pool in
    // `arb_string_piece` is the only place these strings are generated, so a zero here is
    // the pool having lost a member. Measured in place over these `DRAWS`: raw 38,
    // min 40, tbl 33 of 600 drawn documents spell one, so none of the
    // three is a near miss.
    let mut sentinels_spelled = [0usize; 3];
    for _ in 0..DRAWS {
        let doc = arb_hostile_valid().new_tree(&mut runner).unwrap().current();
        for (i, kind) in [
            EncoderId::PASSTHROUGH,
            EncoderId::E1_MINIFY,
            EncoderId::E2_TABULAR,
        ]
        .into_iter()
        .enumerate()
        {
            if doc.contains(sentinel_for(kind).unwrap()) {
                sentinels_spelled[i] += 1;
            }
        }
        let art = c.compress(doc.as_bytes()).unwrap();
        match art.stats.encoder {
            EncoderId::PASSTHROUGH => raw += 1,
            EncoderId::E1_MINIFY => min += 1,
            EncoderId::E2_TABULAR => {
                tbl += 1;
                let body = art.rendering.split_once('\n').unwrap().1;
                for line in body.split('\n') {
                    if line.starts_with('+') {
                        plain_rows += 1;
                    } else if line.starts_with('*') {
                        deviating_rows += 1;
                    } else if line.starts_with('#') && line.contains('\\') {
                        escaped_header_keys += 1;
                    }
                }
            }
            _ => {}
        }
    }
    for (name, n) in [
        ("passthrough", raw),
        ("E1", min),
        ("E2", tbl),
        ("plain rows", plain_rows),
        ("deviating rows", deviating_rows),
        ("escaped header keys", escaped_header_keys),
        ("inputs spelling the raw sentinel", sentinels_spelled[0]),
        ("inputs spelling the min sentinel", sentinels_spelled[1]),
        ("inputs spelling the tbl sentinel", sentinels_spelled[2]),
    ] {
        assert!(n > 0, "no {name} in {DRAWS} hostile draws");
    }

    // `arb_row_edit` weights the five variants 3/3/1/1/1, so the three rare ones are a
    // ninth of the rows each; `TABLE_DRAWS` draws of 2..=10 rows are thousands of rows,
    // so this is not a coin flip. Measured over 3 000 draws of `arb_row_edit` alone the
    // counts were Same 1 019, OneValue 999, Swap 352, Drop 311, Repeat 319 — against a
    // ninth of 3 000 being 333.
    const TABLE_DRAWS: usize = 600;
    let mut edits_seen = [0usize; 5];
    for _ in 0..TABLE_DRAWS {
        let (_, edits) = arb_table_doc_with_edits()
            .new_tree(&mut runner)
            .unwrap()
            .current();
        for edit in edits {
            edits_seen[match edit {
                RowEdit::Same => 0,
                RowEdit::OneValue => 1,
                RowEdit::Swap => 2,
                RowEdit::Drop => 3,
                RowEdit::Repeat => 4,
            }] += 1;
        }
    }
    for (name, n) in ["Same", "OneValue", "Swap", "Drop", "Repeat"]
        .into_iter()
        .zip(edits_seen)
    {
        assert!(
            n > 0,
            "RowEdit::{name} never drawn in {TABLE_DRAWS} table draws"
        );
    }
}

/// [`number_lexemes_survive_every_encoder`] is named after a claim about every
/// encoder, so which encoder each of its shapes reaches is pinned here rather than
/// assumed. Shapes 0-2 must reach passthrough, shape 3 E2 and shape 4 E1; the test
/// asserts each shape lands where its doc says, which fails both if a shape stops
/// reaching its encoder and if the selection changes under it.
#[test]
fn the_number_lexeme_shapes_reach_every_encoder() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    const DRAWS: usize = 200;
    let mut runner = TestRunner::deterministic();
    let c = compressor();

    let expected: [(u8, EncoderId); NUMBER_LEXEME_SHAPES as usize] = [
        (0, EncoderId::PASSTHROUGH),
        (1, EncoderId::PASSTHROUGH),
        (2, EncoderId::PASSTHROUGH),
        (3, EncoderId::E2_TABULAR),
        (4, EncoderId::E1_MINIFY),
    ];
    // The drawn lexemes reach the widest fixtures only by chance — `arb_number_lexeme`
    // picks its fixture arm one draw in four and then one of sixteen — so the ends of
    // the width range are named outright. The narrowest lexeme the grammar has is one
    // byte; the widest the generator can produce is a 1 000-digit fixture, and the
    // widest it *generates* is a 121-digit integer part with a 120-digit fraction and
    // a 5-digit exponent. Shape 4 stacks twenty copies of whichever it gets, so these
    // also pin that the E1 expectation holds from a 141-byte document up to a 20 KiB
    // one rather than only in the middle of the range.
    let edges = [
        "0".to_string(),
        "-0".to_string(),
        "1e-400".to_string(),
        format!("1{}", "0".repeat(999)),
        format!("0.{}1", "0".repeat(998)),
        format!("9{}.{}e-99999", "8".repeat(120), "7".repeat(120)),
    ];
    let drawn = (0..DRAWS)
        .map(|_| arb_number_lexeme().new_tree(&mut runner).unwrap().current())
        .chain(edges);
    for lex in drawn {
        for (shape, want) in expected {
            let doc = number_lexeme_doc(&lex, shape);
            let doc = if shape == 4 {
                inject_whitespace(&doc, "\n    ")
            } else {
                doc
            };
            let art = c.compress(doc.as_bytes()).unwrap();
            assert_eq!(
                art.stats.encoder, want,
                "shape {shape} on lexeme {lex:?} selected {:?}",
                art.stats.encoder
            );
        }
    }
}

/// The E2 decoder must reject what the grammar forbids, or the E2 arm of
/// [`check_accepted`] would accept a malformed body.
#[test]
fn the_e2_decoder_rejects_bodies_that_break_the_grammar() {
    let good = "\n#2[\"a\"]\n+[1]\n*{\"b\":2}\n";
    assert_eq!(
        expand_e2_body(good).as_deref(),
        Some("[{\"a\":1},{\"b\":2}]")
    );
    for bad in [
        "\n#3[\"a\"]\n+[1]\n+[2]\n",      // fewer rows than declared
        "\n#1[\"a\"]\n+[1]\n+[2]\n",      // an undeclared row follows
        "\n#2[\"a\"]\n+[1]\n{\"b\":2}\n", // a line in the block without a marker
        "\n#1[\"a\",\"b\"]\n+[1]\n",      // a plain row short of cells
        "\n#1[\"a\"]\n+[1]]\n",           // a row whose bracket closes early
        "\n#x[\"a\"]\n+[1]\n",            // a count that is not a number
        // Four bodies this decoder accepted until it was made to match the grammar
        // the `e2_tabular` rustdoc publishes. The first three round-tripped to
        // `[{"a":1}]`, `[{"a":1}]` and `[{}]`; the fourth to `[]`.
        "\n#1[\"a\"]\n+[1}\n",               // a closer of the wrong kind
        "\n#+1[\"a\"]\n+[1]\n",              // a count with a leading `+`
        "\n#1[]\n+[]\n",                     // an empty header key list
        "\n#0[]\n",                          // a table declaring no rows
        "\n#2[\"a\"]\n+[{\"b\":1]}\n+[2]\n", // a mismatched closer inside a cell
        "\n#2[\"a\"]\n*{\"b\":1]\n+[2]\n",   // a mismatched closer in a deviating row
    ] {
        assert_eq!(expand_e2_body(bad), None, "accepted {bad:?}");
    }
    // `N >= 2` and `H >= 1` are the encoder's own bounds, not this decoder's taste:
    // a two-row, one-key block is the narrowest the grammar allows, and it must pass.
    assert_eq!(
        expand_e2_body("\n#2[\"a\"]\n+[1]\n+[2]\n").as_deref(),
        Some("[{\"a\":1},{\"a\":2}]")
    );
}

/// A literal arm of `State::Value` dispatches on the first byte alone, so a single `n`
/// is enough to send the parser down the `null` arm. `tape::committed_to_literal` then
/// decides *which message* the refusal carries: the literal is named once the input has
/// committed to it -- matched past the dispatch byte, or ran out inside it -- and
/// otherwise the input gets the same "expected a JSON value" any other stray byte in
/// value position gets. Nothing pinned that choice. `tape.rs`'s own `assert_invalid` and
/// `assert_invalid_at` assert the error *variant* and the byte offset, never the message,
/// so `rejects_same_length_wrong_literals` stays green whichever message each of its
/// inputs gets.
///
/// The table walks the matched prefix length, which is what the decision reads: one byte
/// matched and a mismatch after it is a stray byte, two or three are a commitment, and
/// running out of input is a commitment at every length. Each literal is driven at offset
/// 0 and again inside an array, so a mutation that reads the wrong index cannot pass by
/// reading byte 0 and finding the dispatch byte there.
///
/// What these inputs reach: the `n`/`t`/`f` arms of `State::Value` and the message
/// `literal_error` picks between. They do not reach a *successful* `lit_matches` (every
/// input here fails it), and they claim nothing about any other `InvalidJson` message.
#[test]
fn broken_literals_name_the_literal_only_once_the_input_commits_to_it() {
    const GENERIC: &str = "expected a JSON value";
    const NULL: &str = "invalid literal, expected 'null'";
    const TRUE: &str = "invalid literal, expected 'true'";
    const FALSE: &str = "invalid literal, expected 'false'";
    // (document, byte offset of the literal, the message that offset carries)
    let cases: [(&str, usize, &str); 18] = [
        // One byte matched, then a byte that is not the literal's second: a stray `n`.
        ("nx", 0, GENERIC),
        ("[nx]", 1, GENERIC),
        // Two and three bytes matched: an attempt at `null`, named as one.
        ("nux", 0, NULL),
        ("[nux]", 1, NULL),
        ("nulx", 0, NULL),
        // The input ended inside the literal -- a commitment at every prefix length,
        // including the dispatch byte alone.
        ("n", 0, NULL),
        ("nu", 0, NULL),
        ("nul", 0, NULL),
        ("[n", 1, NULL),
        // `true`, whose arm passes its own literal and its own message.
        ("tx", 0, GENERIC),
        ("[tx]", 1, GENERIC),
        ("trx", 0, TRUE),
        ("t", 0, TRUE),
        ("[tru", 1, TRUE),
        // `false`, the one literal longer than four bytes.
        ("fx", 0, GENERIC),
        ("fax", 0, FALSE),
        ("f", 0, FALSE),
        ("[fals", 1, FALSE),
    ];
    let c = compressor();
    for (doc, offset, want) in cases {
        match c.compress(doc.as_bytes()) {
            Err(CompressError::InvalidJson { byte_offset, msg }) => {
                assert_eq!(byte_offset, offset, "offset for {doc:?}");
                assert_eq!(msg, want, "message for {doc:?}");
            }
            other => panic!("expected InvalidJson for {doc:?}, got {other:?}"),
        }
    }
}

/// A valid archive of one document is accepted in place of another's: substitution
/// needs no second preimage, because the digest is inside the archive it covers.
///
/// `fuzz/fuzz_targets/decompress.rs` asserted the opposite until 2026-09-30. It
/// overwrites an archive `compress` wrote with bytes from the fuzzer and demanded that
/// an accepted result be the original document, on the written reasoning that "every
/// byte of an archive is load-bearing, so a byte-different archive is accepted only by
/// a SHA-256 second preimage". That reasoning bounds *corruption of one archive*; it
/// says nothing about handing over a different, internally consistent one. `{}` and
/// `[]` are both two bytes, so their archives are both 45 bytes and one replaces the
/// other whole — a 48-byte fuzz input away from a crash that would have reported a
/// crate bug that does not exist.
///
/// What the format does force is the other direction, pinned here as well: the archive
/// that decompresses to a given document is unique. The payload fixes `original_len`
/// and the digest, the three metadata fields are compared against fixed values, and
/// `read_uleb128` rejects an overlong length, so no byte is left free to vary.
#[test]
fn a_valid_archive_of_another_document_is_accepted() {
    let c = compressor();
    let curly = c.compress(b"{}").unwrap().archive;
    let square = c.compress(b"[]").unwrap().archive;

    // The precondition the fuzz target's whole-archive overwrite needs.
    assert_eq!(
        curly.len(),
        square.len(),
        "two two-byte documents must give equal-length archives"
    );
    assert_eq!(
        curly.len(),
        45,
        "the archive length quoted in the fuzz target"
    );
    assert_ne!(curly, square);

    // Substitution is accepted, and returns the substituted document, not the original.
    assert_eq!(c.decompress(&square).unwrap().as_slice(), b"[]");

    // Uniqueness: re-compressing is byte-identical, and no single-byte change to the
    // archive of `{}` can be accepted and still hand `{}` back.
    assert_eq!(c.compress(b"{}").unwrap().archive, curly);
    let mut accepted_edits = 0_usize;
    for i in 0..curly.len() {
        for byte in 0_u16..256 {
            let byte = byte as u8;
            if curly[i] == byte {
                continue;
            }
            let mut edited = curly.clone();
            edited[i] = byte;
            if let Ok(out) = c.decompress(&edited) {
                accepted_edits += 1;
                assert_ne!(
                    out.as_slice(),
                    b"{}".as_slice(),
                    "an archive differing at byte {i} (set to {byte:#04x}) returned the original document"
                );
            }
        }
    }
    assert_eq!(
        accepted_edits, 0,
        "no single-byte edit of this archive is accepted at all"
    );
}
