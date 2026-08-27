//! The stdio transport: read a line, hand it to [`Server`], write the answer.
//!
//! This module is deliberately the only place in the crate that touches I/O, and it
//! holds no protocol knowledge at all. Everything it enforces is a transport rule:
//!
//! * Exactly one JSON-RPC message per line, and a message never contains a newline.
//! * One size limit in both directions: [`MAX_LINE_BYTES`] is what a line may be to be
//!   read *and* what an answer may be to be written, so this server never emits a frame
//!   it would refuse as input. Enforcing the write side is [`Server::handle_line`]'s
//!   job, not this module's — see [`MAX_LINE_BYTES`].
//! * **Nothing reaches stdout that is not an MCP message.** A stray `println!` in a
//!   stdio server corrupts the stream and the client sees a protocol fault rather
//!   than the log line someone meant to leave. Diagnostics go to stderr.
//! * The loop ends promptly when stdin reaches EOF, which is how a client shuts a
//!   stdio server down.
//! * Every answer is flushed before the next line is read — a client is blocked
//!   waiting for it, so a buffered reply is a hang. The mirror of that rule is a
//!   precondition on the client: since the loop is not reading while it writes, the
//!   client has to drain stdout as it writes stdin. See [`serve`].

use std::io::{self, BufRead, ErrorKind, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::jsonrpc::{ErrorObject, Id, Response, read_id, render_bounded};
use crate::protocol::error_code;
use crate::server::Server;

/// Largest line accepted, in bytes.
///
/// This is [`crate::MAX_MESSAGE_BYTES`] under the name the transport uses, not a second
/// number that happens to agree with it. The read limit and the emit limit have to be
/// the same value — a server whose output can exceed its own input limit cannot talk to
/// a peer of its own kind — and the surest way to keep two numbers equal is to have one.
/// [`crate::MAX_MESSAGE_BYTES`] carries the reasoning for the size.
///
/// # Both directions, enforced elsewhere
///
/// A reply is always bigger than the call it answers, so the write side needs enforcing
/// and not merely documenting. That happens in [`Server::handle_line`] rather than here:
/// the crate is sans-io, and a bound that lived in this module would protect only the
/// embedders that happen to use this module. What is left for the transport is to refuse
/// an oversized *line*, below.
///
/// # Bytes are only half the bound
///
/// This limit is about the size of the frame and nothing else. What a line *expands
/// into* once parsed is bounded separately, by [`crate::json::MAX_NODES`] — a line
/// comfortably inside this cap can still describe millions of values, and a `Value` is
/// an order of magnitude larger than the text that produced it. That matters here in
/// particular: an allocation failure aborts rather than unwinds, so it would walk
/// straight past the `catch_unwind` bulkhead this module puts around every line.
pub const MAX_LINE_BYTES: usize = crate::MAX_MESSAGE_BYTES;

/// Serves one client to EOF over the given streams.
///
/// # Frames are bounded in both directions
///
/// A line longer than [`MAX_LINE_BYTES`] is refused and skipped, and an answer that
/// would be longer is refused by [`Server::handle_line`] before it reaches this loop, so
/// nothing written here is a frame this same loop would reject on the way in. The one
/// way to break that is to hand in a server built with
/// [`Server::with_max_message_bytes`] set above [`MAX_LINE_BYTES`], which is a deliberate
/// act by the embedder and documented there.
///
/// # The client must read while it writes
///
/// This is one thread running a strict read → handle → write → flush loop: it never
/// reads the next line while it is writing the answer to the previous one. **A client
/// must therefore drain stdout concurrently with writing stdin.** Every mainstream MCP
/// client SDK already does — a stdio transport reads the server's output on its own
/// task — so this is a precondition of the transport rather than a live fault.
///
/// A client that instead writes a burst of requests and only then starts reading will
/// deadlock, and it takes no giant payload to get there: once the accumulated replies
/// fill the OS pipe buffer (64 KiB on macOS) the server blocks in `write_all` and stops
/// draining stdin, and a client with more than a pipe buffer of requests still to write
/// blocks in its own write. Neither side moves again. Measured against a spawned
/// process, 100 pipelined `ping` calls are fine and 2,000 are not — the threshold is
/// one pipe buffer, not one large message.
///
/// Reading and writing concurrently here would mean a second thread or an async
/// runtime in a crate that is deliberately sans-io and dependency-free, so the loop is
/// left as it is and the precondition is stated instead.
///
/// # Errors
///
/// Returns an [`io::Error`] if reading stdin fails. A write failure is *not* an
/// error: a client that closes the pipe while the server is answering is shutting
/// down, which is a normal end to a session and not something to report as a fault.
pub fn serve<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    server: &mut Server,
) -> io::Result<()> {
    let mut buffer = Vec::new();
    loop {
        match read_line(&mut reader, &mut buffer)? {
            Line::Eof => return Ok(()),
            // Written as nested `if`s rather than a let-chain: chains are stable only
            // from 1.88 and this workspace floors at 1.85.
            Line::Text(line) => {
                if let Some(reply) = handle(server, &line) {
                    if !write_line(&mut writer, &reply)? {
                        return Ok(());
                    }
                }
            }
            Line::NotUtf8 => {
                let error = ErrorObject::new(error_code::PARSE_ERROR, "message is not valid UTF-8");
                if !write_error(&mut writer, error)? {
                    return Ok(());
                }
            }
            Line::TooLong => {
                let error = ErrorObject::new(
                    error_code::INVALID_REQUEST,
                    format!("message exceeds the {MAX_LINE_BYTES} byte limit"),
                );
                if !write_error(&mut writer, error)? {
                    return Ok(());
                }
            }
        }
    }
}

