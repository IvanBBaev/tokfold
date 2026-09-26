//! End-to-end tests for the `tokfold` binary, driving the real process over its
//! stdin/stdout/stderr contract and asserting the normative exit codes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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

/// Run the built `tokfold` binary with `args` and no standard input at all.
///
/// Every test that feeds the binary through `--input` wants this rather than
/// `run_tokfold`: the child never reads stdin, so a piped-and-written stdin races the
/// child's exit and can fail the *parent's* write with `BrokenPipe`. Handing the child
/// `/dev/null` removes the race without hiding anything the test cares about.
fn run_tokfold_without_stdin(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run tokfold")
}

/// Wait for `child` for at most ten seconds, killing it if it outlives that, and
/// report whether it exited on its own.
///
/// A test for "this must not hang" that hangs when it regresses reports nothing, so
/// every wait that could block forever goes through here and the timeout becomes an
/// assertion of its own rather than a stalled run.
fn finished_within_ten_seconds(child: &mut Child) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if child.try_wait().expect("poll the child").is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _ = child.kill();
    false
}

/// A per-test archive path under the package's integration-test temp directory.
fn archive_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

/// A fresh, empty per-test directory under the integration-test temp directory.
///
/// Tests in this file run in parallel and `CARGO_TARGET_TMPDIR` survives between
/// runs, so anything that asserts on the *absence* of a file needs its own directory
/// wiped at the start — otherwise a leftover from a previous run decides the result.
fn scratch_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

/// Spell a path for the command line.
fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

/// Run the binary with `dir` as its working directory and no standard input.
///
/// The only way to hand the binary a genuinely *relative* path: every other helper
/// inherits the test runner's working directory, so a test that spells `./data.json`
/// without this is silently testing an absolute path under a different name.
fn run_tokfold_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run tokfold")
}

/// Leave a genuine recovery archive at `archive` by running a successful compress,
/// and assert it really is one before any test builds on it.
fn seed_real_archive(input: &Path, archive: &Path) {
    fs::write(input, COMPRESSIBLE_JSON).expect("write the compressible input");
    let out = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(input),
        "--archive",
        path_str(archive),
    ]);
    assert!(
        out.status.success(),
        "test precondition: seeding a real archive failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read(archive).expect("read the seeded archive").get(..4),
        Some(&b"TKFD"[..]),
        "test precondition: the seeded file must carry the archive magic"
    );
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

/// The offsets the README section "Reading `archive corrupted at byte N`" quotes,
/// read back from the line `expand` prints. The library tests pin the same offsets on
/// `Header::decode`; this one pins the sentence a user actually sees, which the exit
/// code alone never did -- the `Corrupt` message could be reworded, or the offset
/// dropped from it, with every other test in the workspace still green.
#[test]
fn expand_names_the_corrupt_offsets_the_readme_quotes() {
    let archive = archive_path("corrupt-offsets.tkfd");
    let archive_str = archive.to_str().expect("utf-8 archive path");
    let compressed = run_tokfold(&["compress", "--archive", archive_str], br#"{"k":"hello"}"#);
    assert!(compressed.status.success());
    let good = fs::read(&archive).expect("read archive file");
    // The length of a 13-byte original is one ULEB128 byte, at offset 10.
    assert_eq!(good[10], 13, "the fixture's length field moved");

    let spliced_length = |length_field: &[u8]| {
        let mut bytes = good[..10].to_vec();
        bytes.extend_from_slice(length_field);
        bytes.extend_from_slice(&good[11..]);
        bytes
    };
    let mut version_zero = good.clone();
    version_zero[4] = 0;
    let tenth = |last: u8| {
        let mut field = vec![0x80u8; 9];
        field.push(last);
        spliced_length(&field)
    };
    let with_byte = |offset: usize, value: u8| {
        let mut bytes = good.clone();
        bytes[offset] = value;
        bytes
    };

    let cases: [(&str, Vec<u8>, usize); 10] = [
        ("cut to 20 bytes", good[..20].to_vec(), 11),
        ("cut to 43 bytes", good[..43].to_vec(), 10),
        ("version 0", version_zero, 4),
        ("encoder id 9", with_byte(5, 9), 5),
        ("tokenizer id 9", with_byte(6, 9), 6),
        ("named flag bit 0", with_byte(8, good[8] | 1), 8),
        // 13 spelled in two bytes: the second byte is the overlong one.
        ("overlong length", spliced_length(&[0x8D, 0x00]), 11),
        ("nine 0x80 and a 0x82", tenth(0x82), 19),
        ("nine 0x80 and a 0x81", tenth(0x81), 20),
        ("ten 0xFF", spliced_length(&[0xFF; 10]), 19),
    ];
    for (name, bytes, offset) in cases {
        let expanded = run_tokfold(&["expand"], &bytes);
        let stderr = String::from_utf8_lossy(&expanded.stderr);
        assert_eq!(expanded.status.code(), Some(3), "{name}: {stderr}");
        assert!(expanded.stdout.is_empty(), "{name}");
        assert!(
            stderr
                .trim_end()
                .ends_with(&format!(": archive corrupted at byte {offset}")),
            "{name}: wanted byte {offset}, got {stderr}"
        );
    }
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
fn a_rejected_input_is_explained_with_the_numbers_that_rejected_it() {
    // The CLI README says 512 levels are accepted and 513 are not; the line a user
    // reads has to carry both numbers, or it cannot say which limit tripped or how far
    // past it the input went.
    let deep = "[".repeat(600);
    let out = run_tokfold(&["compress"], deep.as_bytes());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(out.stdout, deep.as_bytes());
    assert!(
        stderr.contains(
            "tokfold: passing input through uncompressed: nesting depth 513 exceeds limit 512"
        ),
        "{stderr}"
    );

    let out = run_tokfold(&["compress"], b"[1,]");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "tokfold: passing input through uncompressed: invalid JSON at byte 3: expected a JSON value"
        ),
        "{stderr}"
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

/// Split a `stats` report into its `label -> value` pairs, in the order printed.
fn stats_fields(report: &str) -> Vec<(String, String)> {
    report
        .lines()
        .map(|line| {
            let (label, value) = line
                .split_once(':')
                .unwrap_or_else(|| panic!("every stats line is `label: value`, got {line:?}"));
            (label.trim().to_string(), value.trim().to_string())
        })
        .collect()
}

/// Every assertion this file made about `stats` looked at the two *name* lines. All six
/// numeric lines — both byte counts, both token counts, both ratios — were pinned by
/// nothing: mutation testing put a scrambled `format_stats` past the whole suite, so
/// the report could have printed the wrong field on every line, swapped `before` with
/// `after`, or dropped the ratios' precision without a single test noticing.
///
/// Each number is checked against something outside the report: `bytes before` against
/// the input, `bytes after` against what `compress` actually emits for the same input
/// and profile, and each ratio against the two counts printed above it.
#[test]
fn the_stats_numbers_are_the_measurements_they_claim_to_be() {
    let report = stats_report("balanced", COMPRESSIBLE_JSON);
    let fields = stats_fields(&report);
    let labels: Vec<&str> = fields.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(
        labels,
        [
            "bytes before",
            "bytes after",
            "byte ratio",
            "est tokens before",
            "est tokens after",
            "token ratio",
            "encoder",
            "tokenizer",
        ],
        "the report's field order is part of its contract: {report}"
    );
    let value = |label: &str| -> String {
        fields
            .iter()
            .find(|(l, _)| l == label)
            .unwrap_or_else(|| panic!("no {label:?} line in {report}"))
            .1
            .clone()
    };
    let number = |label: &str| -> f64 {
        value(label)
            .parse()
            .unwrap_or_else(|e| panic!("{label:?} is not a number: {e}"))
    };

    let bytes_before = number("bytes before");
    let bytes_after = number("bytes after");
    let est_before = number("est tokens before");
    let est_after = number("est tokens after");

    #[allow(clippy::cast_precision_loss)]
    let input_len = COMPRESSIBLE_JSON.len() as f64;
    assert!(
        (bytes_before - input_len).abs() < f64::EPSILON,
        "`bytes before` must be the input's own length, {input_len}: {report}"
    );

    // The one measurement no report can check against itself: what the same pass
    // actually writes. `stats` and `compress` share `config_for`, so the byte count
    // one prints is the length of what the other emits.
    let rendered = run_tokfold(&["compress", "--profile", "balanced"], COMPRESSIBLE_JSON);
    assert!(rendered.status.success());
    #[allow(clippy::cast_precision_loss)]
    let rendered_len = rendered.stdout.len() as f64;
    assert!(
        (bytes_after - rendered_len).abs() < f64::EPSILON,
        "`bytes after` must be the length of the rendering compress emits, \
         {rendered_len}: {report}"
    );

    // Both ratios are `after / before` at four decimals, and the input was chosen so
    // both directions are a real win — a report that swapped the two counts, or paired
    // a ratio with the wrong one, would land somewhere else.
    assert!(
        (number("byte ratio") - bytes_after / bytes_before).abs() < 5e-5,
        "`byte ratio` must be `bytes after / bytes before`: {report}"
    );
    assert!(
        (number("token ratio") - est_after / est_before).abs() < 5e-5,
        "`token ratio` must be `est tokens after / est tokens before`: {report}"
    );
    assert!(
        bytes_after < bytes_before && est_after < est_before,
        "the tabular encoder wins on this input in both bytes and tokens: {report}"
    );
    assert!(
        est_before > 0.0 && est_after > 0.0,
        "a non-empty document estimates to a non-zero token count: {report}"
    );
}

/// Every diagnostic used to go out through `eprintln!`, which *panics* when the write
/// fails. Stderr is routinely the stream a caller drops first, and Rust starts every
/// process with `SIGPIPE` ignored, so a stderr whose reader has gone comes back as an
/// error rather than a signal: a `compress` that had correctly passed its input through
/// then died with exit `101` and an "Uncaught panic" report, and a caller dispatching
/// on the exit code read a success as a crash.
///
/// Every command that writes a diagnostic is driven: `compress` passing a rejected
/// input through (exit `0`), `expand` refusing a document that is not an archive (exit
/// `3`, which must not become `101` either), `stats` refusing the same input (exit
/// `2`), and `mcp`, whose start-up notice is a stderr write that comes before the
/// server reads anything (exit `0` at end of input). `stats` was missing while the
/// sentence above already claimed every command, and it is the one that shares its
/// exit code with a failed write.
///
/// A clap usage error is driven too, because it is the one diagnostic tokfold does not
/// format itself: `--bogus` is answered by `usage_failure`, which drops a failed write
/// exactly as `warn` does, and must still exit `2` rather than lose the code.
///
/// The child's stderr is closed *before* its stdin is fed, so the pipe is already
/// broken by the time the input is read and the first diagnostic is written — without
/// that ordering the diagnostic could win the race into the pipe buffer and the test
/// would pass without exercising anything. `mcp` writes its notice without waiting for
/// input, so for it that race is not fully closed; it is the start-up write the
/// ordering cannot guarantee to break, and the case still pins the exit code.
#[test]
fn a_closed_stderr_does_not_cost_the_run_its_exit_code() {
    /// One command line, with the exit code and standard output it must still produce
    /// once its standard error is closed.
    struct Case {
        command: &'static str,
        args: &'static [&'static str],
        /// `None` is a command line that exits without reading standard input; feeding
        /// one would race the child's exit and fail on a broken pipe.
        input: Option<&'static [u8]>,
        code: i32,
        stdout: &'static [u8],
    }

    let cases = [
        Case {
            command: "compress",
            args: &["compress"],
            input: Some(b"not json"),
            code: 0,
            stdout: b"not json",
        },
        Case {
            command: "expand",
            args: &["expand"],
            input: Some(b"not an archive"),
            code: 3,
            stdout: b"",
        },
        Case {
            command: "stats",
            args: &["stats"],
            input: Some(b"not json"),
            code: 2,
            stdout: b"",
        },
        Case {
            command: "mcp",
            args: &["mcp"],
            input: Some(b""),
            code: 0,
            stdout: b"",
        },
        Case {
            command: "--bogus",
            args: &["--bogus"],
            input: None,
            code: 2,
            stdout: b"",
        },
    ];
    for Case {
        command,
        args,
        input,
        code,
        stdout,
    } in cases
    {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tokfold"))
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn tokfold");
        drop(child.stderr.take().expect("child stderr"));
        if let Some(input) = input {
            child
                .stdin
                .take()
                .expect("child stdin")
                .write_all(input)
                .expect("write child stdin");
        }

        let finished = finished_within_ten_seconds(&mut child);
        let out = child
            .wait_with_output()
            .expect("collect the child's output");
        assert!(
            finished,
            "{command}: the child hung with nobody reading its stderr"
        );
        assert_eq!(
            out.status.code(),
            Some(code),
            "{command}: an undeliverable diagnostic must not change the exit code; \
             exit 101 means a diagnostic panicked"
        );
        assert_eq!(
            out.stdout, stdout,
            "{command}: standard output must be what it is with stderr open"
        );
    }
}

/// Standard error is unbuffered, so a diagnostic written with `eprintln!` reached it as
/// one `write` per formatting fragment. A reader that takes each `write` as a record —
/// here a datagram socket, where every `write` is exactly one datagram — saw a
/// diagnostic with an argument arrive as several broken pieces. Each line must be one
/// `write`, so each datagram is exactly one complete line.
///
/// The name is a claim about every diagnostic, so both writers of standard error are
/// driven. tokfold formats its own through `warn`: a rejected `compress --archive`
/// over a stale archive writes three, two of them carrying arguments. clap renders the
/// other kind, and until `usage_failure` took the non-terminal path, clap's printer
/// sent one `write` per fragment — `--bogus` measured ten datagrams, `compress
/// --profile nope` fourteen, none of them a whole line. The clap case is pinned whole
/// rather than by record count, because a count says nothing about where the breaks
/// fell.
#[cfg(unix)]
#[test]
fn every_diagnostic_line_reaches_stderr_in_one_write() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixDatagram;

    fn stderr_records(args: &[&str]) -> (Option<i32>, Vec<String>) {
        let (ours, theirs) = UnixDatagram::pair().expect("datagram pair");
        let out = Command::new(env!("CARGO_BIN_EXE_tokfold"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(OwnedFd::from(theirs)))
            .output()
            .expect("run tokfold");

        ours.set_nonblocking(true).expect("non-blocking reads");
        let mut records = Vec::new();
        let mut buf = [0_u8; 4096];
        while let Ok(n) = ours.recv(&mut buf) {
            records
                .push(String::from_utf8_lossy(buf.get(..n).expect("received length")).into_owned());
        }
        (out.status.code(), records)
    }

    let dir = scratch_dir("one_write_per_line");
    let json = dir.join("in.json");
    let archive = dir.join("a.tkfd");
    seed_real_archive(&json, &archive);
    fs::write(&json, b"not json").expect("make the input one the engine rejects");

    let (code, records) = stderr_records(&[
        "compress",
        "--input",
        path_str(&json),
        "--archive",
        path_str(&archive),
    ]);
    assert_eq!(code, Some(0), "a rejected input is passed through");
    assert_eq!(
        records,
        [
            format!(
                "tokfold: removed {}: it opens with the tokfold archive magic\n",
                archive.display()
            ),
            "tokfold: passing input through uncompressed: \
             invalid JSON at byte 0: expected a JSON value\n"
                .to_owned(),
            "tokfold: no recovery archive written \
             (the input was passed through unchanged)\n"
                .to_owned(),
        ],
        "each diagnostic must arrive as exactly one write of one complete line"
    );

    let (code, records) = stderr_records(&["--bogus"]);
    assert_eq!(code, Some(2), "an unknown argument is a usage error");
    assert_eq!(
        records,
        ["error: unexpected argument '--bogus' found\n\n\
             Usage: tokfold <COMMAND>\n\n\
             For more information, try '--help'.\n"
            .to_owned(),],
        "a clap usage error must arrive as one write, not one per rendered fragment"
    );
}

/// `write_stdout` swallows exactly two errors — `BrokenPipe` and `ConnectionReset`,
/// both a reader that has gone — and must report every other write failure. Only the
/// swallowing half was tested, so widening the guard to accept *any* stdout failure was invisible.
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
    assert_eq!(
        out.status.code(),
        Some(2),
        "a failed archive write is an I/O failure, exit 2: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "rendering was emitted despite the archive write failing: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

// ---------------------------------------------------------------------------
// The archive path: it is destructive, so it may not be the input, and it may not
// outlive the pass that wrote it.
// ---------------------------------------------------------------------------

/// The regression test for the worst bug this binary could produce.
///
/// `compress` treats an input the engine rejects as a passthrough and still exits
/// `0`. It used to leave the archive of an *earlier*, successful run untouched at
/// `--archive`, and that archive still verified its own checksum. A pipeline that
/// re-ran `compress` and then `expand` therefore saw two clean exits and got back a
/// *different* document, with nothing anywhere signalling that it was stale.
///
/// This drives that exact sequence end to end: succeed, then fail onto the same
/// archive path, then try to expand it.
#[test]
fn a_rejected_compress_deletes_the_archive_an_earlier_run_left_behind() {
    let dir = scratch_dir("stale_archive");
    let archive = dir.join("recovery.tkfd");
    let good = dir.join("good.json");
    let bad = dir.join("bad.txt");
    fs::write(&good, COMPRESSIBLE_JSON).expect("write the compressible input");
    fs::write(&bad, b"this is not json at all").expect("write the rejected input");

    let first = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&good),
        "--archive",
        path_str(&archive),
    ]);
    assert!(
        first.status.success(),
        "the first compress failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        archive.is_file(),
        "test precondition: run one must leave a recovery archive behind"
    );
    assert_eq!(
        fs::read(&archive)
            .expect("read the archive run one wrote")
            .get(..4),
        Some(&b"TKFD"[..]),
        "test precondition: the file the reject path must delete is a real archive"
    );

    let second = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&bad),
        "--archive",
        path_str(&archive),
    ]);
    assert!(
        second.status.success(),
        "a rejected input is still a clean passthrough: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        second.stdout, b"this is not json at all",
        "the rejected bytes must pass through unmodified"
    );
    assert!(
        !archive.exists(),
        "the previous run's archive survived a rejected pass — `expand` would now \
         reconstruct a document that has nothing to do with what compress emitted"
    );

    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("it opens with the tokfold archive magic"),
        "deleting the user's file has to be said out loud: {stderr}"
    );

    // The end of the pipeline the bug actually corrupted: expanding what is left must
    // fail loudly rather than hand back run one's document.
    let expanded = run_tokfold_without_stdin(&["expand", "--input", path_str(&archive)]);
    assert_eq!(
        expanded.status.code(),
        Some(2),
        // Not 3: an archive that is not there is a bad input, not a corrupt one, and
        // "any non-zero code" would accept a panic or a signal death as the fix.
        "a missing archive is an I/O failure on the input, stderr: {}",
        String::from_utf8_lossy(&expanded.stderr)
    );
    assert!(
        expanded.stdout.is_empty(),
        "expand emitted bytes for an archive that no longer exists: {:?}",
        String::from_utf8_lossy(&expanded.stdout)
    );
}

