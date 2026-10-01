//! The README's Quickstart session, replayed and checked against what it quotes.
//!
//! The Quickstart shows a session a reader can paste into a shell and the replies it
//! produces. Both halves are read out of `README.md` here rather than copied, so the
//! document cannot drift from the server unnoticed *where this file anchors it* -- and
//! that is under a quarter of the bytes it quotes, the share
//! `the_quoted_replies_anchor_only_part_of_what_they_quote` asserts. The request lines
//! are the ones the reader runs, and every quoted reply is matched against the real
//! one. A quoted reply is trimmed with `…`; each piece between the ellipses must appear
//! in the real line, in order, with the first piece a prefix and the last a suffix.
//!
//! Two limits in particular, named here so that this file's coverage is not read as
//! wider than it is -- not the whole of what it leaves unchecked. An elided run is
//! unconstrained: drift that stays inside a `…` --
//! a changed number, a renamed field -- passes, and
//! `the_quoted_replies_anchor_only_part_of_what_they_quote` pins per reply how much of
//! the real line the pieces actually hold. And the shell line itself,
//! `tokfold mcp <<'EOF'`, is compared as text and never run: the replay drives
//! [`stdio::serve`] in process, so nothing here reaches the binary or its argument
//! parsing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use tokfold_mcp::{Server, json, stdio};

const README: &str = include_str!("../README.md");

/// The body of the fenced block that follows `marker` in the README.
fn block_after(marker: &str) -> &'static str {
    let start = README
        .find(marker)
        .unwrap_or_else(|| panic!("the README has lost its {marker} marker"));
    let rest = &README[start + marker.len()..];
    let open = rest.find("```").expect("a fenced block follows the marker");
    let body_start = open + rest[open..].find('\n').expect("the fence line ends") + 1;
    let body_len = rest[body_start..]
        .find("```")
        .expect("the fenced block is closed");
    &rest[body_start..body_start + body_len]
}

/// The request lines of the `tokfold mcp <<'EOF'` heredoc, as a shell would feed them.
fn session_requests() -> Vec<&'static str> {
    let block = block_after("<!-- quickstart-session -->");
    let mut lines = block.lines();
    assert_eq!(lines.next(), Some("tokfold mcp <<'EOF'"));
    let requests: Vec<&str> = lines.by_ref().take_while(|line| *line != "EOF").collect();
    assert_eq!(lines.next(), None, "nothing follows the heredoc terminator");
    requests
}

fn quoted_replies() -> Vec<&'static str> {
    block_after("<!-- quickstart-replies -->").lines().collect()
}

/// Runs the README's session through the real transport and returns the emitted lines.
fn replay() -> Vec<String> {
    let mut input = session_requests().join("\n");
    input.push('\n');
    let mut server = Server::new();
    let mut output = Vec::new();
    stdio::serve(input.as_bytes(), &mut output, &mut server)
        .expect("an in-memory session cannot fail on I/O");
    String::from_utf8(output)
        .expect("the transport emits UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Whether `actual` is `quoted` with each `…` standing for any run of characters.
fn matches_trimmed(quoted: &str, actual: &str) -> bool {
    let pieces: Vec<&str> = quoted.split('…').collect();
    let (first, last) = (pieces[0], pieces[pieces.len() - 1]);
    if pieces.len() == 1 {
        return quoted == actual;
    }
    if !actual.starts_with(first) || !actual.ends_with(last) {
        return false;
    }
    let end = actual.len() - last.len();
    let mut at = first.len();
    // The prefix and the suffix can overlap -- `ab…bc` against `abc` -- and then `at`
    // is past `end`. That has to be refused here, before the slice below: `actual[at..end]`
    // panics on an inverted range, and with three or more pieces the loop reaches it. The
    // trailing check this replaces could only ever see the two-piece case, because a
    // successful `find` inside `actual[at..end]` leaves `at <= end` by construction.
    if at > end {
        return false;
    }
    for piece in &pieces[1..pieces.len() - 1] {
        match actual[at..end].find(piece) {
            Some(offset) => at += offset + piece.len(),
            None => return false,
        }
    }
    true
}

fn parse(line: &str) -> json::Value {
    json::parse(line).expect("every line is a JSON message")
}

fn path<'a>(value: &'a json::Value, dotted: &str) -> &'a json::Value {
    dotted.split('.').fold(value, |current, segment| {
        current
            .get(segment)
            .unwrap_or_else(|| panic!("no `{segment}` on the way to `{dotted}`"))
    })
}

