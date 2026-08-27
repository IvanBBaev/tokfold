//! The tool catalogue and its invocation path.
//!
//! Three tools, in a fixed order: `tokfold_compress`, `tokfold_decompress`, and
//! `tokfold_estimate`. Names carry the `tokfold_` prefix because a client that
//! aggregates several servers puts every tool in one namespace, where a bare
//! `compress` would collide.
//!
//! # Error routing
//!
//! Two different failures reach this module and they are routed differently, which
//! follows the engine's own invariant that **core errors, adapters pass through**:
//!
//! * A [`CompressError`] is never a caller-visible failure. Input that is not JSON,
//!   is too large, or nests too deeply is returned unchanged with
//!   `compressed: false` and a reason. Compression is an optimization, never a gate,
//!   and a proxy that dropped data on unparseable input would be worse than useless.
//! * A [`DecompressError`] is an integrity failure and is always surfaced. No
//!   partial output is ever emitted.
//!
//! # The archive lives in `structuredContent` only
//!
//! `tokfold_compress` puts the base64 archive in `structuredContent.archive` and
//! nowhere else; `content` carries the rendering alone. Reading only `content` is
//! spec-conformant and is what older clients do, so such a client ends up holding a
//! compressed rendering it can never recover the original from. The tool description
//! says so, because a client author reading the catalogue is the only one who can act
//! on it.
//!
//! Copying the archive into `content` as well is the obvious fix and is deliberately
//! not taken: it is a third echo of the payload, which lifts the compress path's reply
//! multiplier to roughly 4.7x of the request and trips the 400% ratchet in
//! `tests/session.rs`. That is a wire-shape change and an owner decision, not a
//! documentation one.
//!
//! A surfaced decompression failure is reported as a tool result with
//! `isError: true` rather than as a JSON-RPC error. In MCP a JSON-RPC error means the
//! *call itself* was malformed, and clients surface it as a transport fault the model
//! never sees — which is the wrong channel for "your archive is corrupt", a fact the
//! model can act on. What the caller needs either way is a **stable string code** to
//! branch on, and that is carried in `structuredContent.code`.

use tokfold_core::{
    Artifact, CompressError, Compressor, Config, DecompressError, EncoderId, Fidelity, Profile,
    Stats,
};

use crate::base64;
use crate::json::{Object, Value};
use crate::jsonrpc::{ErrorObject, echo};
use crate::protocol::error_code::INVALID_PARAMS;

/// Tool name for the compression call.
pub const COMPRESS: &str = "tokfold_compress";
/// Tool name for the recovery call.
pub const DECOMPRESS: &str = "tokfold_decompress";
/// Tool name for the dry-run measurement call.
pub const ESTIMATE: &str = "tokfold_estimate";

/// Every tool this server exposes, in the order `tools/list` reports them.
///
/// The order is fixed: the specification asks for a deterministic catalogue so the
/// listing can be cached, and a stable byte sequence is also what keeps a client's
/// prompt cache warm across restarts.
pub const NAMES: [&str; 3] = [COMPRESS, DECOMPRESS, ESTIMATE];

/// What a tool produced.
pub struct Outcome {
    /// Text placed in the result's `content` block — what the model reads.
    pub text: String,
    /// Machine-readable result placed in `structuredContent`.
    pub structured: Value,
    /// Whether the call failed in a way the model should see and react to.
    pub is_error: bool,
}

/// Renders the tool catalogue for `tools/list`.
#[must_use]
pub fn catalogue() -> Value {
    Value::Array(vec![compress_tool(), decompress_tool(), estimate_tool()])
}

fn compress_tool() -> Value {
    tool(
        COMPRESS,
        "Compress text",
        "Reversibly compress JSON-shaped text so it costs fewer tokens in a prompt. \
         Returns a rendering to embed and an archive that recovers the input exactly. \
         The archive is returned only in `structuredContent.archive`; the `content` \
         block carries the rendering alone, so a client that reads only `content` keeps \
         text it can never decompress. Input that is not valid JSON is returned \
         unchanged with `compressed` false — the call never fails for that reason and \
         never loses data.",
        Object::new()
            .set("type", Value::string("object"))
            .set(
                "properties",
                Object::new()
                    .set("text", text_property())
                    .set("profile", profile_property())
                    .build(),
            )
            .set("required", Value::Array(vec![Value::string("text")]))
            .set("additionalProperties", Value::Bool(false))
            .build(),
    )
}

