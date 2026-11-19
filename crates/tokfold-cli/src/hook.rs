//! `tokfold hook` — a Claude Code `PostToolUse` hook that hands the model the
//! rendering of a tool's result instead of the result itself.
//!
//! Claude Code runs a hook command with one JSON object on standard input. For a
//! `PostToolUse` event that object carries the tool's result in `tool_response`, and
//! a hook that prints
//! `{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":…}}`
//! on standard output replaces what the model sees with that value. This module
//! decides what, if anything, to print.
//!
//! # The policy
//!
//! A hook sits in front of every tool call it matches, so the one rule above all
//! others is that it never costs the call anything: every path below that does not
//! end in a replacement prints nothing and exits `0`, and Claude Code then shows the
//! model the original result. A replacement is printed only when all of these hold:
//!
//! * standard input is UTF-8 and parses as one JSON object;
//! * `hook_event_name`, when present, is `PostToolUse` — any other event would be
//!   handed a `hookSpecificOutput` it does not define;
//! * `tool_response` is a **non-empty string**. That is how an MCP tool's result
//!   arrives (measured under Claude Code: the JSON text of the result, as a string).
//!   A built-in tool's result arrives as an object with a tool-specific shape — Bash's
//!   is `{stdout, stderr, interrupted, isImage}` — and Claude Code ignores a
//!   replacement that does not match that shape, so an object is left alone;
//! * the engine accepts the string and an encoder other than passthrough wins. A
//!   passthrough rendering is the string itself, so printing it would change nothing
//!   and cost a replacement;
//! * the rendering is cheaper than the string it replaces, by the measure
//!   `--tokenizer` names ([`Price`]): with `heuristic`, the default, it is shorter in
//!   bytes; with `cl100k` or `o200k` it has strictly fewer tokens in that exact
//!   tokenizer. Bytes and tokens disagree often enough to matter (measured in the
//!   CLI README, *Using it as a Claude Code hook*). The engine's own choice of encoder
//!   is not affected — the do-no-harm gate prices with the heuristic whichever
//!   tokenizer is named — so `--tokenizer` decides only whether the rendering the
//!   engine produced is printed. The token rule replaces the byte rule rather than
//!   adding to it, so it could also print a rendering longer in bytes that has fewer
//!   tokens; no real input doing so was found (2 660 tried on 2026-11-14: 480
//!   synthetic arrays and the 2 180 results of the CLI README's corpus), so that branch
//!   is pinned only with a stand-in tokenizer in the unit tests.
//!
//! An exact tokenizer is loaded only once a non-passthrough rendering exists to be
//! priced, and a hook that cannot load one — a build without the `tiktoken` feature,
//! or tables that fail to build — leaves the result alone like every other refusal.
//! `--print-config` is where a build that lacks the tokenizer says so (exit `2`):
//! it is a setup step a person types, so failing there costs no tool call.
//!
//! [`respond`] is that policy as a pure function, so the unit tests below reach every
//! branch without a process; `tests/cli.rs` drives the binary end to end.

use std::io::{self, Read as _};
use std::panic::{self, AssertUnwindSafe};

use tokfold_core::{Compressor, Config, EncoderId, Profile, TokenEstimator};
use tokfold_mcp::json::{self, Object, Value};

/// The only hook event whose output this module knows how to shape.
pub const EVENT: &str = "PostToolUse";

/// The `settings.json` fragment `tokfold hook --print-config` prints, installing
/// `command` (`tokfold hook` followed by any flag the caller gave a value other than
/// its default).
///
/// The matcher is a regular expression over the tool name, and `mcp__.*` selects
/// every MCP tool and nothing else: Claude Code names MCP tools
/// `mcp__<server>__<tool>`, and a matcher of letters, digits, `_` and `|` alone is
/// compared exactly rather than as a pattern, so `mcp__` by itself would match no
/// tool. Narrow it to one server with `mcp__<server>__.*`.
pub fn settings_snippet(command: &str) -> String {
    let command = Value::string(command);
    format!(
        r#"{{
  "hooks": {{
    "PostToolUse": [
      {{
        "matcher": "mcp__.*",
        "hooks": [
          {{
            "type": "command",
            "command": {command}
          }}
        ]
      }}
    ]
  }}
}}
"#
    )
}

