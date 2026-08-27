//! End-to-end sessions driven through the real stdio transport.
//!
//! The unit tests in `server.rs` exercise `handle_line` directly, which is the right
//! place for protocol detail. What they cannot show is that a whole conversation
//! survives the transport: that framing holds across several messages, that a reply
//! never carries an embedded newline, that a notification produces no line at all, and
//! that one bad message does not desynchronise the ones after it. That is what a
//! client actually depends on, so it is tested here, over byte streams.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use tokfold_mcp::{Server, json, stdio};

/// Runs a whole session and returns one entry per emitted line.
fn session(input: &str) -> Vec<json::Value> {
    let mut server = Server::new();
    let mut output = Vec::new();
    stdio::serve(input.as_bytes(), &mut output, &mut server)
        .expect("an in-memory session cannot fail on I/O");
    let text = String::from_utf8(output).expect("the transport emits UTF-8");
    text.lines()
        .map(|line| json::parse(line).expect("every emitted line is a JSON message"))
        .collect()
}

/// Runs a session against a server whose frame limit has been lowered to `max`.
///
/// The limit is a real setting an embedder can choose, not a test hook: nothing else in
/// the server reads it, so lowering it changes which answers are too large and nothing
/// about how any answer is computed. That is what makes it sound to reach the oversized
/// branch here with a few kilobytes instead of the 32 MiB the default would need — the
/// code under test is the same code, and the suite stays fast.
fn bounded_session(max: usize, input: &str) -> Vec<String> {
    let mut server = Server::new().with_max_message_bytes(max);
    let mut output = Vec::new();
    stdio::serve(input.as_bytes(), &mut output, &mut server)
        .expect("an in-memory session cannot fail on I/O");
    String::from_utf8(output)
        .expect("the transport emits UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The one field every client reads first.
fn id_of(message: &json::Value) -> Option<i64> {
    message.get("id").and_then(json::Value::as_i64)
}

fn result_of<'a>(message: &'a json::Value, path: &str) -> Option<&'a json::Value> {
    let mut current = message.get("result")?;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

#[test]
fn a_legacy_client_initializes_lists_tools_and_compresses() {
    let replies = session(concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"legacy","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tokfold_estimate","arguments":{"text":"{ \"a\" :  1 }"}}}"#,
        "\n",
    ));

    // Three requests, one notification: the notification is answered with silence, and
    // getting that wrong is a protocol violation no client would tolerate.
    assert_eq!(replies.len(), 3, "the notification must produce no line");
    assert_eq!(
        replies.iter().filter_map(id_of).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "replies arrive in request order"
    );

    assert_eq!(
        result_of(&replies[0], "protocolVersion").and_then(json::Value::as_str),
        Some("2025-06-18"),
        "a supported requested revision is echoed back"
    );
    assert_eq!(
        result_of(&replies[1], "tools")
            .and_then(json::Value::as_array)
            .map(<[json::Value]>::len),
        Some(3),
        "the catalogue holds compress, decompress, and estimate"
    );
    assert_eq!(
        result_of(&replies[2], "isError").and_then(json::Value::as_bool),
        Some(false),
        "estimating well-formed JSON succeeds"
    );
}

#[test]
fn a_modern_client_discovers_then_calls_without_a_handshake() {
    let replies = session(concat!(
        r#"{"jsonrpc":"2.0","id":"a","method":"server/discover"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":"b","method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
        "\n",
    ));

    assert_eq!(replies.len(), 2);
    assert!(
        result_of(&replies[0], "supportedVersions")
            .and_then(json::Value::as_array)
            .is_some_and(|versions| versions.contains(&json::Value::string("2026-07-28"))),
        "discovery advertises the modern revision"
    );
    // The modern era is stateless: no `initialize` was sent, and the call still works.
    assert!(
        replies[1].get("result").is_some(),
        "a modern client needs no handshake: {}",
        replies[1]
    );
    assert_eq!(
        result_of(&replies[1], "resultType").and_then(json::Value::as_str),
        Some("complete"),
        "every modern result is typed"
    );
}

