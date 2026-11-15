#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(
    missing_docs,
    clippy::panic,
    clippy::missing_docs_in_private_items,
    clippy::indexing_slicing
)]

//! Golden compatibility fixtures: every archive and rendering an earlier build
//! produced must stay recoverable by this one.
//!
//! `tests/golden/` holds six inputs and, for each input `X`, the two outputs the
//! `tokfold compress` CLI wrote for it at commit `a21a26a` — the last commit whose
//! renderings carry the `⟦tkfd:v1:<tag>⟧` frame — under the default configuration:
//! `X.tkfd` (the `--archive` file) and `X.rendering` (stdout). The npm registry's
//! `tokfold-darwin-arm64` 0.0.1 binary (SHA-1 of `bin/tokfold`:
//! `8e2ed283eb205aca9cf34051a7b3ede6f4f70508`) wrote byte-identical files for the
//! five `.json` inputs. It rejects `records.jsonl` (JSON Lines support postdates
//! it) and wrote nothing for it, so those two outputs come from `a21a26a` alone.
//!
//! The fixtures pin four things:
//!
//! * every archive still decompresses to its input byte for byte;
//! * a fresh compression of each input still writes the very same archive bytes
//!   (the archive is the input behind a passthrough header, whatever encoder wins);
//! * every legacy rendering still decodes, through the legacy frame and a reference
//!   decoder for its tag, to a document semantically equal to its input.
//! * a fresh rendering differs from the legacy one only in its frame: the body
//!   behind `#tkfd1 <tag>` is the legacy body byte for byte, and a `raw`
//!   (passthrough) rendering is now the input itself.

#[path = "oracle.rs"]
mod oracle;
use oracle::json_semantically_eq;

use std::path::PathBuf;

use tokfold_core::{Compressor, Config};

/// Every fixture input, with the tag its legacy rendering carries.
const FIXTURES: &[(&str, &str)] = &[
    ("passthrough.json", "raw"),
    ("whitespace-escapes.json", "raw"),
    ("minify.json", "min"),
    ("table.json", "tbl"),
    ("unicode-table.json", "tbl"),
    ("records.jsonl", "jsonl"),
];

/// The legacy frame: `⟦tkfd:v1:` + tag + `⟧` + newline.
const LEGACY_OPEN: &str = "\u{27E6}tkfd:v1:";
const LEGACY_CLOSE: &str = "\u{27E7}\n";

fn golden(name: &str) -> Vec<u8> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "golden", name]
        .iter()
        .collect();
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn golden_str(name: &str) -> String {
    String::from_utf8(golden(name)).unwrap()
}

/// Split a legacy rendering into its tag and body.
fn legacy_frame(rendering: &str) -> (&str, &str) {
    let rest = rendering
        .strip_prefix(LEGACY_OPEN)
        .expect("a legacy rendering opens with the v1 frame");
    let close = rest.find(LEGACY_CLOSE).expect("the legacy frame closes");
    (&rest[..close], &rest[close + LEGACY_CLOSE.len()..])
}