/// A missing archive is the normal first-run case, and it must stay silent. Treating
/// `NotFound` as a failure would turn every ordinary passthrough into exit `2`.
#[test]
fn a_rejected_compress_without_a_previous_archive_is_still_a_clean_passthrough() {
    let dir = scratch_dir("no_stale_archive");
    let archive = dir.join("recovery.tkfd");

    let out = run_tokfold(&["compress", "--archive", path_str(&archive)], b"not json");
    assert!(
        out.status.success(),
        "a first-run passthrough must exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"not json");
    assert!(
        !archive.exists(),
        "a rejected pass must not create an archive"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("it opens with the tokfold archive magic"),
        "nothing was there to remove, so nothing may be claimed: {stderr}"
    );
    let expected = format!(
        "tokfold: no recovery archive written at {} (the input was passed through \
         unchanged)",
        archive.display()
    );
    assert!(
        stderr.contains(&expected),
        "the caller still has to be told there is no archive, and where: {stderr}"
    );
}

/// The mirror of `a_failed_archive_write_suppresses_the_rendering`, for the reject
/// path. There the archive I/O is a read and possibly a removal, and it is fatal for
/// the same reason: emitting bytes that a surviving archive contradicts is precisely
/// the corruption the clearing exists to prevent.
///
/// A directory at the archive path is one the clearing cannot resolve either way —
/// on Unix it opens and then fails the read, on Windows it fails the open — so the
/// run can neither prove the path is harmless nor make it so.
#[test]
fn a_stale_archive_that_cannot_be_removed_suppresses_the_passthrough() {
    let dir = scratch_dir("unremovable_archive");
    let archive = dir.join("recovery.tkfd");
    fs::create_dir(&archive).expect("create the directory standing in for an archive");

    let out = run_tokfold(&["compress", "--archive", path_str(&archive)], b"not json");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an archive that cannot be cleared is an I/O failure, stderr: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "the passthrough was emitted while a contradicting archive was still on disk"
    );
    assert!(
        (stderr.contains("inspecting the archive path")
            || stderr.contains("removing stale archive"))
            && stderr.contains(path_str(&archive)),
        "the failure must name the path it could not clear: {stderr}"
    );
}

/// The deletion is keyed to the archive magic, and a file without it is out of reach.
///
/// `--archive` names a path the user typed, and a typo is all it takes to aim it at
/// a file that has nothing to do with tokfold. Only a file carrying the archive
/// magic can be expanded back into a stale document, so the magic is what the reject
/// path checks before it removes anything: `compress --archive notes.md` on a
/// rejected input has to hand `notes.md` back byte for byte. The magic is a
/// necessary condition for the hazard, not a sufficient one -- four bytes of `TKFD`
/// are enough to be deleted and nowhere near enough to be expanded -- so the promise
/// pinned here holds only for a file that does not open with the magic;
/// `only_the_archive_magic_marks_a_file_the_reject_path_may_delete` pins the other
/// direction. Nothing stale survives the restraint, because `expand` fails closed on
/// anything that is not an archive.
#[test]
fn a_non_tokfold_file_at_the_archive_path_is_left_untouched() {
    let dir = scratch_dir("foreign_archive_path");
    let notes = dir.join("notes.md");
    let contents: &[u8] = b"# my notes\n\nnothing in here is a tokfold archive.\n";
    fs::write(&notes, contents).expect("write the bystanding file");

    let out = run_tokfold(&["compress", "--archive", path_str(&notes)], b"not json");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a rejected input is still a clean passthrough: {stderr}"
    );
    assert_eq!(
        out.stdout, b"not json",
        "the rejected bytes must pass through unmodified"
    );
    assert_eq!(
        fs::read(&notes).expect("re-read the bystanding file"),
        contents,
        "compress destroyed a file that was never one of its archives"
    );
    assert!(
        stderr.contains(&format!(
            "left {} untouched: it is not a tokfold archive",
            path_str(&notes)
        )),
        "declining to touch the path has to be said out loud: {stderr}"
    );
    assert!(
        !stderr.contains("it opens with the tokfold archive magic"),
        "nothing was removed, so nothing may be claimed: {stderr}"
    );
}

/// The magic alone decides, and it decides in both directions.
///
/// Four bytes are all it takes for `expand` to *enter* the archive path rather than
/// bail with "not a recognized payload" — reading one back to the end takes at least
/// 43 bytes and a matching digest, 43 being a floor rather than a size because the
/// ULEB128 length field gains a byte for every 7 bits the original grows — so a
/// truncated or corrupt archive is still an archive and
/// still has to go. Waiting for a file that decodes would let the stale-document
/// hazard back in through every damaged one. The near misses pin the other edge:
/// three bytes cannot carry the magic at all, and a file that merely opens with
/// similar letters belongs to somebody else.
#[test]
fn only_the_archive_magic_marks_a_file_the_reject_path_may_delete() {
    let dir = scratch_dir("archive_magic_only");

    let truncated = dir.join("truncated.tkfd");
    fs::write(&truncated, b"TKFD and then nothing that decodes").expect("write the stub archive");
    let out = run_tokfold(
        &["compress", "--archive", path_str(&truncated)],
        b"not json",
    );
    assert!(
        out.status.success(),
        "a rejected input is still a clean passthrough: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !truncated.exists(),
        "a damaged archive is still an archive — it must not outlive the run that \
         invalidated it just because it would fail to decode"
    );

    for (name, bytes) in [
        ("three_bytes", &b"TKF"[..]),
        ("no_bytes", &b""[..]),
        ("near_miss", &b"TKFE, close but not the magic"[..]),
    ] {
        let path = dir.join(name);
        fs::write(&path, bytes).expect("write the bystanding file");
        let out = run_tokfold(&["compress", "--archive", path_str(&path)], b"not json");
        assert!(
            out.status.success(),
            "{name}: a rejected input is still a clean passthrough: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            fs::read(&path).expect("re-read the bystanding file"),
            bytes,
            "{name} was destroyed by a run that had no claim on it"
        );
        // A file shorter than the magic is foreign, not absent: the caller is told
        // it was left alone, never that the slot was empty.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let left = format!(
            "tokfold: left {} untouched: it is not a tokfold archive",
            path.display()
        );
        assert!(
            stderr.contains(&left) && !stderr.contains(" written at "),
            "{name} was not reported as a foreign file: {stderr}"
        );
    }
}