/// Serves a client on the process's own stdin and stdout.
///
/// # Errors
///
/// Returns an [`io::Error`] if reading stdin fails.
pub fn serve_stdio(server: &mut Server) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve(stdin.lock(), stdout.lock(), server)
}

/// Runs one request through the server, containing a panic rather than dying.
///
/// A panic in one request must not take the process down with it: the client would
/// see a closed pipe and lose every in-flight call, including ones that had nothing
/// to do with the bad input. The workspace deliberately leaves unwinding enabled so
/// this bulkhead can exist. `AssertUnwindSafe` is sound here because [`Server`]'s only
/// state is a handshake flag and the negotiated revision, with no interior mutability,
/// and both are written after every fallible step — a panic cannot leave the pair
/// half-applied.
///
/// The rescue frame goes through [`render_bounded`] like every other answer. Its message
/// is a fixed string, but the id it echoes came off the client's line and can be nearly
/// as large as the line itself, so "an error is obviously small" is not true here.
fn handle(server: &mut Server, line: &str) -> Option<String> {
    catch_unwind(AssertUnwindSafe(|| server.handle_line(line))).unwrap_or_else(|_| {
        Some(render_bounded(
            Response::error(
                recover_id(line),
                ErrorObject::new(error_code::INTERNAL_ERROR, "internal server error"),
            ),
            server.max_message_bytes(),
        ))
    })
}

/// Best-effort recovery of a request id from a line whose handling panicked.
///
/// An error carrying no id is unmatched: it gives the client no way to tie the failure
/// to the call it is blocked on, so a single panic becomes a hang rather than a failed
/// request. Re-parsing costs nothing on a path that is already exceptional.
///
/// Which shapes may be echoed is [`read_id`]'s rule, not a second copy of it — a rule
/// stated twice is a rule that can drift. What this adds is the parse and its own
/// `catch_unwind`, because parsing is one of the things that could have panicked in
/// the first place.
fn recover_id(line: &str) -> Option<Id> {
    catch_unwind(|| read_id(&crate::json::parse(line).ok()?)).unwrap_or(None)
}

/// One line of input, or the reason there is none.
enum Line {
    /// A complete line, with its terminator stripped.
    Text(String),
    /// The stream ended.
    Eof,
    /// The line was not valid UTF-8.
    NotUtf8,
    /// The line exceeded [`MAX_LINE_BYTES`] and was discarded.
    TooLong,
}

