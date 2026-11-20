//! E3 — `refs`: a value dictionary in front of the [E5](super::e5_grid) grid.
//!
//! # What it does
//!
//! A string value that repeats — in the measured workload, a `ServiceNow` `sys_id` that
//! many records point to — is written once, in a legend, and every occurrence is
//! replaced by a short reference `"$i"` to entry `i` of that legend. The document
//! with the references in it is then rendered by E5. The rendering is:
//!
//! ```text
//! #tkfd1 refs\n
//! #refs "$N" = entry N (from 0) of:\n
//! $=[<lexeme_0>,<lexeme_1>,…]\n
//! <E5 body of the substituted document>
//! ```
//!
//! The second line explains the legend in the document itself, and the legend comes
//! before the data. Both were measured: without the explanation a small model reads
//! `"$3"` as the value (Haiku answered 30.4 % of the questions that need a legend
//! value, against 99.5 % on the plain grid), and with it 74.9 %, while a larger model
//! stays at parity in both forms. That is why E3 ships only in
//! [`Profile::Aggressive`](crate::Profile::Aggressive). The measured files were tagged
//! `grid`; this encoder's own tag, `refs`, is the one difference from them.
//!
//! # What is replaced
//!
//! Only string lexemes in **value position** — never a key, a number, a literal or a
//! container. The unit is the lexeme exactly as written, quotes and escapes included,
//! so two spellings of the same string (`"a"` and `"\u0061"`) are two values. A lexeme
//! that holds a [`never_compress`](crate::never_compress) literal is never replaced: it
//! stays verbatim at its own position, which is the contract that module states for
//! "folded into a legend". It is a fidelity safeguard and nothing else.
//!
//! # References and the escape
//!
//! A reference is the string lexeme `"$i"`: a quote, one `$`, the index in decimal
//! with no leading zero, a quote. A value lexeme that already has the shape `"$…$<d>"`
//! — a quote, one or more `$`, one or more ASCII digits, a quote, nothing else — gets
//! one more `$` after its opening quote, so it can never be read as a reference:
//! `"$5"` is written `"$$5"`, and `"$$5"` is written `"$$$5"`. Reading the rendering
//! back, a value `"$i"` is legend entry `i`, a value of the escaped shape loses one
//! `$`, and every other value is itself. The legend holds the original lexemes, so it
//! is a JSON array of strings, and a JSON string cannot hold a raw newline, so the
//! legend is one line. The test module's reference decoder does exactly this and is
//! held to the input byte for byte.
//!
//! # Which values get an entry
//!
//! The rule is the one measured on the corpus (local analysis of 2026-11-20). With
//! `k` the number of occurrences of a lexeme `t`, `cost` the configured
//! [`TokenEstimator`], and `r_i` the reference that entry `i` would get, an entry pays
//! for itself when
//!
//! ```text
//! k × (cost(t) − cost(r_i)) > cost(t) + 1
//! ```
//!
//! — each occurrence saves the difference, and the legend pays for the lexeme once
//! plus its separator.
//!
//! 1. **First pass.** Lexemes with `k ≥ 2` are ordered by `k` (highest first), then by
//!    `cost` (highest first), then by first occurrence. In that order a lexeme is
//!    kept when the rule holds with `i` the number already kept.
//! 2. **Second pass.** E5 renders the document with the first pass's references.
//!    Its constant columns can hoist a reference into a header, where it occurs once,
//!    so each entry is re-priced with `k` its count in that rendering: the number of
//!    string lexemes in the body that are exactly that entry's reference. (A key
//!    written `"$i"` counts too, so the count can only be too high, which keeps an
//!    entry rather than dropping one; either way every reference that remains is in the
//!    legend.) The entries that still pay are ordered by that count, highest first,
//!    ties in first-pass order, renumbered from 0, and the document is rendered again.
//!
//! # When it declines (returns `None`)
//!
//! When no entry survives either pass; when E5 declines the substituted document (it
//! has no array that qualifies as a grid, see E5's module docs); and on a defensive
//! guard that holds for a tape and the input it was parsed from — a span that does
//! not resolve, or a substituted text that does not parse. A reference changes no
//! structure, so the substituted text nests exactly as deep as the input, and it is
//! parsed without a depth limit.
//!
//! Whether the rendering is cheaper than the input, or than E5's own, is decided
//! afterwards by [`select`](super::select), as for every encoder. On an equal
//! estimate the lower id wins, and E3's id (3) is lower than E5's (5).
//!
//! # Determinism
//!
//! The occurrence counts live in an `FxHashMap` keyed by the full lexeme, which is
//! compared on lookup, so a hash collision cannot merge two values. The map is only
//! probed; the entries are kept in first-occurrence order in a `Vec`, and every sort
//! is stable. Identical input and estimator therefore yield identical output.