/// A standard output that is open but not writable is a write failure, on both
/// `compress` outcomes and on `expand`.
///
/// `io::stdout()` treats `EBADF` as success, so before the binary wrote through a
/// duplicate of descriptor 1 each of these exited `0` with the payload gone, and the
/// rejected pass said on stderr that it had passed the input through. A descriptor
/// opened read-only is the portable way to get `EBADF` from a write; a *closed* one is
/// not, because the runtime reopens it on `/dev/null` before `main` runs.
#[cfg(unix)]
#[test]
fn a_standard_output_that_cannot_be_written_fails_the_command() {
    let dir = scratch_dir("read_only_stdout");
    let sink = dir.join("sink");
    fs::write(&sink, b"").expect("write the read-only sink");
    let valid = dir.join("valid.json");
    fs::write(&valid, br#"{ "a" : 1 }"#).expect("write the valid input");
    let invalid = dir.join("invalid.txt");
    fs::write(&invalid, b"not json").expect("write the invalid input");
    let archive = dir.join("recovery.tkfd");

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tokfold"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(fs::File::open(&sink).expect("open the sink read-only"))
            .stderr(Stdio::piped())
            .output()
            .expect("run tokfold")
    };

    let out = run(&["compress", "-i", path_str(&invalid)]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "rejected pass, stderr: {stderr}"
    );
    assert!(
        stderr.contains("writing to standard output")
            && stderr.contains("and passing it through failed")
            && !stderr.contains("passing input through"),
        "a passthrough that never happened was announced: {stderr}"
    );

    // With `--archive` the rejected pass has a second line that claims the input
    // reached stdout; it must not be printed either.
    let rejected_archive = dir.join("rejected.tkfd");
    let out = run(&[
        "compress",
        "-i",
        path_str(&invalid),
        "--archive",
        path_str(&rejected_archive),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "rejected pass with an archive, stderr: {stderr}"
    );
    assert!(
        stderr.contains("and passing it through failed")
            && !stderr.contains("no recovery archive written")
            && !stderr.contains("passing input through"),
        "an announcement that says the input was passed through was printed: {stderr}"
    );
    assert!(
        !rejected_archive.exists(),
        "a rejected pass created an archive"
    );

    let out = run(&[
        "compress",
        "-i",
        path_str(&valid),
        "--archive",
        path_str(&archive),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "successful pass, stderr: {stderr}"
    );
    assert!(stderr.contains("writing to standard output"), "{stderr}");

    let out = run(&["expand", "-i", path_str(&archive)]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "expand, stderr: {stderr}");
    assert!(stderr.contains("writing to standard output"), "{stderr}");
}

/// `--help` and `--version` reach standard output through the same guard, and so does
/// `mcp`: before, clap printed through `io::stdout()` and exited `0` with nothing
/// written, and the transport dropped every reply and exited `0` as well.
#[cfg(unix)]
#[test]
fn help_version_and_mcp_fail_on_a_standard_output_that_cannot_be_written() {
    let dir = scratch_dir("read_only_stdout_text");
    let sink = dir.join("sink");
    fs::write(&sink, b"").expect("write the read-only sink");
    for args in [&["--help"][..], &["--version"], &["compress", "--help"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_tokfold"))
            .args(args)
            .stdin(Stdio::null())
            .stdout(fs::File::open(&sink).expect("open the sink read-only"))
            .stderr(Stdio::piped())
            .output()
            .expect("run tokfold");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}, stderr: {stderr}");
        assert!(stderr.contains("writing to standard output"), "{stderr}");
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(fs::File::open(&sink).expect("open the sink read-only"))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tokfold mcp");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
        .expect("write child stdin");
    assert!(
        finished_within_ten_seconds(&mut child),
        "mcp hung on a read-only stdout"
    );
    let out = child.wait_with_output().expect("wait for tokfold mcp");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "mcp, stderr: {stderr}");
    assert!(stderr.contains("the stdio transport failed"), "{stderr}");
}

/// A standard input that is open but not readable is a read failure for every
/// subcommand, not an empty document.
///
/// `io::stdin()` treats `EBADF` as end of input, so in 0.0.1 `compress --archive` read
/// nothing, passed nothing through and exited `0`, leaving an earlier run's archive at
/// that path in place (the stale-archive clearing added since would have deleted it);
/// `expand` reported a bad magic and exited `3`; `stats` called the empty input
/// invalid JSON; and `mcp` ended its session at once with `0`. A descriptor opened write-only is the
/// portable way to get `EBADF` from a read.
#[cfg(unix)]
#[test]
fn a_standard_input_that_cannot_be_read_fails_the_command() {
    let dir = scratch_dir("write_only_stdin");
    let source = dir.join("source");
    fs::write(&source, b"").expect("write the write-only source");
    let archive = dir.join("recovery.tkfd");
    let input = dir.join("input.json");
    seed_real_archive(&input, &archive);
    let seeded = fs::read(&archive).expect("read the seeded archive");

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tokfold"))
            .args(args)
            .stdin(
                fs::OpenOptions::new()
                    .append(true)
                    .open(&source)
                    .expect("open the source write-only"),
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("run tokfold")
    };

    for args in [
        &["compress", "--archive", path_str(&archive)][..],
        &["expand"],
        &["stats"],
        &["mcp"],
    ] {
        let out = run(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}, stderr: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?} wrote to stdout");
        assert!(
            stderr.contains("Bad file descriptor") || stderr.contains("os error 9"),
            "{args:?} must report the read failure: {stderr}"
        );
    }
    assert_eq!(
        fs::read(&archive).expect("the archive survives"),
        seeded,
        "an unreadable stdin cost the caller a valid archive"
    );
}

/// `NotFound` is the one `File::open` failure the reject path is allowed to swallow,
/// and it swallows it into the quietest outcome the binary has: "no archive here",
/// the passthrough on stdout, exit `0`. Widening that guard to accept *any* open
/// failure reinstates the stale-archive hazard in its worst form — an archive that is
/// still sitting there, still verifying its own checksum, reported as absent because
/// it could not be opened to find out.
///
/// Two Unix failures pin the guard from opposite sides. A symlink that points at
/// itself fails with `ELOOP`, which no privilege level talks its way past, so that half
/// always runs. (A regular file standing where a directory component has to be is no
/// longer such a failure: `ENOTDIR` on the path as typed says no file is there, the
/// way the successful write finds it, and is an empty slot.) An archive whose mode bits deny the process fails
/// with `EACCES`, which is the hazard itself rather than a stand-in for it — but a
/// process running as root opens it anyway, so a canary establishes that the mode
/// bits bite before that half is relied on. Windows reports the first case as
/// `NotFound`, which is why this is `cfg(unix)`.
#[cfg(unix)]
#[test]
fn an_archive_path_that_cannot_be_opened_is_not_treated_as_an_absent_archive() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = scratch_dir("unopenable_archive");

    // (a) ELOOP — a symlink that resolves to itself.
    let behind = dir.join("loop.tkfd");
    std::os::unix::fs::symlink(&behind, &behind).expect("create the self-referencing link");

    let out = run_tokfold(&["compress", "--archive", path_str(&behind)], b"not json");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an archive path that cannot be inspected is an I/O failure, stderr: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "the passthrough was emitted without establishing what sits at --archive"
    );
    assert!(
        stderr.contains("inspecting the archive path") && stderr.contains(path_str(&behind)),
        "the failure must name the path it could not inspect: {stderr}"
    );
    assert!(
        !stderr.contains("no recovery archive written"),
        "a path that could not be opened was reported as an empty one: {stderr}"
    );
    assert!(
        !stderr.contains("passing input through"),
        "a run that emitted nothing announced a passthrough: {stderr}"
    );
    assert!(
        stderr.contains("the input was rejected (invalid JSON at byte 0")
            && stderr.contains("and not passed through"),
        "the failure must still say why the input was rejected: {stderr}"
    );

    // (b) EACCES on a genuine archive. Root ignores the mode bits, so prove they bite
    //     here before asserting on them.
    let canary = dir.join("canary");
    fs::write(&canary, b"canary").expect("write the permission canary");
    fs::set_permissions(&canary, fs::Permissions::from_mode(0o000))
        .expect("drop the canary's permissions");
    if fs::read(&canary).is_ok() {
        return; // running with privileges that ignore the mode bits
    }

    let good = dir.join("good.json");
    let archive = dir.join("real.tkfd");
    fs::write(&good, COMPRESSIBLE_JSON).expect("write the compressible input");
    let first = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&good),
        "--archive",
        path_str(&archive),
    ]);
    assert!(
        first.status.success() && archive.is_file(),
        "test precondition: run one must leave a recovery archive behind: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o000))
        .expect("drop the archive's permissions");

    let out = run_tokfold(&["compress", "--archive", path_str(&archive)], b"not json");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an archive that cannot be opened is an I/O failure, stderr: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "the passthrough was emitted while an archive that contradicts it survived"
    );
    assert!(
        stderr.contains("inspecting the archive path")
            && stderr.contains(path_str(&archive))
            && stderr.contains("and not passed through"),
        "the failure must name the archive it could not open: {stderr}"
    );
    assert!(
        archive.is_file(),
        "an archive that could not even be opened must not be reported as removed"
    );
}

/// The rejected pass is **not** contained by the successful one, and this pins the
/// first of the two ways it escapes.
///
/// The claim these tests exist to refute was once in the source: deleting a
/// magic-bearing file is safe because "a successful pass truncates that path anyway".
/// It is false, because the two paths do not use the same system calls. Success `open`s
/// the file for writing (`fs::write` in 0.0.1, create and truncate now), and
/// therefore needs write permission on *the file*; the reject path is
/// `fs::remove_file`, which needs write permission on *the directory* and does not care
/// about the file's own mode. So a read-only archive in a writable directory survives
/// every successful pass — loudly, at exit `2` — and is deleted by the first rejected
/// one, at exit `0`.
///
/// Nothing in this file asserted that split before: the closest tests vary the
/// archive's *bytes* or its *parent*, never its mode against the two outcomes. Both
/// halves therefore have to run in one test, on one file, or the asymmetry is not what
/// is being measured. Root ignores the mode bits, so a canary establishes that they
/// bite before either half is believed.
#[cfg(unix)]
#[test]
fn a_read_only_archive_survives_a_successful_pass_and_is_deleted_by_a_rejected_one() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = scratch_dir("archive_mode_asymmetry");

    let canary = dir.join("canary");
    fs::write(&canary, b"canary").expect("write the permission canary");
    fs::set_permissions(&canary, fs::Permissions::from_mode(0o444))
        .expect("make the canary read-only");
    if fs::write(&canary, b"overwritten").is_ok() {
        return; // running with privileges that ignore the mode bits
    }

    let input = dir.join("good.json");
    let archive = dir.join("read_only.tkfd");
    seed_real_archive(&input, &archive);
    let seeded = fs::read(&archive).expect("read the seeded archive");
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o444))
        .expect("make the archive read-only");

    // (a) The winning path cannot touch it, and says so.
    let out = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&input),
        "--archive",
        path_str(&archive),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a successful compression that cannot write its archive is an I/O failure, \
         stderr: {stderr}"
    );
    assert!(
        stderr.contains("writing archive to") && stderr.contains(path_str(&archive)),
        "the failure must name the archive it could not write: {stderr}"
    );
    assert_eq!(
        fs::read(&archive).expect("re-read the archive after the successful pass"),
        seeded,
        "the successful pass must have left the read-only archive byte-for-byte intact"
    );

    // (b) The rejected path deletes the very same file, at exit 0.
    let out = run_tokfold(&["compress", "--archive", path_str(&archive)], b"not json");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a rejected input is still a clean passthrough: {stderr}"
    );
    assert!(
        !archive.exists(),
        "the asymmetry this test pins has been closed, or has moved: a read-only \
         archive that a successful pass could not replace was also spared by the \
         rejected one. If that is deliberate, the permissions asymmetry described \
         on `clear_stale_archive` and in the crate README is now wrong"
    );
    assert!(
        stderr.contains("it opens with the tokfold archive magic"),
        "a deletion this wide must at least be announced: {stderr}"
    );
}

/// The two paths used not to agree on which file a spelling names.
///
/// Success `stat`s the path exactly as typed, and a trailing separator after a file is
/// an `ENOTDIR` there. The reject path used to resolve it with `fs::canonicalize` first,
/// and a `realpath` that normalises the separator away handed the removal the file
/// behind it: `--archive a.tkfd/` was refused by a successful pass and deleted by a
/// rejected one. It now asks the typed path the way success does, so both leave it.
#[cfg(unix)]
#[test]
fn a_trailing_separator_after_the_archive_is_left_alone_by_both_paths() {
    let dir = scratch_dir("archive_spelling_asymmetry");

    let input = dir.join("good.json");
    let archive = dir.join("slot.tkfd");
    seed_real_archive(&input, &archive);
    let seeded = fs::read(&archive).expect("read the seeded archive");

    let mut spelled = archive.clone().into_os_string();
    spelled.push("/");
    let spelled = PathBuf::from(spelled);

    // (a) The winning path refuses the spelling outright.
    let out = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&input),
        "--archive",
        path_str(&spelled),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a spelling the winning path cannot stat is an I/O failure, stderr: {stderr}"
    );
    assert_eq!(
        fs::read(&archive).expect("re-read the archive after the successful pass"),
        seeded,
        "the successful pass must have left the archive it refused to name intact"
    );

    // (b) The rejected path finds no archive at that spelling either.
    let out = run_tokfold(&["compress", "--archive", path_str(&spelled)], b"not json");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a rejected input is still a clean passthrough: {stderr}"
    );
    assert_eq!(
        fs::read(&archive).expect("the rejected pass must not have deleted the archive"),
        seeded,
        "a spelling the successful pass cannot reach must not be deleted: {stderr}"
    );
    assert!(
        !stderr.contains("it opens with the tokfold archive magic"),
        "nothing was removed, so nothing may be announced as removed: {stderr}"
    );
}