fn string_at<'a>(value: &'a json::Value, dotted: &str) -> &'a str {
    path(value, dotted)
        .as_str()
        .unwrap_or_else(|| panic!("`{dotted}` is not a string"))
}

#[test]
fn the_heredoc_is_seven_requests_and_the_only_notification_is_initialized() {
    let requests = session_requests();
    assert_eq!(requests.len(), 7);
    let without_id: Vec<&str> = requests
        .iter()
        .copied()
        .filter(|line| parse(line).get("id").is_none())
        .collect();
    assert_eq!(without_id.len(), 1, "exactly one line is a notification");
    // The count alone holds for any id-less line, whatever it asks for. Which
    // notification it is, is part of the session the Quickstart teaches.
    assert_eq!(
        string_at(&parse(without_id[0]), "method"),
        "notifications/initialized"
    );
}

#[test]
fn every_quoted_reply_matches_the_reply_the_server_gives() {
    let actual = replay();
    let quoted = quoted_replies();
    assert_eq!(
        actual.len(),
        6,
        "six replies: one per request, none for the notification"
    );
    assert_eq!(quoted.len(), actual.len(), "the README quotes every reply");
    for (index, (quoted, actual)) in quoted.iter().zip(&actual).enumerate() {
        assert!(
            matches_trimmed(quoted, actual),
            "quoted reply {} no longer matches.\nquoted: {quoted}\nactual: {actual}",
            index + 1
        );
    }
}

#[test]
fn the_ellipsis_matcher_rejects_what_it_should() {
    assert!(matches_trimmed("a…c", "abc"));
    assert!(matches_trimmed("a…c", "ac"));
    assert!(matches_trimmed("a…b…c", "a-b-c"));
    assert!(!matches_trimmed("a…c", "abd"));
    assert!(!matches_trimmed("a…b…c", "a-c-b"));
    assert!(
        !matches_trimmed("ab…bc", "abc"),
        "prefix and suffix overlap"
    );
    assert!(!matches_trimmed("abc", "abcd"));

    // The overlap cases above have two pieces, so they never enter the loop. Replies the
    // README quotes with more than two are the ones that reach the slice, and how many
    // pieces each quote has is pinned by the coverage test
    // `the_quoted_replies_anchor_only_part_of_what_they_quote`, whose last assertion
    // counts the ones with more than two. So the overlap has to be refused for them
    // too, not panicked on.
    assert!(!matches_trimmed("ab…x…bc", "abc"), "overlap, three pieces");
    assert!(
        !matches_trimmed("abc…x…y…abc", "abc"),
        "the prefix alone is the whole line"
    );
    assert!(matches_trimmed("a…b…c…d", "a1b2c3d"));
    assert!(!matches_trimmed("a…b…c…d", "a1c2b3d"), "out of order");
    // A piece may be empty, and an empty piece matches where it stands.
    assert!(matches_trimmed("a……d", "abcd"));
}

#[test]
fn the_unescaped_rendering_is_the_one_the_compress_reply_carries() {
    let replies = replay();
    let compress = parse(&replies[3]);
    let rendering = string_at(&compress, "result.structuredContent.rendering");
    let quoted = block_after("<!-- quickstart-rendering -->");
    assert_eq!(format!("{rendering}\n"), quoted);
    let content = path(&compress, "result.content").as_array().unwrap();
    assert_eq!(
        content[0].get("text").and_then(json::Value::as_str),
        Some(rendering)
    );
}

#[test]
fn the_archive_the_readme_sends_back_is_the_one_compress_returned() {
    let requests = session_requests();
    let replies = replay();
    let compress = parse(&replies[3]);
    let decompress_call = parse(requests[5]);
    let returned = string_at(&compress, "result.structuredContent.archive");
    let sent = string_at(&decompress_call, "params.arguments.archive");
    assert_eq!(sent, returned);
}