fn decompress_tool() -> Value {
    tool(
        DECOMPRESS,
        "Restore compressed text",
        "Recover the exact original text from an archive returned by \
         `tokfold_compress`. A corrupt or truncated archive is reported as an error \
         with a stable `code`; no partial output is ever returned. An archive restores \
         bytes, and this tool returns them as a JSON string, so one produced elsewhere \
         over non-UTF-8 data is reported as an error rather than mangled.",
        Object::new()
            .set("type", Value::string("object"))
            .set(
                "properties",
                Object::new()
                    .set(
                        "archive",
                        Object::new()
                            .set("type", Value::string("string"))
                            .set(
                                "description",
                                Value::string(
                                    "Base64 archive exactly as returned by `tokfold_compress` \
                                     in `structuredContent.archive`.",
                                ),
                            )
                            .build(),
                    )
                    .build(),
            )
            .set("required", Value::Array(vec![Value::string("archive")]))
            .set("additionalProperties", Value::Bool(false))
            .build(),
    )
}

fn estimate_tool() -> Value {
    tool(
        ESTIMATE,
        "Measure without compressing",
        "Report what compressing the text would save, without returning the \
         compressed payload. Use it to decide whether a compression call is worth \
         making; the answer costs no output tokens for the payload itself.",
        Object::new()
            .set("type", Value::string("object"))
            .set(
                "properties",
                Object::new()
                    .set("text", text_property())
                    .set("profile", profile_property())
                    .build(),
            )
            .set("required", Value::Array(vec![Value::string("text")]))
            .set("additionalProperties", Value::Bool(false))
            .build(),
    )
}

fn tool(name: &str, title: &str, description: &str, input_schema: Value) -> Value {
    Object::new()
        .set("name", Value::string(name))
        .set("title", Value::string(title))
        .set("description", Value::string(description))
        .set("inputSchema", input_schema)
        .build()
}

fn text_property() -> Value {
    Object::new()
        .set("type", Value::string("string"))
        .set(
            "description",
            Value::string("The text to process. JSON-shaped input compresses best."),
        )
        .build()
}

fn profile_property() -> Value {
    Object::new()
        .set("type", Value::string("string"))
        .set(
            "enum",
            Value::Array(vec![
                Value::string("conservative"),
                Value::string("balanced"),
                Value::string("aggressive"),
            ]),
        )
        .set(
            "description",
            Value::string(
                "How hard to try. Defaults to `balanced`. Every profile is fully reversible.",
            ),
        )
        .build()
}

/// Invokes a tool by name.
///
/// # Errors
///
/// Returns an [`ErrorObject`] only for a malformed *call* — an unknown tool name or
/// arguments of the wrong shape. A failure inside a tool is reported through
/// [`Outcome::is_error`] instead, so the model can see it.
pub fn call(name: &str, arguments: Option<&Value>) -> Result<Outcome, ErrorObject> {
    match name {
        COMPRESS => run_compress(arguments, true),
        ESTIMATE => run_compress(arguments, false),
        DECOMPRESS => run_decompress(arguments),
        // `name` is peer-supplied and unbounded up to the frame limit; `echo` caps how
        // much of it is quoted back. See [`crate::jsonrpc::echo`].
        _ => Err(ErrorObject::new(
            INVALID_PARAMS,
            format!("unknown tool: {}", echo(name)),
        )),
    }
}

/// Runs the compression path, optionally withholding the payload.
///
/// `tokfold_estimate` is the same measurement as `tokfold_compress` with the
/// rendering and archive left out, so the two share one implementation. Splitting
/// them would let the numbers the estimate reports drift from what a later
/// compression actually does.
fn run_compress(arguments: Option<&Value>, with_payload: bool) -> Result<Outcome, ErrorObject> {
    let text = required_str(arguments, "text")?;
    let profile = optional_profile(arguments)?;
    let compressor = Compressor::new(Config::builder().profile(profile).build());

    match compressor.compress(text.as_bytes()) {
        Ok(artifact) => Ok(compressed_outcome(&artifact, with_payload)),
        Err(error) => Ok(passthrough_outcome(text, &error, with_payload)),
    }
}