/// What a rendering must be cheaper in before it replaces the tool's result.
pub enum Price {
    /// Fewer bytes than the original: `--tokenizer heuristic`, the default.
    Bytes,
    /// Strictly fewer tokens than the original in the tokenizer `load` returns:
    /// `--tokenizer cl100k|o200k`. `load` runs only when there is a rendering to
    /// price, and at most once; `None` from it leaves the result alone.
    Tokens(Box<dyn FnOnce() -> Option<Box<dyn TokenEstimator>>>),
}

/// Apply the hook policy to the bytes Claude Code wrote on standard input.
///
/// Returns the line to print — the hook output object followed by a newline — or
/// `None` when the original result should stand. It never fails: every reason to
/// leave the result alone is a `None`, an exact tokenizer that cannot be loaded
/// among them.
pub fn respond(stdin: &[u8], profile: Profile, price: Price) -> Option<String> {
    let text = std::str::from_utf8(stdin).ok()?;
    let event = json::parse(text).ok()?;
    if event
        .get("hook_event_name")
        .is_some_and(|name| name.as_str() != Some(EVENT))
    {
        return None;
    }
    let original = event.get("tool_response")?.as_str()?;
    if original.is_empty() {
        return None;
    }
    let compressor = Compressor::new(Config::builder().profile(profile).build());
    let artifact = compressor.compress(original.as_bytes()).ok()?;
    if artifact.stats.encoder == EncoderId::PASSTHROUGH {
        return None;
    }
    let cheaper = match price {
        Price::Bytes => artifact.rendering.len() < original.len(),
        Price::Tokens(load) => {
            let counter = load()?;
            counter.estimate(&artifact.rendering) < counter.estimate(original)
        }
    };
    if !cheaper {
        return None;
    }
    let output = Object::new()
        .set(
            "hookSpecificOutput",
            Object::new()
                .set("hookEventName", Value::string(EVENT))
                .set("updatedToolOutput", Value::Str(artifact.rendering))
                .build(),
        )
        .build();
    Some(format!("{output}\n"))
}