/// The deletion the trailing separator reached was not only an archive's: every
/// same-file guard asks `stat`, which refuses `x/`, so none of them saw that
/// `--archive x/` named the input, and a rejected pass — an archive is not JSON —
/// resolved the spelling to `x` and deleted it at exit `0`. `0.0.1` never deleted
/// anything. Each way the input can arrive is driven: `--input`, `<` and, with the
/// input elsewhere, `>>` onto the file named.
#[cfg(unix)]
#[test]
fn an_input_named_as_its_own_archive_with_a_trailing_separator_is_not_deleted() {
    let dir = scratch_dir("trailing_separator_input");
    let json = dir.join("good.json");
    let x = dir.join("x");
    seed_real_archive(&json, &x);
    let seeded = fs::read(&x).expect("read the seeded archive");
    let not_json = dir.join("n.txt");
    fs::write(&not_json, b"not json").expect("write the rejected input");
    let spelled = format!("{}/", x.display());

    let input_args = [
        "compress",
        "--input",
        path_str(&x),
        "--archive",
        spelled.as_str(),
    ];
    let stdin_args = ["compress", "--archive", spelled.as_str()];
    let append_args = [
        "compress",
        "--input",
        path_str(&not_json),
        "--archive",
        spelled.as_str(),
    ];
    let cases: [(&str, &[&str]); 3] = [
        ("--input", &input_args),
        ("stdin", &stdin_args),
        ("append", &append_args),
    ];
    for (how, args) in cases {
        fs::write(&x, &seeded).expect("restore the archive");
        let stdin = if how == "stdin" {
            Stdio::from(fs::File::open(&x).expect("open the archive as stdin"))
        } else {
            Stdio::null()
        };
        let stdout = if how == "append" {
            append_to(&x)
        } else {
            fs::File::create(dir.join("out")).expect("create the output sink")
        };
        let out = run_tokfold_on(args, stdin, stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{how}: a rejected input is a clean passthrough, as in 0.0.1: {stderr}"
        );
        let after = fs::read(&x).expect("the input must still exist");
        assert!(
            after.starts_with(&seeded),
            "{how}: the file named with a trailing separator must survive: {stderr}"
        );
        assert!(
            !stderr.contains("it opens with the tokfold archive magic"),
            "{how}: {stderr}"
        );
    }
}

/// The clearing has to survive the removal *failing*, and until this test existed
/// nothing reached that branch: every test that aimed at it put a directory or an
/// unreadable file at the archive path, and both of those fail earlier, during the
/// inspection. Replacing the whole `remove_file(..)?` with `let _ = remove_file(..)`
/// therefore left the suite green while the binary reported "removed a stale
/// recovery archive" over an archive that was still sitting there — the silent
/// substitution this feature exists to prevent, with a reassuring message on top.
///
/// A read-only *parent* is what separates the two: the archive still opens and still
/// reads as an archive, and only the unlink is refused.
#[test]
#[cfg(unix)]
fn a_stale_archive_whose_removal_fails_is_named_and_left_intact() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = scratch_dir("unremovable_stale_archive");
    let readonly = dir.join("ro");
    fs::create_dir(&readonly).expect("create the read-only directory");
    let archive = readonly.join("recovery.tkfd");
    seed_real_archive(&dir.join("good.json"), &archive);

    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o555))
        .expect("drop the directory's write permission");
    if fs::write(readonly.join("canary"), b"canary").is_ok() {
        // Running with privileges that ignore the mode bits; the branch is
        // unreachable here, so asserting on it would assert on the platform.
        fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755))
            .expect("restore the directory's permissions");
        return;
    }

    let out = run_tokfold(&["compress", "--archive", path_str(&archive)], b"not json");
    // Restore before asserting, so a failing assertion cannot leave a directory
    // behind that the next run's `scratch_dir` is unable to wipe.
    let survived = archive.is_file();
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755))
        .expect("restore the directory's permissions");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an archive that cannot be removed is an I/O failure, stderr: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "the passthrough was emitted while the archive that contradicts it survived"
    );
    assert!(
        stderr.contains("removing stale archive") && stderr.contains(path_str(&archive)),
        "the failure must name the archive it could not remove, and must reach the \
         removal rather than stopping at the inspection: {stderr}"
    );
    assert!(
        !stderr.contains("it opens with the tokfold archive magic"),
        "a removal that failed was reported as having succeeded: {stderr}"
    );
    assert!(
        survived,
        "test precondition: the read-only parent must have kept the archive"
    );
}

/// The symlink test was once named as if it also covered a relative spelling. It
/// promised a relative spelling and never tested one: every path a test builds comes
/// from `scratch_dir`, which is absolute. A guard rewritten to `if a.is_relative() ||
/// b.is_relative() { return a == b; }` passed the whole suite while
/// `--input ./data.json --archive data.json` destroyed the input.
#[test]
fn the_same_file_guard_sees_a_relative_spelling_of_one_file() {
    let dir = scratch_dir("relative_same_file");
    let doc = dir.join("data.json");
    fs::write(&doc, COMPRESSIBLE_JSON).expect("write the input");

    let out = run_tokfold_in(
        &dir,
        &[
            "compress",
            "--input",
            "./data.json",
            "--archive",
            "data.json",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "two spellings of one file must be refused: {stderr}"
    );
    // The whole line, so the clause after the semicolon is pinned too: it once
    // predicted that "a successful pass would overwrite the input with its own
    // archive", which a read-only input or a macOS `/dev/fd` spelling of a non-empty
    // one refutes. It may state only the rule that was checked.
    assert_eq!(
        stderr,
        "tokfold: --archive data.json and --input ./data.json name the same file; \
         the archive may not be aimed at its own input\n",
        "the refusal must say why: {stderr}"
    );
    assert_eq!(
        fs::read(&doc).expect("read the input back"),
        COMPRESSIBLE_JSON,
        "the input was destroyed by an archive aimed at it under another spelling"
    );
}

/// Two hard links are one file under two names, and no amount of path resolution
/// separates them: `canonicalize` answers with the spelling it was handed, so a
/// guard built on paths lets `--input real.json --archive alias.json` overwrite the
/// input it was written to protect. Only the filesystem's own identity sees it.
#[test]
#[cfg(unix)]
fn the_same_file_guard_sees_a_hard_link_to_the_input() {
    let dir = scratch_dir("hard_link_same_file");
    let real = dir.join("real.json");
    let alias = dir.join("alias.json");
    fs::write(&real, COMPRESSIBLE_JSON).expect("write the input");
    fs::hard_link(&real, &alias).expect("hard-link the input under a second name");

    let out = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&real),
        "--archive",
        path_str(&alias),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a hard link to the input is the input: {stderr}"
    );
    assert_eq!(
        fs::read(&real).expect("read the input back"),
        COMPRESSIBLE_JSON,
        "the input was overwritten through a hard link the guard could not see"
    );
}

/// Run the built `tokfold` binary with `args`, its standard input redirected from the
/// file at `stdin_file` — the shell's `< file`, which hands the child a descriptor on
/// the file itself rather than a pipe carrying its bytes.
fn run_tokfold_reading_stdin_from(args: &[&str], stdin_file: &Path) -> Output {
    let file = fs::File::open(stdin_file).expect("open the file to redirect as stdin");
    Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .args(args)
        .stdin(Stdio::from(file))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run tokfold")
}

/// `tokfold compress --archive log.json < log.json` has no `--input` path to compare
/// with, but the shell's redirection gives the binary a descriptor on `log.json`
/// itself, and a descriptor has the same `(dev, ino)` identity a path resolves to.
/// The guard asks it — through `AsFd` and an owned duplicate, without `unsafe` — so
/// the redirect is refused like `--input log.json` is, and through a hard link too.
/// A *different* regular file on stdin must sail through, or the guard would have
/// traded one destructive shape for refusing every redirect.
#[test]
#[cfg(unix)]
fn the_same_file_guard_sees_the_input_redirected_onto_standard_input() {
    let dir = scratch_dir("stdin_redirect_same_file");
    let doc = dir.join("log.json");
    let alias = dir.join("alias.json");
    let other = dir.join("other.json");
    fs::write(&doc, COMPRESSIBLE_JSON).expect("write the input");
    fs::hard_link(&doc, &alias).expect("hard-link the input under a second name");
    fs::write(&other, COMPRESSIBLE_JSON).expect("write a second, distinct file");

    for archive in [&doc, &alias] {
        let out =
            run_tokfold_reading_stdin_from(&["compress", "--archive", path_str(archive)], &doc);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{} is the file on stdin and must be refused: {stderr}",
            archive.display()
        );
        assert_eq!(
            stderr,
            format!(
                "tokfold: --archive {} names the file on standard input; \
                 the archive may not be aimed at its own input\n",
                archive.display()
            ),
            "the refusal must say what it saw: {stderr}"
        );
        assert!(
            out.stdout.is_empty(),
            "a refused pass must emit nothing on stdout"
        );
        assert_eq!(
            fs::read(&doc).expect("read the input back"),
            COMPRESSIBLE_JSON,
            "the input on stdin was overwritten by its own archive"
        );
    }

    let out = run_tokfold_reading_stdin_from(&["compress", "--archive", path_str(&other)], &doc);
    assert!(
        out.status.success(),
        "a different regular file on stdin is not the archive: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.starts_with(SENTINEL_PREFIX),
        "the pass must still compress"
    );
    assert!(
        fs::read(&other)
            .expect("read the archive back")
            .starts_with(b"TKFD"),
        "the archive must have been written to the other file"
    );
}

/// `tokfold compress --archive a.tkfd >> a.tkfd` hands the binary a standard output
/// on the archive path itself. A rejected pass then deleted the file its own
/// passthrough was being appended to — the stale archive and the payload both gone,
/// with exit `0` and stderr reporting a passthrough — and a successful one wrote the
/// archive and the rendering into one file. Both outcomes, and both an appending
/// descriptor (`>>`) and one that neither appends nor truncates, are refused before anything is read or written; a different regular file on stdout
/// is not the archive and must sail through.
#[test]
#[cfg(unix)]
fn the_same_file_guard_sees_standard_output_redirected_into_the_archive() {
    let dir = scratch_dir("stdout_redirect_same_file");
    let good = dir.join("good.json");
    let bad = dir.join("bad.txt");
    let archive = dir.join("a.tkfd");
    let other = dir.join("out.txt");
    seed_real_archive(&good, &archive);
    fs::write(&bad, b"not json").expect("write a rejected input");
    let seeded = fs::read(&archive).expect("read the seeded archive");

    for input in [&good, &bad] {
        for append in [true, false] {
            let stdout = fs::OpenOptions::new()
                .append(append)
                .write(true)
                .open(&archive)
                .expect("open the archive as stdout");
            let out = Command::new(env!("CARGO_BIN_EXE_tokfold"))
                .args([
                    "compress",
                    "--input",
                    path_str(input),
                    "--archive",
                    path_str(&archive),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::piped())
                .output()
                .expect("run tokfold");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                out.status.code(),
                Some(2),
                "{} with stdout on the archive must be refused: {stderr}",
                input.display()
            );
            assert_eq!(
                stderr,
                format!(
                    "tokfold: --archive {} names the file standard output is \
                     redirected to; the archive may not be aimed at the output\n",
                    archive.display()
                ),
                "the refusal must say what it saw: {stderr}"
            );
            assert_eq!(
                fs::read(&archive).expect("read the archive back"),
                seeded,
                "a refused pass must leave the archive byte-for-byte as it was"
            );
        }
    }

    let stdout = fs::File::create(&other).expect("create a distinct stdout file");
    let out = Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .args([
            "compress",
            "--input",
            path_str(&good),
            "--archive",
            path_str(&archive),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::piped())
        .output()
        .expect("run tokfold");
    assert!(
        out.status.success(),
        "a different regular file on stdout is not the archive: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs::read(&other)
            .expect("read the output back")
            .starts_with(SENTINEL_PREFIX),
        "the rendering must have reached the other file"
    );
}

/// Run the binary with `stdin` and `stdout` bound to the given files, capturing stderr.
///
/// The run is killed after ten seconds and the test fails: with the output guard
/// removed, `mcp` answering into its own input is a feedback loop that never ends
/// and grows the file without bound, so an unbounded wait would hang the suite and
/// fill the disk instead of failing.
#[cfg(unix)]
fn run_tokfold_on(args: &[&str], stdin: Stdio, stdout: fs::File) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .args(args)
        .stdin(stdin)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::piped())
        .spawn()
        .expect("run tokfold");
    let finished = finished_within_ten_seconds(&mut child);
    assert!(finished, "{args:?} was still running after ten seconds");
    child.wait_with_output().expect("collect tokfold's stderr")
}

