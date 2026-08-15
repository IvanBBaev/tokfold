//! End-to-end tests for the `tokfold` binary, driving the real process over its
//! stdin/stdout/stderr contract and asserting the normative exit codes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A homogeneous JSON array whose repeated keys make it a clear token win for the
/// tabular encoder — and, being valid JSON, a clean recovery-archive round trip.
const COMPRESSIBLE_JSON: &[u8] = br#"[{"id":0,"name":"item0","active":true},{"id":1,"name":"item1","active":true},{"id":2,"name":"item2","active":true},{"id":3,"name":"item3","active":true}]"#;

/// Pretty-printed, heterogeneous JSON: the objects differ in shape, so the tabular
/// encoder has nothing to hoist and the minifier's whitespace stripping wins on every
/// profile. This is the only input in this file whose rendering is `e1-minify`.
const WHITESPACE_JSON: &[u8] = br#"{
    "service"     : "gateway",
    "version"     : "1.4.2",
    "healthy"     : true,
    "replicas"    : 3,
    "endpoints"   : [
        "https://example.invalid/a",
        "https://example.invalid/b"
    ],
    "limits"      : {
        "cpu"     : "500m",
        "memory"  : "512Mi",
        "timeout" : 30
    },
    "labels"      : {
        "tier"    : "edge",
        "owner"   : "platform"
    }
}"#;

/// The rendering always opens with `⟦tkfd:v1:` regardless of the winning encoder.
const SENTINEL_PREFIX: &[u8] = "\u{27E6}tkfd:v1:".as_bytes();

/// Run the built `tokfold` binary with `args`, feeding `stdin_bytes` on standard
/// input, and capture its output and exit status.
fn run_tokfold(args: &[&str], stdin_bytes: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tokfold");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin_bytes)
        .expect("write child stdin");
    child.wait_with_output().expect("wait for tokfold")
}

/// A per-test archive path under the package's integration-test temp directory.
fn archive_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

#[test]
fn compress_expand_round_trip_via_stdin_stdout() {
    let archive = archive_path("round_trip.tkfd");
    let archive_str = archive.to_str().expect("utf-8 archive path");

    // compress: original on stdin, recovery archive to a file, rendering to stdout.
    let compressed = run_tokfold(&["compress", "--archive", archive_str], COMPRESSIBLE_JSON);
    assert!(
        compressed.status.success(),
        "compress failed: {}",
        String::from_utf8_lossy(&compressed.stderr)
    );
    assert!(
        compressed.stdout.starts_with(SENTINEL_PREFIX),
        "rendering did not open with the tkfd sentinel"
    );

    // expand: archive on stdin, reconstructed original on stdout.
    let archive_bytes = fs::read(&archive).expect("read archive file");
    let expanded = run_tokfold(&["expand"], &archive_bytes);
    assert!(
        expanded.status.success(),
        "expand failed: {}",
        String::from_utf8_lossy(&expanded.stderr)
    );
    assert_eq!(
        expanded.stdout, COMPRESSIBLE_JSON,
        "round trip did not reproduce the original bytes"
    );
}

#[test]
fn expand_of_corrupted_archive_exits_3_and_emits_nothing() {
    let archive = archive_path("corrupted.tkfd");
    let archive_str = archive.to_str().expect("utf-8 archive path");

    let compressed = run_tokfold(&["compress", "--archive", archive_str], COMPRESSIBLE_JSON);
    assert!(compressed.status.success());

    // Flip the final payload byte so the reconstruction fails its checksum.
    let mut archive_bytes = fs::read(&archive).expect("read archive file");
    let last = archive_bytes.last_mut().expect("archive is non-empty");
    *last ^= 0x01;

    let expanded = run_tokfold(&["expand"], &archive_bytes);
    assert_eq!(
        expanded.status.code(),
        Some(3),
        "corrupt archive must exit 3, stderr: {}",
        String::from_utf8_lossy(&expanded.stderr)
    );
    assert!(
        expanded.stdout.is_empty(),
        "a corrupt expand must never emit best-effort bytes"
    );
}

#[test]
fn compress_passes_invalid_json_through_and_succeeds() {
    let input = b"this is not json at all";
    let out = run_tokfold(&["compress"], input);
    assert!(
        out.status.success(),
        "compress must not gate on a compression failure"
    );
    assert_eq!(
        out.stdout, input,
        "the original bytes must pass through unmodified"
    );
    assert!(
        !out.stderr.is_empty(),
        "a passthrough should be explained on stderr"
    );
}