use rustc_hash::FxHashMap;

use super::e2_tabular::{push_usize, span_str};
use super::e5_grid;
use crate::estimator::TokenEstimator;
use crate::never_compress;
use crate::tape::{self, NodeKind, Span, Tape};

/// The line that explains the legend, written right after the sentinel line.
///
/// It is the fixed price of the encoder's readability: in front of the legend it
/// costs 18 estimated tokens and 15 real `cl100k` or `o200k` tokens, on top of the
/// sentinel line (pinned by `the_refs_explanation_costs_what_its_docs_say` in
/// `estimator`). [`select`](super::select) charges it, as every byte of a candidate.
pub(super) const EXPLANATION: &str = "#refs \"$N\" = entry N (from 0) of:\n";
/// Opens the legend line.
const LEGEND_OPEN: &str = "$=[";
/// Closes the legend line.
const LEGEND_CLOSE: &str = "]\n";

/// One distinct value lexeme: its text, how often it occurs, and its estimated cost.
struct Value<'a> {
    lexeme: &'a str,
    count: usize,
    cost: usize,
}

/// Render the tape as an E3 body (everything after the sentinel line), or `None` when
/// no entry pays for itself or E5 declines the substituted document.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn render(tape: &Tape, input: &str, estimator: &dyn TokenEstimator) -> Option<String> {
    // Every value-position string, in source order, with the index of its distinct
    // value (`None` for a protected lexeme, which is never replaced).
    let mut sites: Vec<(Span, Option<usize>)> = Vec::new();
    let mut values: Vec<Value<'_>> = Vec::new();
    let mut index: FxHashMap<&str, usize> = FxHashMap::default();
    for node in tape.nodes() {
        if node.kind != NodeKind::String {
            continue;
        }
        let lexeme = span_str(input, node.span)?;
        if never_compress::is_protected(lexeme).is_some() {
            sites.push((node.span, None));
            continue;
        }
        let at = if let Some(&at) = index.get(lexeme) {
            let value = values.get_mut(at)?;
            value.count = value.count.checked_add(1)?;
            at
        } else {
            let at = values.len();
            index.insert(lexeme, at);
            values.push(Value {
                lexeme,
                count: 1,
                cost: 0,
            });
            at
        };
        sites.push((node.span, Some(at)));
    }

    // First pass: price the repeated values and keep those the rule admits.
    let mut order: Vec<usize> = Vec::new();
    for (at, value) in values.iter_mut().enumerate() {
        if value.count >= 2 {
            value.cost = estimator.estimate(value.lexeme);
            order.push(at);
        }
    }
    order.sort_by_key(|&at| {
        values
            .get(at)
            .map(|v| (std::cmp::Reverse(v.count), std::cmp::Reverse(v.cost)))
    });
    let mut kept: Vec<usize> = Vec::new();
    for at in order {
        let value = values.get(at)?;
        if pays(
            value.count,
            value.cost,
            estimator.estimate(&reference(kept.len())),
        ) {
            kept.push(at);
        }
    }
    if kept.is_empty() {
        return None;
    }

    // Second pass: re-price each entry by its occurrences in the real rendering.
    let first = substituted_body(&sites, input, &kept, values.len())?;
    let occurrences = count_references(&first, kept.len());
    let mut second: Vec<(usize, usize)> = Vec::new(); // (value index, occurrences)
    for (i, &at) in kept.iter().enumerate() {
        let value = values.get(at)?;
        let occ = occurrences.get(i).copied().unwrap_or(0);
        if pays(occ, value.cost, estimator.estimate(&reference(i))) {
            second.push((at, occ));
        }
    }
    second.sort_by_key(|&(_, occ)| std::cmp::Reverse(occ));
    let final_kept: Vec<usize> = second.into_iter().map(|(at, _)| at).collect();
    if final_kept.is_empty() {
        return None;
    }
    let body = if final_kept == kept {
        first
    } else {
        substituted_body(&sites, input, &final_kept, values.len())?
    };

    let mut out = String::with_capacity(body.len().saturating_add(EXPLANATION.len()));
    out.push_str(EXPLANATION);
    out.push_str(LEGEND_OPEN);
    for (i, &at) in final_kept.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(values.get(at)?.lexeme);
    }
    out.push_str(LEGEND_CLOSE);
    out.push_str(&body);
    Some(out)
}