/// Open `path` for appending, as the shell's `>>` does.
#[cfg(unix)]
fn append_to(path: &Path) -> fs::File {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open the file for appending")
}

/// Every command writes to standard output, so a standard output appended to the
/// file the command reads — through `--input` or through standard input — writes the
/// output into its own input. Each of `compress`, `expand`, `stats` and `mcp` must
/// refuse that with exit `2` before it reads or writes anything, leaving the file
/// byte-for-byte as it was. Under `mcp` the unrefused run was a feedback loop: the
/// server read its own replies back as requests and answered each of them, and the
/// file grew until the process was killed.
#[test]
#[cfg(unix)]
fn standard_output_redirected_into_the_input_is_refused() {
    let dir = scratch_dir("stdout_into_input");
    let json = dir.join("in.json");
    let archive = dir.join("a.tkfd");
    let requests = dir.join("requests.jsonl");
    seed_real_archive(&json, &archive);
    fs::write(
        &requests,
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n",
    )
    .expect("write the requests");

    let cases: [(&[&str], &Path); 7] = [
        (&["compress", "--input", path_str(&json)], &json),
        (&["compress"], &json),
        (&["expand", "--input", path_str(&archive)], &archive),
        (&["expand"], &archive),
        (&["stats", "--input", path_str(&json)], &json),
        (&["stats"], &json),
        (&["mcp"], &requests),
    ];
    for (args, file) in cases {
        let before = fs::read(file).expect("read the input");
        let stdin = if args.len() == 1 {
            Stdio::from(fs::File::open(file).expect("open the input as stdin"))
        } else {
            Stdio::null()
        };
        let out = run_tokfold_on(args, stdin, append_to(file));
        let stderr = String::from_utf8_lossy(&out.stderr);
        let named = if args.len() == 1 {
            "standard input".to_owned()
        } else {
            format!("--input {}", file.display())
        };
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} must be refused: {stderr}"
        );
        // The whole line: the refusal once said "the output would be written into
        // the input", which is false for an `expand` of a corrupt archive or a
        // `stats` of non-JSON — neither writes a byte. It states the rule only.
        assert_eq!(
            stderr,
            format!(
                "tokfold: {named} is the file standard output is redirected to; \
                 output may not be written into the input it is read from\n"
            ),
            "{args:?}: the refusal must name the input it saw: {stderr}"
        );
        assert_eq!(
            fs::read(file).expect("read the input back"),
            before,
            "{args:?}: a refused run must leave its input as it was"
        );
    }
}

/// `>` empties its file before tokfold starts, so a `compress --archive` whose
/// standard output truncated its own input read an empty document, rejected it, and
/// deleted the archive at `--archive` as stale — in a command line where that archive
/// was the last copy of the document. The refusal must come before the clearing.
#[test]
#[cfg(unix)]
fn an_input_emptied_by_its_own_redirect_does_not_cost_the_archive() {
    let dir = scratch_dir("stdout_truncates_input");
    let json = dir.join("in.json");
    let archive = dir.join("a.tkfd");
    seed_real_archive(&json, &archive);
    let seeded = fs::read(&archive).expect("read the seeded archive");
    let truncated = fs::File::create(&json).expect("truncate the input, as `>` does");

    let out = run_tokfold_on(
        &[
            "compress",
            "--input",
            path_str(&json),
            "--archive",
            path_str(&archive),
        ],
        Stdio::null(),
        truncated,
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "must be refused: {stderr}");
    assert!(
        !stderr.contains("it opens with the tokfold archive magic"),
        "the archive must not be cleared: {stderr}"
    );
    assert_eq!(
        fs::read(&archive).expect("the archive must survive"),
        seeded,
        "the archive is the only copy of the document left"
    );
}

/// macOS reports `/dev/stdin`, `/dev/stdout` and `/dev/fd/N` to `stat` with the
/// inode of the file behind the descriptor but the device of its `fdesc` filesystem,
/// so every same-file guard compared an identity that could never match and let
/// these spellings through: `--archive /dev/stdout >> a.tkfd` wrote the archive and
/// the rendering into one file, and `--input /dev/stdin --archive log.json <
/// log.json` overwrote the input with its own archive. Linux resolves the same paths
/// through `/proc/self/fd` to the file itself; both must be refused.
#[test]
#[cfg(unix)]
fn the_same_file_guards_see_through_dev_spellings_of_a_descriptor() {
    let dir = scratch_dir("dev_fd_spellings");
    let json = dir.join("in.json");
    let archive = dir.join("a.tkfd");
    seed_real_archive(&json, &archive);
    let seeded = fs::read(&archive).expect("read the seeded archive");
    let document = fs::read(&json).expect("read the input");

    for spelling in ["/dev/stdout", "/dev/fd/1"] {
        let out = run_tokfold_on(
            &[
                "compress",
                "--input",
                path_str(&json),
                "--archive",
                spelling,
            ],
            Stdio::null(),
            append_to(&archive),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{spelling}: {stderr}");
        // The archive here is non-empty, so on macOS a successful pass would refuse
        // the spelling ("opening it did not empty it") rather than write into it: the
        // message may not predict that outcome, only state the rule.
        assert_eq!(
            stderr,
            format!(
                "tokfold: --archive {spelling} names the file standard output is \
                 redirected to; the archive may not be aimed at the output\n"
            ),
            "{spelling}: {stderr}"
        );
        assert_eq!(fs::read(&archive).expect("read the archive"), seeded);
    }

    for spelling in ["/dev/stdin", "/dev/fd/0"] {
        let out = run_tokfold_on(
            &[
                "compress",
                "--input",
                spelling,
                "--archive",
                path_str(&json),
            ],
            Stdio::from(fs::File::open(&json).expect("open the input as stdin")),
            fs::File::create(dir.join("out.txt")).expect("create a distinct stdout"),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{spelling}: {stderr}");
        assert_eq!(
            stderr,
            format!(
                "tokfold: --archive {} and --input {spelling} name the same file; \
                 the archive may not be aimed at its own input\n",
                json.display()
            ),
            "{spelling}: {stderr}"
        );
        assert_eq!(fs::read(&json).expect("read the input"), document);
    }
}

/// An archive path that reopens a descriptor rather than a file — `/dev/stderr`
/// here — is truncated on Linux, where the path reaches the file itself, and is not
/// on macOS, where the open duplicates the descriptor and keeps its offset and append
/// mode, so the archive landed after the bytes already there and the command exited
/// `0` with a file `expand` rejects. Either outcome is acceptable except that one: a
/// clean exit must leave exactly the archive, and a refusal must leave the file as
/// it was.
#[test]
#[cfg(unix)]
fn an_archive_path_that_does_not_empty_on_open_is_refused() {
    let dir = scratch_dir("archive_not_emptied");
    let json = dir.join("in.json");
    let reference = dir.join("reference.tkfd");
    let target = dir.join("stderr.log");
    seed_real_archive(&json, &reference);
    fs::write(&target, b"earlier log line\n").expect("seed the stderr file");

    let out = Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .args([
            "compress",
            "--input",
            path_str(&json),
            "--archive",
            "/dev/stderr",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(append_to(&target)))
        .output()
        .expect("run tokfold");
    let written = fs::read(&target).expect("read the target back");
    match out.status.code() {
        Some(0) => assert_eq!(
            written,
            fs::read(&reference).expect("read the reference archive"),
            "a clean exit must leave exactly the archive"
        ),
        Some(2) => {
            let text = String::from_utf8_lossy(&written);
            assert!(
                text.starts_with("earlier log line\n")
                    && text.contains(
                        "tokfold: refusing to write the archive to /dev/stderr: \
                         opening it did not empty it"
                    )
                    && !written.windows(4).any(|w| w == b"TKFD"),
                "a refusal must write no archive bytes: {text}"
            );
        }
        other => panic!(
            "unexpected exit {other:?}: {}",
            String::from_utf8_lossy(&written)
        ),
    }
}

/// A symlink at `--archive` is the one shape where inspecting and removing can
/// disagree about which file they mean: `File::open` follows the link and reads the
/// archive behind it, while `fs::remove_file` does not follow and unlinks the link.
/// Acting on the typed path therefore deleted the *link*, left the stale archive
/// fully readable, and printed "removed a stale recovery archive" over the top of it.
///
/// The success path opens the path for writing with create and truncate, which
/// follows the link and rewrites the target, so the target is what a rejected pass has to clear.
#[test]
#[cfg(unix)]
fn a_symlink_at_the_archive_path_clears_the_archive_behind_it() {
    let dir = scratch_dir("symlinked_archive");
    let real = dir.join("real.tkfd");
    let link = dir.join("link.tkfd");
    seed_real_archive(&dir.join("good.json"), &real);
    std::os::unix::fs::symlink(&real, &link).expect("link to the archive");
    let resolved = fs::canonicalize(&real).expect("resolve the archive");

    let out = run_tokfold(&["compress", "--archive", path_str(&link)], b"not json");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a rejected input is still a clean passthrough: {stderr}"
    );
    assert_eq!(out.stdout, b"not json");
    // The link itself survives the removal, dangling, so the message must name the
    // file that went, not the link that is still there.
    let removed = format!(
        "tokfold: removed {}, the file the symlink {} resolves to: it opens with the \
         tokfold archive magic\n",
        resolved.display(),
        link.display()
    );
    assert!(
        stderr.starts_with(&removed),
        "the archive behind the link was cleared, so name it: {stderr}"
    );
    assert!(
        fs::symlink_metadata(&link).is_ok(),
        "the link itself is not what was removed"
    );
    assert!(
        !real.exists(),
        "the archive behind the link survived, so `expand` still returns the stale \
         document the passthrough contradicts"
    );
}

/// Through a chain `l1 -> l2 -> real.tkfd` the typed link points at `l2`, which
/// survives the removal along with `l1`; the file that went is `real.tkfd`. The
/// warning once said "the file the symlink l1 points at", naming a relation that is
/// false for every chain longer than one link.
#[test]
#[cfg(unix)]
fn a_symlink_chain_at_the_archive_path_names_the_file_it_resolves_to() {
    let dir = scratch_dir("symlink_chain_archive");
    let real = dir.join("real.tkfd");
    let middle = dir.join("l2.tkfd");
    let link = dir.join("l1.tkfd");
    seed_real_archive(&dir.join("good.json"), &real);
    std::os::unix::fs::symlink(&real, &middle).expect("link to the archive");
    std::os::unix::fs::symlink(&middle, &link).expect("link to the link");
    let resolved = fs::canonicalize(&real).expect("resolve the archive");

    let out = run_tokfold(&["compress", "--archive", path_str(&link)], b"not json");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.starts_with(&format!(
            "tokfold: removed {}, the file the symlink {} resolves to: it opens with \
             the tokfold archive magic\n",
            resolved.display(),
            link.display()
        )),
        "{stderr}"
    );
    assert!(
        !real.exists(),
        "the archive at the end of the chain survived"
    );
    assert!(
        fs::symlink_metadata(&middle).is_ok() && fs::symlink_metadata(&link).is_ok(),
        "neither link is what was removed"
    );
}

/// A read-only input named twice is refused like a writable one, although a
/// successful pass could not have overwritten it (`0.0.1` stopped at "Permission
/// denied") and a rejected one would not have deleted it (`0.0.1` passed it through,
/// exit `0`). The guard runs before either outcome is known, so its message may not
/// predict one: it states the rule it checked.
#[test]
#[cfg(unix)]
fn a_read_only_input_named_as_its_own_archive_is_refused_without_a_prediction() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("read_only_same_file");
    let doc = dir.join("ro.json");
    fs::write(&doc, COMPRESSIBLE_JSON).expect("write the input");
    fs::set_permissions(&doc, fs::Permissions::from_mode(0o444)).expect("chmod 444");

    let out = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&doc),
        "--archive",
        path_str(&doc),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(out.stdout.is_empty(), "a refused pass emits nothing");
    assert_eq!(
        stderr,
        format!(
            "tokfold: --archive {0} and --input {0} name the same file; \
             the archive may not be aimed at its own input\n",
            doc.display()
        )
    );
    assert_eq!(
        fs::read(&doc).expect("read the input back"),
        COMPRESSIBLE_JSON
    );
}

