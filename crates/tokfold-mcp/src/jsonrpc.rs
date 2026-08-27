//! The JSON-RPC 2.0 envelope MCP is carried in.
//!
//! This module knows nothing about MCP methods. It validates the outer object,
//! separates a request from a notification, and builds well-formed responses —
//! so the dispatcher in [`crate::server`] can work with typed values instead of
//! re-checking the envelope at every branch.

use crate::json::{Object, Value};

/// The only protocol version JSON-RPC 2.0 accepts in the `jsonrpc` member.
pub const VERSION: &str = "2.0";

/// A request identifier.
///
/// JSON-RPC allows a string or a number. The value is echoed back verbatim, so it
/// is kept in the shape it arrived in rather than normalized to one of the two.
///
/// # Known limitation
///
/// A numeric id must fit in an `i64`. JSON itself sets no bound, so a client using
/// an unsigned 64-bit counter past `i64::MAX`, or a 128-bit id, is rejected with
/// `INVALID_REQUEST` — the parser widens such a literal to a float, and a float is
/// not an accepted id shape here. That is deliberate: echoing a rounded id would
/// correlate the answer to a *different* in-flight request, which is worse than a
/// visible refusal. No MCP client is known to do this, and the workaround is a
/// string id, which has no bound at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Id {
    /// A numeric identifier.
    Int(i64),
    /// A string identifier.
    Str(String),
}

impl Id {
    /// Renders the identifier back into a JSON value.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Int(n) => Value::Int(*n),
            Self::Str(s) => Value::string(s.clone()),
        }
    }
}

/// Reads an id straight off a message that has not been validated yet.
///
/// Only the two shapes JSON-RPC admits come back. A `null`, a float, a container, or a
/// message that is not an object at all yields `None`, because guessing is worse than
/// admitting there is no id: an answer addressed to an id the client never sent is, on
/// its side, indistinguishable from an answer to a call it is waiting on.
///
/// This is separate from [`decode_request`] because the id is worth recovering on every
/// path that refuses a message *before* it is a [`Request`] — envelope validation here,
/// and the panic bulkhead in [`crate::stdio`], which calls this rather than restating
/// which shapes may be echoed. One rule, one place: the two paths cannot drift apart
/// into answering the same malformed message differently.
#[must_use]
pub fn read_id(value: &Value) -> Option<Id> {
    match value.get("id")? {
        Value::Int(n) => Some(Id::Int(*n)),
        Value::Str(s) => Some(Id::Str(s.clone())),
        _ => None,
    }
}

/// A validated inbound message.
#[derive(Debug, Clone)]
pub struct Request {
    /// Absent for a notification, which must never be answered.
    pub id: Option<Id>,
    /// The method name.
    pub method: String,
    /// The `params` member, if the client sent one.
    pub params: Option<Value>,
}

impl Request {
    /// Whether this message is a notification — a request with no `id`.
    ///
    /// The distinction is load-bearing on stdio: answering a notification puts an
    /// unmatched response on the client's stream, which well-behaved clients treat
    /// as a protocol violation.
    #[must_use]
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Looks up a member of `params`, treating an absent `params` as an empty object.
    #[must_use]
    pub fn param(&self, key: &str) -> Option<&Value> {
        self.params.as_ref()?.get(key)
    }
}

/// Longest run of peer-supplied text this server will copy into an error, in bytes.
///
/// 120 bytes is chosen to be longer than anything real and short enough to be free.
/// The strings that reach an error message this way are method names, tool names, and
/// protocol revisions: the longest that exist are `notifications/initialized` at 26
/// bytes, `tokfold_decompress` at 18, and a revision date at 10. A typo, a version
/// suffix, or a namespaced name from an aggregating client all still arrive intact.
pub const MAX_ECHO_BYTES: usize = 120;

/// Marker appended to a string [`echo`] had to cut.
pub const ECHO_ELLIPSIS: &str = "…";