#[test]
fn stats_reports_the_expected_fields() {
    let out = run_tokfold(&["stats"], COMPRESSIBLE_JSON);
    assert!(out.status.success());
    let report = String::from_utf8(out.stdout).expect("stats report is utf-8");
    for label in [
        "bytes before:",
        "bytes after:",
        "byte ratio:",
        "est tokens before:",
        "est tokens after:",
        "token ratio:",
        "encoder:",
        "tokenizer:",
    ] {
        assert!(report.contains(label), "stats report missing {label:?}");
    }
}

#[test]
fn stats_rejects_invalid_json_with_exit_2() {
    let out = run_tokfold(&["stats"], b"not json");
    assert_eq!(
        out.status.code(),
        Some(2),
        "invalid input to stats is exit 2"
    );
    assert!(
        out.stdout.is_empty(),
        "stats must not emit a report on error"
    );
}

#[test]
fn mcp_prints_the_experimental_notice_and_exits_zero_on_empty_input() {
    let out = run_tokfold(&["mcp"], b"");
    // The subcommand used to exit 69 unconditionally as an unimplemented stub. Now
    // that it serves, empty stdin is an immediate end-of-session, not a failure — the
    // number changed on purpose and the old expectation would have hidden that.
    assert_eq!(
        out.status.code(),
        Some(0),
        "an empty stream is a clean session, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "no request means no reply; stdout carries protocol only"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("EXPERIMENTAL"),
        "mcp must surface the experimental notice on stderr"
    );
}

#[test]
fn mcp_answers_a_tools_list_request_on_stdout() {
    let out = run_tokfold(
        &["mcp"],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n",
    );
    assert_eq!(out.status.code(), Some(0), "a served session exits clean");

    let stdout = String::from_utf8(out.stdout).expect("the transport emits UTF-8");
    // One line per message is the transport's whole framing contract; a stray newline
    // inside a reply would desynchronise every client that reads line by line.
    assert_eq!(
        stdout.lines().count(),
        1,
        "one request, one line of reply: {stdout}"
    );
    assert!(
        stdout.contains("tokfold_compress")
            && stdout.contains("tokfold_decompress")
            && stdout.contains("tokfold_estimate"),
        "the catalogue must list all three tools: {stdout}"
    );
    // The notice belongs on stderr precisely so it cannot land here and corrupt the
    // first exchange.
    assert!(
        !stdout.contains("EXPERIMENTAL"),
        "the notice must not reach stdout: {stdout}"
    );
}

#[test]
fn stdout_reader_closing_the_pipe_early_is_a_clean_exit() {
    use std::io::Read as _;
    use std::thread;

    // A payload far larger than any OS pipe buffer that is NOT valid JSON, so `compress`
    // passes it through unchanged: the child's stdout is then guaranteed to exceed the
    // pipe buffer, so it cannot finish writing once the reader goes away.
    let big = vec![b'x'; 4 * 1024 * 1024];

    let mut child = Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .arg("compress")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn tokfold");

    // Feed stdin from a separate thread so closing stdout can happen concurrently;
    // driving both pipes from this thread would deadlock once the unread stdout fills.
    let mut stdin = child.stdin.take().expect("child stdin");
    let writer = thread::spawn(move || stdin.write_all(&big));

    // Read one byte to be sure the child has started writing, then drop stdout to close
    // the read end. The child's next write then sees BrokenPipe, which must still exit 0.
    let mut stdout = child.stdout.take().expect("child stdout");
    let mut first = [0_u8; 1];
    let _ = stdout.read(&mut first);
    drop(stdout);

    let status = child.wait().expect("wait for tokfold");
    // The child may abandon the stdin read once its output pipe breaks, so the writer's
    // own result is expected to be `Ok` or a `BrokenPipe` error — either is acceptable.
    let _ = writer.join().expect("join stdin writer");
    assert!(
        status.success(),
        "a downstream closing the pipe early must exit 0, got {status:?}"
    );
}