/// Whether an entry for a lexeme of cost `cost` occurring `count` times, referenced
/// by a reference of cost `ref_cost`, pays for itself: `count × (cost − ref_cost) >
/// cost + 1`. Exact integer arithmetic; `u128` cannot overflow for `usize` factors.
fn pays(count: usize, cost: usize, ref_cost: usize) -> bool {
    if cost <= ref_cost {
        return false;
    }
    (count as u128) * ((cost - ref_cost) as u128) > (cost as u128) + 1
}

/// The reference to legend entry `i`: `"$i"`.
fn reference(i: usize) -> String {
    let mut out = String::with_capacity(8);
    out.push_str("\"$");
    push_usize(&mut out, i);
    out.push('"');
    out
}

/// Whether `lexeme` has the shape a reference has or an escaped reference has: a
/// quote, one or more `$`, one or more ASCII digits, a quote, and nothing else.
fn is_reference_shaped(lexeme: &str) -> bool {
    let Some(inner) = lexeme
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return false;
    };
    let digits = inner.trim_start_matches('$');
    digits.len() < inner.len() && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// The input with every kept value replaced by its reference and every
/// reference-shaped value escaped, rendered by E5.
///
/// `kept[i]` is the value index that gets reference `i`; `n_values` is the number of
/// distinct values, so the table can be indexed directly.
fn substituted_body(
    sites: &[(Span, Option<usize>)],
    input: &str,
    kept: &[usize],
    n_values: usize,
) -> Option<String> {
    let mut entry: Vec<Option<usize>> = vec![None; n_values];
    for (i, &at) in kept.iter().enumerate() {
        *entry.get_mut(at)? = Some(i);
    }
    let mut text = String::with_capacity(input.len());
    let mut copied = 0usize;
    for &(span, at) in sites {
        let start = span.start as usize;
        text.push_str(input.get(copied..start)?);
        let lexeme = span_str(input, span)?;
        match at.and_then(|at| entry.get(at).copied().flatten()) {
            Some(i) => text.push_str(&reference(i)),
            None if is_reference_shaped(lexeme) => {
                text.push_str("\"$");
                text.push_str(lexeme.get(1..)?);
            }
            None => text.push_str(lexeme),
        }
        copied = span.end as usize;
    }
    text.push_str(input.get(copied..)?);
    let tape = tape::parse(&text, usize::MAX).ok()?;
    e5_grid::render(&tape, &text)
}

/// For each of the `n` references, how many string lexemes in the E5 `body` are
/// exactly that reference.
///
/// Every `"` in an E5 body opens a complete JSON string lexeme copied from the source
/// — outside them the grammar has no quote — so scanning lexeme by lexeme, with a
/// backslash taking the byte after it, finds exactly the body's strings.
fn count_references(body: &str, n: usize) -> Vec<usize> {
    let mut counts = vec![0usize; n];
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while let Some(&byte) = bytes.get(i) {
        if byte != b'"' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while let Some(&inner) = bytes.get(i) {
            if inner == b'"' {
                break;
            }
            i += if inner == b'\\' { 2 } else { 1 };
        }
        let end = (i + 1).min(bytes.len());
        if let Some(at) = body.get(start..end).and_then(reference_index) {
            if let Some(count) = counts.get_mut(at) {
                *count += 1;
            }
        }
        i = end;
    }
    counts
}