/// Reads one newline-terminated line, bounded by [`MAX_LINE_BYTES`].
fn read_line<R: BufRead>(reader: &mut R, buffer: &mut Vec<u8>) -> io::Result<Line> {
    buffer.clear();
    // Reading through `take` is what makes the bound real: `read_until` on its own
    // would allocate as far as the client cares to write before anyone could object.
    let limit = u64::try_from(MAX_LINE_BYTES).unwrap_or(u64::MAX);
    let read = Read::by_ref(reader).take(limit).read_until(b'\n', buffer)?;
    if read == 0 {
        return Ok(Line::Eof);
    }

    let terminated = buffer.last() == Some(&b'\n');
    if !terminated {
        // Either the stream ended without a terminator, or the line hit the cap. Only
        // the second case needs recovery, and it needs the rest of the line thrown
        // away so the next read starts on a message boundary.
        if read >= MAX_LINE_BYTES {
            skip_to_newline(reader)?;
            return Ok(Line::TooLong);
        }
    }

    let mut bytes = std::mem::take(buffer);
    if terminated {
        bytes.pop();
        // A client on Windows line endings sends CRLF; the CR is framing, not content.
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    Ok(String::from_utf8(bytes).map_or(Line::NotUtf8, Line::Text))
}

/// Discards input up to and including the next newline, in bounded steps.
fn skip_to_newline<R: BufRead>(reader: &mut R) -> io::Result<()> {
    loop {
        let (found, consumed) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(());
            }
            available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or((false, available.len()), |index| {
                    (true, index.saturating_add(1))
                })
        };
        reader.consume(consumed);
        if found {
            return Ok(());
        }
    }
}

