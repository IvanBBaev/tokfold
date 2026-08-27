//! The protocol dispatcher.
//!
//! Everything the server decides happens in [`Server::handle_line`]: a line of text
//! goes in, a line of text comes back, and nothing here touches a file descriptor.
//! That is deliberate — a protocol implementation that owns its I/O can only be
//! tested by spawning a process, and the interesting cases (a truncated line, a
//! duplicate handshake, an unsupported revision) are exactly the ones that are
//! painful to provoke that way. [`crate::stdio`] is the thin loop that adds the I/O.
//!
//! # Two eras in one server
//!
//! Revision `2026-07-28` removed the `initialize` handshake and made every request
//! carry its own protocol metadata. Deployed clients are overwhelmingly still on the
//! older revisions, so this server answers both: a client that opens with
//! `initialize` gets legacy semantics, a client that sends per-request `_meta` or
//! calls `server/discover` gets modern ones. The two paths share one dispatcher and
//! differ only in how the version is learned.
//!
//! # What is deliberately lenient
//!
//! A client that calls `tools/list` without ever having handshaked is served rather
//! than refused. Strictness there would buy nothing: this server holds no
//! session-scoped state that a handshake would have established, and refusing would
//! break clients over a formality. Strictness is spent where it changes an outcome —
//! an unsupported protocol revision, a second `initialize`, a malformed envelope.

use crate::MAX_MESSAGE_BYTES;
use crate::json::{Object, Value, parse};
use crate::jsonrpc::{
    ErrorObject, Request, Response, decode_request, echo, oversize_error, render_bounded,
};
use crate::protocol::{
    CACHE_SCOPE_PUBLIC, CACHE_TTL_MS, LATEST_LEGACY_VERSION, LATEST_PROTOCOL_VERSION,
    RESULT_TYPE_COMPLETE, SERVER_NAME, SERVER_VERSION, SUPPORTED_PROTOCOL_VERSIONS, error_code,
    is_modern_version, is_supported_version, meta, method,
};
use crate::tools;

/// Guidance handed to the model alongside the tool list.
///
/// Deliberately states the shape of input that benefits and the one guarantee that
/// matters, and claims no percentage — a saving depends entirely on the input.
pub const INSTRUCTIONS: &str = "\
tokfold reversibly compresses text so it costs fewer tokens in a prompt. It is built \
for JSON-shaped payloads — tool results, API responses, logs — where repeated keys \
and whitespace dominate. Embed the returned `rendering` in context and keep the \
`archive`; `tokfold_decompress` recovers the original bytes exactly. Call \
`tokfold_estimate` first when you want to know whether compressing is worth it. \
Input that does not compress is returned unchanged, never dropped.";

/// The protocol state machine.
///
/// One instance serves one client connection. It is `Send` and holds no I/O handles,
/// so a caller is free to own it wherever it likes.
#[derive(Debug)]
pub struct Server {
    /// Set once a legacy client completes `initialize`; a second one is a violation.
    handshaked: bool,
    /// The revision agreed during a legacy handshake, if there was one.
    negotiated_version: Option<String>,
    /// Largest frame [`Server::handle_line`] will return. See [`MAX_MESSAGE_BYTES`].
    max_message_bytes: usize,
}

impl Default for Server {
    /// Written out rather than derived: a derived `Default` would set the frame limit
    /// to `0`, which is a working server that refuses every call it can answer.
    fn default() -> Self {
        Self {
            handshaked: false,
            negotiated_version: None,
            max_message_bytes: MAX_MESSAGE_BYTES,
        }
    }
}

impl Server {
    /// Creates a server that has not yet seen a client.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the largest frame [`Server::handle_line`] may return.
    ///
    /// The default is [`MAX_MESSAGE_BYTES`], which is also what [`crate::stdio`] will
    /// read, so the two sides of that transport match. An embedder whose transport
    /// frames differently — a smaller message budget upstream, or a channel with no
    /// line limit at all — sets its own here. Raising it above what the peer's reader
    /// accepts recreates the asymmetry this bound exists to remove, so the number
    /// should be the receiver's limit, not this process's spare memory.
    ///
    /// Nothing else in the server reads this: it changes which answers are refused for
    /// being too large and nothing about how any answer is computed. That is what makes
    /// it usable in a test as the cheap way to cross the bound — a 64-byte limit
    /// exercises exactly the code a 32 MiB one does, without allocating 32 MiB.
    #[must_use]
    pub fn with_max_message_bytes(mut self, max: usize) -> Self {
        self.max_message_bytes = max;
        self
    }

    /// The largest frame this server will return, in bytes.
    #[must_use]
    pub fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    /// The revision agreed during a legacy `initialize`, or the newest supported
    /// revision if no handshake has happened.
    ///
    /// A modern client never handshakes, so this reports a default rather than
    /// anything negotiated for the whole of such a session. For diagnostics only —
    /// nothing in the server branches on it.
    #[must_use]
    pub fn protocol_version(&self) -> &str {
        self.negotiated_version
            .as_deref()
            .unwrap_or(LATEST_PROTOCOL_VERSION)
    }

    /// Handles one line of input, returning the line to write back.
    ///
    /// Returns `None` when there is nothing to send: a blank line, a notification, or
    /// a batch of nothing but notifications. Answering a notification would put an
    /// unmatched response on the client's stream, which JSON-RPC forbids and real
    /// clients treat as a fault — so the silence is load-bearing, not an omission.
    ///
    /// A line holding a JSON array is a batch: every member is handled in order and
    /// the answers come back as one array.
    ///
    /// The returned string never contains a newline, so the caller can frame it by
    /// appending one, and never exceeds [`Server::max_message_bytes`], so a caller that
    /// writes it to a transport with the same limit cannot emit a frame its own peer
    /// would refuse. A request whose answer does not fit is refused with an error
    /// addressed to that request's id; [`crate::jsonrpc::render_bounded`] states the
    /// rule and the reasoning.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        // Blank lines are tolerated rather than answered. They are not valid JSON, but
        // a stray newline from a client's writer is not worth an error frame.
        if line.trim().is_empty() {
            return None;
        }

        let value = match parse(line) {
            Ok(value) => value,
            Err(error) => {
                return Some(self.render(Response::error(
                    None,
                    ErrorObject::new(error_code::PARSE_ERROR, error.to_string()),
                )));
            }
        };