/// Run `stats` over `input` with an explicit `--profile` and return the report.
fn stats_report(profile: &str, input: &[u8]) -> String {
    let out = run_tokfold(&["stats", "--profile", profile], input);
    assert!(
        out.status.success(),
        "stats --profile {profile} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("stats report is utf-8")
}

/// The `--profile` flag has to reach the engine, and the report has to name the
/// encoder that actually ran. Both were untested: every existing assertion only
/// checked that the label `encoder:` appeared, which stays true no matter which
/// encoder won or what it is called.
///
/// The same already-minified array is a clear tabular win but nothing a minifier can
/// improve, so the two profiles land on visibly different encoders — which is exactly
/// what makes the flag observable.
#[test]
fn the_profile_flag_selects_the_encoder_the_report_names() {
    let conservative = stats_report("conservative", COMPRESSIBLE_JSON);
    assert!(
        conservative.contains("passthrough (id 0)"),
        "conservative may not use the tabular encoder: {conservative}"
    );

    let balanced = stats_report("balanced", COMPRESSIBLE_JSON);
    assert!(
        balanced.contains("e2-tabular (id 2)"),
        "balanced must let the tabular encoder compete: {balanced}"
    );

    // `aggressive` is documented as identical to `balanced` in v0.0.1; pin that so the
    // day it diverges is a deliberate change and not a silent one.
    let aggressive = stats_report("aggressive", COMPRESSIBLE_JSON);
    assert_eq!(
        aggressive, balanced,
        "aggressive and balanced are identical in v0.0.1"
    );
}

/// The minifier's own name and id must reach the report. Without an input the
/// minifier wins on, `e1-minify` is a string no test ever observes.
#[test]
fn stats_names_the_minifier_when_it_wins() {
    let report = stats_report("conservative", WHITESPACE_JSON);
    assert!(
        report.contains("e1-minify (id 1)"),
        "the minifier must win on whitespace-heavy JSON: {report}"
    );
}

/// The tokenizer line carries the id recorded in every archive, so its name/id pairing
/// is a wire contract. The CLI has no estimator flag, so the heuristic default is the
/// only pairing reachable end to end; the rest are pinned by the unit tests in
/// `src/main.rs`.
#[test]
fn stats_names_the_default_tokenizer() {
    let report = stats_report("balanced", COMPRESSIBLE_JSON);
    assert!(
        report.contains("heuristic (id 0)"),
        "the default estimator is the heuristic one, id 0: {report}"
    );
}

/// `write_stdout` swallows exactly one error — `BrokenPipe`, a reader that stopped
/// consuming — and must report every other write failure. Only the swallowing half
/// was tested, so widening the guard to accept *any* stdout failure was invisible.
///
/// Hand the child an unbound datagram socket as its standard output: writing to it
/// fails with "destination address required", which is emphatically not a broken pipe,
/// so the failure has to surface as exit 2.
#[cfg(unix)]
#[test]
fn a_stdout_failure_that_is_not_a_broken_pipe_exits_2() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixDatagram;

    let sink = UnixDatagram::unbound().expect("unbound datagram socket");

    let mut child = Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .arg("compress")
        .stdin(Stdio::piped())
        .stdout(Stdio::from(OwnedFd::from(sink)))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tokfold");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"this is not json at all")
        .expect("write child stdin");
    let out = child.wait_with_output().expect("wait for tokfold");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a stdout failure that is not a broken pipe must exit 2, stderr: {stderr}"
    );
    assert!(
        stderr.contains("writing to standard output"),
        "the write failure must be explained on stderr: {stderr}"
    );
}

/// `cmd_compress` documents that the recovery archive is persisted *before* the
/// rendering reaches stdout, "so a caller never sees output without its recovery
/// blob". Nothing tested that ordering, and it is exactly the kind of contract a
/// harmless-looking refactor reverses.
///
/// Point `--archive` at a path inside a directory that does not exist: the write
/// must fail, and stdout must then be completely empty.
#[test]
fn a_failed_archive_write_suppresses_the_rendering() {
    let unwritable = archive_path("no_such_directory").join("archive.tkfd");
    let unwritable_str = unwritable.to_str().expect("utf-8 archive path");
    assert!(
        !unwritable.parent().expect("parent").exists(),
        "test precondition: the archive's parent directory must not exist"
    );

    let out = run_tokfold(
        &["compress", "--archive", unwritable_str],
        COMPRESSIBLE_JSON,
    );
    assert!(
        !out.status.success(),
        "a failed archive write must not report success"
    );
    assert!(
        out.stdout.is_empty(),
        "rendering was emitted despite the archive write failing: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}
