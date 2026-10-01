//! `tokfold_mcp::Server::handle_line` over a whole session of arbitrary lines.
//!
//! The first input byte picks the frame limit; the rest is decoded as UTF-8 (lossily,
//! as a transport that had to accept it would) and split on `\n` into lines that one
//! server handles in order, so state carried between messages is exercised too.
//!
//! * `0`-`63`: the default, `MAX_MESSAGE_BYTES` (32 MiB);
//! * `64`-`127`: `95 + 16 * (byte - 64)` bytes (95 to 1 103), where the bound on
//!   every reply is documented to hold;
//! * `128`-`255`: `byte - 128` bytes (0 to 127), which reaches the range below 95
//!   where only the last-resort refusal may exceed the limit.
//!
//! Must never panic. Every reply must be a single line, must parse with the server's
//! own JSON parser and with the independent oracle, and must fit the frame limit
//! whenever the limit is 95 bytes or more.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tokfold_fuzz::oracle_valid;
use tokfold_mcp::{MAX_MESSAGE_BYTES, Server, json};

/// The smallest limit at which `handle_line` documents every reply as fitting.
const BOUND_HOLDS_FROM: usize = 95;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let max = match selector {
        0..=63 => MAX_MESSAGE_BYTES,
        64..=127 => BOUND_HOLDS_FROM + 16 * usize::from(selector - 64),
        _ => usize::from(selector - 128),
    };
    let mut server = Server::new().with_max_message_bytes(max);
    let text = String::from_utf8_lossy(rest);
    for line in text.split('\n') {
        let Some(reply) = server.handle_line(line) else {
            continue;
        };
        assert!(
            !reply.contains('\n'),
            "a reply carries a newline: {reply:?}"
        );
        if let Err(err) = json::parse(&reply) {
            panic!("a reply is not JSON ({err}): {reply:?}");
        }
        if let Some(valid) = oracle_valid(&reply) {
            assert!(valid, "the oracle rejects a reply: {reply:?}");
        }
        if max >= BOUND_HOLDS_FROM {
            assert!(
                reply.len() <= max,
                "a {}-byte reply exceeds the {max}-byte limit: {reply:?}",
                reply.len()
            );
        } else {
            // Below the documented floor only the last-resort refusal may overflow,
            // and it is 93 bytes plus the digits of the limit.
            let digits = max.to_string().len();
            assert!(
                reply.len() <= max.max(93 + digits),
                "a {}-byte reply at a {max}-byte limit: {reply:?}",
                reply.len()
            );
        }
    }
});