fn compressed_outcome(artifact: &Artifact, with_payload: bool) -> Outcome {
    let structured = Object::new()
        .set("compressed", Value::Bool(true))
        .set("stats", stats_value(&artifact.stats))
        .set_opt(
            "rendering",
            with_payload.then(|| Value::string(artifact.rendering.clone())),
        )
        .set_opt(
            "archive",
            with_payload.then(|| Value::string(base64::encode(&artifact.archive))),
        )
        .build();
    let text = if with_payload {
        artifact.rendering.clone()
    } else {
        summarize(&artifact.stats)
    };
    Outcome {
        text,
        structured,
        is_error: false,
    }
}

/// Builds the result for input core declined to compress.
///
/// The input comes back unchanged and the call still succeeds. That is the
/// pass-through invariant: compression is an optimization, so declining to compress
/// is a normal outcome, not a failure.
///
/// With `with_payload`, the input is echoed **twice** — once as `structured.rendering`
/// and once as the content text — so a declined call is the most expensive shape this
/// server produces: the reply is a little over twice the request. That is intended
/// rather than overlooked. The two fields have two audiences (a program reads
/// `structuredContent`, a model reads `content`), and dropping either one to save the
/// copy would mean a client that reads only its own half sees the input silently
/// vanish on exactly the path whose contract is that nothing is dropped.
///
/// The cost is bounded at both ends, not open-ended: core refuses input above 16 MiB,
/// the transport refuses a line above [`MAX_LINE_BYTES`], and the doubling here cannot
/// carry the answer past that same limit, because a reply too large to frame is replaced
/// by an error addressed to the call — see [`render_bounded`]. So the worst case is a
/// known multiple of a known ceiling, and past the ceiling it is a refusal rather than a
/// frame nothing can read.
///
/// [`MAX_LINE_BYTES`]: crate::stdio::MAX_LINE_BYTES
/// [`render_bounded`]: crate::jsonrpc::render_bounded
fn passthrough_outcome(text: &str, error: &CompressError, with_payload: bool) -> Outcome {
    let reason = compress_error_reason(error);
    let structured = Object::new()
        .set("compressed", Value::Bool(false))
        .set("reason", Value::string(reason.clone()))
        .set("reasonCode", Value::string(compress_error_code(error)))
        .set_opt(
            "rendering",
            with_payload.then(|| Value::string(text.to_owned())),
        )
        .build();
    Outcome {
        text: if with_payload {
            text.to_owned()
        } else {
            format!("not compressed: {reason}")
        },
        structured,
        is_error: false,
    }
}

fn run_decompress(arguments: Option<&Value>) -> Result<Outcome, ErrorObject> {
    let archive_text = required_str(arguments, "archive")?;
    let archive = match base64::decode(archive_text) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Ok(failure("invalid_base64", &error.to_string()));
        }
    };
    let compressor = Compressor::new(Config::default());
    match compressor.decompress(&archive) {
        Ok(bytes) => Ok(String::from_utf8(bytes).map_or_else(
            // Unreachable through this server, which only ever compresses `&str`,
            // but the archive argument is caller-supplied so the case is handled
            // rather than assumed away.
            |_| {
                failure(
                    "not_utf8",
                    "the archive restores bytes that are not valid UTF-8",
                )
            },
            |text| Outcome {
                structured: Object::new()
                    .set("text", Value::string(text.clone()))
                    .build(),
                text,
                is_error: false,
            },
        )),
        Err(error) => Ok(failure(decompress_error_code(&error), &error.to_string())),
    }
}

/// Builds a visible tool failure carrying a stable machine-readable code.
fn failure(code: &str, message: &str) -> Outcome {
    Outcome {
        text: format!("decompression failed: {message}"),
        structured: Object::new()
            .set("code", Value::string(code))
            .set("message", Value::string(message))
            .build(),
        is_error: true,
    }
}