#[test]
fn a_round_trip_through_the_transport_restores_the_original_bytes() {
    let original =
        r#"{"rows":[{"id":1,"name":"ada","ok":true},{"id":2,"name":"grace","ok":false}]}"#;
    let compress = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"tokfold_compress","arguments":{{"text":{}}}}}}}"#,
        json::Value::string(original)
    );
    let replies = session(&format!("{compress}\n"));
    let archive = result_of(&replies[0], "structuredContent.archive")
        .and_then(json::Value::as_str)
        .expect("a compressed result carries its recovery archive");

    let decompress = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"tokfold_decompress","arguments":{{"archive":{}}}}}}}"#,
        json::Value::string(archive)
    );
    let replies = session(&format!("{decompress}\n"));
    assert_eq!(
        result_of(&replies[0], "structuredContent.text").and_then(json::Value::as_str),
        Some(original),
        "the archive restores the input byte for byte"
    );
}

#[test]
fn a_bad_message_does_not_desynchronise_the_stream() {
    let replies = session(concat!(
        "not json at all\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        "\n",
    ));

    assert_eq!(
        replies.len(),
        2,
        "a parse failure is reported, not swallowed"
    );
    assert_eq!(
        replies[0]
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(json::Value::as_i64),
        Some(-32700),
        "unparseable input is a JSON-RPC parse error"
    );
    assert_eq!(
        replies[0].get("id"),
        None,
        "MCP narrows the JSON-RPC id to a string or a number, so an id that could not \
         be read is left out rather than sent as null"
    );
    assert_eq!(id_of(&replies[1]), Some(1), "the next request still works");
}

#[test]
fn every_reply_occupies_exactly_one_line() {
    // A newline inside a message would split it in two, and a line-reading client
    // would treat the halves as separate messages. The text that provokes it is the
    // obvious one: content that itself contains newlines.
    let text = "line one\nline two\r\nline three";
    let call = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"tokfold_compress","arguments":{{"text":{}}}}}}}"#,
        json::Value::string(text)
    );

    let mut server = Server::new();
    let mut output = Vec::new();
    stdio::serve(format!("{call}\n").as_bytes(), &mut output, &mut server).unwrap();

    let raw = String::from_utf8(output).unwrap();
    assert_eq!(
        raw.matches('\n').count(),
        1,
        "one reply, one terminator, no embedded newline: {raw:?}"
    );
    assert!(raw.ends_with('\n'), "every message is newline-terminated");
}

/// One request driven over the transport, with both framed lines measured.
struct Frame {
    request_bytes: usize,
    reply_bytes: usize,
    reply: json::Value,
}

impl Frame {
    /// Reply size as a percentage of request size, terminators counted on both sides.
    ///
    /// Kept in integer percent rather than a float ratio: what is asserted is a bound,
    /// not a value, and integers say it without a lossy cast.
    fn percent_of_request(&self) -> usize {
        self.reply_bytes * 100 / self.request_bytes
    }
}

/// Sends one framed request and measures the framed reply it produced.
fn measure(request: &str) -> Frame {
    let line = format!("{request}\n");
    let mut server = Server::new();
    let mut output = Vec::new();
    stdio::serve(line.as_bytes(), &mut output, &mut server)
        .expect("an in-memory session cannot fail on I/O");
    let text = String::from_utf8(output).expect("the transport emits UTF-8");
    Frame {
        request_bytes: line.len(),
        reply_bytes: text.len(),
        reply: json::parse(text.trim_end_matches('\n')).expect("the reply is one JSON message"),
    }
}

/// Renders a `tools/call` line for a text-taking tool.
fn call_line(tool: &str, text: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{tool}","arguments":{{"text":{}}}}}}}"#,
        json::Value::string(text)
    )
}

