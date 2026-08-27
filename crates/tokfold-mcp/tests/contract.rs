//! The wire contract a client is entitled to depend on.
//!
//! `session.rs` shows that a whole conversation survives the transport. This file pins
//! the values *inside* those messages: the numeric error codes a client branches on,
//! the cache directives it caches by, the failure codes it shows a user, and the
//! promise that a rendering plus an archive restores the input byte for byte whatever
//! the input contained.
//!
//! Everything here is a bare number or a bare string. That is exactly why it needs its
//! own file: a swapped error code, a truncated cache lifetime or a renamed failure code
//! is invisible in review, breaks a client silently, and would not fail a single other
//! test in this workspace. The unit tests next to the code mostly assert that such a
//! field is *present*; presence is not the contract.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use tokfold_mcp::{Server, base64, json, protocol, stdio};

/// Runs a whole session over the real transport and returns one entry per emitted line.
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

/// Every emitted line, unparsed — the bytes a client actually reads.
fn raw_session(input: &str) -> Vec<String> {
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

/// The single reply to a single request.
fn reply(line: &str) -> json::Value {
    let mut replies = session(&format!("{line}\n"));
    assert_eq!(replies.len(), 1, "expected exactly one reply to {line}");
    replies.remove(0)
}

/// One way of damaging an archive: the refusal code it must produce, the mutation
/// itself, and why a caller needs that refusal told apart from the others.
type Damage<'a> = (&'a str, &'a dyn Fn(&mut Vec<u8>), &'a str);

/// The `code` of an error reply, or `None` if the reply was a success.
fn error_code(message: &json::Value) -> Option<i64> {
    message
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(json::Value::as_i64)
}

/// The `id` of a reply, rendered as the JSON text it was sent as.
///
/// Comparing the text rather than a decoded number keeps the two id types apart: `7` and
/// `"7"` are different ids to a client, and a helper that returned an integer either way
/// would let the server change one into the other unnoticed.
fn reply_id(message: &json::Value) -> Option<String> {
    message.get("id").map(json::Value::to_string)
}

/// Builds a `tools/call` line for one tool and one already-encoded argument object.
fn call_line(id: i64, name: &str, arguments: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{arguments}}}}}"#
    )
}