/// Bounds a peer-supplied string before it is copied into an error message.
///
/// Returns the input untouched when it is short enough, so the common path allocates
/// nothing.
///
/// # Why echoing unbounded peer text is a problem
///
/// Escaping is not the issue — [`crate::json`] already escapes correctly, so nothing
/// here is about injecting into the wire format. Two other things are:
///
/// * **Amplification on the error route.** A refusal quotes the offending name, and the
///   unsupported-version refusal quotes it twice, once in `message` and once in
///   `data.requested`. A 30 MB protocol-version string therefore turned a 30 MB request
///   into a ~60 MB answer. The reply bound in [`render_bounded`] now stops that reply
///   from being *sent*, but only after it has been built, and it converts a precise
///   `-32022` into a generic oversize refusal. Cutting the echo at the source is what
///   makes the request cheap to refuse and keeps the answer useful.
/// * **Reflected text.** This is a server whose entire purpose is moving text into and
///   out of prompts. Whatever a peer puts in a method or tool name comes back in a
///   `message` that agent UIs render and models read. A bound is not a defence against
///   what those 120 bytes might say — nothing here sanitises content — but it does keep
///   a name from being used as a channel for a page of instructions.
#[must_use]
pub fn echo(value: &str) -> std::borrow::Cow<'_, str> {
    if value.len() <= MAX_ECHO_BYTES {
        return std::borrow::Cow::Borrowed(value);
    }
    // Walk back to a character boundary. Cutting a UTF-8 sequence in half would panic
    // on the slice, and `str::floor_char_boundary` is still unstable at this MSRV.
    let mut end = MAX_ECHO_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut cut = String::with_capacity(end.saturating_add(ECHO_ELLIPSIS.len()));
    cut.push_str(value.get(..end).unwrap_or_default());
    cut.push_str(ECHO_ELLIPSIS);
    std::borrow::Cow::Owned(cut)
}

/// A JSON-RPC error object.
#[derive(Debug, Clone)]
pub struct ErrorObject {
    /// Numeric error code.
    pub code: i32,
    /// Short human-readable description.
    pub message: String,
    /// Optional structured detail.
    pub data: Option<Value>,
}

impl ErrorObject {
    /// Builds an error with no `data` member.
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attaches a `data` member.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Renders the error object as JSON.
    #[must_use]
    pub fn to_value(&self) -> Value {
        Object::new()
            .set("code", Value::Int(i64::from(self.code)))
            .set("message", Value::string(self.message.clone()))
            .set_opt("data", self.data.clone())
            .build()
    }
}

/// An outbound response: either a result or an error, never both.
#[derive(Debug, Clone)]
pub struct Response {
    /// The identifier being answered, or `None` when the request's own id could not be
    /// determined. `None` omits the member rather than writing JSON `null`: base
    /// JSON-RPC 2.0 asks for a null there, but MCP narrows the id to a string or a
    /// number and makes it optional on an error response, so a null would be the one
    /// value the type does not admit.
    pub id: Option<Id>,
    /// What is being returned.
    pub payload: Payload,
}

/// The body of a [`Response`].
#[derive(Debug, Clone)]
pub enum Payload {
    /// A successful result.
    Result(Value),
    /// A failure.
    Error(ErrorObject),
}

impl Response {
    /// Builds a successful response.
    #[must_use]
    pub fn result(id: Option<Id>, value: Value) -> Self {
        Self {
            id,
            payload: Payload::Result(value),
        }
    }

    /// Builds an error response.
    #[must_use]
    pub fn error(id: Option<Id>, error: ErrorObject) -> Self {
        Self {
            id,
            payload: Payload::Error(error),
        }
    }

    /// Renders the response as JSON, with members in a fixed order.
    ///
    /// Field order is fixed rather than incidental because identical logical
    /// responses must serialize to identical bytes — the prompt-cache invariant this
    /// whole project is built around.
    #[must_use]
    pub fn to_value(&self) -> Value {
        self.clone().into_value()
    }

    /// Renders the response as JSON, consuming it.
    ///
    /// The result of a `tools/call` can be the whole compressed payload, so the
    /// transport uses this rather than [`Response::to_value`]: moving the value into
    /// the envelope avoids a second copy of a message that is already the largest
    /// thing the server handles.
    #[must_use]
    pub fn into_value(self) -> Value {
        let base = Object::new()
            .set("jsonrpc", Value::string(VERSION))
            .set_opt("id", self.id.as_ref().map(Id::to_value));
        match self.payload {
            Payload::Result(value) => base.set("result", value),
            Payload::Error(error) => base.set("error", error.to_value()),
        }
        .build()
    }
}