        if let Value::Array(items) = value {
            return self.handle_batch(items);
        }
        let response = self.handle_message(&value)?;
        Some(self.render(response))
    }

    /// Handles a batch: an array of messages answered with an array of responses.
    ///
    /// Batching is part of JSON-RPC 2.0, but of the revisions this server speaks only
    /// `2025-03-26` carries it: `2024-11-05` never had it and `2025-06-18` removed it
    /// again, so by `2026-07-28` there was nothing left to drop. Support is kept anyway
    /// — one revision on the list requires it, and a client that speaks a revision
    /// without it simply never sends one. Two rules from the
    /// specification are explicit here rather than incidental: an empty array is
    /// itself an invalid request, and a batch made entirely of notifications is
    /// answered with silence, not with an empty array.
    ///
    /// # When the batch as a whole does not fit
    ///
    /// A batch reply is one frame, so the limit applies to the array and not to its
    /// members — a thousand members that are each unremarkable can still add up past it.
    /// The array is therefore filled against a running budget, and the answer degrades
    /// in two steps, each losing exactly one more thing than the last:
    ///
    /// 1. A member whose real answer no longer fits the remaining space is replaced by
    ///    the oversize error **addressed to that member's own id**. Every other member
    ///    keeps its real answer. This is what makes the common case survivable: one
    ///    outsized call in a batch of otherwise small ones fails alone, and the client
    ///    does not have to reissue the calls that succeeded.
    /// 2. If not even that small refusal fits — the batch is long enough that the
    ///    per-member errors alone overflow — the whole array is dropped for a single
    ///    id-less error. Correlation is lost, which is the cost of a frame that cannot
    ///    hold one answer per call, and it is still an answer rather than a truncated
    ///    frame or silence.
    ///
    /// Members are handled before it is known whether the frame will fit, so a batch
    /// refused at step 2 may have had effects — a handshake in an earlier member stands.
    /// That is a property of batching itself, not of the limit: JSON-RPC gives a batch
    /// no atomicity, and the same is true of a batch whose reply is lost in transit.
    fn handle_batch(&mut self, items: Vec<Value>) -> Option<String> {
        let max = self.max_message_bytes;
        if items.is_empty() {
            return Some(self.render(Response::error(
                None,
                ErrorObject::new(error_code::INVALID_REQUEST, "a batch must not be empty"),
            )));
        }

        let mut frame = String::from("[");
        let mut answered = false;
        for item in items {
            let Some(response) = self.handle_message(&item) else {
                continue;
            };
            // What must still fit after this member: the comma in front of it when it
            // is not the first, and the closing bracket.
            let overhead = usize::from(answered) + 1;
            let budget = max.saturating_sub(frame.len() + overhead);
            let Some(member) = Self::fit_member(response, budget) else {
                return Some(Self::refuse_batch(max));
            };
            if answered {
                frame.push(',');
            }
            frame.push_str(&member);
            answered = true;
        }
        if !answered {
            return None;
        }
        frame.push(']');
        Some(frame)
    }

    /// Renders one batch member into `budget` bytes, or `None` if nothing fits.
    ///
    /// The ladder inside [`render_bounded`] does the work — the real answer, else the
    /// refusal addressed to this member's id, else an id-less one. `None` means even the
    /// last of those is too big for what is left, which is the signal to abandon the
    /// array rather than write a member that overruns the frame.
    fn fit_member(response: Response, budget: usize) -> Option<String> {
        let member = render_bounded(response, budget);
        (member.len() <= budget).then_some(member)
    }

    /// The single error that replaces a batch whose answers cannot be made to fit.
    ///
    /// Carries no id because the frame it replaces answered many, and no `data` because
    /// the payload is the thing that did not fit.
    fn refuse_batch(max: usize) -> String {
        render_bounded(Response::error(None, oversize_error(max)), max)
    }

    /// Handles one message: the only one on a line, or one member of a batch.
    ///
    /// Returns `None` when there is nothing to send back.
    fn handle_message(&mut self, value: &Value) -> Option<Response> {
        // The `id` check comes before envelope validation, not after. A message with
        // no `id` is a notification, and the rule that it is never answered holds
        // even when it is malformed: replying would put an unmatched frame on a
        // stream the client is not reading replies from. A non-object is not a
        // notification — it takes the ordinary invalid-request path below.
        if value.as_object().is_some() && value.get("id").is_none() {
            return None;
        }

        let request = match decode_request(value) {
            Ok(request) => request,
            // A refusal is addressed to the id the message carried, whenever it carried
            // one: the client is blocked on that id, and an unaddressed error tells it
            // only that *something* failed. `Rejection` recovers the id itself, so this
            // arm cannot drop it by omission.
            Err(rejection) => return Some(rejection.into_response()),
        };

        // Unreachable given the guard above, and kept as the type-level statement of
        // the same rule: whatever else changes, a notification gets no response.
        if request.is_notification() {
            return None;
        }

        let id = request.id.clone();
        Some(match self.dispatch(&request) {
            Ok(result) => Response::result(id, result),
            Err(error) => Response::error(id, error),
        })
    }

    /// Serializes a response as a single line of at most [`Server::max_message_bytes`].
    ///
    /// Takes the response by value so a tool result — which can be the whole
    /// compressed payload — is moved into the envelope rather than deep-cloned. Every
    /// non-batch answer goes through here, so the limit cannot be missed by a new
    /// branch that builds a `Response` and forgets to bound it.
    fn render(&self, response: Response) -> String {
        render_bounded(response, self.max_message_bytes)
    }

    /// Routes a validated request to its handler.
    fn dispatch(&mut self, request: &Request) -> Result<Value, ErrorObject> {
        // `server/discover` is checked like everything else. Discovery stays
        // non-circular because a request that names *no* revision is served rather
        // than refused — that is the escape hatch a client with no prior knowledge
        // uses. Naming a revision this server does not implement is a different thing,
        // and the specification makes refusing it a MUST on every method.
        check_request_metadata(request)?;

        match request.method.as_str() {
            method::SERVER_DISCOVER => Ok(Self::discover()),
            method::INITIALIZE => self.initialize(request),
            // Answered in either era. A modern client never sends it, and refusing a
            // legacy client's liveness probe buys nothing.
            method::PING => Ok(with_envelope(Object::new().build())),
            method::TOOLS_LIST => Self::tools_list(request),
            method::TOOLS_CALL => Self::tools_call(request),
            // `echo` bounds the quoted name: `other` is whatever the peer wrote, and a
            // refusal is not a reason to copy a megabyte of it back out. See
            // [`crate::jsonrpc::echo`].
            other => Err(ErrorObject::new(
                error_code::METHOD_NOT_FOUND,
                format!("unknown method: {}", echo(other)),
            )),
        }
    }

    /// Answers `server/discover`: what this server is and what it speaks.
    ///
    /// The answer is a cacheable result, so it carries the same freshness hints as the
    /// tool catalogue and for the same reason: both are compiled in, and the only event
    /// that can change either is the binary being replaced.
    fn discover() -> Value {
        let versions = SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .map(|version| Value::string(*version))
            .collect();
        with_envelope(
            Object::new()
                .set("supportedVersions", Value::Array(versions))
                .set("capabilities", capabilities())
                .set("serverInfo", server_info())
                .set("instructions", Value::string(INSTRUCTIONS))
                .set("ttlMs", Value::Int(cache_ttl_ms()))
                .set("cacheScope", Value::string(CACHE_SCOPE_PUBLIC))
                .build(),
        )
    }

    /// Answers the legacy `initialize` handshake.
    ///
    /// Version negotiation follows the older revisions' rule: echo the client's
    /// requested revision if it is supported, otherwise answer with a supported one and
    /// let the client decide whether to continue. `params.protocolVersion` is never
    /// answered with the newer `-32022` — a legacy client does not know that code, and
    /// a supported version in the reply is what its state machine expects.
    ///
    /// That fallback covers the handshake field and nothing else. A revision named in
    /// `params._meta` is the *modern* declaration, and [`check_request_metadata`] holds
    /// every method to it before dispatch, `initialize` included: a handshake carrying
    /// an unsupported revision in `_meta` is answered with `-32022` and never reaches
    /// this function. Nothing sends both fields today — they belong to different eras —
    /// so the combination is documented rather than special-cased; whether `initialize`
    /// should be exempt from the modern check is a contract question, not a defect.
    ///
    /// The fallback is the newest *legacy* revision, not the newest revision outright.
    /// A client that opens with `initialize` has proved it speaks the handshake, and
    /// naming `2026-07-28` back at it would name the one revision that removed the
    /// handshake — a version it cannot use. The same clamp applies if a client asks for
    /// a modern revision by name: `initialize` selects legacy semantics, so the answer
    /// has to be a legacy revision.
    fn initialize(&mut self, request: &Request) -> Result<Value, ErrorObject> {
        if self.handshaked {
            return Err(ErrorObject::new(
                error_code::INVALID_REQUEST,
                "`initialize` was already completed on this connection",
            ));
        }
        let requested = request
            .param("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(LATEST_LEGACY_VERSION);
        let agreed = if is_supported_version(requested) && !is_modern_version(requested) {
            requested
        } else {
            LATEST_LEGACY_VERSION
        };

        self.handshaked = true;
        self.negotiated_version = Some(agreed.to_owned());

        Ok(with_envelope(
            Object::new()
                .set("protocolVersion", Value::string(agreed))
                .set("capabilities", capabilities())
                .set("serverInfo", server_info())
                .set("instructions", Value::string(INSTRUCTIONS))
                .build(),
        ))
    }

    /// Answers `tools/list`.
    ///
    /// The catalogue is a compile-time constant, so it is returned whole with no
    /// `nextCursor`; the cache hints say so explicitly rather than leaving a client to
    /// re-fetch it on every turn.
    fn tools_list(request: &Request) -> Result<Value, ErrorObject> {
        // The catalogue is one page, so this server never mints a cursor — every cursor
        // it could be shown is one it did not issue. Answering page one anyway would
        // look to a paginating client like a list that never ends.
        if request
            .param("cursor")
            .is_some_and(|cursor| !cursor.is_null())
        {
            return Err(ErrorObject::new(
                error_code::INVALID_PARAMS,
                "`params.cursor` is not a cursor this server issued: the tool list is a single page",
            ));
        }
        Ok(with_envelope(
            Object::new()
                .set("tools", tools::catalogue())
                .set("ttlMs", Value::Int(cache_ttl_ms()))
                .set("cacheScope", Value::string(CACHE_SCOPE_PUBLIC))
                .build(),
        ))
    }

    /// Answers `tools/call`.
    ///
    /// A failure *inside* a tool comes back as a result with `isError` set, not as a
    /// JSON-RPC error: the model has to be able to read it and react. A JSON-RPC error
    /// is reserved for a call that was malformed — no name, an unknown name, or
    /// arguments of the wrong shape.
    fn tools_call(request: &Request) -> Result<Value, ErrorObject> {
        let name = request
            .param("name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ErrorObject::new(
                    error_code::INVALID_PARAMS,
                    "`name` is required and must be a string",
                )
            })?;
        let arguments = request.param("arguments");
        let outcome = tools::call(name, arguments)?;

        Ok(with_envelope(
            Object::new()
                .set("content", text_content(&outcome.text))
                .set("structuredContent", outcome.structured)
                .set("isError", Value::Bool(outcome.is_error))
                .build(),
        ))
    }
}

