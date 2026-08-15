//! Generated-input properties for the three hand-written parsers.
//!
//! This crate writes its own JSON codec, its own base64 codec and its own JSON-RPC
//! envelope parser rather than taking dependencies, and every one of them is fed by an
//! untrusted peer over stdio. Example-based tests cover the inputs someone thought of;
//! these cover the ones nobody did.
//!
//! Two shapes of property are worth the machinery, and both appear below:
//!
//! * **Round trips.** Encoding and decoding are written separately, so they can drift
//!   apart on an input class neither test happened to name — a lone control character,
//!   a base64 tail of an awkward length. A round trip states the relationship once and
//!   lets the generator look for the exception.
//! * **Total functions.** Every entry point here takes arbitrary bytes from a peer, so
//!   "returns an error" and "panics" are very different outcomes: the first is a reply,
//!   the second takes down a request the transport has to answer. `unwrap_used` and
//!   `panic` are denied in this crate's source precisely so this stays true, and a
//!   generator is the only thing that checks it against input nobody wrote by hand.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use proptest::prelude::*;
use tokfold_mcp::json::{MAX_DEPTH, Value};
use tokfold_mcp::{Server, base64, json};

proptest! {
    /// The property the whole `tokfold_decompress` tool rests on: an archive is handed
    /// to a client as base64 and comes back as the same bytes.
    #[test]
    fn base64_restores_every_byte_string(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let encoded = base64::encode(&bytes);
        prop_assert!(encoded.is_ascii(), "base64 output must be ASCII: {encoded:?}");
        prop_assert_eq!(base64::decode(&encoded).unwrap(), bytes);
    }

    /// A decoder fed a peer's string must answer, not abort.
    ///
    /// The alphabet is deliberately mixed with characters outside it, so the generator
    /// reaches every rejection path — a bad length, an unknown symbol, padding in the
    /// wrong place, and a final quartet whose discarded bits are set.
    #[test]
    fn base64_decoding_arbitrary_text_never_panics(text in "[A-Za-z0-9+/=!\u{00e9} ]{0,64}") {
        // The result is not asserted on: what is being tested is that there *is* one.
        let _ = base64::decode(&text);
    }

    /// Whatever a decoder accepts, it must accept as the *canonical* encoding.
    ///
    /// Re-encoding what came out has to reproduce the input exactly. Without this, two
    /// distinct strings could decode to the same bytes, which is a malleability bug: an
    /// archive would have more than one valid spelling.
    #[test]
    fn base64_accepts_only_the_canonical_spelling(text in "[A-Za-z0-9+/=]{0,64}") {
        if let Ok(decoded) = base64::decode(&text) {
            prop_assert_eq!(base64::encode(&decoded), text);
        }
    }

    /// Every string a peer can send survives being written out and read back.
    ///
    /// This is where a JSON escape table goes wrong: the writer and the reader hold two
    /// separate lists, and the classes that differ by one arm — a control character
    /// below 0x20, a quote, a backslash, a character outside the basic plane — are
    /// exactly the ones a hand-written example set is thin on.
    #[test]
    fn any_string_survives_being_rendered_and_parsed(text in ".{0,200}") {
        let rendered = Value::string(text.clone()).to_string();
        let parsed = json::parse(&rendered)
            .map_err(|error| TestCaseError::fail(format!("{error} on {rendered:?}")))?;
        prop_assert_eq!(parsed.as_str(), Some(text.as_str()));
    }

    /// A rendered string is safe to put on a line-framed transport.
    ///
    /// The transport's framing rule is one message per line. A raw newline inside a
    /// rendered string would split one message into two, and the client would read the
    /// halves as separate messages and desynchronise permanently.
    #[test]
    fn a_rendered_string_never_carries_a_raw_line_break(text in ".{0,200}") {
        let rendered = Value::string(text).to_string();
        prop_assert!(!rendered.contains('\n'));
        prop_assert!(!rendered.contains('\r'));
    }

    /// A parser fed a peer's text must answer, not abort.
    ///
    /// The generator is seeded with JSON punctuation so it produces near-misses —
    /// a truncated escape, an unterminated string, a stray comma — rather than prose
    /// that fails on the first character.
    #[test]
    fn parsing_arbitrary_text_never_panics(text in r#"[\{\}\[\]",:0-9a-fA-F\\ tnru.eE+-]{0,80}"#) {
        let _ = json::parse(&text);
    }
}