/// Renders `response` as one frame of at most `max` bytes.
///
/// A reply is always larger than the call it answers, so a server that caps only what it
/// reads will eventually emit a frame it would itself refuse to read — a peer of its own
/// kind cannot consume its output, and the size of that output is chosen by the peer.
/// This is the one place that rule is enforced, so every caller — the dispatcher, a
/// batch member, the transport's panic bulkhead — degrades identically.
///
/// # The refusal is addressed to the call it replaces
///
/// The id comes off the [`Response`] being replaced, which took it from the request, so
/// it is right by construction and needs no second reading of the line. That matters
/// more than it looks: a client pairs an answer with a call by id, so an unaddressed
/// refusal is not a failed call but a hung one — the client waits out its own timeout,
/// and in a batch it cannot even tell which member failed.
///
/// # Why `INVALID_PARAMS`
///
/// The specification partitions the implementation-defined range against new codes:
/// `-32000`..`-32019` is grandfathered and `-32020`..`-32099` belongs to the
/// specification itself, so this has to be one of the base JSON-RPC codes.
/// [`INVALID_PARAMS`](crate::protocol::error_code::INVALID_PARAMS) is the one that is
/// true: the envelope was valid, the method exists, and nothing inside the server went
/// wrong — what cannot be served is the *size of what the parameters asked for*. It is
/// also the only choice a client can act on. `INTERNAL_ERROR` would describe a server
/// bug and invite an identical retry, and `INVALID_REQUEST` would send a client hunting
/// for a malformation in a request that had none; `-32602` says the arguments are the
/// problem, which is exactly the thing the client can make smaller.
///
/// # The refusal fits by construction
///
/// An error whose `data` echoed the payload would reproduce the very bug it reports, so
/// the replacement carries no `data` at all and its message is a fixed string plus one
/// decimal number. The only variable-length part of the frame is the id, which came from
/// a request that was itself inside the read cap — and in the pathological case of an id
/// nearly as large as the cap, the last resort below drops it. That final frame has no
/// variable part left, so it is bounded outright.
///
/// A caller that sets `max` below the length of that final frame gets it anyway: emitting
/// nothing would hang the client, which is worse than exceeding a bound the caller chose.
#[must_use]
pub fn render_bounded(response: Response, max: usize) -> String {
    let id = response.id.clone();
    let frame = response.into_value().to_string();
    if frame.len() <= max {
        return frame;
    }
    // Released before the replacement is built: this can be most of the process's
    // resident memory, and the whole point of refusing is not to keep carrying it.
    drop(frame);

    let addressed = Response::error(id, oversize_error(max))
        .into_value()
        .to_string();
    if addressed.len() <= max {
        return addressed;
    }
    Response::error(None, oversize_error(max))
        .into_value()
        .to_string()
}

/// The error that answers a request whose reply will not fit the frame.
///
/// Deliberately carries no `data`: the one thing worth reporting is the limit, and the
/// one thing that must never go in is any part of the payload that broke it.
pub(crate) fn oversize_error(max: usize) -> ErrorObject {
    ErrorObject::new(
        crate::protocol::error_code::INVALID_PARAMS,
        format!("the reply exceeds the {max} byte frame limit"),
    )
}

/// A refused envelope, carrying the id the refusal has to be addressed to.
///
/// The id travels with the error rather than beside it because separating the two is
/// what went wrong before: the error was returned alone and the caller answered with no
/// id at all, including on the three grounds that leave a perfectly readable id on the
/// wire — a wrong `jsonrpc`, an unusable `method`, and a `params` that is not an object.
/// A client matches an answer to a call by id, so an id-less refusal is not a failed
/// call but a hung one: the client waits out its own timeout, and in a batch it cannot
/// even tell which member failed.
///
/// Recovering the id is the whole difference between this type and a bare
/// [`ErrorObject`], and it is not something a new rejection site can forget, because the
/// only constructor takes the message rather than an id.
#[derive(Debug, Clone)]
pub struct Rejection {
    /// The id the message carried, or `None` when it carried none that may be echoed.
    pub id: Option<Id>,
    /// Why the envelope was refused.
    pub error: ErrorObject,
}

impl Rejection {
    /// Refuses `value`, recovering whatever id it carried.
    fn new(value: &Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            id: read_id(value),
            error: ErrorObject::new(code, message),
        }
    }

    /// Builds the response that answers this refusal.
    #[must_use]
    pub fn into_response(self) -> Response {
        Response::error(self.id, self.error)
    }
}