/// Validates the protocol metadata a modern request must carry.
///
/// A request that names no revision is served as legacy, which is what an older
/// client sends. A request that names one must name a revision this server implements,
/// and — if that revision is modern — must also carry the client capabilities the
/// stateless model relies on, since there is no handshake left to have declared them.
///
/// *Absent* and *present but of the wrong type* are kept apart. Treating
/// `"_meta": "2026-07-28"` or a numeric `protocolVersion` as "no revision named" would
/// silently serve a modern client under legacy rules and skip the capability check it
/// was relying on, so a malformed field is an error rather than a fallback.
fn check_request_metadata(request: &Request) -> Result<(), ErrorObject> {
    let Some(meta_object) = request.param("_meta") else {
        return Ok(());
    };
    if meta_object.as_object().is_none() {
        return Err(ErrorObject::new(
            error_code::INVALID_PARAMS,
            "`params._meta` must be an object",
        ));
    }
    let Some(version_value) = meta_object.get(meta::PROTOCOL_VERSION) else {
        return Ok(());
    };
    let Some(version) = version_value.as_str() else {
        return Err(ErrorObject::new(
            error_code::INVALID_PARAMS,
            format!("`_meta.{}` must be a string", meta::PROTOCOL_VERSION),
        ));
    };

    if !is_supported_version(version) {
        let supported = SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .map(|entry| Value::string(*entry))
            .collect();
        // The unbounded string arrives twice here — once in the message, once in
        // `data.requested` — so this refusal used to answer a request with roughly twice
        // its own size. Both copies go through `echo`.
        let requested = echo(version);
        return Err(ErrorObject::new(
            error_code::UNSUPPORTED_PROTOCOL_VERSION,
            format!("unsupported protocol version: {requested}"),
        )
        .with_data(
            Object::new()
                .set("supported", Value::Array(supported))
                .set("requested", Value::string(requested))
                .build(),
        ));
    }

    if is_modern_version(version) {
        match meta_object.get(meta::CLIENT_CAPABILITIES) {
            None => {
                return Err(ErrorObject::new(
                    error_code::INVALID_PARAMS,
                    format!(
                        "`_meta.{}` is required at {version}",
                        meta::CLIENT_CAPABILITIES
                    ),
                ));
            }
            // Checked for shape as well as presence, symmetrically with the version
            // above: a capability set that is not an object cannot be read as one, and
            // accepting it would mean the declaration was never really made.
            Some(capabilities) if capabilities.as_object().is_none() => {
                return Err(ErrorObject::new(
                    error_code::INVALID_PARAMS,
                    format!("`_meta.{}` must be an object", meta::CLIENT_CAPABILITIES),
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Result member naming the request/response pattern. Owned by [`with_envelope`].
const KEY_RESULT_TYPE: &str = "resultType";

/// Result member carrying per-response metadata. Owned by [`with_envelope`].
const KEY_META: &str = "_meta";

/// Adds the members every result of this server carries.
///
/// `resultType` is required from `2026-07-28` onward and ignored by older clients, so
/// it is set unconditionally rather than branched on — one code path is easier to keep
/// correct than two, and the field costs a legacy client nothing.
///
/// # The two envelope keys are reserved
///
/// [`Object::set`] appends without deduplicating and [`Value::get`] is first-wins, so a
/// result that already carried [`KEY_RESULT_TYPE`] or [`KEY_META`] would have its own
/// copy read by a client and the server's copy silently ignored — a tool would be able
/// to overwrite the protocol metadata of the response carrying it.
///
/// **This is currently unreachable, and the assertion below is not evidence of a past
/// bug.** Every key a result can hold is compiled in and the whole set was enumerated:
/// `compressed`, `stats`, `rendering`, `archive`, `reason`, `reasonCode`, `text`,
/// `code`, and `message` from [`crate::tools`], plus `content`, `structuredContent`, and
/// `isError` added on the `tools/call` path. None collides. What the assertion buys is
/// that the day someone adds a tool field named `_meta`, a debug build says so instead
/// of a release build shipping a response whose metadata came from the wrong layer.
fn with_envelope(result: Value) -> Value {
    let mut object = Object::new();
    if let Value::Object(members) = result {
        for (key, value) in members {
            debug_assert!(
                key != KEY_RESULT_TYPE && key != KEY_META,
                "tool result carries the reserved envelope key `{key}`; it would shadow \
                 the envelope's own copy, because object members are first-wins"
            );
            object = object.set(&key, value);
        }
    }
    object
        .set(KEY_RESULT_TYPE, Value::string(RESULT_TYPE_COMPLETE))
        .set(
            KEY_META,
            Object::new().set(meta::SERVER_INFO, server_info()).build(),
        )
        .build()
}

/// What this server can do. `listChanged` is false and always will be: the catalogue
/// is compiled in, so there is no event that could change it while running.
fn capabilities() -> Value {
    Object::new()
        .set(
            "tools",
            Object::new().set("listChanged", Value::Bool(false)).build(),
        )
        .build()
}

fn server_info() -> Value {
    Object::new()
        .set("name", Value::string(SERVER_NAME))
        .set("version", Value::string(SERVER_VERSION))
        .build()
}

/// The catalogue's freshness hint, narrowed to the JSON integer type.
fn cache_ttl_ms() -> i64 {
    i64::try_from(CACHE_TTL_MS).unwrap_or(i64::MAX)
}

/// Wraps text in the single-element `content` array MCP expects.
fn text_content(text: &str) -> Value {
    Value::Array(vec![
        Object::new()
            .set("type", Value::string("text"))
            .set("text", Value::string(text))
            .build(),
    ])
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{KEY_META, KEY_RESULT_TYPE, MAX_MESSAGE_BYTES, Server, with_envelope};
    use crate::json::{Object, Value, parse};
    use crate::jsonrpc::{ECHO_ELLIPSIS, MAX_ECHO_BYTES};
    use crate::protocol::{
        CACHE_SCOPE_PUBLIC, CACHE_TTL_MS, LATEST_LEGACY_VERSION, LATEST_PROTOCOL_VERSION,
        RESULT_TYPE_COMPLETE, SERVER_NAME, SERVER_VERSION, SUPPORTED_PROTOCOL_VERSIONS, error_code,
        meta,
    };

    /// Sends one line and parses whatever came back.
    fn exchange(server: &mut Server, line: &str) -> Value {
        let reply = server
            .handle_line(line)
            .unwrap_or_else(|| panic!("expected a reply to {line}"));
        parse(&reply).unwrap()
    }

    fn result_of(server: &mut Server, line: &str) -> Value {
        let reply = exchange(server, line);
        assert!(
            reply.get("error").is_none(),
            "unexpected error for {line}: {reply}"
        );
        reply.get("result").cloned().expect("no result member")
    }

    fn error_code_of(server: &mut Server, line: &str) -> i64 {
        exchange(server, line)
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_i64)
            .expect("no error code")
    }

    #[test]
    fn a_blank_line_is_ignored() {
        let mut server = Server::new();
        assert!(server.handle_line("").is_none());
        assert!(server.handle_line("   ").is_none());
        assert!(server.handle_line("\t").is_none());
    }

    #[test]
    fn malformed_json_is_a_parse_error_with_no_id() {
        let mut server = Server::new();
        let reply = exchange(&mut server, "{not json");
        assert_eq!(
            reply.get("error").and_then(|e| e.get("code")),
            Some(&Value::Int(i64::from(error_code::PARSE_ERROR)))
        );
        // Absent, not null: MCP admits only a string or a number as an id, and lets an
        // error response leave the member out when the request's own id was unreadable.
        assert_eq!(reply.get("id"), None);
    }

    #[test]
    fn a_bad_envelope_is_an_invalid_request() {
        let mut server = Server::new();
        assert_eq!(
            error_code_of(&mut server, r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#),
            i64::from(error_code::INVALID_REQUEST)
        );
    }

    #[test]
    fn notifications_are_never_answered() {
        let mut server = Server::new();
        for notification in [
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
            // Even an unknown notification stays unanswered: JSON-RPC forbids replying
            // to a message with no id, including with an error.
            r#"{"jsonrpc":"2.0","method":"nonsense/unknown"}"#,
        ] {
            assert!(
                server.handle_line(notification).is_none(),
                "answered {notification}"
            );
        }
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        let mut server = Server::new();
        assert_eq!(
            error_code_of(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#),
            i64::from(error_code::METHOD_NOT_FOUND)
        );
    }

    #[test]
    fn every_reply_is_a_single_line() {
        let mut server = Server::new();
        for line in [
            "{not json",
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"tokfold_compress","arguments":{"text":"{\"a\":1}"}}}"#,
        ] {
            let reply = server.handle_line(line).unwrap();
            assert!(
                !reply.contains('\n') && !reply.contains('\r'),
                "reply to {line} broke framing"
            );
        }
    }

    #[test]
    fn discover_reports_versions_capabilities_and_instructions() {
        let mut server = Server::new();
        let result = result_of(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover"}"#,
        );
        let versions = result.get("supportedVersions").unwrap().as_array().unwrap();
        assert_eq!(
            versions.first().and_then(Value::as_str),
            Some(LATEST_PROTOCOL_VERSION)
        );
        // The whole supported table has to be advertised, not just the newest entry:
        // a legacy client picks its revision from this list.
        let advertised: Vec<&str> = versions.iter().filter_map(Value::as_str).collect();
        assert_eq!(advertised, SUPPORTED_PROTOCOL_VERSIONS);
        assert!(
            result
                .get("capabilities")
                .and_then(Value::as_object)
                .is_some()
        );
        assert_eq!(
            result
                .get("serverInfo")
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str),
            Some(SERVER_NAME)
        );
        assert_eq!(
            result
                .get("serverInfo")
                .and_then(|info| info.get("version"))
                .and_then(Value::as_str),
            Some(SERVER_VERSION)
        );
        // Instructions are what tells a model when reaching for these tools is worth
        // it; an empty string would satisfy a presence check and help nobody.
        let instructions = result.get("instructions").and_then(Value::as_str).unwrap();
        assert!(instructions.contains("tokfold"), "{instructions}");
    }

    #[test]
    fn discover_carries_the_cache_hints_its_result_type_requires() {
        // `DiscoverResult` is a cacheable result, and on those the two hints are
        // required rather than advisory — a client cannot cache a reply that never
        // says for how long or on whose behalf.
        let mut server = Server::new();
        let result = result_of(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover"}"#,
        );
        assert_eq!(
            result.get("ttlMs").and_then(Value::as_i64),
            Some(i64::try_from(CACHE_TTL_MS).unwrap())
        );
        assert_eq!(
            result.get("cacheScope").and_then(Value::as_str),
            Some(CACHE_SCOPE_PUBLIC)
        );
    }

    #[test]
    fn a_discover_declaring_an_unsupported_revision_is_refused() {
        // Discover is exempt from *needing* metadata, not from being held to it. A
        // request that names a revision this server does not implement is refused on
        // every method, discover included.
        let mut server = Server::new();
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{{"_meta":{{"{}":"1999-01-01","{}":{{}}}}}}}}"#,
            meta::PROTOCOL_VERSION,
            meta::CLIENT_CAPABILITIES
        );
        assert_eq!(
            error_code_of(&mut server, &line),
            i64::from(error_code::UNSUPPORTED_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn a_request_that_names_no_revision_is_served_on_every_method() {
        // A client calls discover precisely because it does not yet know what to
        // declare, so demanding metadata there would be circular. The leniency is not a
        // discover-shaped hole, though — there is no exemption in the code at all. This
        // server holds no session state a declaration would have established, so a bare
        // envelope is served on whatever method it names, and discovery is non-circular
        // as a consequence of that rather than as a special case.
        //
        // What is *not* lenient is naming a revision this server does not implement:
        // that is refused on every method, discover included, which
        // `a_discover_declaring_an_unsupported_revision_is_refused` pins from the other
        // side. Together the two say the rule is about the revision named, never about
        // which method named it.
        for bare in [
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tokfold_estimate","arguments":{"text":"{\"a\":1}"}}}"#,
        ] {
            // A fresh server per line: `initialize` is once-only, and reusing one
            // instance would turn a leniency check into a handshake-order check.
            let mut server = Server::new();
            assert!(
                exchange(&mut server, bare).get("error").is_none(),
                "a bare envelope was refused on {bare}"
            );
        }
    }

    #[test]
    fn a_handshake_declaring_an_unsupported_revision_in_meta_is_refused() {
        // `initialize` is not exempt from the modern version check: it runs before the
        // method match, so a handshake that also carries `_meta` metadata is refused
        // with -32022 rather than falling back the way `params.protocolVersion` does.
        // Nothing sends both fields — they belong to different eras — so this pins
        // current behaviour and keeps the doc on `initialize` honest about it. Whether
        // the handshake ought to be exempt is a contract question for the owner, not
        // something this test decides.
        let mut server = Server::new();
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","_meta":{{"{}":"1999-01-01"}}}}}}"#,
            meta::PROTOCOL_VERSION
        );
        assert_eq!(
            error_code_of(&mut server, &line),
            i64::from(error_code::UNSUPPORTED_PROTOCOL_VERSION)
        );
        // And the refusal happened before any state was taken: the handshake did not
        // count, so the client can still open the session properly.
        assert!(!server.handshaked, "a refused handshake must not latch");
    }

    #[test]
    fn every_result_carries_the_modern_envelope() {
        let mut server = Server::new();
        for line in [
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
        ] {
            let result = result_of(&mut server, line);
            assert_eq!(
                result.get("resultType").and_then(Value::as_str),
                Some("complete"),
                "no resultType on {line}"
            );
            assert!(
                result
                    .get("_meta")
                    .and_then(|m| m.get(meta::SERVER_INFO))
                    .is_some(),
                "no serverInfo on {line}"
            );
        }
    }

    #[test]
    fn tools_list_carries_the_cache_hints_the_spec_requires() {
        let mut server = Server::new();
        let result = result_of(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        );
        assert!(result.get("ttlMs").and_then(Value::as_i64).is_some());
        assert_eq!(
            result.get("cacheScope").and_then(Value::as_str),
            Some("public")
        );
        assert_eq!(result.get("tools").unwrap().as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_legacy_client_can_handshake_and_get_its_version_echoed() {
        let mut server = Server::new();
        let result = result_of(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}"#,
        );
        assert_eq!(
            result.get("protocolVersion").and_then(Value::as_str),
            Some("2025-06-18")
        );
        assert_eq!(server.protocol_version(), "2025-06-18");
    }

    #[test]
    fn an_unknown_handshake_version_falls_back_instead_of_failing() {
        // A legacy client does not know -32022; the older rule is to answer with a
        // version the server does support and let the client decide.
        let mut server = Server::new();
        let result = result_of(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        // The fallback is the newest *legacy* revision, not the newest one: the client
        // has just proved it speaks the handshake, and the newest revision is the one
        // that removed it. Naming it back would hand the client a version it cannot use.
        assert_eq!(
            result.get("protocolVersion").and_then(Value::as_str),
            Some(LATEST_LEGACY_VERSION)
        );
        assert_ne!(LATEST_LEGACY_VERSION, LATEST_PROTOCOL_VERSION);
    }

    #[test]
    fn a_handshake_is_never_answered_with_a_handshakeless_revision() {
        // Asking for a modern revision through `initialize` is a contradiction: the
        // request selects legacy semantics, so the answer has to be a legacy revision
        // even though the one asked for is on the supported list. Same for a handshake
        // that names no version at all.
        for opener in [
            format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{LATEST_PROTOCOL_VERSION}"}}}}"#
            ),
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.to_owned(),
        ] {
            let mut server = Server::new();
            let result = result_of(&mut server, &opener);
            let agreed = result.get("protocolVersion").and_then(Value::as_str);
            assert_eq!(agreed, Some(LATEST_LEGACY_VERSION), "{opener}");
            assert!(
                !crate::protocol::is_modern_version(agreed.unwrap()),
                "{opener}"
            );
        }
    }

    #[test]
    fn a_second_handshake_is_rejected() {
        let mut server = Server::new();
        let open = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#;
        let _ = result_of(&mut server, open);
        assert_eq!(
            error_code_of(&mut server, open),
            i64::from(error_code::INVALID_REQUEST)
        );
    }

    #[test]
    fn a_legacy_session_runs_end_to_end() {
        let mut server = Server::new();
        let _ = result_of(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        );
        assert!(
            server
                .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_none()
        );
        let listed = result_of(
            &mut server,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        );
        assert_eq!(listed.get("tools").unwrap().as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_modern_request_is_served_without_a_handshake() {
        let mut server = Server::new();
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{"_meta":{{"{}":"{}","{}":{{}}}}}}}}"#,
            meta::PROTOCOL_VERSION,
            LATEST_PROTOCOL_VERSION,
            meta::CLIENT_CAPABILITIES
        );
        assert!(exchange(&mut server, &line).get("error").is_none());
    }

    #[test]
    fn an_unsupported_declared_version_is_rejected_with_the_spec_code() {
        let mut server = Server::new();
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{"_meta":{{"{}":"1999-01-01"}}}}}}"#,
            meta::PROTOCOL_VERSION
        );
        let reply = exchange(&mut server, &line);
        let error = reply.get("error").unwrap();
        assert_eq!(
            error.get("code"),
            Some(&Value::Int(i64::from(
                error_code::UNSUPPORTED_PROTOCOL_VERSION
            )))
        );
        // The client needs to know what it could have asked for.
        let data = error.get("data").unwrap();
        assert!(data.get("supported").unwrap().as_array().is_some());
        assert_eq!(
            data.get("requested").and_then(Value::as_str),
            Some("1999-01-01")
        );
    }

    #[test]
    fn a_modern_request_without_client_capabilities_is_invalid() {
        let mut server = Server::new();
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{"_meta":{{"{}":"{}"}}}}}}"#,
            meta::PROTOCOL_VERSION,
            LATEST_PROTOCOL_VERSION
        );
        assert_eq!(
            error_code_of(&mut server, &line),
            i64::from(error_code::INVALID_PARAMS)
        );
    }

    #[test]
    fn client_capabilities_that_are_not_an_object_are_invalid() {
        // Present but of the wrong type is not "declared". The field is checked the
        // same way the version beside it is, or a client could satisfy the rule with
        // any scalar and the server would go on to read an object that is not there.
        let mut server = Server::new();
        for capabilities in ["true", "\"none\"", "[]", "null", "7"] {
            let line = format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{"_meta":{{"{}":"{}","{}":{capabilities}}}}}}}"#,
                meta::PROTOCOL_VERSION,
                LATEST_PROTOCOL_VERSION,
                meta::CLIENT_CAPABILITIES
            );
            assert_eq!(
                error_code_of(&mut server, &line),
                i64::from(error_code::INVALID_PARAMS),
                "accepted {capabilities} as client capabilities"
            );
        }
    }

    #[test]
    fn a_tools_list_cursor_this_server_never_issued_is_refused() {
        // The catalogue is a single page, so this server hands out no cursor at all.
        // A cursor coming back can only be one it did not issue, and the specification
        // makes that an invalid-params error rather than a silently ignored field.
        let mut server = Server::new();
        assert_eq!(
            error_code_of(
                &mut server,
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"cursor":"page-2"}}"#
            ),
            i64::from(error_code::INVALID_PARAMS)
        );
        // An explicit null is the JSON way to say "no cursor", so it stays acceptable.
        assert!(
            exchange(
                &mut server,
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"cursor":null}}"#
            )
            .get("error")
            .is_none()
        );
    }

    #[test]
    fn a_legacy_declared_version_needs_no_client_capabilities() {
        let mut server = Server::new();
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{"_meta":{{"{}":"2025-06-18"}}}}}}"#,
            meta::PROTOCOL_VERSION
        );
        assert!(exchange(&mut server, &line).get("error").is_none());
    }

    #[test]
    fn a_tool_call_returns_content_and_structured_output() {
        let mut server = Server::new();
        let result = result_of(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tokfold_compress","arguments":{"text":"{\"a\":1}"}}}"#,
        );
        let content = result.get("content").unwrap().as_array().unwrap();
        assert_eq!(
            content.first().and_then(|block| block.get("type")),
            Some(&Value::string("text"))
        );
        assert_eq!(result.get("isError"), Some(&Value::Bool(false)));
        // The text block is what the model reads and the structured block is what
        // the client branches on; either being empty makes the call useless.
        assert!(
            content
                .first()
                .and_then(|block| block.get("text"))
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty())
        );
        let structured = result.get("structuredContent").unwrap();
        assert_eq!(structured.get("compressed"), Some(&Value::Bool(true)));
        assert!(structured.get("archive").and_then(Value::as_str).is_some());
        assert!(structured.get("stats").and_then(Value::as_object).is_some());
    }

    #[test]
    fn a_failing_tool_is_a_result_not_a_transport_error() {
        // The model has to be able to see this and react; a JSON-RPC error would be
        // swallowed by the client as a transport fault.
        let mut server = Server::new();
        let result = result_of(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tokfold_decompress","arguments":{"archive":"AAAAAAAAAAAAAAAA"}}}"#,
        );
        assert_eq!(result.get("isError"), Some(&Value::Bool(true)));
        // The code is the caller's branch point, so its exact spelling is the
        // contract; asserting only that some code exists would let any string pass.
        assert_eq!(
            result
                .get("structuredContent")
                .and_then(|structured| structured.get("code"))
                .and_then(Value::as_str),
            Some("bad_magic")
        );
    }

    #[test]
    fn a_malformed_tool_call_is_a_transport_error() {
        let mut server = Server::new();
        for bad in [
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope"}}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tokfold_compress"}}"#,
        ] {
            assert_eq!(
                error_code_of(&mut server, bad),
                i64::from(error_code::INVALID_PARAMS),
                "wrong code for {bad}"
            );
        }
    }

    #[test]
    fn a_round_trip_through_the_protocol_recovers_the_input() {
        let mut server = Server::new();
        let original = r#"{"rows":[{"id":1,"v":"a"},{"id":2,"v":"b"}]}"#;
        let arguments = crate::json::Object::new()
            .set("text", Value::string(original))
            .build();
        let params = crate::json::Object::new()
            .set("name", Value::string("tokfold_compress"))
            .set("arguments", arguments)
            .build();
        let call = crate::json::Object::new()
            .set("jsonrpc", Value::string("2.0"))
            .set("id", Value::Int(1))
            .set("method", Value::string("tools/call"))
            .set("params", params)
            .build();
        let archive = result_of(&mut server, &call.to_string())
            .get("structuredContent")
            .and_then(|structured| structured.get("archive"))
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();

        let back_params = crate::json::Object::new()
            .set("name", Value::string("tokfold_decompress"))
            .set(
                "arguments",
                crate::json::Object::new()
                    .set("archive", Value::string(archive))
                    .build(),
            )
            .build();
        let back = crate::json::Object::new()
            .set("jsonrpc", Value::string("2.0"))
            .set("id", Value::Int(2))
            .set("method", Value::string("tools/call"))
            .set("params", back_params)
            .build();
        let restored = result_of(&mut server, &back.to_string());
        assert_eq!(
            restored
                .get("structuredContent")
                .and_then(|structured| structured.get("text"))
                .and_then(Value::as_str),
            Some(original)
        );
    }

    #[test]
    fn identical_requests_produce_identical_bytes() {
        // Prompt-cache stability: a reply that varies between runs invalidates the
        // client's cache for no reason.
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let first = Server::new().handle_line(line).expect("no reply");
        let second = Server::new().handle_line(line).expect("no reply");
        assert_eq!(first, second);
        // Two `None`s would also compare equal, so the reply has to be real.
        assert!(first.contains("tokfold_compress"));
    }

    #[test]
    fn the_response_id_always_matches_the_request() {
        let mut server = Server::new();
        assert_eq!(
            exchange(
                &mut server,
                r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#
            )
            .get("id"),
            Some(&Value::string("abc"))
        );
        assert_eq!(
            exchange(&mut server, r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#).get("id"),
            Some(&Value::Int(42))
        );
    }

    /// A default server is bounded by the transport's own limit.
    ///
    /// The seam that makes the rest of these tests cheap is also a way to build a server
    /// with no useful bound at all, so what the default is has to be pinned separately
    /// from what the seam does.
    #[test]
    fn a_server_is_bounded_by_the_transport_limit_unless_told_otherwise() {
        assert_eq!(Server::new().max_message_bytes(), MAX_MESSAGE_BYTES);
        assert_eq!(Server::default().max_message_bytes(), MAX_MESSAGE_BYTES);
        assert_eq!(
            Server::new().with_max_message_bytes(64).max_message_bytes(),
            64
        );
    }

    /// A single reply that will not fit is refused by id, not truncated or dropped.
    ///
    /// The same ladder is exercised at 32 MiB by a real payload in `tests/session.rs`;
    /// here the limit is lowered instead, because the branch taken is identical and a
    /// unit test that allocates 32 MiB to reach it earns nothing.
    #[test]
    fn a_reply_larger_than_the_limit_is_refused_with_the_requests_own_id() {
        let mut server = Server::new().with_max_message_bytes(120);
        let reply = exchange(
            &mut server,
            r#"{"jsonrpc":"2.0","id":"abc","method":"tools/list"}"#,
        );

        assert_eq!(reply.get("id"), Some(&Value::string("abc")));
        assert!(
            reply.get("result").is_none(),
            "the catalogue must not fit: {reply}"
        );
        assert_eq!(
            reply
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_i64),
            Some(i64::from(error_code::INVALID_PARAMS))
        );
        // The refusal is the thing that has to fit; a bound that only moves the overrun
        // one frame later is not a bound.
        assert!(
            server
                .handle_line(r#"{"jsonrpc":"2.0","id":"abc","method":"tools/list"}"#)
                .is_some_and(|frame| frame.len() <= 120)
        );
    }

    /// One outsized member of a batch fails alone; the rest keep their real answers.
    ///
    /// This is the whole reason the batch path fills against a running budget instead of
    /// rendering the array and checking its length: collapsing the batch would make a
    /// client reissue the calls that succeeded, and it would have no way to tell which
    /// member was the expensive one.
    #[test]
    fn one_oversized_member_of_a_batch_is_refused_without_taking_the_others_down() {
        // Enough for two `ping` results and a refusal, not enough for the tool catalogue.
        let mut server = Server::new().with_max_message_bytes(400);
        let batch = concat!(
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"},"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}]"#
        );
        let frame = server
            .handle_line(batch)
            .expect("a batch of requests is answered");
        assert!(
            frame.len() <= 400,
            "the aggregate must fit: {} bytes",
            frame.len()
        );

        let replies = parse(&frame).unwrap();
        let replies = replies
            .as_array()
            .expect("a batch is answered with an array");
        assert_eq!(replies.len(), 3, "every member is still answered: {frame}");
        assert!(
            replies[0].get("result").is_some(),
            "member 1 kept its answer"
        );
        assert!(
            replies[2].get("result").is_some(),
            "member 3 kept its answer"
        );
        assert_eq!(
            replies[1]
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_i64),
            Some(i64::from(error_code::INVALID_PARAMS)),
            "only the member that did not fit fails: {frame}"
        );
        assert_eq!(
            replies[1].get("id"),
            Some(&Value::Int(2)),
            "and it fails by id"
        );
    }

    /// A batch whose per-member refusals cannot fit collapses to one id-less error.
    ///
    /// The last rung of the ladder, and the only place correlation is lost. It is
    /// reachable in principle at any limit — a batch of hundreds of thousands of
    /// notifications-with-ids is a legal 32 MiB line — so it is a real branch rather
    /// than a defensive one, and it must still produce a well-formed answer.
    #[test]
    fn a_batch_that_cannot_fit_even_as_errors_collapses_to_a_single_refusal() {
        let mut server = Server::new().with_max_message_bytes(100);
        let batch = concat!(
            r#"[{"jsonrpc":"2.0","id":1,"method":"tools/list"},"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}]"#
        );
        let frame = server
            .handle_line(batch)
            .expect("a batch of requests is answered");

        let reply = parse(&frame).unwrap();
        assert!(
            reply.as_array().is_none(),
            "the array is gone, not shortened: {frame}"
        );
        assert!(
            reply.get("id").is_none(),
            "no single id can stand for the batch"
        );
        assert_eq!(
            reply
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_i64),
            Some(i64::from(error_code::INVALID_PARAMS))
        );
    }

    /// The limit does not disturb the answers that fit under it.
    ///
    /// A batch reply is assembled by hand now — brackets and commas written out rather
    /// than rendered from a `Value::Array` — so the framing is code that can be wrong by
    /// one character. This compares a bounded assembly against an unbounded one for the
    /// same batch, which is the only assertion that would catch a missing separator.
    #[test]
    fn a_batch_within_the_limit_is_assembled_exactly_as_before() {
        let batch = concat!(
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},"#,
            r#"{"jsonrpc":"2.0","method":"notify"},"#,
            r#"{"jsonrpc":"2.0","id":"two","method":"ping"}]"#
        );
        let generous = Server::new().handle_line(batch).expect("a reply");
        let tight = Server::new()
            .with_max_message_bytes(generous.len())
            .handle_line(batch)
            .expect("a reply");
        assert_eq!(generous, tight, "a frame that exactly fits is unmodified");

        let replies = parse(&generous).unwrap();
        let replies = replies.as_array().expect("an array");
        assert_eq!(
            replies.len(),
            2,
            "the notification is still unanswered: {generous}"
        );
        assert_eq!(replies[0].get("id"), Some(&Value::Int(1)));
        assert_eq!(replies[1].get("id"), Some(&Value::string("two")));
    }

    /// The longest a bounded echo can be: the cut plus its marker.
    const MAX_ECHOED: usize = MAX_ECHO_BYTES + ECHO_ELLIPSIS.len();

    /// Pulls the human-readable message out of an error reply.
    fn error_message(server: &mut Server, line: &str) -> String {
        exchange(server, line)
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .expect("no error message")
            .to_owned()
    }

    #[test]
    fn an_ordinary_name_is_still_quoted_in_full() {
        // Bounding the echo must not cost the diagnostic. Every name a real client can
        // get wrong is short, and it comes back whole.
        let mut server = Server::new();
        let message = error_message(
            &mut server,
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#,
        );
        assert_eq!(message, "unknown method: resources/list");

        let call = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{}","arguments":{{}}}}}}"#,
            "tokfold_compres"
        );
        assert_eq!(
            error_message(&mut server, &call),
            "unknown tool: tokfold_compres"
        );
    }

    #[test]
    fn a_hostile_method_name_is_not_quoted_back_in_full() {
        // A method name is peer-supplied and bounded only by the frame limit, so before
        // the echo was cut the whole of it came back inside `message`.
        let mut server = Server::new();
        let name = "z".repeat(200_000);
        let line = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{name}"}}"#);
        let reply = server.handle_line(&line).expect("a reply");

        assert_eq!(
            error_code_of(&mut Server::new(), &line),
            i64::from(error_code::METHOD_NOT_FOUND),
            "the refusal is unchanged; only its size is"
        );
        // The answer no longer scales with the question.
        assert!(
            reply.len() < line.len() / 100,
            "reply {} bytes against a request of {}",
            reply.len(),
            line.len()
        );
        let message = parse(&reply)
            .unwrap()
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        assert!(message.ends_with(ECHO_ELLIPSIS), "the cut is visible");
        assert!(message.starts_with("unknown method: zzz"));
        assert_eq!(message.len(), "unknown method: ".len() + MAX_ECHOED);
    }

    #[test]
    fn a_hostile_tool_name_is_not_quoted_back_in_full() {
        let mut server = Server::new();
        let name = "y".repeat(200_000);
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{name}","arguments":{{}}}}}}"#
        );
        let reply = server.handle_line(&line).expect("a reply");
        assert!(reply.len() < line.len() / 100);
        let message = parse(&reply)
            .unwrap()
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        assert!(message.starts_with("unknown tool: yyy"));
        assert_eq!(message.len(), "unknown tool: ".len() + MAX_ECHOED);
    }

    #[test]
    fn a_hostile_protocol_version_is_bounded_in_both_places_it_appears() {
        // This refusal quotes the offending string twice — once in `message`, once in
        // `data.requested` — so it used to answer a request with about twice its own
        // size. Both copies are bounded, and the answer stays the precise -32022 rather
        // than collapsing into an oversize refusal.
        let mut server = Server::new();
        let version = "9".repeat(200_000);
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{"_meta":{{"{}":"{version}"}}}}}}"#,
            meta::PROTOCOL_VERSION
        );
        let reply = server.handle_line(&line).expect("a reply");
        assert!(
            reply.len() < line.len() / 100,
            "reply {} bytes against a request of {}",
            reply.len(),
            line.len()
        );

        let parsed = parse(&reply).unwrap();
        let error = parsed.get("error").unwrap();
        assert_eq!(
            error.get("code"),
            Some(&Value::Int(i64::from(
                error_code::UNSUPPORTED_PROTOCOL_VERSION
            )))
        );
        let message = error.get("message").and_then(Value::as_str).unwrap();
        assert_eq!(
            message.len(),
            "unsupported protocol version: ".len() + MAX_ECHOED
        );
        let requested = error
            .get("data")
            .and_then(|data| data.get("requested"))
            .and_then(Value::as_str)
            .unwrap();
        assert_eq!(requested.len(), MAX_ECHOED);
        assert!(requested.ends_with(ECHO_ELLIPSIS));
    }

    #[test]
    fn every_result_carries_the_envelope_the_server_owns() {
        let mut server = Server::new();
        let result = result_of(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
        assert_eq!(
            result.get(KEY_RESULT_TYPE).and_then(Value::as_str),
            Some(RESULT_TYPE_COMPLETE)
        );
        assert!(result.get(KEY_META).and_then(Value::as_object).is_some());
    }

    /// The reserved envelope keys are refused rather than silently shadowed.
    ///
    /// Currently unreachable through any real call — no tool emits either name — so this
    /// drives `with_envelope` directly. See its documentation for why the invariant is
    /// written down even though nothing can violate it today.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "reserved envelope key")]
    fn a_result_may_not_carry_the_envelope_keys_itself() {
        let _ = with_envelope(Object::new().set(KEY_META, Value::Bool(true)).build());
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "reserved envelope key")]
    fn a_result_may_not_carry_its_own_result_type() {
        let _ = with_envelope(
            Object::new()
                .set(KEY_RESULT_TYPE, Value::string("partial"))
                .build(),
        );
    }
}