/// Writes one framed message. Returns `false` once the client has stopped listening.
fn write_line<W: Write>(writer: &mut W, line: &str) -> io::Result<bool> {
    debug_assert!(
        !line.contains('\n'),
        "a framed message must not contain a newline"
    );
    match writer.write_all(line.as_bytes()).and_then(|()| {
        writer.write_all(b"\n")?;
        writer.flush()
    }) {
        Ok(()) => Ok(true),
        // A closed pipe means the client is gone. That ends the session; it is not a
        // failure worth reporting up, and writing a diagnostic about it would be noise
        // during a normal shutdown.
        Err(error) if is_disconnect(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Writes a transport-level error, which by construction has no id to echo.
///
/// Both callers failed before the line was ever a message — it was not UTF-8, or it
/// was discarded for exceeding the cap — so here there is genuinely nothing to recover
/// an id from, and omitting one is the honest answer.
///
/// It is not the only place that omits one, and reading it as the only place would
/// hide a defect rather than expose it. Since `"id": null` was dropped from error
/// responses, a parse failure, an empty batch, and a panic on a line whose id cannot be
/// re-read all answer without an id too. What separates honest from broken is not which
/// function wrote the frame: it is whether the id was there to be read.
fn write_error<W: Write>(writer: &mut W, error: ErrorObject) -> io::Result<bool> {
    let line = Response::error(None, error).into_value().to_string();
    write_line(writer, &line)
}

fn is_disconnect(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::UnexpectedEof
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use std::io::{self, Cursor, Write};

    use super::{MAX_LINE_BYTES, recover_id, serve};
    use crate::jsonrpc::Id;
    use crate::server::Server;

    /// Drives a whole session and returns everything written to stdout.
    fn session(input: &str) -> String {
        let mut output = Vec::new();
        let mut server = Server::new();
        serve(
            Cursor::new(input.as_bytes().to_vec()),
            &mut output,
            &mut server,
        )
        .unwrap();
        String::from_utf8(output).unwrap()
    }

    fn lines(output: &str) -> Vec<&str> {
        output.lines().collect()
    }

    #[test]
    fn an_empty_stream_produces_no_output() {
        assert_eq!(session(""), "");
    }

    #[test]
    fn the_loop_ends_at_eof_without_a_terminating_newline() {
        // A client that exits without flushing a final newline must not hang the
        // server, and its last message must still be answered.
        let output = session(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(lines(&output).len(), 1);
        assert!(
            output.contains(r#""id":1"#),
            "the last message went unanswered"
        );
        assert!(output.ends_with('\n'), "the reply must still be framed");
    }

    #[test]
    fn one_reply_is_written_per_request() {
        let output = session(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"server/discover"}"#,
            "\n",
        ));
        assert_eq!(lines(&output).len(), 3);
        assert!(output.ends_with('\n'), "the last message must be framed");
    }

    #[test]
    fn notifications_and_blank_lines_produce_nothing() {
        let output = session(concat!(
            "\n",
            "   \n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
        ));
        assert_eq!(output, "");
    }

    #[test]
    fn every_written_line_is_one_complete_json_message() {
        let output = session(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tokfold_compress","arguments":{"text":"{\"a\":\"line\\nbreak\"}"}}}"#,
            "\n",
        ));
        for line in lines(&output) {
            // A rendering containing a newline would break framing if it were not
            // escaped on the way out; this is the regression that would cause it.
            assert!(crate::json::parse(line).is_ok(), "not one message: {line}");
        }
    }

    #[test]
    fn crlf_line_endings_produce_the_same_bytes_as_lf() {
        // Two independent mechanisms accept a `\r`: the reader trims it, and the
        // parser treats it as whitespace. So this cannot isolate either one — what it
        // does pin is the property that matters, that a client on CRLF gets an answer
        // byte-identical to a client on LF rather than a subtly different one.
        let crlf = session("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\r\n");
        let lf = session("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
        assert_eq!(crlf, lf);
        assert_eq!(lines(&crlf).len(), 1);
        assert!(crate::json::parse(lines(&crlf)[0]).is_ok());
    }

    #[test]
    fn malformed_input_is_answered_and_the_session_continues() {
        let output = session(concat!(
            "{not json\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
            "\n",
        ));
        let written = lines(&output);
        assert_eq!(written.len(), 2, "a bad line must not end the session");
        assert!(written[0].contains("-32700"));
        assert!(written[1].contains(r#""id":2"#));
    }

    #[test]
    fn invalid_utf8_is_answered_and_the_session_continues() {
        let mut input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\xFF\"}\n".to_vec();
        input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n");
        let mut output = Vec::new();
        let mut server = Server::new();
        serve(Cursor::new(input), &mut output, &mut server).unwrap();
        let text = String::from_utf8(output).unwrap();
        let written = lines(&text);
        assert_eq!(written.len(), 2);
        assert!(written[0].contains("-32700"));
        assert!(written[1].contains(r#""id":2"#));
    }

    #[test]
    fn an_oversized_line_is_rejected_and_the_stream_resynchronizes() {
        // The whole point of the cap is that the next message still parses: a server
        // that gave up here would be trivially wedged by one bad write.
        let mut input = String::with_capacity(MAX_LINE_BYTES + 128);
        input.push('"');
        input.push_str(&"a".repeat(MAX_LINE_BYTES + 16));
        input.push_str("\"\n");
        input.push_str(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#);
        input.push('\n');

        let output = session(&input);
        let written = lines(&output);
        assert_eq!(written.len(), 2);
        assert!(written[0].contains("-32600"));
        assert!(
            written[1].contains(r#""id":2"#),
            "the stream did not resynchronize"
        );
    }

    /// A writer that always fails with the given kind, counting the attempts.
    struct FailingWriter {
        kind: io::ErrorKind,
        attempts: usize,
    }

    impl FailingWriter {
        const fn new(kind: io::ErrorKind) -> Self {
            Self { kind, attempts: 0 }
        }
    }

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.attempts += 1;
            Err(io::Error::from(self.kind))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(self.kind))
        }
    }

    /// Two answerable requests, so a loop that failed to stop would write twice.
    const TWO_REQUESTS: &str = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
        "\n",
    );

    #[test]
    fn a_client_that_disconnects_ends_the_session_cleanly() {
        // A client closing the pipe mid-answer is a normal shutdown, not a fault —
        // and the session must actually END. The attempt count is the load-bearing
        // assertion: returning `Ok` while continuing to write into a dead pipe would
        // satisfy a bare `is_ok()` check and still be wrong.
        let mut pipe = FailingWriter::new(io::ErrorKind::BrokenPipe);
        let mut server = Server::new();
        let result = serve(
            Cursor::new(TWO_REQUESTS.as_bytes().to_vec()),
            &mut pipe,
            &mut server,
        );
        assert!(result.is_ok(), "a broken pipe must not surface as an error");
        assert_eq!(
            pipe.attempts, 1,
            "the loop kept writing after the disconnect"
        );
    }

    #[test]
    fn a_write_failure_that_is_not_a_disconnect_is_reported() {
        // The mirror of the case above, and the reason the disconnect check has to be
        // narrow: a full disk or a revoked handle is a real fault, and swallowing it
        // would make the server exit 0 having answered nothing.
        let mut pipe = FailingWriter::new(io::ErrorKind::PermissionDenied);
        let mut server = Server::new();
        let error = serve(
            Cursor::new(TWO_REQUESTS.as_bytes().to_vec()),
            &mut pipe,
            &mut server,
        )
        .expect_err("a permission failure must surface");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    /// A reader that fails partway, standing in for a severed stdin.
    struct FailingReader;

    impl io::Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::ConnectionReset))
        }
    }

    impl io::BufRead for FailingReader {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            Err(io::Error::from(io::ErrorKind::ConnectionReset))
        }
        fn consume(&mut self, _: usize) {}
    }

    #[test]
    fn a_read_failure_is_reported_rather_than_swallowed() {
        // A severed stdin is not EOF: reporting it as a clean end of session would
        // make the process exit 0 on what is actually a lost connection.
        let mut server = Server::new();
        let error =
            serve(FailingReader, Vec::new(), &mut server).expect_err("a read failure must surface");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
    }

    #[test]
    fn a_legacy_session_runs_end_to_end_over_the_transport() {
        let output = session(concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tokfold_estimate","arguments":{"text":"{\"a\":1}"}}}"#,
            "\n",
        ));
        let written = lines(&output);
        assert_eq!(written.len(), 3, "the notification must not be answered");
        assert!(written[0].contains("2025-06-18"));
        assert!(written[1].contains("tokfold_compress"));
        assert!(written[2].contains("estTokensBefore"));
    }

    #[test]
    fn a_recovered_id_keeps_the_shape_the_client_sent() {
        // `recover_id` runs on the one path nothing else in this suite can reach: a line
        // whose handling panicked. There is no way to provoke that deliberately — the
        // bulkhead exists precisely for the panics nobody enumerated — so the function is
        // exercised directly rather than left as the only untested code in the file.
        //
        // What it has to get right is the id's *shape*, not just its presence. JSON-RPC
        // matches an answer to a call by comparing the id with `===`-style equality, so a
        // numeric 7 recovered as the string "7" leaves the client blocked on a request it
        // will never see answered — the exact hang the recovery exists to prevent.
        assert_eq!(
            recover_id(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#),
            Some(Id::Int(7)),
            "a numeric id comes back numeric"
        );
        assert_eq!(
            recover_id(r#"{"jsonrpc":"2.0","id":"abc","method":"tools/list"}"#),
            Some(Id::Str("abc".to_owned())),
            "a string id comes back as a string"
        );

        // Everything else yields no id, which makes the reply a notification-shaped
        // error rather than one addressed to the wrong call. Guessing would be worse
        // than admitting the id is unrecoverable.
        for unrecoverable in [
            r#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":1.5,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":{"nested":1},"method":"tools/list"}"#,
            "not json at all",
            "",
        ] {
            assert_eq!(
                recover_id(unrecoverable),
                None,
                "{unrecoverable:?} carries no usable id"
            );
        }

        // Recovery re-parses a line that has already proved it can break something, so
        // it must survive input that defeats the parser itself.
        let too_deep = format!("{{\"id\":1,\"x\":{}{}}}", "[".repeat(600), "]".repeat(600));
        assert_eq!(
            recover_id(&too_deep),
            None,
            "a line the parser refuses yields no id instead of panicking again"
        );
    }
}
