//! `tokfold_mcp::json::parse` and the value writer, on arbitrary UTF-8.
//!
//! Must never panic, including on nesting past `MAX_DEPTH` (the parser is recursive
//! and bounded by that constant). What it accepts must be JSON by the independent
//! oracle — one direction only, since this parser deliberately rejects lone
//! surrogates and duplicate keys that the oracle accepts. And the writer must reach a
//! fixed point: a value written, re-parsed and written again gives the same text,
//! which is itself JSON by both parsers.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tokfold_fuzz::oracle_valid;
use tokfold_mcp::json;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(value) = json::parse(input) else {
        return;
    };
    if let Some(valid) = oracle_valid(input) {
        assert!(
            valid,
            "json::parse accepted what the oracle rejects: {input:?}"
        );
    }
    let written = value.to_string();
    let reparsed = json::parse(&written)
        .unwrap_or_else(|err| panic!("the writer's output does not parse ({err}): {written:?}"));
    let rewritten = reparsed.to_string();
    assert_eq!(written, rewritten, "the writer has no fixed point");
    if let Some(valid) = oracle_valid(&written) {
        assert!(valid, "the oracle rejects the writer's output: {written:?}");
    }
});