/// Stable string codes for decompression failures.
///
/// These are part of the tool's contract: callers branch on them, so a rename is a
/// breaking change even though the enum they come from is `#[non_exhaustive]`.
fn decompress_error_code(error: &DecompressError) -> &'static str {
    match error {
        DecompressError::BadMagic => "bad_magic",
        DecompressError::UnsupportedVersion { .. } => "unsupported_version",
        DecompressError::Corrupt { .. } => "corrupt",
        DecompressError::ChecksumMismatch => "checksum_mismatch",
        DecompressError::ReservedBitsSet => "reserved_bits_set",
        _ => "unrecognized",
    }
}

/// Stable string codes for the reasons compression was declined.
fn compress_error_code(error: &CompressError) -> &'static str {
    match error {
        CompressError::InvalidJson { .. } => "invalid_json",
        CompressError::InputTooLarge { .. } => "input_too_large",
        CompressError::DepthExceeded { .. } => "depth_exceeded",
        CompressError::NotUtf8 => "not_utf8",
        _ => "unrecognized",
    }
}

fn compress_error_reason(error: &CompressError) -> String {
    error.to_string()
}

/// Renders [`Stats`] with camel-cased keys, matching MCP's JSON conventions.
fn stats_value(stats: &Stats) -> Value {
    Object::new()
        .set("bytesBefore", usize_value(stats.bytes_before))
        .set("bytesAfter", usize_value(stats.bytes_after))
        .set("estTokensBefore", usize_value(stats.est_tokens_before))
        .set("estTokensAfter", usize_value(stats.est_tokens_after))
        .set("byteRatio", Value::Float(stats.byte_ratio()))
        .set("tokenRatio", Value::Float(stats.token_ratio()))
        .set("encoder", Value::string(encoder_name(stats.encoder)))
        .set("encoderId", Value::Int(i64::from(stats.encoder.0)))
        .set("tokenizerId", Value::Int(i64::from(stats.tokenizer_id)))
        .set("minSavingBps", Value::Int(i64::from(stats.min_saving_bps)))
        .set("fidelity", fidelity_value(&stats.fidelity))
        .build()
}

/// Converts a count to a JSON integer, saturating rather than wrapping.
///
/// A `usize` above `i64::MAX` cannot occur for a buffer that fits in memory; the
/// saturation exists so the conversion needs no `unwrap`.
fn usize_value(value: usize) -> Value {
    Value::Int(i64::try_from(value).unwrap_or(i64::MAX))
}

/// Names the encoder that shaped a rendering.
///
/// Written as comparisons rather than a `match`: [`EncoderId`] is `#[non_exhaustive]`,
/// so a downstream crate cannot name its variants in a pattern. An id this build does
/// not know still reports its number through `encoderId`.
fn encoder_name(encoder: EncoderId) -> &'static str {
    if encoder == EncoderId::PASSTHROUGH {
        "passthrough"
    } else if encoder == EncoderId::E1_MINIFY {
        "minify"
    } else if encoder == EncoderId::E2_TABULAR {
        "tabular"
    } else {
        "unknown"
    }
}

fn fidelity_value(fidelity: &Fidelity) -> Value {
    match fidelity {
        Fidelity::Lossless => Value::string("lossless"),
        Fidelity::Lossy { .. } => Value::string("lossy"),
        _ => Value::string("unknown"),
    }
}

/// One deterministic line describing a measurement, for the `content` block.
fn summarize(stats: &Stats) -> String {
    format!(
        "encoder={} bytes {}->{} tokens(est) {}->{}",
        encoder_name(stats.encoder),
        stats.bytes_before,
        stats.bytes_after,
        stats.est_tokens_before,
        stats.est_tokens_after,
    )
}

fn required_str<'a>(arguments: Option<&'a Value>, key: &str) -> Result<&'a str, ErrorObject> {
    arguments
        .and_then(|args| args.get(key))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ErrorObject::new(
                INVALID_PARAMS,
                format!("`{key}` is required and must be a string"),
            )
        })
}

