//! `tokfold_core::tape::parse` on arbitrary UTF-8.
//!
//! The document is the input after the first byte. An unbounded parse always runs; the
//! first byte decides whether a bounded one runs beside it and compares against it —
//! bit 7 set, no bounded parse at all; bit 7 clear, `max_depth` is bits 0-4 (`0` to
//! `31`). Bits 5 and 6 are unused.
//!
//! Must never panic. On success the tape must be well formed: every span lies inside
//! the input on character boundaries, strings and keys are quoted, container counts
//! match their children, and depths are exact. It must agree with the independent
//! oracle about which documents are valid. And a depth limit may only turn a result
//! into `DepthExceeded` — it may never change a tape or an `InvalidJson` offset.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tokfold_core::CompressError;
use tokfold_core::tape::{self, NodeKind, Tape};
use tokfold_fuzz::oracle_valid;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let Ok(input) = std::str::from_utf8(rest) else {
        return;
    };

    let unbounded = tape::parse(input, usize::MAX);
    if let Ok(tape) = &unbounded {
        check_tape(input, tape);
    }
    if let Some(valid) = oracle_valid(input) {
        assert_eq!(
            unbounded.is_ok(),
            valid,
            "tape and oracle disagree: tape {unbounded:?}"
        );
    }

    if selector & 0x80 == 0 {
        let limit = usize::from(selector & 0x1F);
        match (tape::parse(input, limit), &unbounded) {
            (Ok(bounded), Ok(full)) => {
                assert_eq!(bounded.len(), full.len());
                for (a, b) in bounded.nodes().iter().zip(full.nodes()) {
                    assert_eq!((a.kind, a.span, a.depth), (b.kind, b.span, b.depth));
                }
                let deepest = full.nodes().iter().map(|n| n.depth).max().unwrap_or(0);
                assert!(deepest as usize <= limit, "a tape deeper than its limit");
            }
            (Ok(_), Err(err)) => panic!("a depth limit turned {err:?} into a tape"),
            (
                Err(CompressError::DepthExceeded {
                    limit: reported, ..
                }),
                _,
            ) => {
                assert_eq!(reported, limit);
            }
            (Err(bounded), Err(full)) => assert_eq!(
                format!("{bounded:?}"),
                format!("{full:?}"),
                "a depth limit changed an error it did not cause"
            ),
            (Err(err), Ok(_)) => panic!("{err:?} under a limit on a valid document"),
        }
    }
});

fn check_tape(input: &str, tape: &Tape) {
    let mut open: Vec<(usize, u32)> = Vec::new(); // (node index, children seen)
    let mut depth: u32 = 0;
    let mut expect_value_after_key = false;
    for (index, node) in tape.nodes().iter().enumerate() {
        let (start, end) = (node.span.start as usize, node.span.end as usize);
        let lexeme = input
            .get(start..end)
            .unwrap_or_else(|| panic!("span {start}..{end} is not a slice of the input"));
        let closes = matches!(node.kind, NodeKind::ObjectEnd | NodeKind::ArrayEnd);
        if closes {
            depth = depth.checked_sub(1).expect("a close with nothing open");
        }
        assert_eq!(node.depth, depth, "node {index} at the wrong depth");

        if !closes && !expect_value_after_key {
            if let Some((_, seen)) = open.last_mut() {
                *seen += 1;
            }
        }
        expect_value_after_key = false;
        match node.kind {
            NodeKind::Null => assert_eq!(lexeme, "null"),
            NodeKind::Bool(value) => assert_eq!(lexeme, if value { "true" } else { "false" }),
            NodeKind::Number => assert!(!lexeme.is_empty()),
            NodeKind::String => {
                assert!(lexeme.len() >= 2 && lexeme.starts_with('"') && lexeme.ends_with('"'))
            }
            NodeKind::Key => {
                assert!(lexeme.len() >= 2 && lexeme.starts_with('"') && lexeme.ends_with('"'));
                expect_value_after_key = true;
            }
            NodeKind::ObjectStart { .. } | NodeKind::ArrayStart { .. } => {
                open.push((index, 0));
                depth += 1;
            }
            NodeKind::ObjectEnd | NodeKind::ArrayEnd => {
                let (start_index, seen) = open.pop().expect("an unmatched close");
                match (tape.nodes()[start_index].kind, node.kind) {
                    (NodeKind::ObjectStart { members }, NodeKind::ObjectEnd) => {
                        assert_eq!(members, seen, "object member count");
                    }
                    (NodeKind::ArrayStart { elements }, NodeKind::ArrayEnd) => {
                        assert_eq!(elements, seen, "array element count");
                    }
                    (opened, closed) => panic!("{opened:?} closed by {closed:?}"),
                }
            }
            _ => {}
        }
    }
    assert!(open.is_empty(), "a container left open");
    assert_eq!(depth, 0);
}
