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

/// The alphabet the two dispatcher properties draw their lines from.
///
/// Named rather than repeated so that
/// [`the_generated_lines_reach_the_answers_the_properties_assert`] cannot drift away
/// from the strategy it certifies: a liveness check against a *different* regex would
/// certify nothing.
const ARBITRARY_LINE: &str = r#"[\{\}\[\]",:0-9a-z_/\\ .-]{0,120}"#;

/// Lines for [`no_answer_ever_exceeds_the_frame_limit`]: mostly junk from the alphabet
/// above, sometimes a request that has a real answer.
///
/// Junk alone was measured to be nearly useless there. A malformed line is answered by a
/// compact parse error that fits every limit the property generates, so 1495 draws
/// produced exactly **one** answer the limit had to refuse, and the property's claim to
/// be exercising the refusal machinery rather than the happy path was false. A
/// well-formed `tools/list` answers with the whole catalogue, which overruns any limit
/// in range, so the refusal path is now reached on purpose instead of by accident.
fn arb_framed_body() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => ARBITRARY_LINE.prop_map(String::from),
        1 => prop_oneof![
            Just(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_string()),
            Just(r#"{"jsonrpc":"2.0","id":2,"method":"server/discover"}"#.to_string()),
            Just(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#.to_string()),
        ],
    ]
}

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
    fn the_dispatcher_answers_arbitrary_lines_without_panicking(line in ARBITRARY_LINE) {
        let mut server = Server::new();
        // Whatever comes back must at least be a JSON message, because the transport
        // writes it to the client verbatim. Silence is a valid answer; malformed
        // output is not.
        if let Some(reply) = server.handle_line(&line) {
            prop_assert!(!reply.contains('\n'), "a reply must fit one line: {reply:?}");
            prop_assert!(json::parse(&reply).is_ok(), "a reply must be JSON: {reply:?}");
        }
    }

    /// Whatever comes back fits the frame, whatever went in.
    ///
    /// The size limit is stated in one place and relied on everywhere, so the way it
    /// fails is a path that forgot it — a new branch that builds a response and renders
    /// it directly, or a batch shape nobody sized. Example tests cover the two paths
    /// someone had in mind. This drives the limit down into the hundreds of bytes and
    /// asserts the one thing that has to hold whatever comes back: it fits, and it is
    /// still a message.
    ///
    /// How often the limit actually *bites* is a measurement, not a hope, and the first
    /// measurement was bad: drawing bodies from the junk alphabet alone, one draw in
    /// 1495 produced an answer too large to frame, because a malformed line is answered
    /// by a compact parse error that fits anything. [`arb_framed_body`] mixes in
    /// requests with real answers for that reason and lifts it to 271 in 1493;
    /// [`the_generated_lines_reach_the_answers_the_properties_assert`] holds it there.
    ///
    /// The floor is the one documented exception — below the size of a bare error frame
    /// there is nothing valid left to emit, and a frame that overruns an absurd limit is
    /// better than silence, which hangs the caller. That frame is 96 bytes here (a fixed
    /// message and the limit's own digits), so generating from 128 upward keeps the
    /// property above the exception while staying far under any real answer.
    #[test]
    fn no_answer_ever_exceeds_the_frame_limit(
        max in 128_usize..512,
        body in arb_framed_body(),
        wrap_in_a_batch in any::<bool>(),
    ) {
        let line = if wrap_in_a_batch { format!("[{body},{body}]") } else { body };
        let mut server = Server::new().with_max_message_bytes(max);
        if let Some(reply) = server.handle_line(&line) {
            prop_assert!(
                reply.len() <= max,
                "a {} byte answer to a {max} byte limit: {reply:?}",
                reply.len()
            );
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
    /// that is not an object. Those are answered from their own path, they
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

/// The floor under the property above, measured rather than described.
///
/// `no_answer_ever_exceeds_the_frame_limit` generates from 128 upward because below some
/// limit the refusal frame stops fitting inside the limit it reports. That size was prose
/// here and nothing at all in the server's own rustdoc, which promised the bound with no
/// exception in three places — one of them recommending a 64-byte limit as the cheap way
/// to exercise it, which is 31 bytes under what it emits.
///
/// There are three boundaries under a real answer, not one, and the first draft of this
/// test found that out by failing: `render_bounded` tries the answer, then a refusal
/// addressed to the id, then an id-less refusal it emits unconditionally. So a small
/// limit does not merely refuse — between 95 and 102 it refuses *without the id*, which
/// is the one thing a waiting client cannot recover from. All four regimes are pinned
/// here so the docs that now quote the numbers cannot rot: rewording the message by one
/// character moves 93, and adding a field to a `ping` result moves the size at which real
/// answers start coming back.
#[test]
fn a_limit_below_a_real_answer_refuses_in_three_measurably_different_ways() {
    const PING: &str = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    /// The id-less refusal, without the digits of the limit it quotes.
    const REFUSAL: usize = 93;
    /// What addressing that refusal to this fixture's id costs: `"id":1,`.
    const ID_FIELD: usize = 7;

    // What a ping costs with nothing squeezing it. Derived rather than written down: the
    // envelope grows whenever `serverInfo` or the version string does.
    let answer_len = Server::new()
        .handle_line(PING)
        .expect("a ping is answered")
        .len();
    assert_eq!(
        answer_len, 143,
        "the size the rustdoc calls the top of the last regime"
    );

    for max in 0..answer_len {
        let reply = Server::new()
            .with_max_message_bytes(max)
            .handle_line(PING)
            .expect("a ping is answered at every limit, if only to refuse");
        assert!(
            reply.contains("frame limit"),
            "a {max} byte limit is under the {answer_len} byte answer, so this must be a \
             refusal: {reply:?}"
        );

        let digits = max.to_string().len();
        let addressed = REFUSAL + ID_FIELD + digits;
        let expected = if addressed <= max {
            addressed
        } else {
            REFUSAL + digits
        };
        assert_eq!(
            reply.len(),
            expected,
            "at a {max} byte limit the refusal is {} rather than the {expected} bytes the \
             rungs of `render_bounded` predict: {reply:?}",
            reply.len()
        );
        assert_eq!(
            reply.contains(r#""id":1"#),
            addressed <= max,
            "a refusal carries the id exactly when the addressed rung fits: {reply:?}"
        );
    }

    let smallest_honoured = (0..answer_len)
        .find(|&max| {
            Server::new()
                .with_max_message_bytes(max)
                .handle_line(PING)
                .is_some_and(|reply| reply.len() <= max)
        })
        .expect("some limit under the answer size is honoured by the refusal frame");
    assert_eq!(
        smallest_honoured, 95,
        "the floor moved: `Server::with_max_message_bytes` and `Server::handle_line` \
         both name 95 as the smallest limit this server never exceeds"
    );

    // The one value the rustdoc used to recommend by name, because it reads like a safe
    // knob for a test that wants to cross the bound cheaply.
    assert_eq!(
        Server::new()
            .with_max_message_bytes(64)
            .handle_line(PING)
            .expect("a ping is answered")
            .len(),
        95,
        "a 64 byte limit still emits 95 bytes; that is the documented exception"
    );
}

/// A wide id loses the addressed refusal before it could push the frame over the limit.
///
/// The docs say a request whose answer does not fit is refused "addressed to that
/// request's own id", and qualify it: the id survives only while the addressed refusal
/// itself fits, so an id nearly as wide as the limit is refused id-less. At the default
/// limit that is a string id within about a hundred bytes of 32 MiB, which the reader
/// admits; measured there, a 33,554,289-character id was refused addressed in
/// 33,554,398 bytes and a request of exactly 32 MiB id-less in 101. The same ladder is
/// walked here under a 200-byte limit, so every rung is reached without allocating the
/// real one, and each boundary is derived rather than written down.
#[test]
fn an_id_too_wide_for_the_addressed_refusal_is_refused_without_it() {
    const MAX: usize = 200;
    /// The id-less refusal at this limit: 93 bytes plus the three digits of `MAX`.
    const ID_LESS: usize = 96;

    let mut rungs = [false; 3];
    for width in 0..=120 {
        let id = "x".repeat(width);
        let request = format!(r#"{{"jsonrpc":"2.0","id":"{id}","method":"ping"}}"#);
        let unbounded = Server::new()
            .handle_line(&request)
            .expect("a ping is answered");
        let reply = Server::new()
            .with_max_message_bytes(MAX)
            .handle_line(&request)
            .expect("a ping is answered at every limit, if only to refuse");
        assert!(
            reply.len() <= MAX,
            "a {width}-character id pushed the frame to {} bytes, over the {MAX} byte \
             limit: {reply:?}",
            reply.len()
        );

        // `"id":"<id>",` in the addressed refusal: the refusal carries the id the same
        // way the answer does, so it costs the id's width plus eight bytes of framing.
        let addressed = ID_LESS + width + 8;
        let quoted_id = format!(r#""id":"{id}""#);
        if unbounded.len() <= MAX {
            assert_eq!(reply, unbounded, "an answer that fits goes out unchanged");
            rungs[0] = true;
        } else if addressed <= MAX {
            assert_eq!(
                reply.len(),
                addressed,
                "a {width}-character id: the addressed refusal is {addressed} bytes \
                 by derivation: {reply:?}"
            );
            assert!(reply.contains(&quoted_id), "addressed: {reply:?}");
            rungs[1] = true;
        } else {
            assert_eq!(
                reply.len(),
                ID_LESS,
                "a {width}-character id leaves no room for the addressed refusal, so \
                 the id-less one goes out: {reply:?}"
            );
            // Error responses carry no `id` member at all when there is none to echo;
            // `"id": null` was dropped from them, so its absence is what id-less means.
            assert!(!reply.contains(r#""id""#), "id-less: {reply:?}");
            assert!(reply.contains("frame limit"), "still a refusal: {reply:?}");
            rungs[2] = true;
        }
    }
    assert_eq!(
        rungs, [true; 3],
        "the widths walked must reach the answer, the addressed refusal and the id-less one"
    );
}

/// Generator liveness for the two dispatcher properties above.
///
/// Both state their assertions inside `if let Some(reply) = server.handle_line(&line)`,
/// because silence is a legal answer to a notification and to a line carrying no
/// recoverable id. That arm is correct and it is also the shape a property dies of: a
/// dispatcher that answered *nothing* would satisfy both of them completely. Measured
/// here rather than assumed — inserting `return None` as the first statement of
/// `handle_line` leaves both properties green and only this test red.
///
/// The frame property claims more than reachability. Its doc says the small limits it
/// generates make almost every answer overrun, so that the refusal machinery rather
/// than the happy path is what gets exercised; that claim is worth as much as its
/// measurement, so the refusals are counted too.
///
/// A fixed seed (`TestRunner::deterministic`) keeps this a hard assertion rather than a
/// flaky one.
#[test]
fn the_generated_lines_reach_the_answers_the_properties_assert() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;

    const DRAWS: usize = 1500;

    let mut runner = TestRunner::deterministic();
    let mut answered = 0usize;
    let mut silent = 0usize;
    let mut bounded_answers = 0usize;
    let mut refusals = 0usize;

    for _ in 0..DRAWS {
        let Ok(tree) = ARBITRARY_LINE.new_tree(&mut runner) else {
            continue;
        };
        let line = tree.current();

        match Server::new().handle_line(&line) {
            Some(_) => answered += 1,
            None => silent += 1,
        }

        let (Ok(max), Ok(batch), Ok(body)) = (
            (128_usize..512).new_tree(&mut runner),
            any::<bool>().new_tree(&mut runner),
            arb_framed_body().new_tree(&mut runner),
        ) else {
            continue;
        };
        let max = max.current();
        let body = body.current();
        let framed = if batch.current() {
            format!("[{body},{body}]")
        } else {
            body
        };
        if let Some(reply) = Server::new()
            .with_max_message_bytes(max)
            .handle_line(&framed)
        {
            bounded_answers += 1;
            if reply.contains("frame limit") {
                refusals += 1;
            }
        }
    }

    assert!(
        answered > 0,
        "both dispatcher properties are vacuous: none of {DRAWS} generated lines was \
         answered at all"
    );
    assert!(
        silent > 0,
        "the generator never produces a line the dispatcher stays silent on, so the \
         `if let` arm the properties are written around is untested"
    );
    assert!(
        bounded_answers > 0,
        "no_answer_ever_exceeds_the_frame_limit is vacuous: {DRAWS} lines produced no \
         answer to size at all"
    );
    // Not `refusals > 0`. That is the assertion this test was first written with, and
    // it would have passed on the generator it was written against — which produced
    // exactly one refusal in 1495 draws while the property's docs claimed almost every
    // answer overruns. A liveness check that a 0.07% hit rate satisfies certifies
    // nothing. The floor is a *proportion*, well under the 18% measured here so that
    // generator noise cannot trip it, and far above the rate the old generator reached.
    let floor = bounded_answers / 20;
    assert!(
        refusals > floor,
        "the frame property barely reaches the refusal machinery it exists for: \
         {refusals} refusals out of {bounded_answers} sized answers, at or below the \
         {floor} this test requires. Widen `arb_framed_body` towards requests whose \
         answers are large, or the property is testing the happy path under a name \
         that promises otherwise"
    );
}
