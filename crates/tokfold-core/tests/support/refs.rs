//! A strict reference decoder for the E3 `refs` body, shared by the integration tests
//! that include it next to `support/grid.rs` (as `mod grid`).
//!
//! It is written from the `encoder::e3_refs` module docs rather than from the
//! encoder: the explanation line, then the legend line `$=[…]`, then an E5 body of
//! the substituted document. Reading it back, a value lexeme `"$i"` is legend entry
//! `i`, a value lexeme of the escaped shape (a quote, two or more `$`, ASCII digits, a
//! quote) loses one `$`, and every other lexeme is itself. Keys are never touched. It
//! panics on anything those docs do not allow — a missing line, a legend entry that
//! is not a string, an index past the legend, an entry no reference uses, an
//! unescaped reference-shaped value — so a body that only *happens* to decode cannot
//! pass.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code
)]

use tokfold_core::tape::{NodeKind, parse};

/// The second line of every E3 rendering, newline included.
pub const EXPLANATION: &str = "#refs \"$N\" = entry N (from 0) of:\n";

/// Decode an E3 body (the rendering minus its `#tkfd1 refs` line) into the minified
/// JSON the E5 body stands for, with every reference resolved.
pub fn decode_refs_body(body: &str) -> String {
    decode_refs_body_counted(body).0
}

/// [`decode_refs_body`], plus how many references it resolved and how many escaped
/// values it unescaped, so a test can show which paths its inputs reach.
pub fn decode_refs_body_counted(body: &str) -> (String, usize, usize) {
    let (mut resolved, mut unescaped) = (0, 0);
    let rest = body
        .strip_prefix(EXPLANATION)
        .unwrap_or_else(|| panic!("no explanation line: {body:?}"));
    let (legend_line, grid_body) = rest
        .split_once('\n')
        .unwrap_or_else(|| panic!("no legend line: {body:?}"));
    let legend_json = legend_line
        .strip_prefix("$=")
        .unwrap_or_else(|| panic!("legend line does not open with `$=`: {legend_line:?}"));
    let legend_tape = parse(legend_json, 1).expect("the legend is one JSON array");
    let legend: Vec<&str> = legend_tape
        .nodes()
        .iter()
        .filter(|n| !matches!(n.kind, NodeKind::ArrayStart { .. } | NodeKind::ArrayEnd))
        .map(|n| {
            assert_eq!(
                n.kind,
                NodeKind::String,
                "legend holds strings only: {legend_json}"
            );
            &legend_json[n.span.start as usize..n.span.end as usize]
        })
        .collect();
    assert!(!legend.is_empty(), "an E3 legend is never empty");
    let mut used = vec![false; legend.len()];

    let substituted = crate::grid::decode_grid_body(grid_body);
    let tape = parse(&substituted, usize::MAX).expect("the E5 body decodes to JSON");
    let mut out = String::with_capacity(substituted.len());
    let mut copied = 0;
    for node in tape.nodes() {
        if node.kind != NodeKind::String {
            continue;
        }
        let (s, e) = (node.span.start as usize, node.span.end as usize);
        out.push_str(&substituted[copied..s]);
        let lexeme = &substituted[s..e];
        match dollar_digits(lexeme) {
            Some((1, digits)) => {
                assert!(
                    digits.len() == 1 || !digits.starts_with('0'),
                    "reference with a leading zero: {lexeme}"
                );
                let i: usize = digits.parse().expect("reference index");
                let entry = legend
                    .get(i)
                    .unwrap_or_else(|| panic!("{lexeme} is past a {}-entry legend", legend.len()));
                used[i] = true;
                resolved += 1;
                out.push_str(entry);
            }
            Some((_, _)) => {
                unescaped += 1;
                out.push('"');
                out.push_str(&lexeme[2..]);
            }
            None => out.push_str(lexeme),
        }
        copied = e;
    }
    out.push_str(&substituted[copied..]);
    assert!(
        used.iter().all(|&u| u),
        "a legend entry no reference uses: {legend_line}"
    );
    (out, resolved, unescaped)
}

/// For a lexeme of the shape `"` `$`+ digits+ `"`, the number of `$` and the digits.
fn dollar_digits(lexeme: &str) -> Option<(usize, &str)> {
    let inner = lexeme.strip_prefix('"')?.strip_suffix('"')?;
    let digits = inner.trim_start_matches('$');
    let dollars = inner.len() - digits.len();
    (dollars > 0 && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .then_some((dollars, digits))
}