/// Deep nesting is refused rather than recursed into.
///
/// The parser recurses, so the input that would take the process down is not malformed
/// at all — it is well-formed JSON that is simply deep. `MAX_DEPTH` exists for that, and
/// what is checked here is that the limit holds by *refusing*, on both bracket shapes
/// and at sizes far past the bound, rather than by surviving one example.
#[test]
fn nesting_past_the_limit_is_refused_and_not_recursed_into() {
    for depth in [MAX_DEPTH + 1, MAX_DEPTH * 4, MAX_DEPTH * 64] {
        let arrays = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(
            json::parse(&arrays).is_err(),
            "{depth} levels of array nesting must be refused"
        );
        let objects = format!("{}1{}", r#"{"k":"#.repeat(depth), "}".repeat(depth));
        assert!(
            json::parse(&objects).is_err(),
            "{depth} levels of object nesting must be refused"
        );
    }
}

proptest! {
    /// The dispatcher itself must be total over arbitrary lines.
    ///
    /// `stdio` wraps each request in `catch_unwind` so one bad message cannot kill the
    /// process, but that bulkhead is a last resort, not a design: a panic it catches is
    /// still a request answered with `internal server error` instead of a real reply.
    /// This asserts the layer under it does not need rescuing.
    #[test]
    fn the_dispatcher_answers_arbitrary_lines_without_panicking(
        line in r#"[\{\}\[\]",:0-9a-z_/\\ .-]{0,120}"#
    ) {
        let mut server = Server::new();
        // Whatever comes back must at least be a JSON message, because the transport
        // writes it to the client verbatim. Silence is a valid answer; malformed
        // output is not.
        if let Some(reply) = server.handle_line(&line) {
            prop_assert!(!reply.contains('\n'), "a reply must fit one line: {reply:?}");
            prop_assert!(json::parse(&reply).is_ok(), "a reply must be JSON: {reply:?}");
        }
    }

    /// The three rules that hold for every answered message, whatever the message was.
    ///
    /// A reply carries the `jsonrpc` marker, exactly one of `result` or `error`, and the
    /// id it is answering. The first two are a shape a hand-built response object drifts
    /// out of when a new branch is added; the third is the one a client cannot work
    /// around, because an id is the only thing tying a reply to the call that is waiting
    /// for it.
    ///
    /// The envelope is assembled from generated fragments rather than being well-formed
    /// by construction, and that is the point. A valid envelope reaches the dispatcher,
    /// where echoing the id is almost unavoidable — the decoded request carries it. The
    /// half worth generating is the refusals that happen *before* a request exists: a
    /// wrong or missing `jsonrpc`, a `method` that is absent or not a string, a `params`
    /// that is neither object nor array. Those are answered from their own path, they
    /// are where the id went missing once, and a generator that only builds valid
    /// envelopes never reaches them.
    ///
    /// Every fragment set keeps the `id` member, so every generated line is a request
    /// rather than a notification and must therefore be answered.
    #[test]
    fn every_reply_is_a_well_formed_jsonrpc_response(
        version in prop_oneof![
            Just(r#""jsonrpc":"2.0","#),
            Just(r#""jsonrpc":"1.0","#),
            Just(r#""jsonrpc":2.0,"#),
            Just(""),
        ],
        method in prop_oneof![
            Just(r#","method":"initialize""#),
            Just(r#","method":"tools/list""#),
            Just(r#","method":"tools/call""#),
            Just(r#","method":"server/discover""#),
            Just(r#","method":"ping""#),
            Just(r#","method":"nope""#),
            Just(r#","method":5"#),
            Just(""),
        ],
        params in prop_oneof![
            Just(""),
            Just(r#","params":{}"#),
            Just(r#","params":null"#),
            Just(r#","params":[]"#),
            Just(r#","params":7"#),
        ],
        id in 1_i64..1000,
        id_is_a_string in any::<bool>(),
    ) {
        // Both id shapes JSON-RPC allows, since recovering one is a match on its type.
        let id_text = if id_is_a_string { format!("\"{id}\"") } else { id.to_string() };
        let line = format!(r#"{{{version}"id":{id_text}{method}{params}}}"#);
        let mut server = Server::new();
        let reply = server.handle_line(&line).expect("a message carrying an id is answered");
        let reply = json::parse(&reply).unwrap();

        prop_assert_eq!(reply.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
        prop_assert_eq!(
            reply.get("id").map(Value::to_string),
            Some(id_text),
            "the reply must be addressed to the id the message carried: {}",
            line
        );
        let has_result = reply.get("result").is_some();
        let has_error = reply.get("error").is_some();
        prop_assert!(has_result != has_error, "exactly one of result/error: {reply}");
    }
}