/// Opening a FIFO blocks until the other end appears — a writer for the read, a reader
/// for the write — so an archive path that happens to name one turns `--archive` into an unbounded hang. A FIFO also holds no bytes a
/// later `expand` could read back, so neither path may open it: the rejected input must
/// leave it alone and still pass through cleanly, and the *winning* pass must refuse
/// before it opens anything rather than stall with the archive unwritten and the
/// rendering unemitted.
///
/// Only the rejected leg was ever driven, and the winning one hung: the fix that taught
/// the inspection not to open a FIFO covered the path that inspects, not the path that
/// writes.
#[test]
#[cfg(unix)]
fn a_fifo_at_the_archive_path_neither_hangs_nor_is_removed() {
    let dir = scratch_dir("fifo_archive");
    let rejected = dir.join("rejected.tkfd");
    let winning = dir.join("winning.tkfd");
    for fifo in [&rejected, &winning] {
        let made = Command::new("mkfifo")
            .arg(path_str(fifo))
            .status()
            .is_ok_and(|status| status.success());
        if !made {
            return; // no `mkfifo` on this box; nothing to assert about
        }
    }

    let drive = |archive: &Path, stdin_bytes: &[u8]| -> (Output, bool) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tokfold"))
            .args(["compress", "--archive", path_str(archive)])
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
        let finished = finished_within_ten_seconds(&mut child);
        let out = child
            .wait_with_output()
            .expect("collect the child's output");
        (out, finished)
    };

    let (out, finished) = drive(&rejected, b"not json");
    assert!(
        finished,
        "a FIFO at --archive hung the passthrough; it was killed after 10s"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a rejected input is still a clean passthrough: {stderr}"
    );
    assert_eq!(out.stdout, b"not json");
    assert!(
        fs::symlink_metadata(&rejected).is_ok(),
        "a FIFO cannot hold a stale archive, so it must be left where it was"
    );

    let (out, finished) = drive(&winning, COMPRESSIBLE_JSON);
    assert!(
        finished,
        "a FIFO at --archive hung the winning pass; it was killed after 10s"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an archive that cannot be written is an I/O failure, exit 2: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "the rendering must not be emitted when the archive it recovers was never \
         written: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("not a regular file"),
        "the refusal must say why the path was rejected: {stderr}"
    );
    assert!(
        fs::symlink_metadata(&winning).is_ok(),
        "the refused FIFO must be left where it was"
    );
}

/// `--archive X --input X` would have the recovery archive overwrite the document it
/// exists to recover. It has to be refused, and refused *before* anything is read or
/// written, so the input is still byte-identical afterwards.
#[test]
fn compress_refuses_an_archive_that_would_overwrite_its_own_input() {
    let dir = scratch_dir("archive_is_input");
    let doc = dir.join("data.json");
    fs::write(&doc, COMPRESSIBLE_JSON).expect("write the input");

    let out = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&doc),
        "--archive",
        path_str(&doc),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a self-destructive invocation is a usage error, stderr: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "nothing may be emitted for a refused invocation"
    );
    assert_eq!(
        stderr,
        format!(
            "tokfold: --archive {0} and --input {0} name the same file; \
             the archive may not be aimed at its own input\n",
            doc.display()
        ),
        "the refusal has to say what is wrong: {stderr}"
    );
    assert_eq!(
        fs::read(&doc).expect("the input file must still be readable"),
        COMPRESSIBLE_JSON,
        "the input file was modified by an invocation that was supposed to be refused"
    );
}

/// The guard's sharpest edge. Once a rejected pass deletes the archive path, the
/// same-file case stops being a mere overwrite and becomes an outright `rm` of the
/// user's document — the input is rejected, so there is no rendering to recover it
/// from either.
#[test]
fn the_guard_stops_a_rejected_pass_from_deleting_its_own_input() {
    let dir = scratch_dir("archive_is_rejected_input");
    let doc = dir.join("notes.txt");
    fs::write(&doc, b"this is not json at all").expect("write the input");

    let out = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&doc),
        "--archive",
        path_str(&doc),
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        doc.is_file(),
        "the user's input file was deleted by the stale-archive removal"
    );
    assert_eq!(
        fs::read(&doc).expect("read the input back"),
        b"this is not json at all",
        "the input file must be untouched"
    );
}

/// A literal string comparison would call `alias.json` and `data.json` two different
/// files and let the destructive path through. Canonicalization is what closes that,
/// and only a symlink test can tell the two implementations apart.
#[cfg(unix)]
#[test]
fn the_same_file_guard_sees_through_a_symlink() {
    let dir = scratch_dir("archive_is_input_symlink");
    let doc = dir.join("data.json");
    let alias = dir.join("alias.json");
    fs::write(&doc, COMPRESSIBLE_JSON).expect("write the input");
    std::os::unix::fs::symlink(&doc, &alias).expect("create the symlink");

    let out = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&alias),
        "--archive",
        path_str(&doc),
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a symlinked spelling of the input must be refused too, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read(&doc).expect("the input file must still be readable"),
        COMPRESSIBLE_JSON,
        "the input file was overwritten through its symlink"
    );

    // Two genuinely different files must still be allowed: a guard that refused
    // everything would pass every assertion above and break the tool.
    let archive = dir.join("recovery.tkfd");
    let allowed = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&doc),
        "--archive",
        path_str(&archive),
    ]);
    assert!(
        allowed.status.success() && archive.is_file(),
        "distinct paths must still compress: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
}

// ---------------------------------------------------------------------------
// Diagnostics: every failure names what it was looking at.
// ---------------------------------------------------------------------------

/// A directory handed to `--input` reaches `fs::read` as a platform-specific errno —
/// "Is a directory" on Unix, but "Access is denied" on Windows, which sends the
/// reader off checking permissions on a path whose real problem is its type.
#[test]
fn an_input_that_is_a_directory_is_reported_as_a_directory() {
    let dir = scratch_dir("input_is_a_directory");

    let out = run_tokfold_without_stdin(&["compress", "--input", path_str(&dir)]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a directory is bad input, stderr: {stderr}"
    );
    assert!(
        stderr.contains("is a directory, not a file") && stderr.contains(path_str(&dir)),
        "the message must name both the mistake and the path: {stderr}"
    );
}

/// Zero bytes really do fail the magic check, but reporting that as "bad magic" sends
/// the reader hunting for corruption inside a file that has no content at all. The
/// exit code stays `3` — an empty archive is still unrecoverable.
#[test]
fn an_empty_archive_is_refused_with_exit_3_and_says_it_is_empty() {
    let dir = scratch_dir("empty_archive");
    let archive = dir.join("recovery.tkfd");
    fs::write(&archive, b"").expect("create the empty archive");

    let out = run_tokfold_without_stdin(&["expand", "--input", path_str(&archive)]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(3),
        "an unrecoverable archive is exit 3, stderr: {stderr}"
    );
    assert!(
        out.stdout.is_empty(),
        "an empty archive must not produce output"
    );
    // Not merely `contains("empty")`: the scratch path itself carries that word, so a
    // looser assertion passes even when the special case is gone and the message has
    // reverted to the generic magic failure.
    assert!(
        stderr.contains("the archive is empty (0 bytes)") && stderr.contains(path_str(&archive)),
        "the message must say it is empty and name it: {stderr}"
    );
    assert!(
        !stderr.contains("bad magic"),
        "zero bytes must not be reported as corruption inside a file that has none: \
         {stderr}"
    );

    // The same distinction over the pathless source, which has no filename to lean on.
    let piped = run_tokfold(&["expand"], b"");
    let piped_stderr = String::from_utf8_lossy(&piped.stderr);
    assert_eq!(
        piped.status.code(),
        Some(3),
        "an empty archive on standard input is exit 3 too, stderr: {piped_stderr}"
    );
    assert!(
        piped_stderr.contains("the archive is empty (0 bytes)")
            && piped_stderr.contains("standard input")
            && !piped_stderr.contains("bad magic"),
        "the pathless source needs the same diagnostic: {piped_stderr}"
    );
}

/// A script that expands archives in a loop learns nothing from "cannot expand
/// archive"; it needs to know which one. Bytes piped in have no path, so that source
/// has to be named in words instead.
#[test]
fn expand_names_the_source_of_an_archive_it_cannot_read() {
    let dir = scratch_dir("expand_names_source");
    let archive = dir.join("recovery.tkfd");
    fs::write(&archive, b"definitely not a tkfd archive").expect("write the junk archive");

    let from_file = run_tokfold_without_stdin(&["expand", "--input", path_str(&archive)]);
    let stderr = String::from_utf8_lossy(&from_file.stderr);
    assert_eq!(from_file.status.code(), Some(3), "stderr: {stderr}");
    assert!(
        stderr.contains(path_str(&archive)),
        "the failure must name the archive it read: {stderr}"
    );

    let piped = run_tokfold(&["expand"], b"definitely not a tkfd archive");
    let piped_stderr = String::from_utf8_lossy(&piped.stderr);
    assert_eq!(piped.status.code(), Some(3), "stderr: {piped_stderr}");
    assert!(
        piped_stderr.contains("standard input"),
        "an archive with no path still has a source worth naming: {piped_stderr}"
    );
}

// ---------------------------------------------------------------------------
// Usage surface: the exit code for a malformed command line is normative too.
// ---------------------------------------------------------------------------

/// Exit `2` for a usage error is part of the published contract, but it was inherited
/// from clap's default and nothing pinned it — a clap upgrade or a `#[command(...)]`
/// tweak could move it without a single test noticing. Each case also has to name the
/// token the user actually got wrong.
#[test]
fn a_malformed_command_line_exits_2_and_names_what_is_wrong() {
    for (args, needle) in [
        (vec!["compress", "--bogus"], "--bogus"),
        (vec!["compress", "--archive"], "--archive"),
        (vec!["stats", "--profile", "turbo"], "turbo"),
        (vec!["frobnicate"], "frobnicate"),
    ] {
        let out = run_tokfold_without_stdin(&args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(2),
            "usage error {args:?} must exit 2, stderr: {stderr}"
        );
        assert!(
            stderr.contains(needle),
            "the message for {args:?} must name {needle:?}: {stderr}"
        );
    }

    // An unusable `--profile` value must list the ones that exist; "invalid value" on
    // its own leaves the user guessing.
    let profile = run_tokfold_without_stdin(&["stats", "--profile", "turbo"]);
    let stderr = String::from_utf8_lossy(&profile.stderr);
    for value in ["conservative", "balanced", "aggressive"] {
        assert!(
            stderr.contains(value),
            "a rejected profile must list {value:?}: {stderr}"
        );
    }
}

/// `--help` is where a pipeline author looks for the stream and exit-code contract,
/// and it used to state neither.
#[test]
fn help_states_the_stream_and_exit_code_contract() {
    let out = run_tokfold_without_stdin(&["--help"]);
    assert!(out.status.success(), "--help must exit 0");
    let help = String::from_utf8_lossy(&out.stdout);

    for fragment in [
        "standard input",
        "standard output",
        "standard error",
        "Exit codes",
    ] {
        assert!(help.contains(fragment), "--help omits {fragment:?}: {help}");
    }
    for subcommand in ["compress", "expand", "stats", "mcp"] {
        assert!(
            help.contains(subcommand),
            "--help omits the {subcommand:?} subcommand: {help}"
        );
    }

    // A flag that overwrites and deletes files has to say so where a user looks it
    // up, not only in a README — including which of the two a rejected pass does, and
    // to what.
    let compress = run_tokfold_without_stdin(&["compress", "--help"]);
    let compress_help = String::from_utf8_lossy(&compress.stdout);
    // "same file as --input" rather than a bare "--input": every `compress --help`
    // screen names that flag, so the shorter needle passed no matter what the text
    // said about the collision it exists to check.
    for phrase in [
        "overwrites",
        "deletes",
        "TKFD",
        "left untouched",
        "same file as --input",
    ] {
        assert!(
            compress_help.contains(phrase),
            "`compress --help` must document that --archive is destructive and how far \
             that goes; it never says {phrase:?}: {compress_help}"
        );
    }
}

/// An archive holds the input in the clear, and the surface where that matters is the
/// one where a path is chosen: `compress --help`. Three of the six committed READMEs
/// carried the "exactly as sensitive as its plaintext" warning and the one crate
/// documenting `--archive` did not. Matched against the flattened screen, because clap re-wraps long help to the
/// terminal width and a phrase that sits on one line in the source need not sit on one
/// here.
#[test]
fn compress_help_says_what_an_archive_actually_holds() {
    let help = flatten(&run_tokfold_without_stdin(&["compress", "--help"]).stdout);
    for phrase in [
        "original bytes verbatim",
        "not encrypted",
        "as sensitive as the input",
        "the saving is in tokens",
    ] {
        assert!(
            help.contains(phrase),
            "`compress --help` must say that an archive is the input in the clear and \
             saves nothing on disk; it never says {phrase:?}: {help}"
        );
    }
}

