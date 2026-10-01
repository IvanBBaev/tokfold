//! Helpers shared by the fuzz targets in `fuzz_targets/`.
//!
//! The semantic-equality oracle is the one the `tokfold-core` test suite uses, included
//! by path rather than copied, so a fix to it reaches the fuzzers too. It is an
//! independent recursive-descent parser with no depth limit of its own, which is fine
//! for the hand-written documents the test suite feeds it and not for arbitrary fuzz
//! input: `[` repeated a few hundred thousand times would overflow its stack and
//! report a crash in the oracle rather than in the code under test. Every call here
//! is therefore gated on [`nesting_depth`] first.

#[path = "../../crates/tokfold-core/tests/oracle.rs"]
#[allow(dead_code)]
mod oracle;

/// The deepest document the oracle is asked about. Far below any stack the fuzzer
/// runs on, and far above the depth any seed or interesting input needs.
pub const ORACLE_MAX_DEPTH: usize = 256;

/// The longest document the oracle is asked about, in bytes. The oracle is written
/// for clarity rather than speed; this keeps its share of each execution small.
pub const ORACLE_MAX_LEN: usize = 64 * 1024;

/// An upper bound on the bracket nesting of `text`, skipping brackets inside string
/// literals. It is a scan, not a parse: on malformed input it may over-count, which
/// only ever makes [`oracle_eq`] decline to answer, never answer wrongly.
#[must_use]
pub fn nesting_depth(text: &str) -> usize {
    let mut depth = 0usize;
    let mut max = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Whether the oracle may be asked about `text` without risking its stack or the
/// fuzzer's time budget.
#[must_use]
pub fn oracle_can_judge(text: &str) -> bool {
    text.len() <= ORACLE_MAX_LEN && nesting_depth(text) <= ORACLE_MAX_DEPTH
}

/// `json_semantically_eq(a, b)` from the core test suite, or `None` when either
/// document is too deep or too long to hand to it (see [`oracle_can_judge`]).
#[must_use]
pub fn oracle_eq(a: &str, b: &str) -> Option<bool> {
    if oracle_can_judge(a) && oracle_can_judge(b) {
        Some(oracle::json_semantically_eq(a, b))
    } else {
        None
    }
}

/// Whether the oracle considers `text` a valid JSON document, or `None` when it may
/// not be asked. The oracle's equality is false for any invalid document, so a
/// document is valid exactly when it equals itself.
#[must_use]
pub fn oracle_valid(text: &str) -> Option<bool> {
    oracle_eq(text, text)
}