/// Run the hook: read standard input, apply [`respond`], print what it returns.
///
/// Nothing here can fail the process. A read error, a panic anywhere in the engine
/// or the tokenizer (none is known — the bulkhead is there because a hook that aborts
/// a tool call is the one outcome this subcommand exists to rule out) and a failed
/// write all end in the same place as "nothing to replace": exit `0`. A write that fails part-way can
/// leave a truncated object on standard output; Claude Code cannot parse it as hook
/// output and shows the original result.
pub fn run(profile: Profile, price: Price) {
    let mut stdin = Vec::new();
    if io::stdin().lock().read_to_end(&mut stdin).is_err() {
        return;
    }
    let reply = panic::catch_unwind(AssertUnwindSafe(|| respond(&stdin, profile, price)));
    if let Ok(Some(line)) = reply {
        let _ = crate::write_stdout(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{EVENT, Price, respond, settings_snippet};
    use std::cell::Cell;
    use std::rc::Rc;
    use tokfold_core::{Compressor, Config, Profile, TokenEstimator};
    use tokfold_mcp::json::{self, Object, Value};

    /// A homogeneous array a table encoder wins on: E5's grid, which also hoists the
    /// constant `active` column into its header.
    const TABLE: &str = r#"[{"id":0,"name":"item0","active":true},{"id":1,"name":"item1","active":true},{"id":2,"name":"item2","active":true},{"id":3,"name":"item3","active":true}]"#;

    /// Build the stdin object Claude Code sends, with `tool_response` set to `response`.
    fn event(response: Value) -> String {
        Object::new()
            .set("session_id", Value::string("s"))
            .set("hook_event_name", Value::string(EVENT))
            .set("tool_name", Value::string("mcp__demo__query"))
            .set("tool_input", Object::new().build())
            .set("tool_response", response)
            .set("tool_use_id", Value::string("toolu_1"))
            .build()
            .to_string()
    }

    fn updated_output(line: &str) -> String {
        assert!(line.ends_with('\n'), "the reply is one line: {line:?}");
        let reply = json::parse(line.trim_end_matches('\n')).expect("the reply is JSON");
        let inner = reply.get("hookSpecificOutput").expect("hookSpecificOutput");
        assert_eq!(
            inner.get("hookEventName").and_then(Value::as_str),
            Some(EVENT)
        );
        inner
            .get("updatedToolOutput")
            .and_then(Value::as_str)
            .expect("updatedToolOutput is a string")
            .to_owned()
    }

    #[test]
    fn a_compressible_string_result_is_replaced_by_its_rendering() {
        let line = respond(
            event(Value::string(TABLE)).as_bytes(),
            Profile::Balanced,
            Price::Bytes,
        )
        .expect("a table win is replaced");
        let expected = Compressor::new(Config::default())
            .compress(TABLE.as_bytes())
            .unwrap()
            .rendering;
        assert!(expected.starts_with("#tkfd1 grid\n"), "{expected}");
        assert_eq!(updated_output(&line), expected);
    }

    #[test]
    fn the_profile_reaches_the_engine() {
        // `Conservative` has no table encoder; a compact array leaves minify nothing
        // to remove, so the same input that `Balanced` replaces stands.
        assert!(
            respond(
                event(Value::string(TABLE)).as_bytes(),
                Profile::Conservative,
                Price::Bytes,
            )
            .is_none()
        );
    }

    #[test]
    fn a_passthrough_rendering_is_never_printed() {
        // Valid JSON the engine accepts but no encoder shrinks: a short scalar.
        let response = "\"a short string\"";
        let artifact = Compressor::new(Config::default())
            .compress(response.as_bytes())
            .unwrap();
        assert_eq!(artifact.stats.encoder, tokfold_core::EncoderId::PASSTHROUGH);
        assert_eq!(artifact.rendering, response);
        assert!(
            respond(
                event(Value::string(response)).as_bytes(),
                Profile::Balanced,
                Price::Bytes
            )
            .is_none()
        );
    }

    #[test]
    fn a_rejected_string_is_left_alone() {
        assert!(
            respond(
                event(Value::string("plain text, not JSON")).as_bytes(),
                Profile::Balanced,
                Price::Bytes,
            )
            .is_none()
        );
        assert!(
            respond(
                event(Value::string("")).as_bytes(),
                Profile::Balanced,
                Price::Bytes
            )
            .is_none()
        );
    }

    #[test]
    fn a_result_that_is_not_a_string_is_left_alone() {
        // A built-in tool's result is an object whose shape Claude Code checks; even
        // when its text would compress, the hook does not touch it.
        let bash = Object::new()
            .set("stdout", Value::string(TABLE))
            .set("stderr", Value::string(""))
            .set("interrupted", Value::Bool(false))
            .set("isImage", Value::Bool(false))
            .build();
        assert!(respond(event(bash).as_bytes(), Profile::Balanced, Price::Bytes).is_none());
        assert!(
            respond(
                event(Value::Null).as_bytes(),
                Profile::Balanced,
                Price::Bytes
            )
            .is_none()
        );
    }

    #[test]
    fn bad_stdin_and_other_events_produce_nothing() {
        for stdin in [
            &b""[..],
            b"not json",
            b"[]",
            b"{}",
            b"\xff\xfe",
            br#"{"tool_name":"x"}"#,
        ] {
            assert!(
                respond(stdin, Profile::Balanced, Price::Bytes).is_none(),
                "{stdin:?}"
            );
        }
        let pre = event(Value::string(TABLE)).replace(EVENT, "PreToolUse");
        assert!(respond(pre.as_bytes(), Profile::Balanced, Price::Bytes).is_none());
    }

    #[test]
    fn an_event_without_a_name_is_still_answered() {
        let bare = Object::new()
            .set("tool_response", Value::string(TABLE))
            .build()
            .to_string();
        assert!(respond(bare.as_bytes(), Profile::Balanced, Price::Bytes).is_some());
    }

    /// A two-row array whose grid rendering is *longer* in bytes (40 against 39)
    /// although the engine's heuristic still picks the grid encoder for it. Its cells
    /// differ, so there is no constant column for E5 to hoist.
    const LONGER_IN_BYTES: &str = r#"[{"id":10,"v":"ab"},{"id":20,"v":"cd"}]"#;

    /// A stand-in tokenizer: every rendering (it opens with the sentinel) costs
    /// `rendering`, every other text costs `original`.
    struct Fake {
        rendering: usize,
        original: usize,
    }

    impl TokenEstimator for Fake {
        fn estimate(&self, text: &str) -> usize {
            if text.starts_with("#tkfd1 ") {
                self.rendering
            } else {
                self.original
            }
        }
        fn tokenizer_id(&self) -> u16 {
            u16::MAX
        }
    }

    /// A `Price::Tokens` whose loader records that it ran and returns `Fake`.
    fn priced(rendering: usize, original: usize) -> (Price, Rc<Cell<u32>>) {
        let calls = Rc::new(Cell::new(0));
        let seen = Rc::clone(&calls);
        let price = Price::Tokens(Box::new(move || {
            seen.set(seen.get() + 1);
            Some(Box::new(Fake {
                rendering,
                original,
            }) as Box<dyn TokenEstimator>)
        }));
        (price, calls)
    }

    #[test]
    fn a_token_price_replaces_only_on_strictly_fewer_tokens() {
        let stdin = event(Value::string(TABLE));
        let by_bytes = respond(stdin.as_bytes(), Profile::Balanced, Price::Bytes).unwrap();

        let (cheaper, calls) = priced(9, 10);
        let line = respond(stdin.as_bytes(), Profile::Balanced, cheaper).unwrap();
        assert_eq!(line, by_bytes, "the same reply, priced another way");
        assert_eq!(calls.get(), 1);

        for (rendering, original) in [(10, 10), (11, 10)] {
            let (price, calls) = priced(rendering, original);
            assert!(
                respond(stdin.as_bytes(), Profile::Balanced, price).is_none(),
                "{rendering} against {original} tokens"
            );
            assert_eq!(calls.get(), 1);
        }
    }

    /// The token rule replaces the byte rule rather than adding to it: a rendering
    /// longer in bytes but cheaper in tokens is forwarded, and the byte rule refuses it.
    /// `Fake` stands in for the tokenizer because neither exact one is known to price
    /// a byte-longer rendering cheaper (`cl100k` and `o200k` both count this one 19 -> 21).
    #[test]
    fn a_token_price_does_not_also_compare_bytes() {
        let stdin = event(Value::string(LONGER_IN_BYTES));
        assert!(respond(stdin.as_bytes(), Profile::Balanced, Price::Bytes).is_none());
        let (price, _) = priced(1, 2);
        let rendering =
            updated_output(&respond(stdin.as_bytes(), Profile::Balanced, price).unwrap());
        assert!(rendering.len() > LONGER_IN_BYTES.len(), "{rendering:?}");
    }

    #[test]
    fn a_tokenizer_that_cannot_load_leaves_the_result_alone() {
        let price = Price::Tokens(Box::new(|| None));
        assert!(
            respond(
                event(Value::string(TABLE)).as_bytes(),
                Profile::Balanced,
                price
            )
            .is_none()
        );
    }

    /// The tokenizer is loaded only once there is a rendering to price: every other
    /// reason to leave a result alone returns before the loader runs.
    #[test]
    fn the_tokenizer_is_not_loaded_without_a_rendering_to_price() {
        let other_event = event(Value::string(TABLE)).replace(EVENT, "PreToolUse");
        let cases = [
            event(Value::string("\"a short string\"")),
            event(Value::string("plain text, not JSON")),
            event(Value::string("")),
            event(Value::Null),
            other_event,
            "not json".to_owned(),
        ];
        for stdin in cases {
            let (price, calls) = priced(1, 2);
            assert!(
                respond(stdin.as_bytes(), Profile::Balanced, price).is_none(),
                "{stdin}"
            );
            assert_eq!(calls.get(), 0, "{stdin}");
        }
    }

    #[test]
    fn the_settings_snippet_is_json_with_the_documented_matcher() {
        let settings = json::parse(&settings_snippet("tokfold hook")).expect("the snippet is JSON");
        let entry = &settings
            .get("hooks")
            .and_then(|h| h.get(EVENT))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .expect("a PostToolUse entry");
        assert_eq!(
            entry.get("matcher").and_then(Value::as_str),
            Some("mcp__.*")
        );
        let hook = &entry
            .get("hooks")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .unwrap();
        assert_eq!(hook.get("type").and_then(Value::as_str), Some("command"));
        assert_eq!(
            hook.get("command").and_then(Value::as_str),
            Some("tokfold hook")
        );
    }
}