/// Reads a validated [`Request`] out of a parsed JSON value.
///
/// # Errors
///
/// Returns a [`Rejection`]: the envelope violation together with the id the message
/// carried, if any. Three of the grounds for refusal — a wrong `jsonrpc`, a missing or
/// non-string `method`, a `params` that is not an object — say nothing about the id, so
/// the reply is addressed to it exactly as a successful one would be. The other three
/// have nothing to address it to: a message that is not an object, an `id` of `null`,
/// and an `id` of a type JSON-RPC does not admit.
pub fn decode_request(value: &Value) -> Result<Request, Rejection> {
    use crate::protocol::error_code::INVALID_REQUEST;

    if value.as_object().is_none() {
        return Err(Rejection::new(
            value,
            INVALID_REQUEST,
            "a JSON-RPC message must be an object",
        ));
    }
    match value.get("jsonrpc").and_then(Value::as_str) {
        Some(VERSION) => {}
        _ => {
            return Err(Rejection::new(
                value,
                INVALID_REQUEST,
                "`jsonrpc` must be exactly \"2.0\"",
            ));
        }
    }
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| Rejection::new(value, INVALID_REQUEST, "`method` must be a string"))?
        .to_owned();

    let id = match value.get("id") {
        None => None,
        // An explicit `null` id is not treated as a notification. The specification
        // discourages null ids, and silently downgrading a request to a
        // notification would leave a client waiting forever for a reply.
        Some(Value::Null) => {
            return Err(Rejection::new(
                value,
                INVALID_REQUEST,
                "`id` must not be null; omit it for a notification",
            ));
        }
        Some(Value::Int(n)) => Some(Id::Int(*n)),
        Some(Value::Str(s)) => Some(Id::Str(s.clone())),
        Some(_) => {
            return Err(Rejection::new(
                value,
                INVALID_REQUEST,
                "`id` must be a string or an integer",
            ));
        }
    };

    // The specification permits an array here, but every MCP method takes named
    // arguments, so an array is a client bug worth naming rather than ignoring.
    let params = match value.get("params") {
        None | Some(Value::Null) => None,
        Some(params @ Value::Object(_)) => Some(params.clone()),
        Some(_) => {
            return Err(Rejection::new(
                value,
                INVALID_REQUEST,
                "`params` must be an object",
            ));
        }
    };

    Ok(Request { id, method, params })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{
        ECHO_ELLIPSIS, ErrorObject, Id, MAX_ECHO_BYTES, Payload, Rejection, Response,
        decode_request, echo, render_bounded,
    };
    use crate::json::{Value, parse};

    fn decode(text: &str) -> Result<super::Request, Rejection> {
        decode_request(&parse(text).unwrap())
    }

    /// The id a refused envelope is answered with.
    fn rejected_id(text: &str) -> Option<Id> {
        decode(text).expect_err("this envelope is refused").id
    }

    #[test]
    fn a_well_formed_request_decodes() {
        let request = decode(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert_eq!(request.id, Some(Id::Int(1)));
        assert_eq!(request.method, "tools/list");
        assert!(request.params.is_none());
        assert!(!request.is_notification());
    }

    #[test]
    fn a_string_id_survives_as_a_string() {
        // Echoing `1` for a request that asked with `"1"` breaks client correlation.
        let request = decode(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#).unwrap();
        assert_eq!(request.id, Some(Id::Str("abc".to_owned())));
        assert_eq!(request.id.unwrap().to_value(), Value::string("abc"));
    }

    #[test]
    fn a_missing_id_marks_a_notification() {
        let request = decode(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap();
        assert!(request.is_notification());
    }

    #[test]
    fn params_are_readable_by_key() {
        let request =
            decode(r#"{"jsonrpc":"2.0","id":1,"method":"m","params":{"a":"b"}}"#).unwrap();
        assert_eq!(request.param("a").and_then(Value::as_str), Some("b"));
        assert!(request.param("missing").is_none());
    }

    #[test]
    fn param_lookup_is_safe_without_params() {
        let request = decode(r#"{"jsonrpc":"2.0","id":1,"method":"m"}"#).unwrap();
        assert!(request.param("a").is_none());
    }

    #[test]
    fn malformed_envelopes_are_rejected() {
        for bad in [
            "[]",
            "1",
            r#"{"id":1,"method":"m"}"#,
            r#"{"jsonrpc":"1.0","id":1,"method":"m"}"#,
            r#"{"jsonrpc":2.0,"id":1,"method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":5}"#,
            r#"{"jsonrpc":"2.0","id":null,"method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":{},"method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"m","params":[]}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"m","params":5}"#,
        ] {
            assert!(decode(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn a_refusal_is_addressed_to_the_id_the_message_carried() {
        // The three grounds that say nothing about the id. The message is refused, but
        // the id in it is exactly as usable as it would have been on a request that
        // decoded, so the answer has to carry it — a client pairs answers with calls by
        // id and has no other handle on the failure.
        for (bad, expected) in [
            (r#"{"jsonrpc":"1.0","id":42,"method":"ping"}"#, Id::Int(42)),
            (r#"{"jsonrpc":"2.0","id":42}"#, Id::Int(42)),
            (
                r#"{"jsonrpc":"2.0","id":"abc","method":5}"#,
                Id::Str("abc".to_owned()),
            ),
            (
                r#"{"jsonrpc":"2.0","id":"abc","method":"ping","params":[]}"#,
                Id::Str("abc".to_owned()),
            ),
        ] {
            assert_eq!(rejected_id(bad), Some(expected), "id dropped from {bad}");
        }
    }

    #[test]
    fn a_refusal_carries_no_id_when_the_message_had_none_to_carry() {
        // The complement, and the reason the recovery cannot be "always echo something":
        // none of these holds an id this server may put on the wire, and inventing one
        // would address the answer to a call the client never made.
        for bad in [
            "[]",
            "1",
            r#""a string""#,
            r#"{"jsonrpc":"2.0","id":null,"method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":1.5,"method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":{},"method":"m"}"#,
        ] {
            assert_eq!(rejected_id(bad), None, "invented an id for {bad}");
        }
    }

    #[test]
    fn a_recovered_id_keeps_the_shape_it_arrived_in() {
        // A numeric 7 answered as the string "7" is not a match under JSON-RPC's
        // equality, so a shape-losing recovery hangs the client just as an absent id
        // would — while looking correct in a log.
        let rejection = decode(r#"{"jsonrpc":"1.0","id":7,"method":"ping"}"#)
            .expect_err("this envelope is refused");
        assert_eq!(
            rejection.into_response().to_value().to_string(),
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32600,"message":"`jsonrpc` must be exactly \"2.0\""}}"#
        );
    }

    #[test]
    fn a_numeric_id_too_large_for_i64_is_refused_rather_than_rounded() {
        // The documented limitation, pinned. The parser widens such a literal to a
        // float, and the danger is not the refusal but the alternative: a rounded id
        // would be echoed against a different in-flight request, so the client would
        // pair an answer with the wrong call and never know.
        let request = decode(r#"{"jsonrpc":"2.0","id":18446744073709551615,"method":"ping"}"#);
        assert!(request.is_err(), "an out-of-range id must not be accepted");
        // The bound itself is usable, so the refusal is about range and nothing else.
        let at_bound = decode(r#"{"jsonrpc":"2.0","id":9223372036854775807,"method":"ping"}"#);
        assert_eq!(at_bound.unwrap().id, Some(Id::Int(i64::MAX)));
        // And the documented workaround actually works.
        let as_string =
            decode(r#"{"jsonrpc":"2.0","id":"18446744073709551615","method":"ping"}"#).unwrap();
        assert_eq!(
            as_string.id,
            Some(Id::Str("18446744073709551615".to_owned()))
        );
    }

    #[test]
    fn an_explicit_null_params_is_treated_as_absent() {
        let request = decode(r#"{"jsonrpc":"2.0","id":1,"method":"m","params":null}"#).unwrap();
        assert!(request.params.is_none());
    }

    #[test]
    fn a_result_response_serializes_in_a_fixed_order() {
        let response = Response::result(Some(Id::Int(7)), Value::Bool(true));
        assert_eq!(
            response.to_value().to_string(),
            r#"{"jsonrpc":"2.0","id":7,"result":true}"#
        );
    }

    #[test]
    fn an_error_response_carries_code_and_message() {
        let response = Response::error(Some(Id::Int(7)), ErrorObject::new(-32601, "nope"));
        assert_eq!(
            response.to_value().to_string(),
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"nope"}}"#
        );
    }

    #[test]
    fn an_unknown_id_is_omitted_rather_than_written_as_null() {
        // Base JSON-RPC 2.0 says to write a null here. MCP does not: its `RequestId` is
        // a string or a number and nothing else, and it makes `id` optional on an error
        // response precisely so that an unreadable request has somewhere to go. Writing
        // the null would emit the one value the type forbids.
        let response = Response::error(None, ErrorObject::new(-32700, "bad json"));
        assert_eq!(
            response.to_value().to_string(),
            r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"bad json"}}"#
        );
    }

    #[test]
    fn error_data_is_included_when_present() {
        let error = ErrorObject::new(-32022, "bad version").with_data(Value::string("x"));
        let response = Response::error(Some(Id::Int(1)), error);
        assert!(response.to_value().to_string().contains(r#""data":"x""#));
    }

    #[test]
    fn a_response_is_never_both_a_result_and_an_error() {
        // JSON-RPC forbids a response carrying both members, and a client that sees
        // both is entitled to reject the whole message. The check runs in both
        // directions because the two members are written by different match arms.
        let success = Response::result(Some(Id::Int(1)), Value::Null);
        assert!(matches!(success.payload, Payload::Result(_)));
        let text = success.to_value().to_string();
        assert!(text.contains(r#""result":null"#));
        assert!(!text.contains(r#""error""#));

        let failure = Response::error(Some(Id::Int(1)), ErrorObject::new(-32603, "boom"));
        assert!(matches!(failure.payload, Payload::Error(_)));
        let text = failure.to_value().to_string();
        assert!(text.contains(r#""error""#));
        assert!(!text.contains(r#""result""#));
    }

    #[test]
    fn consuming_and_borrowing_renderers_agree() {
        // `into_value` exists only to avoid cloning a payload that can be the whole
        // compressed archive; if it ever diverged from `to_value` the transport would
        // emit something the unit tests never see.
        let response = Response::result(Some(Id::Str("x".to_owned())), Value::string("body"));
        let borrowed = response.to_value();
        assert_eq!(borrowed, response.into_value());
    }

    /// A reply that fits is handed over untouched.
    ///
    /// The bound is a safety net for the pathological case, and a safety net that
    /// rewrites ordinary traffic is a bug. Every reply this server sends goes through
    /// here, so "the normal path is byte-identical" is the property that keeps the
    /// prompt-cache invariant true.
    #[test]
    fn a_response_within_the_limit_is_rendered_unchanged() {
        let response = Response::result(Some(Id::Int(7)), Value::string("body"));
        let expected = response.to_value().to_string();
        assert_eq!(render_bounded(response, 1024), expected);
        // The boundary itself is inclusive: a frame exactly as long as the limit fits.
        let exact = Response::result(Some(Id::Int(7)), Value::string("body"));
        assert_eq!(render_bounded(exact, expected.len()), expected);
    }

    /// One byte over the limit is a refusal, and the refusal answers the same call.
    ///
    /// The id is the whole point. A client blocks on the id it sent, so an oversized
    /// answer that came back unaddressed — or not at all — is not a failed call but a
    /// hung one, and in a batch it cannot even be attributed to a member.
    #[test]
    fn a_response_over_the_limit_becomes_an_error_addressed_to_the_same_id() {
        // Large enough that the refusal, which is longer than the word "body", is still
        // comfortably inside the limit the result overran.
        let response = Response::result(
            Some(Id::Str("call-1".to_owned())),
            Value::string("x".repeat(4096)),
        );
        let frame = parse(&render_bounded(response, 1024)).unwrap();

        assert_eq!(frame.get("id").and_then(Value::as_str), Some("call-1"));
        assert!(
            frame.get("result").is_none(),
            "the payload must not survive"
        );
        let error = frame.get("error").expect("an oversized reply is an error");
        assert_eq!(
            error.get("code").and_then(Value::as_i64),
            Some(i64::from(crate::protocol::error_code::INVALID_PARAMS)),
            "the code a client branches on is -32602"
        );
    }

    /// The refusal carries no trace of what it refused.
    ///
    /// An error whose `data` echoed the payload would be the same bug wearing a
    /// different member name, so what is asserted is absence: no `data` at all, and a
    /// frame small enough that no plausible limit is troubled by it.
    #[test]
    fn the_oversize_refusal_is_small_and_carries_no_payload() {
        let payload = "x".repeat(4096);
        let response = Response::result(Some(Id::Int(1)), Value::string(payload.clone()));
        let frame = render_bounded(response, 64);

        assert!(
            !frame.contains(&payload),
            "the refusal must not echo the input"
        );
        assert!(
            frame.len() < 128,
            "the refusal is a fixed message and one number: {frame}"
        );
        assert!(
            parse(&frame)
                .unwrap()
                .get("error")
                .unwrap()
                .get("data")
                .is_none(),
            "data is where an echo of the payload would hide: {frame}"
        );
    }

    /// An id too large to echo is dropped rather than allowed to overrun the frame.
    ///
    /// This is the case that makes "an error is obviously small" false: nothing bounds
    /// a string id below the transport's own line limit, so a client can send an id that
    /// on its own will not fit a reply. Correlation is sacrificed only here, and only
    /// because the alternative is emitting the very frame the limit exists to prevent.
    #[test]
    fn an_id_too_large_to_echo_is_dropped_rather_than_overrunning_the_frame() {
        let id = Id::Str("i".repeat(4096));
        let response = Response::result(Some(id), Value::string("body"));
        let frame = render_bounded(response, 128);

        assert!(
            frame.len() <= 128,
            "the frame must respect the limit: {frame}"
        );
        let parsed = parse(&frame).unwrap();
        assert!(parsed.get("id").is_none(), "an unechoable id is omitted");
        assert_eq!(
            parsed
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_i64),
            Some(i64::from(crate::protocol::error_code::INVALID_PARAMS))
        );
    }

    /// A limit smaller than any legal frame still produces a frame.
    ///
    /// The documented last resort: below a certain size there is nothing valid left to
    /// send, and answering with a well-formed frame that exceeds an absurd limit is
    /// better than answering with silence, which hangs the caller. Pinned because it is
    /// the one place the function deliberately breaks its own postcondition.
    #[test]
    fn an_impossibly_small_limit_still_yields_a_well_formed_frame() {
        let response = Response::result(Some(Id::Int(1)), Value::string("body"));
        let frame = render_bounded(response, 0);
        assert!(parse(&frame).unwrap().get("error").is_some(), "{frame}");
    }

    #[test]
    fn a_short_echo_is_returned_whole_and_unallocated() {
        // The common case: every name this server actually refuses is far under the
        // limit, so bounding the echo must cost nothing on that path.
        for name in [
            "ping",
            "tools/call",
            "notifications/initialized",
            "tokfold_compress",
        ] {
            assert!(
                matches!(echo(name), std::borrow::Cow::Borrowed(_)),
                "{name}"
            );
            assert_eq!(echo(name), name);
        }
        // The boundary itself: at the limit is whole, one byte past it is cut.
        let at_limit = "a".repeat(MAX_ECHO_BYTES);
        assert_eq!(echo(&at_limit), at_limit);
        let past_limit = "a".repeat(MAX_ECHO_BYTES + 1);
        assert_ne!(echo(&past_limit), past_limit);
    }

    #[test]
    fn a_long_echo_is_cut_to_the_limit_and_marked() {
        let name = "b".repeat(4096);
        let cut = echo(&name);
        assert!(cut.ends_with(ECHO_ELLIPSIS), "the cut is visible: {cut}");
        assert_eq!(cut.len(), MAX_ECHO_BYTES + ECHO_ELLIPSIS.len());
        assert!(cut.starts_with(&"b".repeat(MAX_ECHO_BYTES)));
    }

    #[test]
    fn an_echo_is_cut_on_a_character_boundary() {
        // A multi-byte character straddling the limit must not be halved: slicing a
        // `str` mid-sequence panics, and a lone continuation byte is not UTF-8 at all.
        // Cyrillic is two bytes per character, so the cut lands mid-character whenever
        // the limit is odd relative to the run; the emoji case is four bytes and shifts
        // the boundary again.
        for unit in ["\u{430}", "\u{4e00}", "\u{1f600}"] {
            for extra in 0..8 {
                let name = format!("{}{}", "x".repeat(extra), unit.repeat(4096));
                let cut = echo(&name);
                // Valid UTF-8 by construction — `Cow<str>` could not hold it otherwise —
                // and never longer than the limit plus the marker.
                assert!(cut.len() <= MAX_ECHO_BYTES + ECHO_ELLIPSIS.len());
                assert!(cut.ends_with(ECHO_ELLIPSIS));
                // Nothing was lost from the front, and the kept prefix is whole.
                let kept = cut.strip_suffix(ECHO_ELLIPSIS).unwrap();
                assert!(name.starts_with(kept), "prefix mangled: {kept}");
                // The cut is as late as the character grid allows: one more character
                // would not have fitted.
                assert!(kept.len() + unit.len() > MAX_ECHO_BYTES);
            }
        }
    }
}