#[test]
fn a_reply_outgrows_its_request_by_a_bounded_multiple_on_both_tool_paths() {
    // Nothing else weighs a reply — `every_reply_occupies_exactly_one_line` counts
    // lines, not bytes — and the weight is what the transport is asymmetric about.
    //
    // A reply is always larger than the call that produced it, by construction: the
    // payload comes back twice, once as `content` for the model and once as
    // `structuredContent` for the program, and the compress path adds a base64 archive
    // that inflates its bytes by a third. So the structural ceiling is about 2x when
    // the engine declines and about 3.3x when it compresses but the archive barely
    // shrinks. The bounds below sit just above those two figures. They are a ratchet:
    // a change that adds a third echo of the payload, or that stops compressing the
    // archive, trips here rather than in a client's memory.
    //
    // Kilobytes are deliberate. The envelope is a few hundred fixed bytes, so on both
    // paths the ratio is flat in the input size — it held within 2% across a 16x span
    // when this was written — and a multi-megabyte case would only make the suite slow
    // to say the same thing.
    //
    // The second assertion is the one that matters beyond regression, and it is the
    // reason the frame limit applies to what is written as well as what is read. At the
    // ratio measured here, a request well inside `MAX_LINE_BYTES` already has an answer
    // that is not: the largest request whose reply still fits is a fraction of what the
    // reader itself admits. This pins that the multiplier is real rather than a claim
    // about hypothetical inputs — which is what makes the refusal pinned in
    // `a_reply_that_would_not_fit_the_frame_is_refused_by_id_rather_than_truncated`
    // reachable through a legal request instead of dead code.
    let rows: Vec<String> = (0..120)
        .map(|index| {
            format!(
                r#"{{"id":{index},"name":"user{index}","active":true,"score":{}}}"#,
                index * 7
            )
        })
        .collect();
    let compressible = format!(r#"{{"rows":[{}]}}"#, rows.join(","));
    // Not JSON, so core declines it and the tool echoes it back unchanged. No newlines
    // in it: they would be escaped identically on both sides and only add noise.
    let declined = "2026-08-12T00:00:00Z INFO request served in 12ms; ".repeat(120);

    let compressed = measure(&call_line("tokfold_compress", &compressible));
    let passthrough = measure(&call_line("tokfold_compress", &declined));

    // Which path ran is asserted, not assumed: an input that quietly stopped
    // compressing would move both measurements onto the same path and the bounds would
    // still hold, measuring nothing.
    assert_eq!(
        result_of(&compressed.reply, "structuredContent.compressed"),
        Some(&json::Value::Bool(true)),
        "the compress path was not exercised: {}",
        compressed.reply
    );
    assert!(
        result_of(&compressed.reply, "structuredContent.archive").is_some(),
        "a compressed reply carries the base64 archive that dominates its size"
    );
    assert_eq!(
        result_of(&passthrough.reply, "structuredContent.compressed"),
        Some(&json::Value::Bool(false)),
        "the passthrough path was not exercised: {}",
        passthrough.reply
    );

    for (path, frame, bound) in [
        ("compress", &compressed, 400),
        ("passthrough", &passthrough, 250),
    ] {
        let observed = frame.percent_of_request();
        assert!(
            observed < bound,
            "the {path} path answered {} request bytes with {} reply bytes ({observed}% \
             of the request), above the {bound}% bound",
            frame.request_bytes,
            frame.reply_bytes
        );
        assert!(
            observed > 100,
            "the {path} path is expected to answer larger than it was asked; \
             {observed}% means the reply shape changed"
        );
        // At this multiplier, the largest request whose reply still fits the frame is a
        // fraction of what the frame itself admits as input.
        let largest_answerable_request = stdio::MAX_LINE_BYTES * 100 / observed;
        assert!(
            largest_answerable_request < stdio::MAX_LINE_BYTES,
            "the {path} reply no longer outgrows its request, so a readable request can \
             no longer produce an unwritable answer and this test is obsolete"
        );
    }
}

#[test]
fn a_reply_that_would_not_fit_the_frame_is_refused_by_id_rather_than_truncated() {
    // The other half of the measurement above. Because a reply outgrows its request,
    // there are requests this transport will read whose answers it could not write, and
    // what happens then is a contract: the client gets a well-formed error carrying its
    // own id, on one line, small enough to pass back through the same reader. Anything
    // else is worse than a failure — a truncated frame desynchronises the stream, and
    // silence leaves the call to time out.
    //
    // Driven through `stdio::serve` rather than `handle_line` on purpose: the unit tests
    // pin the ladder, this pins that the frame the client actually reads off the wire is
    // the bounded one.
    let payload = "x".repeat(8192);
    let session = bounded_session(
        4096,
        &format!("{}\n", call_line("tokfold_compress", &payload)),
    );

    assert_eq!(session.len(), 1, "exactly one line comes back");
    let line = &session[0];
    assert!(
        line.len() <= 4096,
        "the refusal must itself fit the frame: {} bytes",
        line.len()
    );
    assert!(
        !line.contains(&payload),
        "an error that echoed the payload would be the same bug again"
    );

    let reply = json::parse(line).expect("the refusal is a JSON message");
    assert_eq!(
        id_of(&reply),
        Some(1),
        "the client's own id, or the call hangs"
    );
    assert!(reply.get("result").is_none(), "no partial result: {reply}");
    assert_eq!(
        reply
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(json::Value::as_i64),
        Some(-32602),
        "the arguments are what the client can make smaller"
    );
}

#[test]
fn a_reply_that_fits_is_delivered_untouched_by_the_frame_limit() {
    // The guard on the test above: a bound that quietly rewrote ordinary replies would
    // pass every assertion there and still be a serious defect. A limit set exactly at
    // the length of the real answer must deliver that answer byte for byte — the same
    // bytes an unbounded server would have written.
    let line = call_line("tokfold_compress", r#"{"rows":[{"a":1},{"a":2},{"a":3}]}"#);
    let unrestricted = session(&format!("{line}\n"));
    assert_eq!(unrestricted.len(), 1);
    let expected = unrestricted[0].to_string();

    let exact = bounded_session(expected.len(), &format!("{line}\n"));
    assert_eq!(exact.len(), 1, "the answer still comes back");
    assert_eq!(
        exact[0], expected,
        "a reply at exactly the limit is the unmodified reply"
    );

    // One byte less is the first size that cannot be delivered, which is what makes the
    // comparison above a boundary rather than a coincidence.
    let short = bounded_session(expected.len() - 1, &format!("{line}\n"));
    assert!(
        json::parse(&short[0]).unwrap().get("error").is_some(),
        "one byte under the exact fit must refuse: {}",
        short[0]
    );
}

#[test]
fn an_unsupported_revision_is_refused_with_the_versions_that_exist() {
    let replies = session(concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2099-01-01","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
        "\n",
    ));

    let error = replies[0]
        .get("error")
        .expect("an unknown revision is refused");
    assert_eq!(
        error.get("code").and_then(json::Value::as_i64),
        Some(-32022),
        "the spec reserves -32022 for this"
    );
    // A bare refusal leaves the client guessing; the data payload is what lets it
    // pick a revision and retry.
    assert_eq!(
        error
            .get("data")
            .and_then(|data| data.get("requested"))
            .and_then(json::Value::as_str),
        Some("2099-01-01")
    );
    assert!(
        error
            .get("data")
            .and_then(|data| data.get("supported"))
            .and_then(json::Value::as_array)
            .is_some_and(|supported| !supported.is_empty()),
        "the refusal names what the server does speak"
    );
}

#[test]
fn a_node_dense_line_is_refused_although_it_fits_the_frame() {
    // The byte cap and the node budget are two different bounds, and this line proves
    // it: about 8.4 MB, a quarter of `MAX_MESSAGE_BYTES`, and still refused — because
    // `[0,0,0,…]` spends two input bytes per value and would have expanded into
    // 134 MiB of tree. Uncapped, the same shape at the frame limit reaches half a
    // gigabyte at rest and about 1.5 GiB across the last `Vec` doubling, and an
    // allocation failure aborts the process rather than unwinding into the bulkhead.
    let count = json::MAX_NODES; // the array itself is the value that tips it over
    let mut dense = String::with_capacity(count * 2 + 2);
    dense.push('[');
    for index in 0..count {
        if index > 0 {
            dense.push(',');
        }
        dense.push('0');
    }
    dense.push(']');
    assert!(
        dense.len() < tokfold_mcp::MAX_MESSAGE_BYTES,
        "fits the frame"
    );

    let mut input = dense;
    input.push('\n');
    // A well-formed call after it: a refusal must not cost the client its session.
    input.push_str(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#);
    input.push('\n');

    let replies = session(&input);
    assert_eq!(replies.len(), 2);

    let error = replies[0].get("error").expect("the dense line is refused");
    assert_eq!(
        error.get("code").and_then(json::Value::as_i64),
        Some(-32700),
        "a line the parser refuses is a parse error, whatever made it refuse"
    );
    // The id member is absent, exactly as for any other unparseable line: the parse
    // failed, so there is no tree to read one out of, and guessing would address an
    // answer to a call the client may not have made. One rule for every parse failure,
    // not a second one invented for this bound.
    assert_eq!(replies[0].get("id"), None);
    assert_eq!(id_of(&replies[0]), None);

    assert_eq!(
        id_of(&replies[1]),
        Some(7),
        "the session survives the refusal"
    );
}