/// The same help puts a size on the header — "43 to 46 bytes" — and no test read it:
/// the test above pins phrases, none of them a number, and a number on a help screen
/// is a claim like any other. The figure is derived here, not trusted: the header is
/// 42 fixed bytes plus the original length as a `ULEB128` varint, which gains a byte at
/// each power of 128, so an original under 128 bytes gets 43 and one of 2^21 bytes or
/// more gets 46 — the top of the ladder, because the next step is 2^28 and the binary
/// refuses anything over 16 MiB. The ladder is measured on `Header::encode_into` at
/// each boundary, and its top on the binary itself: an original of exactly 16 MiB, the
/// largest `compress` accepts, leaves an `--archive` file 46 bytes longer than itself.
#[test]
fn compress_help_header_size_is_the_measured_ladder() {
    use tokfold_core::format::{Flags, Header};

    let help = flatten(&run_tokfold_without_stdin(&["compress", "--help"]).stdout);
    assert!(
        help.contains("a header of 43 to 46 bytes"),
        "`compress --help` must size the header as the measured ladder; it says: {help}"
    );

    let header_len = |original_len: u64| {
        let mut out = Vec::new();
        Header::new(0, 0, Flags::default(), original_len, [0; 32]).encode_into(&mut out);
        out.len()
    };
    let ceiling = 16 * 1024 * 1024;
    for (original_len, expected) in [
        (0, 43),
        (127, 43),
        (128, 44),
        (16_383, 44),
        (16_384, 45),
        (2_097_151, 45),
        (2_097_152, 46),
        (ceiling, 46),
        (1 << 28, 47),
    ] {
        assert_eq!(
            header_len(original_len),
            expected,
            "header length for an original of {original_len} bytes"
        );
    }

    // Valid JSON, because an input `compress` rejects passes through with no archive
    // at all — and almost all whitespace, because the debug binary parses 16 MiB of
    // spaces in under a second and a 16 MiB string in half a minute.
    let archive = archive_path("header_ladder_top.tkfd");
    let mut input = vec![b' '; usize::try_from(ceiling).expect("16 MiB fits usize")];
    input.pop();
    input.push(b'0');
    let out = run_tokfold(&["compress", "--archive", path_str(&archive)], &input);
    assert!(
        out.status.success() && out.stderr.is_empty(),
        "an original of exactly 16 MiB must be accepted and compressed, not passed \
         through: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let archive_len = fs::metadata(&archive).expect("stat the archive").len();
    assert_eq!(
        archive_len,
        ceiling + 46,
        "the archive of a 16 MiB original must be the original plus a 46-byte header"
    );
}

/// The callers the contract was written for — pipelines and agent harnesses that
/// branch on `$?` — open `tokfold compress --help`, not the bare root screen. clap
/// does not propagate `after_help` down a subcommand tree, so every screen that
/// claims the contract has to opt in by hand and nothing but a test keeps a newly
/// added subcommand from silently dropping it.
#[test]
fn every_payload_subcommand_repeats_the_stream_and_exit_code_contract() {
    for subcommand in ["compress", "expand", "stats"] {
        let out = run_tokfold_without_stdin(&[subcommand, "--help"]);
        assert!(out.status.success(), "`{subcommand} --help` must exit 0");
        let help = String::from_utf8_lossy(&out.stdout);

        for fragment in [
            "standard input",
            "standard output",
            "standard error",
            "Exit codes",
        ] {
            assert!(
                help.contains(fragment),
                "`{subcommand} --help` omits {fragment:?}: {help}"
            );
        }
    }

    // `mcp` is excluded on purpose. It speaks a JSON-RPC stream rather than reading a
    // payload, takes no --input, and never exits 3, so most of the contract would be
    // a false statement there rather than a redundant one. It can still exit 2: a
    // stdio transport failure reaches the same error arm as any other command.
    let mcp = run_tokfold_without_stdin(&["mcp", "--help"]);
    let mcp_help = String::from_utf8_lossy(&mcp.stdout);
    assert!(
        !mcp_help.contains("Exit codes"),
        "`mcp --help` must not claim the payload exit-code contract: {mcp_help}"
    );

    // Sharing one text across the three screens is only safe while the text is true
    // of all three, and it was not. `stats` is the one payload subcommand that emits
    // no payload — its own one-line description says so — and the shared text told
    // its callers below it on the same help screen that "the payload always goes to standard
    // output". clap wraps `after_help` to the terminal width, so match on flattened
    // whitespace rather than on the laid-out lines.
    let stats_help = flatten(&run_tokfold_without_stdin(&["stats", "--help"]).stdout);
    assert!(
        stats_help.contains("`stats` writes no payload"),
        "`stats --help` must say it emits no payload: {stats_help}"
    );
    assert!(
        !stats_help.contains("The payload goes to standard output"),
        "`stats --help` claims a payload it never writes: {stats_help}"
    );

    // The root screen is the only one that lists `mcp` beside the payload
    // subcommands, so it is the only one that has to say `mcp` is not covered.
    let root_help = flatten(&run_tokfold_without_stdin(&["--help"]).stdout);
    assert!(
        root_help.contains("`mcp` is outside this contract"),
        "the root screen lists `mcp` without excluding it from the contract: {root_help}"
    );
}

/// Help output with every run of whitespace collapsed to one space, so an assertion
/// about a phrase does not depend on where clap chose to wrap it.
fn flatten(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The version string is what a bug report quotes, so it has to be the real crate
/// version rather than a hand-maintained literal that drifts.
#[test]
fn version_reports_the_crate_version() {
    let out = run_tokfold_without_stdin(&["--version"]);
    assert!(out.status.success(), "--version must exit 0");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("tokfold {}", env!("CARGO_PKG_VERSION"))
    );
}

/// Substring assertions could not catch what went wrong here: `compress --help` said
/// "a pass that falls back to passthrough deletes what is there" and "2 bad input or
/// usage", and every needle the tests looked for — "deletes", "overwrites", "Exit
/// codes" — was present in both. The words were right and the outcomes they were
/// attached to were wrong. So this pins the help against the binary's observed
/// behaviour rather than against a word list: each of the three claims below is made
/// by running the case the sentence describes.
#[test]
fn the_help_screen_agrees_with_the_outcomes_it_describes() {
    let dir = scratch_dir("help-agrees-with-behaviour");
    let archive = dir.join("recovery.tkfd");
    let compress_help =
        String::from_utf8_lossy(&run_tokfold_without_stdin(&["compress", "--help"]).stdout)
            .into_owned();

    // Claim 1: a pass the passthrough *encoder* wins is a success that still writes.
    // `{"a":1}` has no whitespace to strip and nothing to hoist, so no encoder beats
    // the input and `stats` reports `passthrough` — the word the old help used for
    // the deleting path.
    let incompressible = dir.join("incompressible.json");
    fs::write(&incompressible, br#"{"a":1}"#).unwrap();
    let stats = run_tokfold_without_stdin(&["stats", "--input", path_str(&incompressible)]);
    assert!(
        String::from_utf8_lossy(&stats.stdout).contains("passthrough"),
        "test precondition: this input must be the passthrough case"
    );
    let won = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&incompressible),
        "--archive",
        path_str(&archive),
    ]);
    assert!(
        won.status.success(),
        "the passthrough encoder winning is a success"
    );
    assert_eq!(
        fs::read(&archive)
            .expect("the passthrough encoder still writes an archive")
            .get(..4),
        Some(&b"TKFD"[..]),
        "a pass the passthrough encoder wins must leave a real archive behind"
    );
    assert!(
        !compress_help.contains("falls back to passthrough deletes"),
        "`compress --help` must not credit the deletion to the encoder that writes: \
         {compress_help}"
    );

    // Claim 2: only a *rejected* input deletes, and it still exits 0. The archive
    // written just above is the stale one this clears.
    let rejected = dir.join("rejected.json");
    fs::write(&rejected, b"not json at all").unwrap();
    let out = run_tokfold_without_stdin(&[
        "compress",
        "--input",
        path_str(&rejected),
        "--archive",
        path_str(&archive),
    ]);
    assert!(
        out.status.success(),
        "a rejected `compress` exits 0, so the help must not send a harness to a 2 arm"
    );
    assert!(!archive.exists(), "a rejected pass clears the archive slot");
    assert!(
        !compress_help.contains("2 bad input or usage"),
        "`compress --help` claimed 2 for an input this command forwards with 0: \
         {compress_help}"
    );

    // Claim 3: the exit code the help does attribute to a rejected input is `stats`'.
    let refused = run_tokfold_without_stdin(&["stats", "--input", path_str(&rejected)]);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "the help names `stats` as the subcommand that refuses a rejected input"
    );
    assert!(
        compress_help.contains("rejected by `stats`"),
        "`compress --help` must say which subcommand a rejected input exits 2 on: \
         {compress_help}"
    );
}

/// Run `tokfold compress --archive /dev/fd/3` with descriptor 3 opened by the shell
/// through `redirect` (`3>`, `3<>`, `3>>`) on `slot`, feeding `stdin_bytes`.
#[cfg(unix)]
fn compress_into_descriptor_3(redirect: &str, slot: &Path, stdin_bytes: &[u8]) -> Output {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "exec \"$0\" compress --archive /dev/fd/3 3{redirect}\"$1\""
        ))
        .arg(env!("CARGO_BIN_EXE_tokfold"))
        .arg(slot)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run tokfold under sh");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(stdin_bytes)
        .expect("feed stdin");
    child.wait_with_output().expect("wait for tokfold")
}

/// A rejected pass whose `--archive` names a descriptor on an empty file passes its
/// input through, as `0.0.1` did. On macOS `realpath` of `/dev/fd/3` returns
/// `/dev/fd/<the file's basename>`, which names nothing, and the stale-archive check
/// once followed it there and failed the run with "No such file or directory".
#[test]
#[cfg(unix)]
fn a_rejected_pass_through_a_descriptor_spelling_still_passes_through() {
    let dir = scratch_dir("descriptor-slot-empty");
    let slot = dir.join("arch.tkfd");
    let out = compress_into_descriptor_3(">", &slot, b"not json");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"not json");
    assert_eq!(fs::read(&slot).expect("read the slot"), b"");
}

/// A stale archive behind a descriptor spelling cannot be removed — no name reaches
/// it — so a rejected pass refuses rather than passing through beside it, and says the
/// input was not passed through, because nothing reached stdout, and exits `2`. A file
/// of four bytes or more behind a write-only descriptor cannot be read to rule an archive out, and is
/// refused the same way. macOS only: Linux resolves `/dev/fd/3` to the file's own
/// path, where the ordinary removal applies.
#[test]
#[cfg(target_os = "macos")]
fn a_stale_archive_behind_a_descriptor_spelling_is_refused_not_kept() {
    let dir = scratch_dir("descriptor-slot-stale");
    let archive = dir.join("a.tkfd");
    seed_real_archive(&dir.join("in.json"), &archive);
    let before = fs::read(&archive).expect("read the seeded archive");
    let out = compress_into_descriptor_3("<>", &archive, b"not json");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        out.stdout, b"",
        "nothing may pass through beside the archive"
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "tokfold: the input was rejected (invalid JSON at byte 0: expected a JSON value) \
         and not passed through: a file opening with the tokfold archive magic is open \
         behind /dev/fd/3, which names a descriptor rather than a file, so it cannot be \
         removed\n"
    );
    assert_eq!(fs::read(&archive).expect("reread the archive"), before);

    let log = dir.join("log");
    fs::write(&log, b"earlier log line\n").expect("seed the log");
    let out = compress_into_descriptor_3(">>", &log, b"not json");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(out.stdout, b"");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("and not passed through: inspecting the archive path /dev/fd/3"),
        "{stderr}"
    );
    assert_eq!(
        fs::read(&log).expect("reread the log"),
        b"earlier log line\n"
    );
}

/// The magic behind a descriptor spelling is read at offset `0`, and the caller's
/// offset is not moved. macOS opens `/dev/stdin` by duplicating the descriptor, and a
/// duplicate shares the offset: a plain read took four bytes from the caller's stream,
/// and read from where that stream stood — here just past the magic, so a stale archive
/// was called "not a tokfold archive" and the input passed through beside it. The
/// descriptor is open for reading and writing, so the archive write reaches it and a
/// stale archive there is one a successful pass would have replaced.
#[test]
#[cfg(target_os = "macos")]
fn a_descriptor_slot_is_read_from_its_start_without_moving_the_callers_offset() {
    use std::io::{Read as _, Seek as _};

    let dir = scratch_dir("descriptor-slot-offset");
    let archive = dir.join("a.tkfd");
    seed_real_archive(&dir.join("in.json"), &archive);
    let before = fs::read(&archive).expect("read the seeded archive");
    let bad = dir.join("bad.txt");
    fs::write(&bad, b"not json").expect("write the rejected input");

    let mut caller = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&archive)
        .expect("open the archive as stdin");
    let mut magic = [0u8; 4];
    caller.read_exact(&mut magic).expect("move past the magic");
    let stdin = caller.try_clone().expect("share the descriptor");
    let out = run_tokfold_on(
        &[
            "compress",
            "--input",
            path_str(&bad),
            "--archive",
            "/dev/stdin",
        ],
        Stdio::from(stdin),
        fs::File::create(dir.join("out")).expect("create the output file"),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("a file opening with the tokfold archive magic is open behind /dev/stdin"),
        "{stderr}"
    );
    assert_eq!(
        caller.stream_position().expect("read the caller's offset"),
        4,
        "inspecting the slot moved the caller's offset"
    );
    assert_eq!(fs::read(&archive).expect("reread the archive"), before);
}

