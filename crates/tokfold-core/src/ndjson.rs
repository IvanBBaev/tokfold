//! JSON Lines (NDJSON) recognition: an input that is not one JSON document but a
//! sequence of them, one per line.
//!
//! # When it is tried
//!
//! Only after [`tape::parse`] has rejected the whole input as
//! [`CompressError::InvalidJson`](crate::CompressError::InvalidJson). A document the
//! parser accepts is never re-read as records, so every input `compress` accepted
//! before this module existed is handled exactly as before. When recognition fails,
//! the caller reports the original whole-input error, so a rejected input is rejected
//! with the same offset and message as before too.
//!
//! # The rule
//!
//! The input is split at every line feed (`U+000A`). A line is **blank** when every
//! byte in it is a space (`0x20`), a horizontal tab (`0x09`) or a carriage return
//! (`0x0D`) — JSON's insignificant whitespace minus the line feed, which is the
//! separator — and an empty line is blank. Blank lines are skipped. Every other line
//! must parse on its own as exactly one JSON value under the configured `max_depth`;
//! a carriage return before the line feed is therefore accepted, since it is JSON
//! whitespace. At least two records are required.
//!
//! Two consequences are stated rather than left to be discovered. A record nested
//! past `max_depth` on any line but the first fails recognition, so such an input is
//! reported as the whole-input `InvalidJson`, not as `DepthExceeded`. On the first
//! line the whole-input parse meets the nesting before it meets the second record,
//! so that input is reported as `DepthExceeded` and recognition is never tried.
//!
//! # What it produces
//!
//! The records, in line order, as one JSON array: `[`, each record line verbatim,
//! joined by `,`, then `]`. That array is what the record encoder renders; it is never
//! written to the archive, which always holds the original bytes. Blank lines and the
//! line structure itself are not in the array — the rendering describes the records,
//! and only the archive recovers the bytes.

use crate::tape::{self, Tape};

/// A recognised JSON Lines input, re-expressed as one JSON array of its records.
pub struct Records {
    /// `[r1,r2,…]`: every non-blank line verbatim, in order, joined by `,`.
    pub array: String,
    /// The tape of [`Records::array`], parsed one level deeper than `max_depth` so
    /// the array itself does not count against a record's depth.
    pub tape: Tape,
}

/// Whether `b` may appear in a blank line: JSON whitespace other than the line feed.
const fn is_blank_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r')
}

/// Recognise `text` as JSON Lines under the rule in the module docs, or `None`.
pub fn records(text: &str, max_depth: usize) -> Option<Records> {
    if !text.contains('\n') {
        // One line: the whole-input parse that failed already read it.
        return None;
    }
    let mut array = String::with_capacity(text.len().saturating_add(2));
    array.push('[');
    let mut count = 0usize;
    for line in text.split('\n') {
        if line.bytes().all(is_blank_byte) {
            continue;
        }
        tape::parse(line, max_depth).ok()?;
        if count > 0 {
            array.push(',');
        }
        array.push_str(line);
        count = count.saturating_add(1);
    }
    if count < 2 {
        return None;
    }
    array.push(']');
    let tape = tape::parse(&array, max_depth.saturating_add(1)).ok()?;
    Some(Records { array, tape })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::records;

    const DEPTH: usize = 512;

    #[test]
    fn two_records_become_one_array() {
        let r = records("{\"a\":1}\n{\"a\":2}\n", DEPTH).unwrap();
        assert_eq!(r.array, "[{\"a\":1},{\"a\":2}]");
    }

    #[test]
    fn blank_lines_and_crlf_are_accepted() {
        let r = records("\n{\"a\":1}\r\n \t\r\n\r\n[2]\r\n", DEPTH).unwrap();
        assert_eq!(r.array, "[{\"a\":1}\r,[2]\r]");
    }

    #[test]
    fn any_json_value_is_a_record() {
        let r = records("1\n\"s\"\nnull\n[true]", DEPTH).unwrap();
        assert_eq!(r.array, "[1,\"s\",null,[true]]");
    }

    #[test]
    fn a_line_that_is_not_one_value_fails_recognition() {
        for input in [
            "{\"a\":1}\n{\"a\":",
            "{\"a\":1}\n1 2",
            "{\"a\":1}\n{\"a\":1},{\"a\":2}",
            "{\n\"a\":1\n}\n{\"a\":2}",
            "{\"a\":1}\nx",
        ] {
            assert!(records(input, DEPTH).is_none(), "{input:?}");
        }
    }

    #[test]
    fn fewer_than_two_records_fail_recognition() {
        for input in ["", "\n", "{\"a\":1}\n", "\n \n{\"a\":1}\r\n\t"] {
            assert!(records(input, DEPTH).is_none(), "{input:?}");
        }
    }

    #[test]
    fn a_vertical_tab_or_nbsp_line_is_not_blank() {
        // Only space, tab and carriage return make a line blank; anything else is a
        // record that must parse, and these do not.
        for sep in ["\u{0B}", "\u{0C}", "\u{A0}"] {
            let input = format!("1\n{sep}\n2");
            assert!(records(&input, DEPTH).is_none(), "{input:?}");
        }
    }

    #[test]
    fn the_depth_limit_applies_to_each_record_not_to_the_array() {
        let at_limit = format!("{}{}\n1", "[".repeat(4), "]".repeat(4));
        let r = records(&at_limit, 4).unwrap();
        assert_eq!(r.array, format!("[{}{},1]", "[".repeat(4), "]".repeat(4)));
        let past = format!("{}{}\n1", "[".repeat(5), "]".repeat(5));
        assert!(records(&past, 4).is_none());
    }
}