#[test]
fn the_restored_text_is_the_input_byte_for_byte_and_the_numbers_quoted_hold() {
    let requests = session_requests();
    let replies = replay();
    let estimate_call = parse(requests[3]);
    let compress_call = parse(requests[4]);
    let input = string_at(&compress_call, "params.arguments.text");
    assert_eq!(
        input,
        string_at(&estimate_call, "params.arguments.text"),
        "estimate and compress are given the same document"
    );
    let restored = parse(&replies[4]);
    assert_eq!(string_at(&restored, "result.structuredContent.text"), input);
    let content = path(&restored, "result.content").as_array().unwrap();
    assert_eq!(
        content[0].get("text").and_then(json::Value::as_str),
        Some(input)
    );

    // The prose beside the rendering quotes these; the reply lines pin them too.
    assert_eq!(input.len(), 290);
    let compress = parse(&replies[3]);
    let stats = path(&compress, "result.structuredContent.stats");
    for (key, expected) in [
        ("bytesBefore", 290),
        ("bytesAfter", 233),
        ("estTokensBefore", 174),
        ("estTokensAfter", 136),
        ("tokenizerId", 0),
    ] {
        assert_eq!(stats.get(key).and_then(json::Value::as_i64), Some(expected));
    }
    for phrase in [
        "instead of the 290-byte original — 233 bytes, and 136 tokens against 174",
        "(`tokenizerId` `0`,",
    ] {
        assert!(
            README.replace('\n', " ").contains(phrase),
            "{phrase:?} is gone"
        );
    }
}

#[test]
fn the_quoted_replies_anchor_only_part_of_what_they_quote() {
    // What the module doc says this file cannot see, as a measurement rather than a
    // caveat. Each row is (bytes the quoted pieces hold, bytes the real reply has); the
    // difference is the elided run, where the README can drift with every test here
    // green. The first number is how many pieces the quote is trimmed into, which says
    // how much of `matches_trimmed` each row exercises. Everything is pinned exactly, and
    // that is the point: a re-trim of a quote or a change in a reply moves a row, so the
    // coverage this file provides can never shrink unnoticed. The `tools/list` reply is
    // the one to look at -- the widest reply is the least anchored, at 205 bytes of 2521.
    const COVERAGE: [(usize, usize, usize); 6] = [
        (3, 214, 1321),
        (5, 205, 2521),
        (2, 452, 527),
        (6, 301, 1502),
        (4, 210, 921),
        (2, 311, 386),
    ];

    let replies = replay();
    let quoted = quoted_replies();
    assert_eq!(quoted.len(), COVERAGE.len(), "a row per quoted reply");
    let mut anchored_total = 0;
    let mut actual_total = 0;
    let mut past_the_loop = 0;
    for (index, (quote, (pieces, anchored, actual))) in quoted.iter().zip(COVERAGE).enumerate() {
        // `matches_trimmed` charges nothing for the `…` itself, so neither does this.
        let held: usize = quote.split('…').map(str::len).sum();
        assert_eq!(
            quote.split('…').count(),
            pieces,
            "reply {index} is trimmed into {} pieces, not {pieces}",
            quote.split('…').count()
        );
        if pieces > 2 {
            past_the_loop += 1;
        }
        assert_eq!(
            held, anchored,
            "reply {index} anchors {held} bytes, not {anchored}"
        );
        assert_eq!(
            replies[index].len(),
            actual,
            "reply {index} is {} bytes, not {actual}",
            replies[index].len()
        );
        anchored_total += held;
        actual_total += actual;
    }
    assert_eq!((anchored_total, actual_total), (1693, 7178));
    // The rows that reach the loop inside `matches_trimmed`, rather than only its prefix
    // and suffix check. Derived here so that no comment has to carry the count.
    assert_eq!(past_the_loop, 4, "quotes with more than two pieces");
    // Stated as the inequality rather than as "23.6%", so that the assertion says what it
    // means when a row moves: under a quarter of the quoted session is held down.
    assert!(
        anchored_total * 4 < actual_total,
        "{anchored_total} of {actual_total} bytes"
    );
}