fn optional_profile(arguments: Option<&Value>) -> Result<Profile, ErrorObject> {
    let Some(value) = arguments.and_then(|args| args.get("profile")) else {
        return Ok(Profile::default());
    };
    if value.is_null() {
        return Ok(Profile::default());
    }
    match value.as_str() {
        Some("conservative") => Ok(Profile::Conservative),
        Some("balanced") => Ok(Profile::Balanced),
        Some("aggressive") => Ok(Profile::Aggressive),
        _ => Err(ErrorObject::new(
            INVALID_PARAMS,
            "`profile` must be one of: conservative, balanced, aggressive",
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use tokfold_core::{CompressError, Compressor, Config};

    use super::{
        COMPRESS, DECOMPRESS, ESTIMATE, NAMES, call, catalogue, compress_error_code,
        compress_error_reason,
    };
    use crate::base64;
    use crate::json::{Object, Value, parse};

    fn args(text: &str) -> Value {
        parse(text).unwrap()
    }

    /// Makes the real engine produce a compression failure.
    ///
    /// [`CompressError`] is `#[non_exhaustive]`, so this crate cannot build a variant —
    /// and should not want to. A hand-made value would pin the mapping to a shape core
    /// may have stopped producing; only an error the engine actually returned proves
    /// the arm is still live. The limits are pushed down instead of the input being
    /// pushed up, so provoking the 16 MiB ceiling costs a few bytes rather than 16 MiB.
    fn engine_error(config: Config, input: &[u8]) -> CompressError {
        Compressor::new(config)
            .compress(input)
            .expect_err("the engine was expected to decline this input")
    }

    #[test]
    fn the_catalogue_lists_every_tool_in_a_fixed_order() {
        let listed = catalogue();
        let items = listed.as_array().unwrap();
        assert_eq!(items.len(), NAMES.len());
        for (item, expected) in items.iter().zip(NAMES) {
            assert_eq!(item.get("name").and_then(Value::as_str), Some(expected));
        }
        // Rendering twice must produce identical bytes, or a cached listing goes stale
        // for no reason.
        assert_eq!(listed.to_string(), catalogue().to_string());
    }

    #[test]
    fn every_catalogue_entry_carries_the_fields_a_client_needs() {
        for item in catalogue().as_array().unwrap() {
            for field in ["name", "title", "description", "inputSchema"] {
                let text = item.get(field).and_then(Value::as_str);
                // `inputSchema` is an object, so only the three prose fields are
                // checked for content; an empty title or description is what a client
                // renders to the user, and blank is worse than missing.
                if field != "inputSchema" {
                    assert!(
                        text.is_some_and(|value| !value.trim().is_empty()),
                        "{field} is missing or blank in {item}"
                    );
                }
            }
            let schema = item.get("inputSchema").unwrap();
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "an open schema lets a typo through as a silently ignored argument"
            );
            let properties = schema.get("properties").unwrap().as_object().unwrap();
            let required = schema.get("required").unwrap().as_array().unwrap();
            assert!(
                !required.is_empty(),
                "every tool takes at least one argument"
            );
            // The defect this catches: a schema that requires a key it never declares.
            // Strict client-side validators reject such a schema outright, so the tool
            // becomes uncallable without a single line of server code being wrong.
            for key in required {
                let key = key.as_str().unwrap();
                assert!(
                    properties.iter().any(|(name, _)| name == key),
                    "{key} is required but not declared in properties"
                );
            }
            for (name, property) in properties {
                assert_eq!(
                    property.get("type").and_then(Value::as_str),
                    Some("string"),
                    "{name} has no declared type"
                );
            }
        }
    }

    #[test]
    fn tool_names_are_prefixed_so_they_survive_aggregation() {
        for name in NAMES {
            assert!(name.starts_with("tokfold_"), "{name} is unprefixed");
        }
    }

    #[test]
    fn compress_returns_a_rendering_an_archive_and_stats() {
        let outcome = call(COMPRESS, Some(&args(r#"{"text":"{\"a\": 1}"}"#))).unwrap();
        assert!(!outcome.is_error);
        assert_eq!(
            outcome.structured.get("compressed"),
            Some(&Value::Bool(true))
        );
        assert!(outcome.structured.get("archive").is_some());
        assert!(outcome.structured.get("stats").is_some());
        // The content block is the rendering, because that is what gets embedded.
        assert_eq!(
            outcome.structured.get("rendering").and_then(Value::as_str),
            Some(outcome.text.as_str())
        );
    }

    #[test]
    fn compress_then_decompress_recovers_the_input_exactly() {
        let original = r#"{"users":[{"id":1,"name":"a"},{"id":2,"name":"b"}]}"#;
        let arguments = Object::new().set("text", Value::string(original)).build();
        let outcome = call(COMPRESS, Some(&arguments)).unwrap();
        let archive = outcome
            .structured
            .get("archive")
            .and_then(Value::as_str)
            .unwrap();

        let back = Object::new().set("archive", Value::string(archive)).build();
        let restored = call(DECOMPRESS, Some(&back)).unwrap();
        assert!(!restored.is_error);
        assert_eq!(restored.text, original);
    }

    #[test]
    fn non_json_input_passes_through_instead_of_failing() {
        let outcome = call(COMPRESS, Some(&args(r#"{"text":"not json at all"}"#))).unwrap();
        assert!(!outcome.is_error, "pass-through must not be an error");
        assert_eq!(
            outcome.structured.get("compressed"),
            Some(&Value::Bool(false))
        );
        assert_eq!(outcome.text, "not json at all");
        assert_eq!(
            outcome.structured.get("reasonCode").and_then(Value::as_str),
            Some("invalid_json")
        );
    }

    #[test]
    fn estimate_reports_stats_without_the_payload() {
        let outcome = call(ESTIMATE, Some(&args(r#"{"text":"{\"a\": 1}"}"#))).unwrap();
        assert!(!outcome.is_error);
        assert!(outcome.structured.get("stats").is_some());
        assert!(
            outcome.structured.get("archive").is_none(),
            "estimate must not return a payload"
        );
        assert!(outcome.structured.get("rendering").is_none());
        assert!(outcome.text.contains("encoder="));
    }

    #[test]
    fn estimate_and_compress_agree_on_the_numbers() {
        let arguments = args(r#"{"text":"{\"a\":[1,2,3],\"b\":\"x\"}"}"#);
        let estimated = call(ESTIMATE, Some(&arguments)).unwrap();
        let compressed = call(COMPRESS, Some(&arguments)).unwrap();
        assert_eq!(
            estimated.structured.get("stats"),
            compressed.structured.get("stats")
        );
    }

    #[test]
    fn a_corrupt_archive_is_a_visible_error_with_a_stable_code() {
        let arguments = args(r#"{"archive":"AAAAAAAAAAAAAAAA"}"#);
        let outcome = call(DECOMPRESS, Some(&arguments)).unwrap();
        assert!(outcome.is_error);
        assert_eq!(
            outcome.structured.get("code").and_then(Value::as_str),
            Some("bad_magic")
        );
    }

    #[test]
    fn a_malformed_archive_string_is_a_visible_error() {
        let arguments = args(r#"{"archive":"not base64!!"}"#);
        let outcome = call(DECOMPRESS, Some(&arguments)).unwrap();
        assert!(outcome.is_error);
        assert_eq!(
            outcome.structured.get("code").and_then(Value::as_str),
            Some("invalid_base64")
        );
    }

    #[test]
    fn a_flipped_bit_in_the_archive_never_yields_output() {
        let arguments = Object::new()
            .set("text", Value::string(r#"{"a":1}"#))
            .build();
        let archive = call(COMPRESS, Some(&arguments))
            .unwrap()
            .structured
            .get("archive")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        let mut bytes = base64::decode(&archive).unwrap();
        let last = bytes.len() - 1;
        if let Some(byte) = bytes.get_mut(last) {
            *byte ^= 0x01;
        }
        let tampered = Object::new()
            .set("archive", Value::string(base64::encode(&bytes)))
            .build();
        let outcome = call(DECOMPRESS, Some(&tampered)).unwrap();
        assert!(outcome.is_error, "a tampered archive must not decode");
    }

    #[test]
    fn an_unknown_tool_is_a_call_error_not_a_tool_error() {
        assert!(call("nope", None).is_err());
    }

    #[test]
    fn missing_or_mistyped_arguments_are_call_errors() {
        assert!(call(COMPRESS, None).is_err());
        assert!(call(COMPRESS, Some(&args("{}"))).is_err());
        assert!(call(COMPRESS, Some(&args(r#"{"text":5}"#))).is_err());
        assert!(call(DECOMPRESS, Some(&args("{}"))).is_err());
    }

    #[test]
    fn every_documented_profile_is_accepted_and_others_are_not() {
        // The profile list is read out of the advertised schema rather than repeated
        // here. A hardcoded list would keep passing after the enum and the handler
        // drifted apart — which is precisely the failure a client hits: it validates
        // against the advertised enum, sends a value that enum permits, and the
        // server rejects it as INVALID_PARAMS.
        let listed = catalogue();
        let schema = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool.get("name").and_then(Value::as_str) == Some(COMPRESS))
            .and_then(|tool| tool.get("inputSchema"))
            .unwrap();
        let advertised = schema
            .get("properties")
            .and_then(|properties| properties.get("profile"))
            .and_then(|profile| profile.get("enum"))
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(advertised.len(), 3, "the advertised profile list changed");
        for profile in advertised {
            let profile = profile.as_str().unwrap();
            let arguments = Object::new()
                .set("text", Value::string(r#"{"a":1}"#))
                .set("profile", Value::string(profile))
                .build();
            assert!(
                call(COMPRESS, Some(&arguments)).is_ok(),
                "{profile} is advertised but rejected"
            );
        }
        let bad = Object::new()
            .set("text", Value::string(r#"{"a":1}"#))
            .set("profile", Value::string("turbo"))
            .build();
        assert!(call(COMPRESS, Some(&bad)).is_err());
    }

    #[test]
    fn an_absent_or_null_profile_falls_back_to_the_default() {
        // A client that serializes an unset option as `null` must get the same
        // compression a client that omitted the key gets — not merely "no error".
        let absent = call(COMPRESS, Some(&args(r#"{"text":"{\"a\":1}"}"#))).unwrap();
        let explicit_null = call(
            COMPRESS,
            Some(&args(r#"{"text":"{\"a\":1}","profile":null}"#)),
        )
        .unwrap();
        assert_eq!(absent.structured, explicit_null.structured);
        // And the fallback is the *default* profile, not an arbitrary one.
        let named_default = call(
            COMPRESS,
            Some(&args(r#"{"text":"{\"a\":1}","profile":"balanced"}"#)),
        )
        .unwrap();
        assert_eq!(absent.structured, named_default.structured);
    }

    #[test]
    fn stats_expose_every_field_a_caller_needs() {
        let outcome = call(ESTIMATE, Some(&args(r#"{"text":"{\"a\":1}"}"#))).unwrap();
        let stats = outcome.structured.get("stats").unwrap();
        for field in [
            "bytesBefore",
            "bytesAfter",
            "estTokensBefore",
            "estTokensAfter",
            "byteRatio",
            "tokenRatio",
            "encoder",
            "encoderId",
            "tokenizerId",
            "minSavingBps",
            "fidelity",
        ] {
            assert!(stats.get(field).is_some(), "{field} missing from {stats}");
        }
        // Presence is not enough: a caller reading these branches on their JSON types,
        // and a count or ratio that arrived as a string would break it silently.
        assert_eq!(
            stats.get("bytesBefore").and_then(Value::as_i64),
            Some(i64::try_from(r#"{"a":1}"#.len()).unwrap())
        );
        assert!(matches!(stats.get("byteRatio"), Some(Value::Float(_))));
        assert!(matches!(stats.get("tokenRatio"), Some(Value::Float(_))));
        assert!(matches!(stats.get("encoderId"), Some(Value::Int(_))));
        assert!(matches!(stats.get("tokenizerId"), Some(Value::Int(_))));
        assert!(matches!(stats.get("minSavingBps"), Some(Value::Int(_))));
        assert_eq!(
            stats.get("fidelity").and_then(Value::as_str),
            Some("lossless")
        );
    }

    #[test]
    fn empty_text_is_handled_without_panicking() {
        // The empty string is not JSON, so it takes the passthrough path. "Not an
        // error" is the least of what has to hold: the input must come back
        // unchanged, and the payload keys must be *absent* rather than empty —
        // a client that saw `archive: ""` would hand that back to `decompress`
        // and get a corruption error for input that was never compressed.
        let outcome = call(COMPRESS, Some(&args(r#"{"text":""}"#))).unwrap();
        assert!(!outcome.is_error);
        assert_eq!(
            outcome.structured.get("compressed"),
            Some(&Value::Bool(false))
        );
        assert_eq!(outcome.text, "");
        assert_eq!(
            outcome.structured.get("rendering").and_then(Value::as_str),
            Some("")
        );
        assert!(outcome.structured.get("archive").is_none());
        assert!(outcome.structured.get("stats").is_none());
        // The decline is explained, so a caller can tell "empty input" from a real
        // refusal without parsing prose.
        assert_eq!(
            outcome.structured.get("reasonCode").and_then(Value::as_str),
            Some("invalid_json")
        );
    }

    #[test]
    fn every_reachable_compression_failure_maps_to_its_documented_wire_code() {
        // These strings are contract, not debug output: a caller branches on
        // `reasonCode` to decide what to do next. Lose an arm and its failure quietly
        // collapses into the generic `unrecognized` bucket — the client stops telling
        // "split this input, it is too big" apart from "this was never JSON, stop
        // retrying", and shows the user the wrong thing. Nothing else notices, because
        // the call still succeeds: pass-through is a success by design.
        assert_eq!(
            compress_error_code(&engine_error(Config::default(), b"not json at all")),
            "invalid_json"
        );
        assert_eq!(
            compress_error_code(&engine_error(
                Config::builder().max_input_bytes(1).build(),
                br#"{"a":1}"#
            )),
            "input_too_large"
        );
        assert_eq!(
            compress_error_code(&engine_error(
                Config::builder().max_depth(1).build(),
                br#"{"a":{"b":1}}"#
            )),
            "depth_exceeded"
        );
    }

    #[test]
    fn the_not_utf8_code_is_pinned_although_no_client_can_reach_it() {
        // Honest scope: this arm is unreachable *through this server* today, and the
        // test does not claim otherwise. `text` arrives as a parsed JSON string, so by
        // the time it reaches core it is a Rust `String` and `as_bytes` is valid UTF-8
        // by construction — and the two layers below rule out the ways it might not be:
        // the transport answers a non-UTF-8 line without parsing it, and the parser
        // rejects an unpaired surrogate escape rather than substituting a replacement
        // character. So the arm is pinned because the mapping is part of the published
        // contract and core may route new bytes here later, not because a client can
        // trigger it. Deleting it would be invisible to every end-to-end test.
        assert_eq!(
            compress_error_code(&engine_error(Config::default(), &[0xFF, 0xFE])),
            "not_utf8"
        );
    }

    #[test]
    fn a_declined_compression_is_explained_in_prose_specific_to_the_failure() {
        // `reason` is the sentence a model reads when its input came back
        // uncompressed. Blank it, or return one fixed string for every failure, and
        // every structural assertion still passes — the model simply never learns
        // whether to split the input, give up, or ignore the whole thing.
        let invalid_json = engine_error(Config::default(), b"not json at all");
        let too_large = engine_error(Config::builder().max_input_bytes(1).build(), br#"{"a":1}"#);

        for error in [&invalid_json, &too_large] {
            let reason = compress_error_reason(error);
            assert!(!reason.trim().is_empty(), "a blank reason explains nothing");
            assert!(
                reason.contains(' '),
                "a reason is a sentence for a human, not a second copy of the code: {reason}"
            );
        }

        // Two unrelated failures must read differently, or the reason carries nothing
        // the code did not already carry.
        assert_ne!(
            compress_error_reason(&invalid_json),
            compress_error_reason(&too_large)
        );
        assert_ne!(
            compress_error_reason(&invalid_json),
            compress_error_code(&invalid_json),
            "the reason must say more than the machine-readable code already says"
        );

        // And it describes this failure, not merely its category: the same variant
        // raised against a different limit has to report that limit, which is the part
        // that makes the sentence actionable.
        let other_limit = engine_error(Config::builder().max_input_bytes(2).build(), br#"{"a":1}"#);
        assert_ne!(
            compress_error_reason(&too_large),
            compress_error_reason(&other_limit),
            "the reason must carry the numbers that make it actionable"
        );
    }
}