/// Split a minified-JSON array or object `s` (opening bracket included) into its
/// top-level element slices, tracking nesting and string state so that a delimiter
/// inside a nested value or a string is not a split point.
fn split_top_level(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut elems = Vec::new();
    let (mut depth, mut in_str, mut esc, mut start) = (0i32, false, false, 1usize);
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
            b'[' | b'{' => depth += 1,
            b']' | b'}' if depth > 0 => depth -= 1,
            b']' | b'}' => {
                if i > start {
                    elems.push(&s[start..i]);
                }
                return elems;
            }
            b',' if depth == 0 => {
                elems.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    elems
}

/// Reference decoder for the `tbl`/`jsonl` body grammar (encoder ids 2 and 4): a
/// table block `\n#<N>[<keys>]` followed by `N` rows, each `+[<values>]` (paired
/// back with the header keys) or `*<object>` (verbatim), expands back into an array.
/// Everything else is minified JSON, copied as is.
fn reconstruct_table_body(body: &str) -> String {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut out = String::new();
    let mut idx = 0;
    while idx < lines.len() {
        let line = lines[idx];
        idx += 1;
        let Some(header) = line.strip_prefix('#') else {
            out.push_str(line);
            continue;
        };
        let bracket = header.find('[').unwrap();
        let count: usize = header[..bracket].parse().unwrap();
        let keys = split_top_level(&header[bracket..]);
        out.push('[');
        for r in 0..count {
            if r > 0 {
                out.push(',');
            }
            let row = lines[idx];
            idx += 1;
            if let Some(cells) = row.strip_prefix('+') {
                let vals = split_top_level(cells);
                assert_eq!(vals.len(), keys.len(), "row {row:?}");
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
                out.push_str(row.strip_prefix('*').expect("a row marker"));
            }
        }
        out.push(']');
    }
    out
}

/// The legacy rendering's body decoded back to JSON, by its tag.
fn decode_legacy(tag: &str, body: &str) -> String {
    match tag {
        "raw" | "min" => body.to_owned(),
        "tbl" | "jsonl" => reconstruct_table_body(body),
        other => panic!("unknown legacy tag {other:?}"),
    }
}

/// What a decoded body must equal: the input itself, or for JSON Lines (tag
/// `jsonl`) the records gathered into one array (the shape that body describes).
fn expected_document(tag: &str, input: &str) -> String {
    if tag != "jsonl" {
        return input.to_owned();
    }
    let records: Vec<&str> = input.lines().filter(|l| !l.trim().is_empty()).collect();
    format!("[{}]", records.join(","))
}

#[test]
fn every_golden_archive_decompresses_to_its_input() {
    let compressor = Compressor::new(Config::default());
    for (name, _) in FIXTURES {
        let input = golden(name);
        let archive = golden(&format!("{name}.tkfd"));
        let restored = compressor
            .decompress(&archive)
            .unwrap_or_else(|e| panic!("{name}.tkfd: {e}"));
        assert_eq!(restored, input, "{name}.tkfd restores different bytes");
    }
}

#[test]
fn a_fresh_compression_writes_the_golden_archive_bytes() {
    let compressor = Compressor::new(Config::default());
    for (name, _) in FIXTURES {
        let artifact = compressor.compress(&golden(name)).unwrap();
        assert_eq!(
            artifact.archive,
            golden(&format!("{name}.tkfd")),
            "{name}: the archive bytes changed"
        );
    }
}

#[test]
fn every_legacy_rendering_decodes_to_its_input() {
    for (name, tag) in FIXTURES {
        let input = golden_str(name);
        let rendering = golden_str(&format!("{name}.rendering"));
        let (found_tag, body) = legacy_frame(&rendering);
        assert_eq!(found_tag, *tag, "{name}: legacy tag");
        if *tag == "raw" {
            assert_eq!(body, input, "{name}: a raw body is the input verbatim");
        }
        let decoded = decode_legacy(tag, body);
        assert!(
            json_semantically_eq(&decoded, &expected_document(tag, &input)),
            "{name}: decoded legacy body differs from the input\n{decoded}"
        );
    }
}

#[test]
fn a_fresh_rendering_is_the_legacy_body_under_the_new_frame() {
    let compressor = Compressor::new(Config::default());
    for (name, tag) in FIXTURES {
        let input = golden_str(name);
        let legacy = golden_str(&format!("{name}.rendering"));
        let (_, body) = legacy_frame(&legacy);
        let fresh = compressor.compress(input.as_bytes()).unwrap().rendering;
        if *tag == "raw" {
            assert_eq!(fresh, input, "{name}: a passthrough rendering is the input");
        } else {
            assert_eq!(
                fresh,
                format!("#tkfd1 {tag}\n{body}"),
                "{name}: only the frame changed"
            );
        }
    }
}