/// A stale archive behind a read-only descriptor spelling is not a slot the archive
/// write reaches — a successful pass fails on it with `Permission denied` — so there is
/// nothing a rejected pass must clear, and it passes its input through at exit `0`, as
/// `0.0.1` did, leaving the file as it was and saying so. Here the archive is its own
/// input: every byte of it reaches stdout.
#[test]
#[cfg(target_os = "macos")]
fn a_stale_archive_behind_a_read_only_descriptor_is_not_the_archive_slot() {
    let dir = scratch_dir("descriptor-slot-read-only");
    let archive = dir.join("a.tkfd");
    seed_real_archive(&dir.join("in.json"), &archive);
    let before = fs::read(&archive).expect("read the seeded archive");
    let out_path = dir.join("out");
    let out = run_tokfold_on(
        &["compress", "--archive", "/dev/stdin"],
        Stdio::from(fs::File::open(&archive).expect("open the archive read-only")),
        fs::File::create(&out_path).expect("create the output file"),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stderr.contains("tokfold: left /dev/stdin untouched: it cannot be opened for writing\n"),
        "{stderr}"
    );
    assert_eq!(fs::read(&out_path).expect("read the output"), before);
    assert_eq!(fs::read(&archive).expect("reread the archive"), before);
}

/// The write-reach question is asked before the length, so a read-only descriptor gets
/// the same answer whatever its file holds: one too short to carry the magic is not
/// reported as "not a tokfold archive" while a longer one would be reported as nothing.
#[test]
#[cfg(target_os = "macos")]
fn a_short_file_behind_a_read_only_descriptor_is_reported_like_a_long_one() {
    let dir = scratch_dir("descriptor-slot-read-only-short");
    let input = dir.join("bad.txt");
    fs::write(&input, b"not json").expect("write the input");
    let short = dir.join("short");
    fs::write(&short, b"TK").expect("write the short file");
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg("exec \"$0\" compress --input \"$1\" --archive /dev/fd/3 3<\"$2\"")
        .arg(env!("CARGO_BIN_EXE_tokfold"))
        .arg(&input)
        .arg(&short)
        .stdin(Stdio::null())
        .output()
        .expect("run tokfold under sh");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(out.stdout, b"not json");
    assert!(
        stderr.contains("tokfold: left /dev/fd/3 untouched: it cannot be opened for writing\n"),
        "{stderr}"
    );
    assert_eq!(fs::read(&short).expect("reread the short file"), b"TK");
}

/// A directory behind a descriptor spelling is reported as a directory, exit `2`, as
/// one named by its own path is — not skipped as a file that holds no archive.
#[test]
#[cfg(target_os = "macos")]
fn a_directory_behind_a_descriptor_spelling_is_reported_as_one() {
    let dir = scratch_dir("descriptor-slot-directory");
    let input = dir.join("bad.txt");
    fs::write(&input, b"not json").expect("write the input");
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg("exec \"$0\" compress --input \"$1\" --archive /dev/fd/3 3<\"$2\"")
        .arg(env!("CARGO_BIN_EXE_tokfold"))
        .arg(&input)
        .arg(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("run tokfold under sh");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(out.stdout.is_empty(), "{stderr}");
    assert!(
        stderr.contains("inspecting the archive path /dev/fd/3: Is a directory"),
        "{stderr}"
    );
}

/// A pipe behind a descriptor spelling is left alone even when it holds the magic: it
/// is not a file a later `expand` could read back, so the rejected pass passes through
/// and never reads from it. macOS reports the bytes waiting in a pipe as its size, so
/// only the `is_file` check keeps this pipe from being read for the magic — and the
/// `cat` after tokfold proves not one byte of it was consumed. Nothing was read, so
/// stderr says it is not a regular file, not that it is not an archive.
#[test]
#[cfg(target_os = "macos")]
fn a_pipe_behind_a_descriptor_spelling_is_neither_read_nor_refused() {
    let dir = scratch_dir("descriptor-slot-pipe");
    let input = dir.join("in.txt");
    fs::write(&input, b"not json").expect("write the input");
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg(
            "printf TKFDabcdef | { sleep 0.2; \"$0\" compress --archive /dev/fd/3 \
             3<&0 0<\"$1\"; code=$?; cat; exit $code; }",
        )
        .arg(env!("CARGO_BIN_EXE_tokfold"))
        .arg(&input)
        .stdin(Stdio::null())
        .output()
        .expect("run tokfold under sh");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(out.stdout, b"not jsonTKFDabcdef", "stderr: {stderr}");
    assert!(
        stderr.contains("left /dev/fd/3 untouched: it is not a regular file"),
        "{stderr}"
    );
}

/// A standard output opened read-only on the input cannot destroy it, so it is not
/// reported as a collision: the run fails on the first write, as any read-only stdout
/// does.
#[test]
#[cfg(unix)]
fn a_read_only_stdout_on_the_input_is_not_called_a_collision() {
    let dir = scratch_dir("read-only-stdout-on-input");
    let doc = dir.join("in.json");
    fs::write(&doc, COMPRESSIBLE_JSON).expect("write the input");
    let out = Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .args(["compress", "--input", path_str(&doc)])
        .stdin(Stdio::null())
        .stdout(fs::File::open(&doc).expect("open the input read-only"))
        .stderr(Stdio::piped())
        .output()
        .expect("run tokfold");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("tokfold: writing to standard output: "),
        "{stderr}"
    );
    assert_eq!(fs::read(&doc).expect("reread the input"), COMPRESSIBLE_JSON);
}

/// Only a regular file can be destroyed, so the same-file guard compares nothing
/// else: `--input /dev/null --archive /dev/null` names one device twice, and a
/// rejected pass over it passes the empty input through and exits `0`, as `0.0.1`
/// did, rather than being refused as a collision.
#[test]
#[cfg(unix)]
fn the_same_file_guard_ignores_two_spellings_of_one_device() {
    let out =
        run_tokfold_without_stdin(&["compress", "--input", "/dev/null", "--archive", "/dev/null"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("name the same file"), "{stderr}");
    assert!(
        stderr.contains("left /dev/null untouched: it is not a regular file"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty());
}

/// A standard input opened write-only on the archive reads nothing through that
/// descriptor, so it is not an input the archive could destroy. It must fail as the
/// unreadable standard input it is — exit `2`, the `EBADF` read error — and leave
/// the archive as it was, not be reported as a collision.
#[test]
#[cfg(unix)]
fn a_write_only_stdin_on_the_archive_is_a_read_failure_not_a_collision() {
    let dir = scratch_dir("write-only-stdin-on-archive");
    let good = dir.join("good.json");
    let archive = dir.join("a.tkfd");
    seed_real_archive(&good, &archive);
    let seeded = fs::read(&archive).expect("read the seeded archive");
    let stdin = fs::OpenOptions::new()
        .append(true)
        .open(&archive)
        .expect("open the archive write-only");
    let out = Command::new(env!("CARGO_BIN_EXE_tokfold"))
        .args(["compress", "--archive", path_str(&archive)])
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("run tokfold");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.starts_with("tokfold: reading standard input: "),
        "{stderr}"
    );
    assert!(!stderr.contains("redirected"), "{stderr}");
    assert_eq!(fs::read(&archive).expect("reread the archive"), seeded);
}

/// An input that does not exist is not a file the archive could destroy, even when
/// `--archive` spells the same path — and neither is a dangling symlink. The read
/// must fail with its real reason, as in `0.0.1`, not be called a collision.
#[test]
fn a_missing_input_named_as_its_own_archive_is_a_read_failure() {
    let dir = scratch_dir("missing-input-as-archive");
    let missing = dir.join("nope");
    let dangling = dir.join("dangling");
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.join("absent-target"), &dangling)
        .expect("create a dangling symlink");
    let paths: &[&Path] = if cfg!(unix) {
        &[&missing, &dangling]
    } else {
        &[&missing]
    };
    for path in paths {
        let out = run_tokfold_without_stdin(&[
            "compress",
            "--input",
            path_str(path),
            "--archive",
            path_str(path),
        ]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.starts_with(&format!("tokfold: reading input file {}: ", path.display())),
            "{stderr}"
        );
        assert!(!stderr.contains("name the same file"), "{stderr}");
    }
}

/// `--archive /dev/stdin` over a standard input opened read-only on the input — with
/// the input also named by `--input`, and without it: on
/// macOS the path reopens that descriptor, the write-only open the archive needs is
/// refused, and nothing written through it could reach the input — so a rejected
/// pass passes the input through and exits `0`, as `0.0.1` did, rather than being
/// refused as a collision. Linux reopens the file itself, writably, so there it is a
/// real collision and is refused. Either way the input is left as it was.
#[test]
#[cfg(unix)]
fn a_read_only_descriptor_spelling_of_the_archive_is_not_a_collision() {
    let dir = scratch_dir("read-only-descriptor-archive");
    let input = dir.join("bad.txt");
    fs::write(&input, b"not json").expect("write the input");
    let named: &[&str] = &["--input", path_str(&input)];
    for input_args in [named, &[]] {
        let mut args = vec!["compress"];
        args.extend_from_slice(input_args);
        args.extend_from_slice(&["--archive", "/dev/stdin"]);
        let out = run_tokfold_on(
            &args,
            Stdio::from(fs::File::open(&input).expect("open the input read-only")),
            fs::File::create(dir.join("out.txt")).expect("create a distinct stdout"),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        if cfg!(target_os = "macos") {
            assert_eq!(out.status.code(), Some(0), "{args:?}: {stderr}");
            assert!(!stderr.contains("the same file"), "{args:?}: {stderr}");
            assert!(!stderr.contains("on standard input"), "{args:?}: {stderr}");
            assert_eq!(
                fs::read(dir.join("out.txt")).expect("read the output"),
                b"not json"
            );
        } else {
            assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        }
        assert_eq!(fs::read(&input).expect("reread the input"), b"not json");
    }
}

/// `--input /dev/stdin` over a standard input opened write-only on the archive: on
/// macOS the read-only open the input read makes is refused, so there is no input to
/// protect and the honest failure is that read's own `Permission denied`, as in
/// `0.0.1`. Linux reopens the file itself, readably, so there the archive would
/// overwrite what is read and the run is refused as a collision. Either way the
/// archive is left as it was.
#[test]
#[cfg(unix)]
fn a_write_only_stdin_spelled_as_the_input_is_a_read_failure_on_macos() {
    let dir = scratch_dir("write-only-stdin-as-input");
    let good = dir.join("good.json");
    let archive = dir.join("a.tkfd");
    seed_real_archive(&good, &archive);
    let seeded = fs::read(&archive).expect("read the seeded archive");
    let out = run_tokfold_on(
        &[
            "compress",
            "--input",
            "/dev/stdin",
            "--archive",
            path_str(&archive),
        ],
        Stdio::from(append_to(&archive)),
        fs::File::create(dir.join("out.txt")).expect("create a distinct stdout"),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    if cfg!(target_os = "macos") {
        assert!(
            stderr.starts_with("tokfold: reading input file /dev/stdin: "),
            "{stderr}"
        );
        assert!(!stderr.contains("name the same file"), "{stderr}");
    } else {
        assert!(stderr.contains("name the same file"), "{stderr}");
    }
    assert_eq!(fs::read(&archive).expect("reread the archive"), seeded);
}

/// `--input /dev/stdout` over a standard output opened for appending to a file: on
/// macOS the read-only open the input read makes is refused, so nothing is read from
/// that file and nothing written to it can be the input. The run fails on that read,
/// with `0.0.1`'s `Permission denied`, rather than being called output-into-input.
/// Linux reopens the file itself, readably, so there it is refused as output into
/// input. Either way the file is left as it was.
#[test]
#[cfg(unix)]
fn a_write_only_stdout_spelled_as_the_input_is_a_read_failure_on_macos() {
    let dir = scratch_dir("write-only-stdout-as-input");
    let target = dir.join("t.json");
    fs::write(&target, COMPRESSIBLE_JSON).expect("write the file");
    let out = run_tokfold_on(
        &["compress", "--input", "/dev/stdout"],
        Stdio::null(),
        append_to(&target),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    if cfg!(target_os = "macos") {
        assert!(
            stderr.starts_with("tokfold: reading input file /dev/stdout: "),
            "{stderr}"
        );
        assert!(!stderr.contains("redirected"), "{stderr}");
    } else {
        assert!(stderr.contains("redirected"), "{stderr}");
    }
    assert_eq!(
        fs::read(&target).expect("reread the file"),
        COMPRESSIBLE_JSON
    );
}