/// Compresses `text` and returns the `structuredContent` of the result.
fn compress(text: &str) -> json::Value {
    let arguments = format!(r#"{{"text":{}}}"#, json::Value::string(text));
    let message = reply(&call_line(1, "tokfold_compress", &arguments));
    message
        .get("result")
        .and_then(|result| result.get("structuredContent"))
        .cloned()
        .unwrap_or_else(|| panic!("compress returned no structured content: {message}"))
}

/// Decompresses an archive and returns the `structuredContent` of the result.
fn decompress(archive: &str) -> json::Value {
    let arguments = format!(r#"{{"archive":{}}}"#, json::Value::string(archive));
    let message = reply(&call_line(2, "tokfold_decompress", &arguments));
    message
        .get("result")
        .and_then(|result| result.get("structuredContent"))
        .cloned()
        .unwrap_or_else(|| panic!("decompress returned no structured content: {message}"))
}

#[test]
fn each_kind_of_malformed_request_carries_its_documented_wire_code() {
    // A client does not read the message text; it switches on the number. These five
    // are the whole vocabulary a client can provoke with a message it wrote, and each
    // one tells it something different to do: retry as-is, fix the envelope, stop
    // calling that method, fix the arguments, or renegotiate the revision. Swapping any
    // two of them sends a client down the wrong branch while every reply still looks
    // well-formed. (The server emits one further code that no request can reach; the
    // tail of this test says why it is held apart.)
    let cases: &[(&str, i64, &str)] = &[
        ("this is not json", -32700, "unparseable input"),
        (
            r#"{"jsonrpc":"2.0","id":1}"#,
            -32600,
            "a request with no method",
        ),
        (
            r#"{"jsonrpc":"1.0","id":1,"method":"tools/list"}"#,
            -32600,
            "a request that is not JSON-RPC 2.0",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"no/such/method"}"#,
            -32601,
            "an unknown method",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tokfold_nonexistent","arguments":{}}}"#,
            -32602,
            "a call naming a tool that does not exist",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1999-01-01","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
            -32022,
            "a revision this server does not implement",
        ),
    ];

    for (line, expected, what) in cases {
        assert_eq!(
            error_code(&reply(line)),
            Some(*expected),
            "{what} must be reported as {expected}"
        );
    }

    // The vocabulary is closed, and it has to stay out of the band the specification
    // grandfathered for pre-existing implementations. Widening it silently — a sixth
    // code appearing for a case the table above already covers — is how a client ends
    // up with an `unknown error` branch it can do nothing with.
    let mut codes: Vec<i64> = cases.iter().map(|(_, code, _)| *code).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(
        codes,
        vec![-32700, -32602, -32601, -32600, -32022],
        "the five distinct codes a request can provoke"
    );
    assert!(
        !codes.iter().any(|code| (-32019..=-32000).contains(code)),
        "the grandfathered range must not be used by new code"
    );

    // The sixth code the module defines is deliberately absent from the table above:
    // `INTERNAL_ERROR` is only ever emitted by the panic bulkhead, so no well-formed or
    // malformed request can provoke it and no test can drive it from the outside. Its
    // value is still contract — it is what a client sees on the one path where the
    // server has already lost the thread — and there is nothing else in the suite that
    // would notice if it drifted, so it is asserted directly.
    assert_eq!(
        protocol::error_code::INTERNAL_ERROR,
        -32603,
        "the bulkhead reports the standard JSON-RPC internal error"
    );
}

#[test]
fn a_refused_envelope_is_answered_with_the_id_the_client_sent() {
    // The code above is only half of what a client needs. It matches a reply to a call
    // by id and by nothing else, so a refusal that arrives without one is not a failed
    // call but a hung one: the entry stays in the pending table until the client's own
    // timeout fires, and the error that did arrive belongs to no call it can name.
    //
    // Three of the grounds for refusing an envelope leave the id perfectly readable —
    // the `jsonrpc` member is wrong, the `method` is missing or not a string, the
    // `params` is neither object nor array — and each is exercised here with both id
    // shapes JSON-RPC allows, because recovering an id is a match on its type.
    let cases: &[(&str, &str, &str)] = &[
        (
            r#"{"jsonrpc":"1.0","id":42,"method":"ping"}"#,
            "42",
            "a wrong protocol version, integer id",
        ),
        (
            r#"{"jsonrpc":"1.0","id":"abc","method":"ping"}"#,
            r#""abc""#,
            "a wrong protocol version, string id",
        ),
        (
            r#"{"jsonrpc":"2.0","id":42,"method":5}"#,
            "42",
            "a method that is not a string, integer id",
        ),
        (
            r#"{"jsonrpc":"2.0","id":"abc"}"#,
            r#""abc""#,
            "a missing method, string id",
        ),
        (
            r#"{"jsonrpc":"2.0","id":42,"method":"ping","params":[]}"#,
            "42",
            "params that are not an object, integer id",
        ),
        (
            r#"{"jsonrpc":"2.0","id":"abc","method":"ping","params":7}"#,
            r#""abc""#,
            "params that are not an object, string id",
        ),
    ];

    for (line, expected, what) in cases {
        let message = reply(line);
        assert_eq!(
            error_code(&message),
            Some(-32600),
            "{what} is an invalid request"
        );
        assert_eq!(
            reply_id(&message).as_deref(),
            Some(*expected),
            "{what} must be answered with the id it carried, not anonymously: {message}"
        );
    }
}

#[test]
fn a_refusal_carries_no_id_when_the_message_carried_none_worth_echoing() {
    // The other half of the same contract, and the reason the id is recovered by a match
    // on its type rather than by best effort. Guessing an id is worse than admitting
    // there is none: a reply addressed to an id the client never sent is, on the
    // client's side, indistinguishable from a reply to a call it is waiting on, so it
    // does not merely fail to resolve one call — it resolves the wrong one.
    let cases: &[(&str, &str)] = &[
        ("this is not json", "a line that is not JSON at all"),
        ("5", "a message that is not an object"),
        (r#""a string""#, "a message that is a bare string"),
        (
            r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
            "a null id, which JSON-RPC does not allow a request to use",
        ),
        (
            r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
            "a fractional id, which is not one of the two permitted types",
        ),
        (
            r#"{"jsonrpc":"2.0","id":{},"method":"ping"}"#,
            "an id that is a container",
        ),
        ("[]", "an empty batch, which has no member to attribute"),
    ];

    for (line, what) in cases {
        let message = reply(line);
        assert!(
            error_code(&message).is_some(),
            "{what} must still be refused: {message}"
        );
        assert!(
            message.get("id").is_none(),
            "{what} must not be answered with an invented id: {message}"
        );
    }
}

#[test]
fn a_batch_mixing_valid_and_refused_members_answers_each_one_by_id() {
    // A batch is where an unaddressed error stops being merely unhelpful. The client
    // gets one array back, its members are not required to correspond positionally to
    // the requests — notifications contribute nothing — and so the id is the only thing
    // that says which member the failure belongs to. One reply short of an id makes the
    // whole batch ambiguous, not just that member.
    let batch = concat!(
        "[",
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"},"#,
        r#"{"jsonrpc":"2.0","id":"two","method":"ping","params":[]},"#,
        r#"{"jsonrpc":"2.0","method":"ping"},"#,
        r#"{"jsonrpc":"1.0","id":4,"method":"ping"}"#,
        "]"
    );
    let mut replies = session(&format!("{batch}\n"));
    assert_eq!(replies.len(), 1, "a batch is answered on a single line");
    let json::Value::Array(members) = replies.remove(0) else {
        panic!("a batch is answered with an array");
    };
    assert_eq!(
        members.len(),
        3,
        "three of the four members are requests; the notification is answered with \
         silence"
    );

    let ids: Vec<Option<String>> = members.iter().map(reply_id).collect();
    assert_eq!(
        ids,
        vec![
            Some("1".to_owned()),
            Some(r#""two""#.to_owned()),
            Some("4".to_owned())
        ],
        "every member of the answer names the call it belongs to: {members:?}"
    );
    assert_eq!(
        members.iter().map(error_code).collect::<Vec<_>>(),
        vec![None, Some(-32600), Some(-32600)],
        "one member succeeded and two were refused, and the ids say which"
    );
}

#[test]
fn the_tool_catalogue_advertises_a_lifetime_that_is_worth_caching() {
    let listed = reply(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
    let result = listed.get("result").expect("tools/list succeeds");

    // The unit test next to this code asserts `ttlMs` is *some* integer. That passes
    // just as happily at zero, which tells a client to re-fetch a compile-time
    // constant catalogue on every single turn — the exact cost the field exists to
    // avoid. The value is an hour, and the point of pinning it is that shortening it
    // is a real regression with no visible symptom.
    assert_eq!(
        result.get("ttlMs").and_then(json::Value::as_i64),
        Some(3_600_000),
        "the catalogue is a compile-time constant and is cacheable for an hour"
    );
    assert_eq!(
        result.get("cacheScope").and_then(json::Value::as_str),
        Some("public"),
        "nothing in the catalogue is client-specific, so the cache may be shared"
    );
    assert!(
        result.get("nextCursor").is_none(),
        "the catalogue is returned whole; a cursor would send a client paging for a \
         page that does not exist"
    );
}

#[test]
fn the_compress_tool_warns_that_the_archive_is_not_in_the_content_block() {
    // The one thing a client author cannot discover by reading `content`: the archive
    // is not there. `tokfold_compress` returns it in `structuredContent.archive` only,
    // while `content` carries the rendering alone — so a spec-conformant client that
    // reads only `content` (the common shape for older ones) embeds a compressed
    // rendering and has thrown away the only means of ever recovering the original.
    //
    // The catalogue is the sole channel that reaches that author, and this warning is
    // prose inside a string constant: delete it and the tools still list, still call,
    // still round-trip, and every other test in this workspace stays green. Hence a
    // pin, in the file that pins bare strings.
    let listed = reply(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
    let tools = listed
        .get("result")
        .and_then(|result| result.get("tools"))
        .and_then(json::Value::as_array)
        .expect("tools/list returns the catalogue");

    let description = tools
        .iter()
        .find(|tool| tool.get("name").and_then(json::Value::as_str) == Some("tokfold_compress"))
        .and_then(|tool| tool.get("description"))
        .and_then(json::Value::as_str)
        .expect("the compress tool is advertised with a description");

    assert!(
        description.contains("structuredContent.archive"),
        "the description must name the field the archive actually arrives in: \
         {description:?}"
    );
    assert!(
        description.contains("content"),
        "naming the field is not enough — the description must contrast it with the \
         `content` block the client may be reading instead: {description:?}"
    );
    assert!(
        description.contains("never decompress"),
        "the description must state the consequence, not merely the field layout: a \
         content-only client keeps text it can never decompress: {description:?}"
    );

    // And the loop closes from the other side: a client holding a compress result has
    // to be told where to read the archive from when it calls decompress.
    let archive_argument = tools
        .iter()
        .find(|tool| tool.get("name").and_then(json::Value::as_str) == Some("tokfold_decompress"))
        .and_then(|tool| tool.get("inputSchema"))
        .and_then(|schema| schema.get("properties"))
        .and_then(|properties| properties.get("archive"))
        .and_then(|archive| archive.get("description"))
        .and_then(json::Value::as_str)
        .expect("the decompress tool documents its one argument");
    assert!(
        archive_argument.contains("structuredContent.archive"),
        "the archive argument must say where that archive is read from: \
         {archive_argument:?}"
    );
}

#[test]
fn every_json_escape_class_survives_a_round_trip_unchanged() {
    // The encoder rewrites strings, so every escape form the parser accepts has to come
    // back exactly as it went in. These are the classes that differ from one another
    // only in a single arm of the escape handling: the two-character escapes, a control
    // character below 0x20 that JSON *requires* be escaped, a character that may be
    // escaped or not, a non-ASCII character, and an astral-plane character that arrives
    // as a surrogate pair.
    let cases: &[(&str, &str)] = &[
        (r#"{"k":"a\"b"}"#, "an escaped quote"),
        (r#"{"k":"a\\b"}"#, "an escaped backslash"),
        (r#"{"k":"a\/b"}"#, "an escaped solidus, which is optional"),
        (r#"{"k":"a\bb"}"#, "a backspace"),
        (r#"{"k":"a\fb"}"#, "a form feed"),
        (r#"{"k":"a\nb"}"#, "a line feed"),
        (r#"{"k":"a\rb"}"#, "a carriage return"),
        (r#"{"k":"a\tb"}"#, "a tab"),
        (
            r#"{"k":"a\u0000b"}"#,
            "a NUL, which JSON requires be escaped",
        ),
        (
            r#"{"k":"a\u001Fb"}"#,
            "the last control character, escaped in upper case",
        ),
        (
            r#"{"k":"a\qb"}"#,
            "an escape JSON does not define, which must be declined rather than repaired",
        ),
        (r#"{"k":"Ω ∑ é"}"#, "non-ASCII text carried literally"),
        (
            r#"{"k":"\ud83d\ude00"}"#,
            "an astral character as a surrogate pair",
        ),
        (r#"{"k":"😀"}"#, "the same character carried literally"),
    ];

    for (original, what) in cases {
        let outcome = compress(original);
        let restored = if outcome.get("compressed").and_then(json::Value::as_bool) == Some(true) {
            let archive = outcome
                .get("archive")
                .and_then(json::Value::as_str)
                .unwrap_or_else(|| panic!("{what}: a compressed result carries its archive"));
            decompress(archive)
                .get("text")
                .and_then(json::Value::as_str)
                .unwrap_or_else(|| panic!("{what}: decompression returned no text"))
                .to_owned()
        } else {
            // A declined input is echoed rather than dropped, so the same equality
            // holds on that path and this test covers both.
            outcome
                .get("rendering")
                .and_then(json::Value::as_str)
                .unwrap_or_else(|| panic!("{what}: a declined input is still returned"))
                .to_owned()
        };
        assert_eq!(&restored, original, "{what} did not survive the round trip");
    }
}

#[test]
fn each_escape_is_written_in_its_shortest_canonical_spelling() {
    // JSON lets a control character be written either as its own two-character escape or
    // as the six-character `u`-form, and every parser must accept both. That freedom
    // stops at this crate's door. The library promises byte-identical output for
    // identical input so that an upstream prompt cache keeps hitting, and a server that
    // spells a tab one way today and the other way after an innocuous refactor breaks
    // that promise without changing a single parsed value anywhere.
    //
    // No round-trip test can see this: both spellings parse back to the same string, so
    // `every_json_escape_class_survives_a_round_trip_unchanged` above stays green either
    // way. The spelling has to be asserted on the bytes themselves, which is what this
    // does.
    let two_character: &[(char, &str, &str)] = &[
        ('"', "\\\"", "a quote"),
        ('\\', "\\\\", "a backslash"),
        ('\n', "\\n", "a line feed"),
        ('\r', "\\r", "a carriage return"),
        ('\t', "\\t", "a tab"),
        ('\x08', "\\b", "a backspace"),
        ('\x0c', "\\f", "a form feed"),
    ];

    for (raw, canonical, what) in two_character {
        let rendered = json::Value::string(format!("a{raw}b")).to_string();
        assert_eq!(
            rendered,
            format!("\"a{canonical}b\""),
            "{what} must keep its two-character form"
        );
    }

    // Every other character below 0x20 has no short form, so it must use the
    // six-character one — in lower-case hex, four digits wide, naming its own code
    // point. Spelling the expected output out as a literal would put the exact byte
    // sequence this repo's tooling has repeatedly mangled into a source file, so it is
    // checked by shape and value instead.
    for code in 0_u32..0x20 {
        let raw = char::from_u32(code).expect("every value below 0x20 is a character");
        if two_character.iter().any(|(short, _, _)| *short == raw) {
            continue;
        }
        let rendered = json::Value::string(raw.to_string()).to_string();
        let body = &rendered[1..rendered.len() - 1];
        assert_eq!(
            body.chars().count(),
            6,
            "0x{code:02x} has no short escape, so it must use the six-character one: {rendered}"
        );
        let mut chars = body.chars();
        assert_eq!(chars.next(), Some('\\'), "escape starts with a backslash");
        assert_eq!(chars.next(), Some('u'), "0x{code:02x} uses the `u` form");
        let digits: String = chars.collect();
        assert!(
            digits
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "the digits are lower-case hex, not {digits}"
        );
        assert_eq!(
            u32::from_str_radix(&digits, 16).ok(),
            Some(code),
            "the escape names the code point it stands for"
        );
    }

    // At or above 0x20 only the quote and the backslash are escaped at all. The solidus
    // in particular is left alone: escaping it is legal JSON, and one character longer
    // for no gain, in a crate whose entire purpose is to emit fewer tokens.
    for literal in ['/', '~', '\x7f', 'Ω', '😀'] {
        let rendered = json::Value::string(literal.to_string()).to_string();
        assert_eq!(
            rendered,
            format!("\"{literal}\""),
            "{literal:?} is written as itself"
        );
    }

    // And the renderer checked above is the one the transport really uses. A tab handed
    // to a tool comes back over the wire in the same two-character form, which is the
    // only place the caching promise is actually made.
    let arguments = format!(r#"{{"text":{}}}"#, json::Value::string("a\tb"));
    let lines = raw_session(&format!(
        "{}\n",
        call_line(1, "tokfold_compress", &arguments)
    ));
    assert_eq!(lines.len(), 1, "one call, one reply");
    assert!(
        lines[0].contains("a\\tb"),
        "the wire spells the tab the way the renderer does: {}",
        lines[0]
    );
}

#[test]
fn a_damaged_archive_is_refused_with_a_code_that_names_the_damage() {
    // These codes are the tool's contract: a caller branches on them to decide whether
    // to re-encode its transport, to give up, or to ask for the data again. They are
    // also the one place where a wrong answer is worse than an error — silently
    // returning damaged bytes as if they were the original would corrupt whatever the
    // caller does next.
    // Note the key: a *decompression* failure names itself under `code`, while a
    // declined *compression* uses `reasonCode`. The two are separate outcomes with
    // separate shapes, and both spellings are pinned — here and in the decline test
    // below — so neither drifts to match the other without a deliberate edit.
    let not_base64 = decompress("not base64 at all !!!");
    assert_eq!(
        not_base64.get("code").and_then(json::Value::as_str),
        Some("invalid_base64"),
        "a transport-level problem is named as one: {not_base64}"
    );

    let not_an_archive = base64::encode(b"this decodes cleanly but is not an archive");
    let bad_magic = decompress(&not_an_archive);
    assert_eq!(
        bad_magic.get("code").and_then(json::Value::as_str),
        Some("bad_magic"),
        "well-formed base64 of something else is a format problem, not a transport one: \
         {bad_magic}"
    );

    // A real archive with one bit flipped in its body. The specific code depends on
    // which field the flip lands in, so what is pinned is the part that matters: it is
    // refused, it is refused with one of the documented codes rather than the catch-all,
    // and no text comes back.
    let original = r#"{"rows":[{"id":1,"name":"ada"},{"id":2,"name":"grace"}]}"#;
    let archive = compress(original)
        .get("archive")
        .and_then(json::Value::as_str)
        .expect("this input compresses")
        .to_owned();
    let mut bytes = base64::decode(&archive).expect("the server emits valid base64");
    let last = bytes.len() - 1;
    bytes[last] ^= 0b0000_0001;
    let tampered = decompress(&base64::encode(&bytes));
    let code = tampered
        .get("code")
        .and_then(json::Value::as_str)
        .unwrap_or_else(|| panic!("a tampered archive must be refused: {tampered}"));
    assert!(
        ["corrupt", "checksum_mismatch", "reserved_bits_set"].contains(&code),
        "a tampered archive must be named by a documented code, got {code:?}"
    );
    assert!(
        tampered.get("text").is_none(),
        "a refusal must not also hand back bytes: {tampered}"
    );
}

#[test]
fn each_kind_of_archive_damage_is_named_by_its_own_code() {
    // The test above proves a damaged archive is refused by *one of* the documented
    // codes. That is the weaker half of the contract. The other half is that the codes
    // are distinguishable: a caller is supposed to retry on one and give up on another,
    // and a mapping that collapsed two of them into one — or into the catch-all — would
    // still satisfy the "refused with a documented code" assertion while making the
    // distinction useless. So each kind of damage is provoked deliberately here.
    //
    // Doing that means writing into the header by offset. The layout is frozen in
    // tokfold-core's `format` module and is part of the archive format, not an internal
    // detail:
    //
    //   magic[4] | version:u8 | encoder_id:u8 | tokenizer_id:u16 | flags:u16
    //            | original_len:uleb128 | checksum[32] | payload...
    const VERSION_OFFSET: usize = 4;
    const FLAGS_OFFSET: usize = 8;
    const ORIGINAL_LEN_OFFSET: usize = 10;

    let original = r#"{"rows":[{"id":1,"name":"ada"},{"id":2,"name":"grace"}]}"#;
    let archive = compress(original)
        .get("archive")
        .and_then(json::Value::as_str)
        .expect("this input compresses")
        .to_owned();
    let good = base64::decode(&archive).expect("the server emits valid base64");

    assert_eq!(&good[..4], b"TKFD", "the archive opens with the magic");
    // This input is well under 128 bytes, so its length varint occupies exactly one
    // byte and the checksum starts immediately after it. Asserting that here means a
    // future input change cannot silently move the offsets used below.
    assert!(
        good[ORIGINAL_LEN_OFFSET] < 0x80,
        "the length varint is a single byte for an input this small"
    );
    let checksum_offset = ORIGINAL_LEN_OFFSET + 1;

    let damage: &[Damage] = &[
        (
            "unsupported_version",
            &|bytes: &mut Vec<u8>| bytes[VERSION_OFFSET] = 2,
            "an archive from a newer build must say so, so the caller knows to upgrade \
             rather than to treat its data as lost",
        ),
        (
            "reserved_bits_set",
            // Bit 3 of the flags field is the lowest reserved-must-be-zero bit.
            &|bytes: &mut Vec<u8>| bytes[FLAGS_OFFSET] |= 0b0000_1000,
            "a flag this build does not understand is refused rather than ignored, or a \
             future format extension would be silently dropped",
        ),
        (
            "checksum_mismatch",
            &|bytes: &mut Vec<u8>| bytes[checksum_offset] ^= 0b0000_0001,
            "the payload still decodes, so only the integrity check stands between the \
             caller and bytes that are not what was compressed",
        ),
        (
            "corrupt",
            // Cut the archive off inside its own header, before the checksum is complete.
            &|bytes: &mut Vec<u8>| bytes.truncate(ORIGINAL_LEN_OFFSET + 1),
            "a structurally incomplete archive is a different problem from one that is \
             complete but wrong, and a caller may reasonably retry only one of them",
        ),
    ];

    for (expected, damage_fn, why) in damage {
        let mut bytes = good.clone();
        damage_fn(&mut bytes);
        assert_ne!(
            bytes, good,
            "{expected}: the damage must actually change bytes"
        );
        let refused = decompress(&base64::encode(&bytes));
        assert_eq!(
            refused.get("code").and_then(json::Value::as_str),
            Some(*expected),
            "{expected}: {why} -- got {refused}"
        );
        assert!(
            refused.get("text").is_none(),
            "{expected}: a refusal must not also hand back bytes: {refused}"
        );
    }

    // And the undamaged archive still restores the input, so the four cases above are
    // failing for the reason claimed and not because the fixture was broken to begin
    // with.
    assert_eq!(
        decompress(&archive)
            .get("text")
            .and_then(json::Value::as_str),
        Some(original),
        "the pristine archive must still round-trip"
    );
}

#[test]
fn the_transport_line_limit_is_the_one_the_documentation_promises() {
    // The limit stays above the engine's own 16 MiB input ceiling, which is what makes a
    // maximum-size *request* readable. It does not follow that the reply to one fits, and
    // measurement through `stdio::serve` says it does not. A reply is 2.00x the request
    // on the passthrough path — the input comes back twice, once as `content` and once as
    // `structuredContent.rendering` — and 3.33x when the input is compressed but does not
    // shrink: the same two copies plus a base64 archive, which is a third larger than the
    // bytes it carries.
    //
    // Both ratios cross the cap from underneath it. A `tokfold_compress` call whose
    // `text` is one byte past core's ceiling is a 16,777,324-byte request; it passes
    // through, and the reply would be 33,554,773 bytes — 341 over the limit. The compress
    // path is not close: a 10,485,871-byte request of high-entropy JSON would be answered
    // with 34,953,143 bytes, 1,398,711 over.
    //
    // Neither of those frames is emitted any more. The same number bounds what is written
    // as well as what is read, so a request whose answer would cross it is refused by id
    // instead — `tests/session.rs` pins that refusal and the multiplier that makes it
    // reachable. What is left for this test is the constant itself: a client sizes its
    // reader by it, and both directions now depend on it.
    const _: () = assert!(stdio::MAX_LINE_BYTES > 16 * 1024 * 1024);

    // The transport's limit and the dispatcher's are one number, not two that agree
    // today. If these ever differed, one side of the session would refuse what the other
    // considers legal.
    assert_eq!(
        stdio::MAX_LINE_BYTES,
        tokfold_mcp::MAX_MESSAGE_BYTES,
        "the read limit and the emit limit must be the same constant"
    );
    assert_eq!(
        Server::new().max_message_bytes(),
        stdio::MAX_LINE_BYTES,
        "a default server must be bounded by what the transport can carry"
    );

    // A client sizes its own buffers by this number and chunks its payloads under it.
    // It is written as an arithmetic expression, so an editing slip can change it by
    // three orders of magnitude while still looking like "32 megabytes" at a glance.
    assert_eq!(
        stdio::MAX_LINE_BYTES,
        33_554_432,
        "the documented limit is 32 MiB"
    );
}

#[test]
fn the_encoder_name_and_its_wire_id_never_disagree() {
    // Both fields are reported, and a caller may key on either: the name is what a
    // human reads in a log, the id is what an archive records. They are produced by two
    // different pieces of code, so they can drift apart, and a reader comparing a log
    // line against an archive header would then be quietly wrong. The pairing is frozen
    // — an id may be added, but an existing one may never be renumbered.
    let pairs: &[(&str, i64)] = &[("passthrough", 0), ("minify", 1), ("tabular", 2)];

    // Three inputs chosen to reach three different encoders: uniform rows are what the
    // tabular encoder exists for, an irregular object can only be minified, and text
    // that is not JSON at all cannot be encoded.
    let inputs = [
        r#"{"rows":[{"id":1,"name":"ada","ok":true},{"id":2,"name":"grace","ok":false},{"id":3,"name":"alan","ok":true}]}"#,
        r#"{ "a" : { "b" : [ 1 , 2 , 3 ] } , "c" : "some text that is long enough to matter" }"#,
        "plain prose that is not JSON",
    ];

    let mut seen = Vec::new();
    for input in inputs {
        let outcome = compress(input);
        let Some(stats) = outcome.get("stats") else {
            continue; // A declined input reports no stats; that path is tested above.
        };
        let name = stats.get("encoder").and_then(json::Value::as_str).unwrap();
        let id = stats
            .get("encoderId")
            .and_then(json::Value::as_i64)
            .unwrap();
        assert!(
            pairs.contains(&(name, id)),
            "encoder {name:?} was reported with id {id}, which is not the frozen pairing"
        );
        seen.push(name.to_owned());
    }
    assert!(
        !seen.is_empty(),
        "no input reached an encoder, so nothing was actually checked"
    );
}

#[test]
fn a_batch_is_answered_as_one_line_holding_one_array() {
    // Batching is the part of JSON-RPC most easily got wrong in a line-framed
    // transport: the answers must arrive as a single array on a single line, and the
    // notification in the middle must contribute no element at all. A client that
    // reads one message per line desynchronises permanently if this is broken.
    let batch = format!(
        "[{},{},{}]\n",
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"server/discover"}"#
    );

    let mut server = Server::new();
    let mut output = Vec::new();
    stdio::serve(batch.as_bytes(), &mut output, &mut server).unwrap();
    let raw = String::from_utf8(output).unwrap();

    assert_eq!(
        raw.matches('\n').count(),
        1,
        "a batch is answered on exactly one line: {raw:?}"
    );
    let replies = json::parse(raw.trim_end()).expect("the answer is one JSON array");
    let replies = replies
        .as_array()
        .expect("a batch is answered with an array");
    assert_eq!(
        replies.len(),
        2,
        "the notification must contribute no element: {raw}"
    );
    assert_eq!(
        replies
            .iter()
            .filter_map(|item| item.get("id").and_then(json::Value::as_i64))
            .collect::<Vec<_>>(),
        vec![1, 2],
        "the answers keep the order of the requests"
    );
}

#[test]
fn a_batch_of_nothing_but_notifications_is_answered_with_silence() {
    // The complementary rule, and the one a naive implementation breaks: an empty array
    // is not a valid answer here, so the correct output is no line whatsoever.
    let batch = format!(
        "[{},{}]\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#
    );

    let mut server = Server::new();
    let mut output = Vec::new();
    stdio::serve(batch.as_bytes(), &mut output, &mut server).unwrap();
    assert!(
        output.is_empty(),
        "an all-notification batch produces no output, not an empty array: {:?}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn the_instructions_promise_is_kept_for_input_that_does_not_compress() {
    // The tool instructions handed to the model end with "Input that does not compress
    // is returned unchanged, never dropped." A model acts on that sentence: it embeds
    // whatever comes back without re-checking. If the declined path ever returned an
    // empty rendering, or an error, the model would silently lose the tool output it
    // asked to compress — and no test that only checks `isError` would notice.
    let text = "plain prose, no JSON here at all";
    let arguments = format!(r#"{{"text":{}}}"#, json::Value::string(text));
    let message = reply(&call_line(1, "tokfold_compress", &arguments));
    let result = message.get("result").expect("a decline is not an error");

    assert_eq!(
        result.get("isError").and_then(json::Value::as_bool),
        Some(false),
        "declining to compress is a normal outcome, not a failure: {message}"
    );
    let structured = result.get("structuredContent").unwrap();
    assert_eq!(
        structured.get("compressed").and_then(json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        structured.get("rendering").and_then(json::Value::as_str),
        Some(text),
        "the input must come back unchanged"
    );
    // And the reason is machine-readable, so a caller can tell "not worth it" from
    // "could not read it" without parsing prose. The code itself is pinned, not merely
    // its presence: a caller branches on the string, and a mapping that collapsed every
    // decline into one bucket would still pass a presence check.
    assert_eq!(
        structured.get("reasonCode").and_then(json::Value::as_str),
        Some("invalid_json"),
        "prose is not JSON, and the decline says so: {structured}"
    );
    // The prose companion is for a human reading a log; it must not be empty, or the
    // log line says nothing.
    assert!(
        structured
            .get("reason")
            .and_then(json::Value::as_str)
            .is_some_and(|reason| !reason.is_empty()),
        "a decline explains itself in words too: {structured}"
    );
}

#[test]
fn a_decline_distinguishes_unreadable_input_from_input_it_will_not_go_deeper_into() {
    // `invalid_json` and `depth_exceeded` mean opposite things to a caller: the first
    // says the payload is not what it claimed to be, the second says the payload is
    // fine but larger than this engine's configured envelope. A caller retries the
    // second against a differently configured engine and gives up on the first, so the
    // two must never collapse into one code.
    //
    // The engine's default depth limit is 512. Nesting well past it is reachable from a
    // client because the nested document travels as a JSON *string*: the transport's own
    // parser sees one string, not 600 open brackets, so this exercises core's limit
    // rather than the transport's.
    let deep = format!("{}{}", "[".repeat(600), "]".repeat(600));
    let arguments = format!(r#"{{"text":{}}}"#, json::Value::string(deep.clone()));
    let message = reply(&call_line(1, "tokfold_compress", &arguments));
    let structured = message
        .get("result")
        .and_then(|result| result.get("structuredContent"))
        .unwrap_or_else(|| panic!("a decline is still a result: {message}"));

    assert_eq!(
        structured.get("reasonCode").and_then(json::Value::as_str),
        Some("depth_exceeded"),
        "too deep is its own reason, not a parse failure: {structured}"
    );
    // The promise that nothing is dropped holds on this path too, which is what makes
    // the decline safe to hand back to a model unchecked.
    assert_eq!(
        structured.get("rendering").and_then(json::Value::as_str),
        Some(deep.as_str()),
        "a refused-as-too-deep input is still returned whole"
    );
}
