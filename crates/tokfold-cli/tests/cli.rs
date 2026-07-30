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
fn mcp_prints_experimental_notice_and_exits_69() {
    let out = run_tokfold(&["mcp"], b"");
    assert_eq!(
        out.status.code(),
        Some(69),
        "mcp must exit 69 (EX_UNAVAILABLE), distinct from bad input (2), stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("EXPERIMENTAL"),
        "mcp must surface the experimental notice on stderr"
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