/// The index a reference lexeme `"$i"` names, or `None` when `lexeme` is not exactly
/// a reference (an escaped value, a leading zero, anything else).
fn reference_index(lexeme: &str) -> Option<usize> {
    let digits = lexeme.strip_prefix("\"$")?.strip_suffix('"')?;
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{EXPLANATION, count_references, is_reference_shaped, reference_index, render};
    use crate::encoder::e2_tabular::emit_compact;
    use crate::encoder::e5_grid::grid_reference::decode_grid_body;
    use crate::estimator::{ByteLenEstimator, HeuristicEstimator, TokenEstimator};
    use crate::tape::{NodeKind, parse};

    const SYS_A: &str = "7bdbfe53dbd5981044eb9eb6db9619d3";
    const SYS_B: &str = "6816f79cc0a8016401c5a33be04be441";

    fn rendered(input: &str, estimator: &dyn TokenEstimator) -> Option<String> {
        render(&parse(input, 512).unwrap(), input, estimator)
    }

    /// The whole document as minified JSON with verbatim lexemes.
    fn compact(input: &str) -> String {
        let t = parse(input, 512).unwrap();
        let nodes = t.nodes();
        let mut out = String::new();
        emit_compact(nodes, input, &mut out, 0, nodes.len() - 1).unwrap();
        out
    }

    /// A reference reader written from the module docs: split off the explanation and
    /// the legend, rebuild the substituted document with E5's reference decoder, then
    /// resolve every value-position reference and un-escape every escaped value.
    fn decode(rendering: &str) -> String {
        let rest = rendering
            .strip_prefix(EXPLANATION)
            .expect("explanation line");
        let (legend_line, body) = rest.split_once('\n').expect("legend line");
        let legend_json = legend_line.strip_prefix("$=").expect("legend opener");
        let legend_tape = parse(legend_json, 1).unwrap();
        let legend: Vec<&str> = legend_tape
            .nodes()
            .iter()
            .map(|n| {
                if matches!(n.kind, NodeKind::ArrayStart { .. } | NodeKind::ArrayEnd) {
                    ""
                } else {
                    assert_eq!(n.kind, NodeKind::String, "legend holds strings only");
                    &legend_json[n.span.start as usize..n.span.end as usize]
                }
            })
            .filter(|s| !s.is_empty())
            .collect();
        let substituted = decode_grid_body(body);
        let tape = parse(&substituted, usize::MAX).unwrap();
        let mut out = String::new();
        let mut copied = 0;
        for node in tape.nodes() {
            if node.kind != NodeKind::String {
                continue;
            }
            let (s, e) = (node.span.start as usize, node.span.end as usize);
            out.push_str(&substituted[copied..s]);
            let lexeme = &substituted[s..e];
            if let Some(i) = reference_index(lexeme) {
                out.push_str(legend[i]);
            } else if is_reference_shaped(lexeme) {
                assert!(lexeme.starts_with("\"$$"), "unescaped value {lexeme}");
                out.push('"');
                out.push_str(&lexeme[2..]);
            } else {
                out.push_str(lexeme);
            }
            copied = e;
        }
        out.push_str(&substituted[copied..]);
        out
    }

    fn assert_roundtrip(input: &str, estimator: &dyn TokenEstimator) -> String {
        let out = rendered(input, estimator).expect("E3 should apply");
        assert_eq!(decode(&out), compact(input), "rendering: {out}");
        out
    }

    fn legend_of(out: &str) -> &str {
        out.lines().nth(1).unwrap()
    }

    fn records(n: usize) -> String {
        let rows: Vec<String> = (0..n)
            .map(|i| {
                let user = if i % 3 == 0 { SYS_B } else { SYS_A };
                format!(r#"{{"name":"row{i}","user":"{user}","updated":"2026-09-11 07:28:44"}}"#)
            })
            .collect();
        format!(r#"{{"count":{n},"records":[{}]}}"#, rows.join(","))
    }

    #[test]
    fn a_repeated_value_moves_into_the_legend_and_decodes_back() {
        let input = records(12);
        let out = assert_roundtrip(&input, &HeuristicEstimator);
        assert!(out.starts_with(EXPLANATION), "{out}");
        // SYS_A occurs 8 times and SYS_B 4 times, so SYS_A is entry 0.
        assert_eq!(legend_of(&out), format!(r#"$=["{SYS_A}","{SYS_B}"]"#));
        assert_eq!(out.matches(SYS_A).count(), 1, "{out}");
        assert_eq!(out.matches(SYS_B).count(), 1, "{out}");
        assert!(out.contains(r#""$0""#) && out.contains(r#""$1""#), "{out}");
    }

    #[test]
    fn a_constant_column_does_not_keep_an_entry_it_no_longer_pays_for() {
        // Every record carries the same timestamp. The first pass gives it an entry;
        // E5 hoists the column into the header, where it occurs once, so the second
        // pass drops the entry and the value is written in the header itself.
        let input = records(12);
        let out = assert_roundtrip(&input, &HeuristicEstimator);
        assert!(!legend_of(&out).contains("2026-09-11"), "{out}");
        assert!(out.contains(r#"updated="2026-09-11 07:28:44""#), "{out}");
    }

    #[test]
    fn keys_are_never_replaced() {
        let key = "a_rather_long_key_name_that_repeats_everywhere";
        // The value alternates, so E5 cannot hoist it as a constant column.
        let rows: Vec<String> = (0..8)
            .map(|i| {
                let v = if i % 2 == 0 { SYS_A } else { SYS_B };
                format!(r#"{{"{key}":{i},"v":"{v}","w":"{SYS_A}{i}"}}"#)
            })
            .collect();
        let input = format!("[{}]", rows.join(","));
        let out = assert_roundtrip(&input, &HeuristicEstimator);
        assert_eq!(legend_of(&out), format!(r#"$=["{SYS_A}","{SYS_B}"]"#));
        assert!(out.contains(key), "{out}");
    }

    #[test]
    fn a_reference_shaped_value_is_escaped_and_decodes_back() {
        let rows: Vec<String> = (0..8)
            .map(|i| {
                let odd = ["$0", "$$5", "$12x", "$"][i % 4];
                let id = if i % 2 == 0 { SYS_A } else { SYS_B };
                format!(r#"{{"id":"{id}","odd":"{odd}","n":{i}}}"#)
            })
            .collect();
        let input = format!("[{}]", rows.join(","));
        let out = assert_roundtrip(&input, &ByteLenEstimator);
        assert!(out.contains(r#""$$0""#), "{out}");
        assert!(out.contains(r#""$$$5""#), "{out}");
        assert!(out.contains(r#""$12x""#) && out.contains(r#""$""#), "{out}");
    }

    #[test]
    fn a_repeated_reference_shaped_value_can_itself_be_an_entry() {
        // The legend holds the original lexeme, so the escape is not needed there.
        let long = "$123456789012345678901234567890";
        // It alternates with another value, so E5 cannot hoist it as a constant.
        let rows: Vec<String> = (0..8)
            .map(|i| {
                let v = if i % 2 == 0 { long } else { SYS_A };
                format!(r#"{{"v":"{v}","n":{i}}}"#)
            })
            .collect();
        let input = format!("[{}]", rows.join(","));
        let out = assert_roundtrip(&input, &ByteLenEstimator);
        assert!(legend_of(&out).contains(&format!("\"{long}\"")), "{out}");
    }

    #[test]
    fn a_protected_value_is_never_replaced() {
        let denied = "permission denied for the requested resource";
        let rows: Vec<String> = (0..8)
            .map(|i| {
                let v = if i % 2 == 0 { denied } else { SYS_A };
                let w = if i % 2 == 0 { SYS_B } else { denied };
                format!(r#"{{"err":"{v}","other":"{w}","n":{i}}}"#)
            })
            .collect();
        let input = format!("[{}]", rows.join(","));
        let out = assert_roundtrip(&input, &ByteLenEstimator);
        assert!(!legend_of(&out).contains("denied"), "{out}");
        assert_eq!(out.matches(denied).count(), 8, "{out}");
    }

    #[test]
    fn it_declines_without_a_repeated_value() {
        let input = r#"[{"a":"x1","b":1},{"a":"x2","b":2},{"a":"x3","b":3}]"#;
        assert_eq!(rendered(input, &HeuristicEstimator), None);
    }

    #[test]
    fn it_declines_when_e5_declines() {
        // The value repeats, but no array of objects qualifies as a grid.
        let input = format!(r#"{{"a":"{SYS_A}","b":"{SYS_A}","c":["{SYS_A}","{SYS_A}"]}}"#);
        assert_eq!(rendered(&input, &HeuristicEstimator), None);
    }

    #[test]
    fn short_values_are_not_worth_an_entry() {
        let rows: Vec<String> = (0..10)
            .map(|i| {
                format!(
                    r#"{{"s":"{}","n":{i}}}"#,
                    if i % 2 == 0 { "ok" } else { "no" }
                )
            })
            .collect();
        let input = format!("[{}]", rows.join(","));
        assert_eq!(rendered(&input, &HeuristicEstimator), None);
    }

    #[test]
    fn the_rendering_is_deterministic() {
        let input = records(20);
        assert_eq!(
            rendered(&input, &HeuristicEstimator),
            rendered(&input, &HeuristicEstimator)
        );
    }

    #[test]
    fn references_are_counted_as_whole_lexemes() {
        let body = r#"{"a":"$0","b":"x\"$0","c":"$01","d":"$$0","e":"$1","f":"$9"}"#;
        assert_eq!(count_references(body, 2), vec![1, 1]);
    }

    #[test]
    fn reference_shapes() {
        for yes in [r#""$0""#, r#""$$5""#, r#""$$$123""#] {
            assert!(is_reference_shaped(yes), "{yes}");
        }
        for no in [
            r#""$""#,
            r#""5""#,
            r#""$5x""#,
            r#""x$5""#,
            "\"$\u{663}\"",
            "$5",
            "\"",
        ] {
            assert!(!is_reference_shaped(no), "{no}");
        }
        assert_eq!(reference_index(r#""$7""#), Some(7));
        assert_eq!(reference_index(r#""$0""#), Some(0));
        assert_eq!(reference_index(r#""$07""#), None);
        assert_eq!(reference_index(r#""$$7""#), None);
    }
}
