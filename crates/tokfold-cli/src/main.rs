//! `tokfold` — the one shipped binary for the reversible context-compression engine.
//!
//! Four subcommands sit on top of [`tokfold_core`]:
//!
//! * `compress` — read input, emit the token-reduced rendering, optionally persist a
//!   recovery archive. Compression is an optimization, never a gate: a
//!   [`CompressError`](tokfold_core::CompressError) forwards the original bytes
//!   unchanged and exits `0`. That path also *deletes* an archive left at
//!   `--archive` by an earlier run, because an archive that no longer describes the
//!   bytes on stdout is worse than no archive at all. It deletes nothing else: it
//!   resolves the path first, opens nothing that is neither a regular file nor a
//!   directory, and removes only a file that opens with the `TKFD` magic and still
//!   has the identity it was inspected under. Anything else it can inspect — and any
//!   file behind a descriptor spelling the archive write cannot open — is left as it
//!   was found and reported on stderr. Clearing the slot can still fail the command: if the
//!   slot cannot be inspected or cleared, nothing is emitted and the exit
//!   is `2`, the same rule as a failed archive write. It is not the only thing that
//!   can — writing the passthrough to stdout can too, on any I/O error that is not a
//!   reader closing the pipe early. What survives a `CompressError` is the *payload*,
//!   not the command.
//! * `expand` — reconstruct the exact original from a recovery archive. Fail-closed:
//!   any integrity error exits `3` and emits nothing.
//! * `stats` — report what a compression pass would achieve, without emitting the
//!   payload.
//! * `mcp` — serve the engine as Model Context Protocol tools over stdio. It prints
//!   the crate's experimental notice to stderr before serving anything: the server
//!   sits in the secrets path and has never been through a hardening audit. It
//!   shipped in the 0.0.1 npm release in exactly that state — labelled experimental
//!   at every layer a user can meet (the stderr notice, the subcommand's help text,
//!   the README, and this paragraph) rather than held back until it is hardened.
//!
//! Exit codes are normative: `0` success, `2` bad input (usage, I/O, or an input the
//! compressor rejects on `stats`), `3` a corrupt, empty, or otherwise unrecoverable
//! archive on `expand`. Those are the codes the process *chooses*: a process killed
//! by a signal returns none of them, and the CLI README says what a caller should
//! make of a code outside that set.
//! A downstream that closes the pipe early (`… | head`) is a clean exit, never an
//! error. `anyhow` is confined to this binary; the library crates never depend on it.
//!
//! `mcp` exits `0` when the client closes the stream — on clean end-of-input and on a
//! broken pipe alike, since a client that hangs up has ended the session rather than
//! failed it. Earlier in development this subcommand was an unimplemented stub that
//! always exited `69`; that code is gone now that it does something.

#![forbid(unsafe_code)]

use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, Read, Seek as _, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tokfold_core::format::MAGIC as ARCHIVE_MAGIC;
use tokfold_core::{Compressor, Config, Profile, Stats};

/// Bad usage, an I/O failure, or an input the compressor rejects on `stats`.
const EXIT_BAD_INPUT: u8 = 2;

/// A corrupt, empty, or otherwise unrecoverable archive handed to `expand`.
const EXIT_CORRUPT: u8 = 3;

/// Left-hand column width for the aligned `stats` report.
const STATS_LABEL_WIDTH: usize = 20;

/// The exit-code half of the stream contract, identical on every screen that claims
/// it.
///
/// A macro rather than a `const` because the screens below are assembled with
/// `concat!`, which takes literals only, and clap's `after_help` wants a
/// `&'static str` it can print as-is.
macro_rules! exit_codes {
    () => {
        "Exit codes: 0 success, including a `compress` whose input the engine rejects \
         and forwards unchanged; 2 usage, I/O, or an input rejected by `stats`; 3 a \
         corrupt, empty, or otherwise unrecoverable archive on `expand`."
    };
}

/// The stream and exit-code contract for `compress` and `expand`, appended to their
/// `--help` screens.
///
/// It lives in the help output rather than only in a README because the callers who
/// most need it — shell pipelines and agent harnesses that branch on `$?` — are
/// exactly the ones that will never open the README, and those callers reach for
/// `tokfold compress --help`, not the bare root screen. clap does not propagate
/// `after_help` down a subcommand tree, so each screen has to opt in by hand.
///
/// There are three screens rather than one because one text was not true of all of
/// them. `stats` gets [`STATS_STREAM_AND_EXIT_HELP`]: it writes no payload, which is
/// the whole difference between it and `compress`, and this text used to tell its
/// callers the opposite, below the subcommand's own description on the same screen. The root
/// screen gets [`ROOT_STREAM_AND_EXIT_HELP`], because it also lists `mcp`, which is
/// not in this contract at all.
const STREAM_AND_EXIT_HELP: &str = concat!(
    "Input is read from standard input unless --input names a file. The payload goes \
     to standard output and every diagnostic to standard error, so it can be piped \
     onward without filtering.\n\n",
    exit_codes!()
);

/// The same contract as [`STREAM_AND_EXIT_HELP`], corrected for the one payload
/// subcommand that emits no payload.
const STATS_STREAM_AND_EXIT_HELP: &str = concat!(
    "Input is read from standard input unless --input names a file. `stats` writes no \
     payload: the measurement report goes to standard output and every diagnostic to \
     standard error. The pass still runs the compressor and drops the rendering it \
     builds; none of it reaches either stream.\n\n",
    exit_codes!()
);

/// The contract as stated on the root screen, which lists a subcommand the other two
/// texts do not cover.
///
/// `mcp` is deliberately outside the contract: it speaks a JSON-RPC stream rather
/// than reading a payload, has no `--input`, and never exits 3, so most of this text
/// would be false there rather than merely redundant. It can still exit 2 — a
/// transport failure reaches the same error arm as any other — which is why the
/// exclusion rests on the stream description and not on the exit codes alone. The
/// root screen is the one place a reader sees `mcp` listed beside the others, so it
/// is the one place that has to say so.
const ROOT_STREAM_AND_EXIT_HELP: &str = concat!(
    "For `compress`, `expand` and `stats`: input is read from standard input unless \
     --input names a file, and every diagnostic goes to standard error. `compress` and \
     `expand` write their payload to standard output; `stats` writes a measurement \
     report there and no payload. `mcp` is outside this contract: it speaks a JSON-RPC \
     stream, takes no --input, and never exits 3.\n\n",
    exit_codes!()
);

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return parse_failure(&err),
    };
    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            // I/O and usage failures land here; `{err:#}` prints the whole context
            // chain on one line. Everything reaching this arm is a bad-input error.
            warn(format_args!("tokfold: {err:#}"));
            ExitCode::from(EXIT_BAD_INPUT)
        }
    }
}

/// Finish a command line that did not parse into a command: `--help`, `--version`, or
/// a usage error.
///
/// Text bound for a terminal goes through clap's own printer, which picks colours for
/// the stream it writes to and exits `2` or `0`. Help and version text bound for
/// anything else is written through [`write_stdout`] instead; a usage error bound for
/// anything else goes to [`usage_failure`], which is this branch's mirror on standard
/// error. clap prints through `io::stdout()`, which treats `EBADF` as success, so
/// `tokfold --help 1<file` exited `0` having printed nothing; through [`write_stdout`]
/// that is a write failure and exits `2`. A terminal opened read-only as stdout
/// (`tokfold --help 1</dev/tty`) still takes clap's path and is not reported.
fn parse_failure(err: &clap::Error) -> ExitCode {
    use clap::error::ErrorKind;
    use std::io::IsTerminal as _;
    let is_text = matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    );
    if !is_text {
        return usage_failure(err);
    }
    if io::stdout().is_terminal() {
        err.exit();
    }
    match write_stdout(err.render().to_string().as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(write) => {
            warn(format_args!("tokfold: {write:#}"));
            ExitCode::from(EXIT_BAD_INPUT)
        }
    }
}

/// Print a clap usage error to standard error in one write and return its exit code.
///
/// The mirror of [`parse_failure`]'s own branch. That branch renders help and version
/// to a string and hands it to one `write_stdout` rather than letting clap print it,
/// and the same reason applies on standard error: `err.exit()` uses clap's printer,
/// which issues one `write` per rendered fragment. A reader that takes each `write`
/// as a record therefore sees a usage error in pieces. Measured against a datagram
/// standard error, clap's printer sent `tokfold --bogus` as ten datagrams (`b"error:"`,
/// `b" unexpected argument '"`, `b"--bogus"`, …) and `compress --profile nope` as
/// fourteen, none of them a whole line — only the last ended at a line boundary —
/// while every diagnostic [`warn`] formats arrived as a single whole-line datagram.
/// Through here each of those is one datagram.
///
/// The bytes are unchanged: clap renders without ANSI when the stream is not a
/// terminal, which is exactly what `render().to_string()` produces. All six usage
/// errors the tests drive are byte-identical to what `err.exit()` wrote.
///
/// A terminal keeps `err.exit()`, because `render().to_string()` would drop the colour
/// that path has. A fixture has to ask for it: on a pty, `tokfold --bogus` carries 12
/// ANSI escapes under `TERM=xterm-256color` and none under `TERM=dumb`, since anstream
/// and not clap makes that choice.
///
/// The write is dropped if it fails, for the reason [`warn`] drops its own: the exit
/// code is worth more than the text. `err.exit()` already behaved that way — a clap
/// usage error with standard error closed exits `2`, not `101` — and this keeps it.
fn usage_failure(err: &clap::Error) -> ExitCode {
    use std::io::IsTerminal as _;
    if io::stderr().is_terminal() {
        err.exit();
    }
    let _ = io::stderr()
        .lock()
        .write_all(err.render().to_string().as_bytes());
    // Every clap error that is not help or version exits 2, which is `EXIT_BAD_INPUT`
    // already; ask clap rather than assume it, and fall back to the same value.
    ExitCode::from(u8::try_from(err.exit_code()).unwrap_or(EXIT_BAD_INPUT))
}

/// Dispatch a parsed command line to its handler.
fn run(cli: &Cli) -> Result<ExitCode> {
    match &cli.command {
        Command::Compress(args) => cmd_compress(args),
        Command::Expand(args) => cmd_expand(args),
        Command::Stats(args) => cmd_stats(args),
        Command::Mcp => cmd_mcp(),
    }
}

/// `compress`: emit the rendering to stdout and, when asked, the recovery archive to
/// a file. A `CompressError` is not fatal — the original bytes pass through unchanged
/// and the command exits `0`, because dropping an agent's tool output would be worse
/// than shipping it uncompressed. What a `CompressError` cannot fail is the payload;
/// it can still fail the command. [`clear_stale_archive`] runs before the passthrough
/// is written, so a slot that cannot be inspected or cleared exits `2` and emits
/// nothing, and [`write_stdout`] itself returns an error for any I/O failure that is
/// not a reader closing the pipe early. The stderr line that announces the
/// passthrough is written only after the passthrough write has returned, so a run
/// that exits `2` never claims one; its error carries the rejection reason instead.
/// A reader that closed early counts as a returned write, so the announcement can
/// follow a passthrough that reader took only part of.
fn cmd_compress(args: &CompressArgs) -> Result<ExitCode> {
    ensure_output_is_not_the_input(args.input.as_deref())?;
    ensure_archive_is_not_the_input(args.input.as_deref(), args.archive.as_deref())?;

    let input = read_source(args.input.as_deref())?;
    let compressor = Compressor::new(config_for(args.profile));

    match compressor.compress(&input) {
        Ok(artifact) => {
            // Persist the archive first: if that I/O fails we exit before writing any
            // rendering, so a caller never sees output without its recovery blob.
            if let Some(path) = args.archive.as_deref() {
                write_file(path, &artifact.archive)?;
            }
            write_stdout(artifact.rendering.as_bytes())?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            // Clear, then emit, then announce. The clearing comes before the
            // passthrough reaches stdout: it is what makes "no recovery archive
            // written" true, and a caller must never be able to read that line while
            // a contradicting archive is still on disk. The announcement comes last
            // because every line of it says the input *was* passed through, and
            // either step before it can still exit `2` — the clearing having emitted
            // nothing, the write possibly having emitted part of the input before it
            // failed — so the failure carries the rejection reason instead: "not
            // passed through" when the clearing refused, since nothing has reached
            // stdout yet, and "passing it through failed" when the write did, because
            // part of the input may already be out.
            let declined =
                || format!("the input was rejected ({err}) and passing it through failed");
            let not_passed = || format!("the input was rejected ({err}) and not passed through");
            let cleared = match args.archive.as_deref() {
                Some(path) => Some(clear_stale_archive(path).with_context(not_passed)?),
                None => None,
            };
            write_stdout(&input).with_context(declined)?;
            warn(format_args!(
                "tokfold: passing input through uncompressed: {err}"
            ));
            if let (Some(path), Some(cleared)) = (args.archive.as_deref(), cleared) {
                announce_no_archive(path, cleared);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Refuse a `compress` that would aim its recovery archive at its own input.
///
/// Both `compress` outcomes can be destructive to the archive path — a successful
/// pass truncates a regular file there, a rejected one deletes it when it opens with
/// the `TKFD` magic — so pointing it at the source document can destroy the very
/// bytes the archive exists to recover. The check runs before the input is read and
/// before anything is written, so the refusal cannot arrive after the damage — and,
/// for the same reason, it cannot know which outcome the pass would have had. It
/// refuses both, so a rejected input that does not open with the magic, which the
/// pass would have left untouched, is refused too. The same holds where *neither*
/// outcome could touch the file: a read-only input (a successful pass fails on
/// "Permission denied"), and on macOS a non-empty input behind a `/dev/fd/N` or
/// `/dev/stdin` spelling (a successful pass refuses a descriptor whose open did not
/// empty it). The messages therefore state the rule that was checked — "the archive
/// may not be aimed at its own input" — and predict no outcome; the tests pin them
/// whole.
///
/// The input has a path only under `--input`. On standard input it has a descriptor
/// instead, and on Unix a descriptor has an identity too: `tokfold compress --archive
/// log.json < log.json` is refused by [`stdin_is_the_file_at`], which asks the
/// descriptor. What the guard cannot see is an input that reaches stdin through a
/// pipe — `cat log.json | tokfold compress --archive log.json` — because the bytes in
/// a pipe have no file behind them to compare.
///
/// Standard output is the mirror: `tokfold compress --archive a.tkfd >> a.tkfd` hands
/// the binary a descriptor on the archive path, and a success writes the archive and
/// the rendering into one file, while a rejection deletes the file the passthrough is
/// being appended to whenever it opens with the magic, so both the stale archive and
/// the payload are lost. A rejection over a file without the magic would only have
/// appended the passthrough, and is refused all the same. On Unix that is refused the same way, by
/// [`stdout_is_the_file_at`].
///
/// The archive path is compared by [`archive_identity`] and the input path by
/// [`input_identity`], so a `/dev/stdin`, `/dev/stdout` or `/dev/fd/N` spelling of the
/// same descriptor is the same file on macOS as well as on Linux — when the operation
/// that path is given to can open it. A spelling the archive write cannot open, such
/// as `/dev/stdin` over a read-only descriptor on macOS, destroys nothing and is not
/// refused. Standard error is not compared: a diagnostic appended into the archive
/// path is the only thing it could lose, and a success writes no diagnostic.
fn ensure_archive_is_not_the_input(input: Option<&Path>, archive: Option<&Path>) -> Result<()> {
    let Some(archive) = archive else {
        return Ok(());
    };
    ensure!(
        !stdout_is_the_file_at(archive),
        "--archive {} names the file standard output is redirected to; \
         the archive may not be aimed at the output",
        archive.display()
    );
    match input {
        Some(input) => ensure!(
            !is_same_file(input, archive),
            "--archive {} and --input {} name the same file; \
             the archive may not be aimed at its own input",
            archive.display(),
            input.display()
        ),
        None => ensure!(
            !stdin_is_the_file_at(archive),
            "--archive {} names the file on standard input; \
             the archive may not be aimed at its own input",
            archive.display()
        ),
    }
    Ok(())
}

/// Refuse a run whose standard output is redirected into the file it reads.
///
/// Every command writes to standard output, and a regular file there that is also the
/// input — `tokfold compress -i log.json >> log.json`, `tokfold expand < a.tkfd >
/// a.tkfd` — is overwritten or appended to by the very run reading it, whenever that
/// run writes anything. The check runs before the input is read, so it cannot know
/// whether it would: an `expand` of a corrupt archive or a `stats` of non-JSON writes
/// nothing to standard output and is refused all the same (the former exits `2` here
/// where `0.0.1` exited `3`). The message states the rule, not a predicted write. Under `mcp` the
/// server reads its own replies back as requests and answers each one, so the file
/// grows until the disk is full. With `>` the shell has already emptied the file
/// before tokfold starts, so the refusal cannot save its bytes; what it saves is the
/// rest of the run. A `compress --archive` that read that emptied input as a rejected
/// document would otherwise delete the stale archive at its archive path — which, in
/// that command line, was the last copy of the document.
///
/// Runs first in every command, before anything is read, cleared or written. Unix
/// only, like the other same-file guards: elsewhere [`stdout_identity`] is `None` and
/// nothing is refused.
fn ensure_output_is_not_the_input(input: Option<&Path>) -> Result<()> {
    ensure!(
        !stdout_is_the_input(input),
        "{} is the file standard output is redirected to; \
         output may not be written into the input it is read from",
        input.map_or_else(
            || "standard input".to_owned(),
            |path| format!("--input {}", path.display())
        )
    );
    Ok(())
}

/// Whether standard output is the regular file the input is read from — the file at
/// `input`, or the file behind standard input when there is no `input`.
fn stdout_is_the_input(input: Option<&Path>) -> bool {
    let Some(stdout) = stdout_identity() else {
        return false;
    };
    let input = input.map_or_else(stdin_identity, input_identity);
    input == Some(stdout)
}

/// A file's identity as the operating system understands it, independent of any path
/// used to reach it.
///
/// `None` where the standard library exposes no such identity, which makes every
/// caller fall back to comparing paths. That is a real narrowing rather than a
/// portability detail, so each caller says what it loses.
// The `Option` is load-bearing on the platforms this is compiled for, but clippy only
// ever sees one `cfg` arm at a time and reads the Unix one as a function that cannot
// fail. Unwrapping the return type here would make the non-Unix arm unwritable.
#[allow(clippy::unnecessary_wraps)]
fn identity_of(meta: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// The identity of whatever `path` resolves to, or `None` if it cannot be reached.
///
/// A regular file reached through `/dev` is asked through an open descriptor rather
/// than through `stat`. macOS reports `/dev/stdin`, `/dev/stdout` and `/dev/fd/N` with
/// the inode of the file behind the descriptor but the device number of its `fdesc`
/// filesystem, so the `stat` identity of `--archive /dev/stdout` never equals the
/// identity of the file standard output is redirected to, and every guard comparing
/// the two was blind to that spelling. Opening such a path duplicates the descriptor,
/// and the duplicate's metadata carries the real device. The open is tried read-only
/// and then write-only — without create or truncate — because the descriptor's own
/// access mode decides which one succeeds; if neither does, the `stat` identity
/// stands. Only a regular file is opened, and only under `/dev/fd`, where nothing a
/// caller can create is a FIFO that an open would block on.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    if meta.is_file() && is_reached_through_dev(path) {
        if let Some(identity) = opened_identity(path) {
            return Some(identity);
        }
    }
    identity_of(&meta)
}

/// The identity of `path` as the input read reaches it, or `None` if that read
/// cannot reach it.
///
/// [`file_identity`] opens a `/dev` spelling whichever way its descriptor allows, so it
/// answers for a write-only descriptor the input read can never open. That read opens
/// read-only, and so does this: `-i /dev/stdin 0>>a.tkfd` has no input to protect, and
/// the honest failure is the one the read itself reports. Any other path is its `stat`
/// identity, as in [`file_identity`].
fn input_identity(path: &Path) -> Option<(u64, u64)> {
    reached_identity(path, |path| fs::File::open(path))
}

/// The identity of `path` as the archive write reaches it, or `None` if that write
/// cannot reach it.
///
/// The write opens write-only, without create or truncate, and so does this. On macOS
/// a `/dev/fd/N` spelling of a read-only descriptor refuses that open, so the archive
/// can destroy nothing through it — `--archive /dev/stdin < log.json` passes a
/// rejected input through as `0.0.1` did, and a successful pass fails on the write
/// instead of being called a collision.
fn archive_identity(path: &Path) -> Option<(u64, u64)> {
    reached_identity(path, |path| fs::OpenOptions::new().write(true).open(path))
}

/// The identity of `path` through `open` when it is a regular file reached through
/// `/dev`, or its `stat` identity otherwise; `None` if either fails.
fn reached_identity(
    path: &Path,
    open: impl FnOnce(&Path) -> io::Result<fs::File>,
) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    if meta.is_file() && is_reached_through_dev(path) {
        let file = open(path).ok()?;
        return file.metadata().ok().as_ref().and_then(identity_of);
    }
    identity_of(&meta)
}

/// Whether `path`, with every symlink resolved, is a descriptor spelling under
/// `/dev/fd` — on macOS also where `/dev/stdin`, `/dev/stdout` and `/dev/stderr` resolve.
///
/// Only `/dev/fd`, the prefix [`inspect_archive_slot`] routes to the descriptor path: a
/// regular file elsewhere under `/dev` (Linux's `/dev/shm`) has a real path, and asking
/// it through an open made a read-only one unreachable by the archive write, so a
/// rejected pass named as its own input deleted that input.
fn is_reached_through_dev(path: &Path) -> bool {
    fs::canonicalize(path).is_ok_and(|resolved| resolved.starts_with("/dev/fd"))
}

/// The identity of the file an open of `path` yields, read-only or else write-only.
fn opened_identity(path: &Path) -> Option<(u64, u64)> {
    let file = fs::File::open(path)
        .or_else(|_| fs::OpenOptions::new().write(true).open(path))
        .ok()?;
    file.metadata().ok().as_ref().and_then(identity_of)
}

/// The identity of the regular file standing behind standard input, or `None` when
/// there is no such file — stdin is a pipe, a terminal or a closed descriptor — or
/// when the platform exposes no identity.
///
/// No path is involved and no `unsafe` is needed: the descriptor is duplicated into
/// an owned one through `AsFd`, wrapped in a `File`, and asked for its metadata; the
/// duplicate closes on drop and the original is not read from. Only a regular file
/// is reported, because that is the only kind of stdin an `--archive` path can name
/// and the only kind the guard's refusal describes — and only when the descriptor can
/// read: `0>>file` opens it write-only, nothing is read from it, and the honest
/// failure is the `Bad file descriptor` the first real read reports. A zero-length
/// `read` asks the descriptor without consuming anything: it fails with `EBADF` on a
/// write-only descriptor and returns `0` on a readable one.
fn stdin_identity() -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd as _;
        let identity = regular_file_identity(io::stdin().as_fd())?;
        let fd = io::stdin().as_fd().try_clone_to_owned().ok()?;
        (&fs::File::from(fd))
            .read(&mut [])
            .ok()
            .filter(|&n| n == 0)?;
        Some(identity)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The identity of the regular file standing behind standard output, on the terms
/// [`stdin_identity`] gives for standard input — and only when that descriptor can
/// write: `1<file` opens it read-only, nothing written through it can reach the file,
/// and the honest failure is the `Bad file descriptor` the first real write reports.
/// A zero-length `write` asks the descriptor without changing the file: it fails with
/// `EBADF` on a read-only descriptor and returns `0` on a writable one.
///
/// The file type is settled first and the probe is made only on a regular file. On
/// anything else a zero-length `write` is not a no-op: a datagram socket delivers it
/// as an empty record ahead of the real output, and a terminal stops a background job
/// under `stty tostop` although the command may never write to it at all.
fn stdout_identity() -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd as _;
        let identity = regular_file_identity(io::stdout().as_fd())?;
        let fd = io::stdout().as_fd().try_clone_to_owned().ok()?;
        // `write_all(&[])` would return before reaching the system call.
        (&fs::File::from(fd)).write(&[]).ok().filter(|&n| n == 0)?;
        Some(identity)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The identity of the regular file behind `fd`, through an owned duplicate that
/// closes on drop; `None` for anything else.
#[cfg(unix)]
fn regular_file_identity(fd: std::os::fd::BorrowedFd<'_>) -> Option<(u64, u64)> {
    let file = fs::File::from(fd.try_clone_to_owned().ok()?);
    let meta = file.metadata().ok()?;
    if meta.is_file() {
        identity_of(&meta)
    } else {
        None
    }
}

/// Whether the document arriving on standard input is the very file at `archive`.
///
/// The same `(dev, ino)` comparison [`is_same_file`] makes for `--input`, reached
/// through the descriptor instead of a path, so on Unix `< log.json` and `--input
/// log.json` are refused alike. `false` whenever stdin has no identity to compare — see
/// [`stdin_identity`] — which keeps the guard out of the way of every pipe.
///
/// Split out of [`ensure_archive_is_not_the_input`] for the reason
/// [`removal_is_still_the_inspected_file`] gives: a condition inside an `ensure!`
/// body is invisible to this workspace's mutation testing.
fn stdin_is_the_file_at(archive: &Path) -> bool {
    stdin_identity().is_some_and(|stdin| archive_identity(archive) == Some(stdin))
}

/// Whether standard output is redirected into the very file at `archive` — the
/// comparison [`stdin_is_the_file_at`] makes, on the other stream. `false` for a pipe,
/// a terminal, `/dev/null` and every other non-regular file.
fn stdout_is_the_file_at(archive: &Path) -> bool {
    stdout_identity().is_some_and(|stdout| archive_identity(archive) == Some(stdout))
}

/// Whether two command-line paths designate one and the same file.
///
/// Asking the filesystem for each side's identity is what makes this robust, and it
/// is strictly stronger than comparing resolved paths: two *hard links* to one inode
/// are different canonical paths and the same file, and only the identity sees that.
/// `--input real.json --archive alias.json` over one inode would otherwise sail past
/// this guard and let the archive overwrite its own input.
///
/// Each side is asked the way its own operation reaches it — `a` as the input read
/// opens it, `b` as the archive write opens it — through [`input_identity`] and
/// [`archive_identity`]. A side that operation cannot reach is not the same file as
/// anything: the operation fails on its own terms, and a rejected pass, which never
/// writes the archive, passes the input through.
///
/// A path that does not exist, a dangling symlink and a symlink loop are never the
/// same file either. If the input is one of them the read fails with the real reason
/// — `No such file or directory` rather than a collision — and if the archive is one
/// of them there is no file there yet to destroy.
///
/// On a platform with no identity this degrades to comparing canonical paths, which
/// sees symlinks and relative spellings but not hard links.
///
/// Only a regular file can be destroyed by the archive: a successful pass refuses to
/// write anything else, and a rejected one leaves anything else untouched. So two
/// spellings of one device or pipe — `--input /dev/null --archive /dev/null` — are not
/// the same *file* here, and are passed through as `0.0.1` passed them.
fn is_same_file(a: &Path, b: &Path) -> bool {
    let is_a_file = |path: &Path| fs::metadata(path).is_ok_and(|meta| meta.is_file());
    if !is_a_file(a) || !is_a_file(b) {
        return false;
    }
    #[cfg(unix)]
    {
        input_identity(a)
            .zip(archive_identity(b))
            .is_some_and(|(a, b)| a == b)
    }
    #[cfg(not(unix))]
    {
        matches!(
            (fs::canonicalize(a), fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
    }
}

/// What a rejected pass finds sitting at the `--archive` path.
enum ArchiveSlot {
    /// Nothing is there — the ordinary first-run case.
    Empty,
    /// A regular file whose first four bytes are the `TKFD` magic.
    ///
    /// That is a **necessary, not a sufficient** condition for the file to be an
    /// archive `expand` can read back: a note that happens to open with the word
    /// `TKFD` lands here too, and `expand` would reject it. The check is
    /// deliberately the looser of the two — see [`clear_stale_archive`] for why
    /// over-inclusion is the safe direction at this path.
    ///
    /// `target` is the `--archive` path with every symlink resolved — the file that
    /// was inspected, and therefore the only file the removal may act on. `identity`
    /// pins the inode behind it, so the removal can refuse a target that was swapped
    /// after the check.
    Archive {
        target: PathBuf,
        identity: Option<(u64, u64)>,
    },
    /// A regular file that is not a `TKFD` archive: different bytes, or too short to
    /// carry the magic at all.
    Foreign,
    /// A FIFO, socket or device. It is never opened or read, so nothing is known about
    /// its bytes — only that no later `expand` can read an archive back from it.
    NotAFile,
    /// A regular file behind a descriptor spelling that refuses to be opened for
    /// writing — on macOS, a descriptor open read-only. The archive write cannot reach
    /// it, so it is not a slot a successful pass would have filled; it is never read,
    /// so nothing is known about its bytes.
    Unwritable,
    /// A regular file that opens with the magic but is reached through a descriptor
    /// spelling — on macOS `/dev/fd/N`, `/dev/stdin`, `/dev/stdout`, `/dev/stderr` —
    /// so no name for it is known and it cannot be removed.
    Unremovable,
}

/// Whether the file the removal is about to unlink is still the one that was
/// inspected.
///
/// Split out of [`clear_stale_archive`] because the window it guards opens and
/// closes entirely inside that function: nothing driving the binary from the
/// outside can observe it, and the check itself sits in an `ensure!` body, which
/// this workspace's mutation testing cannot reach. Both blind spots overlapped, and
/// deleting the check outright left the whole end-to-end suite green.
///
/// `None` means the platform exposes no file identity. There is then nothing to
/// compare, and this degrades to the unguarded removal rather than refusing every
/// removal — the check is a narrowing of a race, not a permission.
fn removal_is_still_the_inspected_file(target: &Path, identity: Option<(u64, u64)>) -> bool {
    identity.is_none() || file_identity(target) == identity
}

/// Clear the recovery archive an *earlier* run left at `path`, so a rejected pass
/// cannot leave one behind — and clear nothing else.
///
/// This is the fix for the worst outcome this binary could produce. `compress`
/// treats a `CompressError` as a passthrough and exits `0` when this function is
/// content with the slot; a previous,
/// successful run's archive at the same path used to survive that, and it still
/// verified its own checksum. A pipeline that re-ran `compress` and then `expand`
/// therefore saw two clean exits and reconstructed a *different* document, with no
/// signal anywhere that it was stale — silent, undetectable substitution.
///
/// The deletion is keyed to the magic, and the magic is a *necessary* condition for
/// the hazard, not a sufficient one: only a file carrying it can be expanded into a
/// stale document, but carrying it does not make a file expandable. The check is
/// over-inclusive on purpose, in that direction: a file *without* the magic is left
/// byte-for-byte as it was, and stderr says so. What it is not is a promise about
/// files the user did not name: `compress --archive notes.md` leaves `notes.md`
/// alone only while `notes.md` does not open with `TKFD`. Nothing stale survives
/// either way, because `expand` fails closed with exit `3` on anything it cannot
/// verify.
///
/// This used to be justified with "a successful pass truncates that path anyway, so
/// deleting is never wider than what success does". **That is false, measured** —
/// the two paths do not use the same syscalls. It was false in two ways; the path
/// spelling is now asked the way success asks it, and the permissions remain:
///
/// * **Permissions.** Success is [`write_file`], which `open`s the file for writing
///   and needs write permission on *the file*. This function needs write permission
///   on *the directory*. A mode-`444` archive in a writable directory therefore
///   survives every successful pass — exit `2`, "Permission denied" — and is deleted
///   by the first rejected one, at exit `0`. The mirror runs the other way: an archive
///   in a read-only directory is rewritten in place by a successful pass and cannot be
///   unlinked by a rejected one, which then exits `2` with the archive intact and
///   nothing on stdout — loud, and never a stale answer.
/// * **Path spelling.** Success `stat`s the path as typed, and a trailing `/` or `/.`
///   after a file is `ENOTDIR` there. This now asks the typed path the same way before
///   it resolves anything, so `--archive a.tkfd/` is an empty slot and deletes nothing
///   — it used to be resolved by `realpath` to `a.tkfd` and deleted, which also let
///   `--input x --archive x/` delete its own input.
///
/// The deletion is still the right behaviour, but on its own merits rather than by
/// containment: an archive that outlives the run it belonged to is a *silent* wrong
/// answer — two clean exits and a different document — while a file this deletes that
/// success could not have replaced is a loud one, announced on stderr. Widening a
/// loud failure to prevent a silent one is the trade this makes; it is not the
/// absence of a trade. See the `--archive` section of the crate README, which states
/// the same boundary for users.
///
/// Three things keep "only such a file" honest, and each closes a way the naive
/// spelling gets it wrong:
///
/// * The path is **resolved first**, so a symlink at `--archive` has the archive it
///   resolves to cleared, through however many links. Removing the link instead would leave that archive readable
///   while stderr claimed it was gone — the exact silent substitution above, with a
///   reassuring message on top.
/// * A FIFO, a socket or a device is **never opened**. Neither can hold a readable
///   archive, and opening a FIFO blocks until a writer appears. A directory is the
///   one non-file this refuses to skip: it is opened on purpose, so the read fails
///   with the platform's own diagnostic rather than silently reporting nothing to
///   clear at a path that plainly is not empty.
/// * The **identity is re-checked** immediately before the unlink, so a target
///   swapped after the magic was read is refused rather than deleted.
///
/// A missing file is the normal case, not an error. Any other I/O failure is fatal
/// and propagates: emitting the passthrough while a stale archive still sat there
/// would ship exactly the corruption this clearing exists to prevent.
fn clear_stale_archive(path: &Path) -> Result<Cleared> {
    match inspect_archive_slot(path)? {
        ArchiveSlot::Empty => Ok(Cleared::Empty),
        ArchiveSlot::Archive { target, identity } => {
            // The check read an open handle; the removal names a path, and the two
            // are only the same file for as long as nobody moves anything in
            // between. Re-reading the identity immediately before the unlink turns
            // an unbounded window into a vanishing one, so a swapped target is
            // refused instead of deleted. It is a narrowing, not a proof: nothing
            // here is atomic, and `never_compress`-style fidelity safeguards are not
            // security boundaries. Where the platform exposes no identity the check
            // is vacuous and this degrades to the unguarded removal.
            ensure!(
                removal_is_still_the_inspected_file(&target, identity),
                "the file at {} was replaced between the archive check and its \
                 removal; nothing was deleted",
                path.display()
            );
            fs::remove_file(&target)
                .with_context(|| format!("removing stale archive {}", path.display()))?;
            // Reported now, not with the announcement: the file is gone whether or
            // not the passthrough that follows succeeds. When the typed path is itself
            // a symlink, the removal left it in place, dangling, and took the file it
            // resolved to — so name that file, not the link that is still there. Say
            // "resolves to", not "points at": through a chain `l1 -> l2 -> real` the
            // link typed points at `l2`, which survives, and it is `real` that went. A
            // symlink further up the path needs no such care: the typed path then
            // names the removed file and no longer resolves.
            let typed_is_a_link =
                fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
            if typed_is_a_link {
                warn(format_args!(
                    "tokfold: removed {}, the file the symlink {} resolves to: it \
                     opens with the tokfold archive magic",
                    target.display(),
                    path.display()
                ));
            } else {
                warn(format_args!(
                    "tokfold: removed {}: it opens with the tokfold archive magic",
                    path.display()
                ));
            }
            Ok(Cleared::Removed)
        }
        ArchiveSlot::Unremovable => bail!(
            "a file opening with the tokfold archive magic is open behind {}, which \
             names a descriptor rather than a file, so it cannot be removed",
            path.display()
        ),
        ArchiveSlot::Foreign => {
            warn(format_args!(
                "tokfold: left {} untouched: it is not a tokfold archive",
                path.display()
            ));
            Ok(Cleared::Foreign)
        }
        ArchiveSlot::NotAFile => {
            warn(format_args!(
                "tokfold: left {} untouched: it is not a regular file",
                path.display()
            ));
            Ok(Cleared::Foreign)
        }
        ArchiveSlot::Unwritable => {
            warn(format_args!(
                "tokfold: left {} untouched: it cannot be opened for writing",
                path.display()
            ));
            Ok(Cleared::Foreign)
        }
    }
}

/// What [`clear_stale_archive`] found at the archive path, for the announcement that
/// follows a successful passthrough.
#[derive(Clone, Copy)]
enum Cleared {
    /// Nothing was there.
    Empty,
    /// A stale archive was there and has been removed.
    Removed,
    /// A file that is not an archive, not a regular file, or not writable through the
    /// spelling given was there and has been left alone.
    Foreign,
}

/// Tell the caller that a rejected pass wrote no archive. Called only once the
/// passthrough has reached stdout, because the line says it did.
fn announce_no_archive(path: &Path, cleared: Cleared) {
    match cleared {
        Cleared::Empty => warn(format_args!(
            "tokfold: no recovery archive written at {} (the input was passed \
             through unchanged)",
            path.display()
        )),
        Cleared::Removed | Cleared::Foreign => warn(format_args!(
            "tokfold: no recovery archive written (the input was passed through \
             unchanged)"
        )),
    }
}

/// Classify the file at `path` by its opening bytes, reading no more of it than the
/// magic is long.
///
/// The magic is [`tokfold_core::format::MAGIC`] itself rather than a copy of it, so
/// a change to the archive header cannot leave this check looking for a header the
/// engine no longer writes.
fn inspect_archive_slot(path: &Path) -> Result<ArchiveSlot> {
    // Ask the path as typed first, the way the successful write reaches it. When `stat`
    // finds nothing there (`ENOENT`), or refuses a trailing `/` or `/.` after a file
    // (`ENOTDIR`), the path names no file this pass could replace, so it names none it
    // may delete either. `canonicalize` would normalise the separator away and hand the
    // removal the file behind it: `--input x --archive x/` deleted its own input at exit
    // `0`, although every same-file guard, asking `stat`, saw no archive. Any other
    // failure — a symlink loop, a name too long, an unsearchable parent — is reported
    // below and ends the pass with exit `2`, which the crate README documents.
    match fs::metadata(path) {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(ArchiveSlot::Empty);
        }
        _ => {}
    }

    // Resolve before anything else, so the file that is inspected and the file that is
    // removed are the same one. The success path opens the path for writing with create
    // and truncate, which follows symlinks and rewrites the *target*; the target is
    // therefore what a rejected pass has to clear. `fs::remove_file` does not follow
    // the final component, so acting on the typed path would unlink the link and leave
    // the stale archive behind it fully readable — while stderr reported it removed.
    let target = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ArchiveSlot::Empty),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("inspecting the archive path {}", path.display()));
        }
    };

    // On macOS `realpath` of a `/dev/fd/N` (or `/dev/stdin`, `/dev/stdout`,
    // `/dev/stderr`) open on a regular file does not return that file's path: it
    // returns `/dev/fd/<the file's basename>`, which names nothing — or, for a file
    // called `0`, `1` or `2`, a *different* descriptor. The typed path is the only
    // spelling that reaches the file, and it cannot be unlinked, so a stale archive
    // behind it is refused rather than left in place. Only `/dev/fd` is routed here:
    // a regular file elsewhere under `/dev` (Linux's `/dev/shm`) has a real path and
    // is cleared like any other, and Linux resolves `/dev/fd/N` to the file's own
    // path, so this branch is reached on macOS alone.
    if target.starts_with("/dev/fd") {
        return inspect_descriptor_slot(path);
    }

    let meta = fs::symlink_metadata(&target)
        .with_context(|| format!("inspecting the archive path {}", path.display()))?;

    // A directory falls through to the open below on purpose: `File::open` succeeds
    // on one and the read then fails with the platform's own diagnostic, which is
    // what a caller wants to see. Everything else that is not a regular file — FIFO,
    // socket, device — cannot hold bytes a later `expand` would read back, and must
    // not be opened blind: opening a FIFO blocks until a writer appears, which would
    // hang the one path this binary promises always completes.
    if !meta.is_file() && !meta.is_dir() {
        return Ok(ArchiveSlot::NotAFile);
    }

    let mut file = match fs::File::open(&target) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ArchiveSlot::Empty),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("inspecting the archive path {}", path.display()));
        }
    };

    let mut head = [0u8; ARCHIVE_MAGIC.len()];
    match file.read_exact(&mut head) {
        Ok(()) => {}
        // Shorter than the magic, so it cannot be an archive whatever else it is.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(ArchiveSlot::Foreign),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("inspecting the archive path {}", path.display()));
        }
    }

    if head == ARCHIVE_MAGIC {
        let identity = file.metadata().ok().as_ref().and_then(identity_of);
        Ok(ArchiveSlot::Archive { target, identity })
    } else {
        Ok(ArchiveSlot::Foreign)
    }
}

/// Classify an archive path that resolves under `/dev/fd`: a descriptor spelling that
/// stands for a file, pipe or device already open in this process.
///
/// `metadata` follows the descriptor to the file behind it, so a FIFO, socket or
/// device is still never opened, and a directory is opened and read on purpose, as on
/// the plain path, so the read reports the platform's own diagnostic.
///
/// A regular file is first asked the way the successful write reaches it: a write-only
/// open, without create or truncate. On macOS that open duplicates the descriptor and
/// is refused for one open read-only (`< a.tkfd`), which a successful pass therefore
/// cannot write either; a stale archive is something that pass would have replaced,
/// so there is none to clear, and the rejected input passes through as in `0.0.1` —
/// the same answer the same-file guard gives through [`archive_identity`]. That is asked
/// before the length, so every read-only descriptor gets the same answer and the same
/// line on stderr whatever its file holds. A file the write does reach is read for the
/// magic through the typed path, the one spelling that reaches it. One that cannot be
/// read (a descriptor open write-only on a file of four bytes or more) is an error: it
/// may hold a stale archive, and a rejected pass never passes through beside one it
/// could not rule out.
///
/// The magic is read at offset `0`, not at the current position. Opening a `/dev/fd`
/// spelling on macOS duplicates the descriptor, and a duplicate shares its file offset:
/// a plain read took four bytes from the caller's own stream (`{ tokfold compress
/// --archive /dev/stdin …; cat; } < notes` lost them from `cat`), and read at wherever
/// that stream stood — past the magic, or at the end of a stdin already consumed — so
/// an archive was classified as foreign and reported "not a tokfold archive".
fn inspect_descriptor_slot(path: &Path) -> Result<ArchiveSlot> {
    let context = || format!("inspecting the archive path {}", path.display());
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ArchiveSlot::Empty),
        Err(e) => return Err(e).with_context(context),
    };
    if !meta.is_file() && !meta.is_dir() {
        return Ok(ArchiveSlot::NotAFile);
    }
    if meta.is_file() {
        if fs::OpenOptions::new().write(true).open(path).is_err() {
            return Ok(ArchiveSlot::Unwritable);
        }
        // Too short for the magic is settled by the length alone. That matters here,
        // not only as a shortcut: a descriptor open write-only — `3>file`, `2>log` —
        // refuses to be opened for reading through its `/dev/fd` spelling, and the file
        // a `>` redirect has just truncated is exactly such a one.
        if meta.len() < ARCHIVE_MAGIC.len() as u64 {
            return Ok(ArchiveSlot::Foreign);
        }
    }
    let mut head = [0u8; ARCHIVE_MAGIC.len()];
    let read = fs::File::open(path)
        .and_then(|file| read_head_at_start(&file, &mut head))
        .map(|()| head == ARCHIVE_MAGIC);
    match read {
        Ok(true) => Ok(ArchiveSlot::Unremovable),
        Ok(false) => Ok(ArchiveSlot::Foreign),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(ArchiveSlot::Foreign),
        Err(e) => Err(e).with_context(context),
    }
}

/// Fill `head` from the start of `file` without moving the offset it shares with any
/// duplicate of its descriptor.
#[cfg(unix)]
fn read_head_at_start(file: &fs::File, head: &mut [u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt as _;
    file.read_exact_at(head, 0)
}

/// No `/dev/fd` spelling is routed here off Unix; a fresh handle starts at `0`.
#[cfg(not(unix))]
fn read_head_at_start(mut file: &fs::File, head: &mut [u8]) -> io::Result<()> {
    file.read_exact(head)
}

/// `expand`: reconstruct and emit the exact original bytes. Fail-closed — a
/// `DecompressError` exits `3` and never emits best-effort bytes.
///
/// Every failure names where the archive came from. Without it, a script that
/// expands many archives in a loop reports "cannot expand" with no way to tell which
/// one, and an archive read from standard input is not distinguishable from a file
/// at all.
fn cmd_expand(args: &ExpandArgs) -> Result<ExitCode> {
    ensure_output_is_not_the_input(args.input.as_deref())?;
    let source = source_label(args.input.as_deref());
    let archive = read_source(args.input.as_deref())?;

    if archive.is_empty() {
        // Zero bytes really do fail the magic check, but "bad magic" sends the reader
        // hunting for corruption inside a file that has no content at all. The usual
        // cause is an archive that was never written — a `compress` that fell back to
        // passthrough, or a redirect that created the file before the writer failed.
        // There is a third cause, and it is the one an operator is most likely to be
        // staring at: `write_file` truncates before it writes, so
        // an overwrite that fails partway — a full volume, an `RLIMIT_FSIZE` — leaves a
        // truncated or empty file where a *previously good* archive was. Nothing here
        // can tell those apart after the fact, which is why the message names the size
        // rather than guessing the cause.
        warn(format_args!(
            "tokfold: cannot expand {source}: the archive is empty (0 bytes)"
        ));
        return Ok(ExitCode::from(EXIT_CORRUPT));
    }

    let compressor = Compressor::new(Config::default());

    match compressor.decompress(&archive) {
        Ok(original) => {
            write_stdout(&original)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            warn(format_args!("tokfold: cannot expand {source}: {err}"));
            Ok(ExitCode::from(EXIT_CORRUPT))
        }
    }
}

/// `stats`: report the pass without emitting the payload. Unlike `compress`, an input
/// the compressor rejects is a hard error here (there is nothing to measure): report
/// it and exit `EXIT_BAD_INPUT`.
fn cmd_stats(args: &StatsArgs) -> Result<ExitCode> {
    ensure_output_is_not_the_input(args.input.as_deref())?;
    let input = read_source(args.input.as_deref())?;
    let compressor = Compressor::new(config_for(args.profile));

    match compressor.compress(&input) {
        Ok(artifact) => {
            let report = format_stats(&artifact.stats)?;
            write_stdout(report.as_bytes())?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            warn(format_args!("tokfold: cannot compute stats: {err}"));
            Ok(ExitCode::from(EXIT_BAD_INPUT))
        }
    }
}

/// `mcp`: serve the engine as MCP tools on stdin/stdout until the client hangs up.
///
/// The notice goes out before the transport starts, and to stderr, for two separate
/// reasons: an operator who never reads further has still seen it, and stdout belongs
/// entirely to the protocol — one JSON-RPC message per line, nothing else — so a
/// friendly banner there would corrupt the very first exchange.
fn cmd_mcp() -> Result<ExitCode> {
    ensure_output_is_not_the_input(None)?;
    warn(format_args!("{}", tokfold_mcp::EXPERIMENTAL_NOTICE));
    let mut server = tokfold_mcp::Server::new();
    tokfold_mcp::stdio::serve_stdio(&mut server).context("mcp: the stdio transport failed")?;
    Ok(ExitCode::SUCCESS)
}

/// Build a [`Config`] from the shared profile flag; all other knobs stay at their
/// defaults (16 MiB input ceiling, depth 512, heuristic estimator).
fn config_for(profile: ProfileArg) -> Config {
    Config::builder().profile(profile.into()).build()
}

/// Read the whole input from `path`, or from standard input when `path` is `None`.
fn read_source(path: Option<&Path>) -> Result<Vec<u8>> {
    if let Some(path) = path {
        // A directory reaches `fs::read` as a platform-specific errno — "Is a
        // directory" on Unix, "Access is denied" on Windows — and the Windows
        // spelling in particular sends the reader off to check permissions on a
        // path whose real problem is that it is not a file. Name the mistake.
        ensure!(
            !path.is_dir(),
            "input path {} is a directory, not a file",
            path.display()
        );
        fs::read(path).with_context(|| format!("reading input file {}", path.display()))
    } else {
        // Read through a duplicate of descriptor 0 on Unix, for the reason
        // [`write_stdout`] writes through a duplicate of descriptor 1: `io::stdin()`
        // treats `EBADF` as end of input, so a standard input open but not readable
        // (`tokfold compress 0>>file`) read as an empty document — `compress` passed
        // nothing through and exited `0` (with the stale-archive clearing, deleting a
        // valid `--archive` on the way), and `expand` reported a bad magic. The duplicate returns the kernel's `EBADF`
        // and the command exits `2`.
        #[cfg(unix)]
        let mut input = {
            use std::os::fd::AsFd as _;
            fs::File::from(
                io::stdin()
                    .as_fd()
                    .try_clone_to_owned()
                    .context("reading standard input")?,
            )
        };
        #[cfg(not(unix))]
        let mut input = io::stdin().lock();
        let mut buf = Vec::new();
        input
            .read_to_end(&mut buf)
            .context("reading standard input")?;
        Ok(buf)
    }
}

/// Name a byte source for a diagnostic: the path it came from, or standard input.
fn source_label(path: Option<&Path>) -> String {
    path.map_or_else(
        || "standard input".to_owned(),
        |path| path.display().to_string(),
    )
}

/// Write `bytes` to standard output and flush, so a downstream reader that consumes the
/// whole stream sees the entire payload before the process exits.
///
/// A reader that closes the pipe early (`… | head`, a model harness that stops reading)
/// surfaces as [`io::ErrorKind::BrokenPipe`], because Rust starts every process with
/// `SIGPIPE` ignored — or, when standard output is a socket whose peer has gone, as
/// [`io::ErrorKind::ConnectionReset`], see [`is_reader_gone`]. That is a routine end of consumption, not a `tokfold` failure, so
/// it resolves to a clean exit — otherwise a `set -o pipefail` shell or an agent harness
/// that inspects the exit code would misread a normal early close as an error (exit 2).
///
/// A standard output that is *closed* before the process starts
/// (`tokfold compress >&-`) is not a write failure either, and nothing here can make
/// it one: Rust's start-up code reopens a closed descriptor 0, 1 or 2 on `/dev/null`
/// before `main` runs, so by the time this function writes, stdout *is* `/dev/null`
/// — measured on macOS by comparing the device and inode of a `try_clone_to_owned`
/// of stdout under `>&-` with those of `/dev/null`. The write succeeds, the payload
/// goes nowhere, and the exit is `0`; an `--archive` file is still written.
///
/// A standard output that is *open but not writable* (`tokfold compress 1<file`) is
/// a write failure, and on Unix this reports it as one. `io::stdout()` cannot: it
/// treats `EBADF` as success, because that is how it tolerates a closed descriptor,
/// so writing through it exited `0` with the payload gone. This writes through a
/// duplicate of descriptor 1 instead, which returns the `EBADF` the kernel gives, and
/// the command exits `2`. Every write this binary makes to stdout goes through here —
/// including `--help` and `--version` text not bound for a terminal, see
/// [`parse_failure`] — except `mcp`, whose transport writes through its own duplicate
/// for the same reason, so bypassing the standard handle's buffer loses no ordering.
fn write_stdout(bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    let mut out = {
        use std::os::fd::AsFd as _;
        fs::File::from(
            io::stdout()
                .as_fd()
                .try_clone_to_owned()
                .context("writing to standard output")?,
        )
    };
    #[cfg(not(unix))]
    let mut out = io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if is_reader_gone(e.kind()) => Ok(()),
        Err(e) => Err(e).context("writing to standard output"),
    }
}

/// Whether a write failed only because the reader of the stream has gone.
///
/// A pipe whose reader has closed fails the write with `EPIPE`. A socket whose peer has
/// closed fails it with `EPIPE` too — every case measured on macOS did, including a
/// peer that closed with unread data queued — but a platform that reports the reset
/// such a peer sends fails it with `ECONNRESET` instead. Both mean the same thing to
/// `tokfold` — nobody is reading any more — so both end the run cleanly.
fn is_reader_gone(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    )
}

/// Write one diagnostic line to standard error, dropping it if it cannot be delivered.
///
/// Every diagnostic tokfold itself formats goes through here instead of `eprintln!`,
/// because `eprintln!` *panics* when the write fails. clap renders its own usage
/// errors, but off a terminal [`usage_failure`] writes them the same way this does:
/// one `write_all`, and a failed write dropped. Stderr is routinely the stream a caller closes first — a harness that
/// keeps stdout and drops stderr, a `2>` redirect into a reader that exits — and Rust
/// starts every process with `SIGPIPE` ignored, so that close comes back as an error
/// rather than a signal. Panicking on it costs the run its exit code: a `compress` that
/// passed its input through correctly, and a `stats` that correctly reported a
/// rejection, would both report `101` and an "Uncaught panic" instead of the code the
/// caller dispatches on. Losing the text of a diagnostic nobody is reading is the right
/// outcome; losing the exit code is not.
///
/// The line is formatted first and written with one `write_all`. Standard error is
/// unbuffered, so `writeln!` straight to it issues one `write` per formatting
/// fragment — a message with an argument reached a reader as three or four pieces,
/// which a reader that takes each `write` as a record (a datagram socket, a logger
/// reading a pipe one `read` at a time alongside other writers) sees as that many
/// broken lines.
fn warn(args: fmt::Arguments<'_>) {
    let _ = io::stderr()
        .lock()
        .write_all(format!("{args}\n").as_bytes());
}

/// Write `bytes` to `path`, replacing any existing file.
///
/// A path that already exists and is neither a regular file nor a directory is refused
/// before anything is opened, which is the same classification — and the same reason —
/// that [`inspect_archive_slot`] applies on the rejected path. Opening a FIFO for
/// writing blocks until a reader appears, so `--archive` pointed at one would hang the *winning* path
/// forever, having emitted neither the archive nor the rendering; a device or a socket
/// holds no bytes a later `expand` could read back, so writing to one is never what the
/// caller meant. Directories fall through deliberately, so the platform's own "Is a
/// directory" error is what the caller sees.
fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    // `metadata` follows symlinks, so a link aimed at a FIFO is refused too, and it
    // only ever `stat`s: unlike `open`, that cannot block on any of the file types it
    // is here to reject.
    match fs::metadata(path) {
        Ok(meta) => ensure!(
            meta.is_file() || meta.is_dir(),
            "refusing to write the archive to {}: it is not a regular file",
            path.display()
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e)
                .with_context(|| format!("inspecting the archive path {}", path.display()));
        }
    }
    let context = || format!("writing archive to {}", path.display());
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(context)?;
    // An open that truncates does not always empty the file. On macOS `/dev/fd/N`
    // (and `/dev/stdout`, `/dev/stderr`) duplicates the descriptor instead of opening
    // the file: the truncation is ignored and the descriptor's offset and append mode
    // carry over, so the archive landed on top of — or after — the bytes already there
    // and a clean exit left a file `expand` rejects. Refuse before writing anything.
    let emptied = file.metadata().with_context(context)?.len() == 0
        && file.stream_position().with_context(context)? == 0;
    ensure!(
        emptied,
        "refusing to write the archive to {}: opening it did not empty it",
        path.display()
    );
    file.write_all(bytes).with_context(context)
}

/// Render [`Stats`] as a fixed-order, aligned, human-readable report.
///
/// Field order and formatting are deterministic (fixed `{:.4}` ratios, integer
/// counts), so the same input always produces byte-identical output.
fn format_stats(stats: &Stats) -> Result<String> {
    let mut out = String::new();
    let w = STATS_LABEL_WIDTH;
    writeln!(out, "{:<w$}{}", "bytes before:", stats.bytes_before)?;
    writeln!(out, "{:<w$}{}", "bytes after:", stats.bytes_after)?;
    writeln!(out, "{:<w$}{:.4}", "byte ratio:", stats.byte_ratio())?;
    writeln!(
        out,
        "{:<w$}{}",
        "est tokens before:", stats.est_tokens_before
    )?;
    writeln!(out, "{:<w$}{}", "est tokens after:", stats.est_tokens_after)?;
    writeln!(out, "{:<w$}{:.4}", "token ratio:", stats.token_ratio())?;
    writeln!(
        out,
        "{:<w$}{} (id {})",
        "encoder:",
        encoder_name(stats.encoder.0),
        stats.encoder.0
    )?;
    writeln!(
        out,
        "{:<w$}{} (id {})",
        "tokenizer:",
        tokenizer_name(stats.tokenizer_id),
        stats.tokenizer_id
    )?;
    Ok(out)
}

/// Human-readable name for a frozen encoder id, or `unknown` for a value this build
/// does not recognize.
///
/// The numbers are wire constants: an archive written by any build records the id it
/// used, so a name may be added here but an existing pairing may never change.
fn encoder_name(id: u8) -> &'static str {
    match id {
        0 => "passthrough",
        1 => "e1-minify",
        2 => "e2-tabular",
        _ => "unknown",
    }
}

/// Human-readable name for a frozen tokenizer id, or `unknown` for a value this build
/// does not recognize.
///
/// Same rule as [`encoder_name`]: the ids are recorded in archives, so they are
/// append-only.
fn tokenizer_name(id: u16) -> &'static str {
    use tokfold_core::estimator::ids;
    match id {
        ids::HEURISTIC => "heuristic",
        ids::BYTE_LEN => "byte-length",
        ids::CL100K_BASE => "cl100k-base",
        ids::O200K_BASE => "o200k-base",
        ids::HUGGING_FACE => "hugging-face",
        _ => "unknown",
    }
}

/// The reversible context-compression engine for LLM agents.
#[derive(Debug, Parser)]
#[command(
    name = "tokfold",
    version,
    long_about = None,
    after_help = ROOT_STREAM_AND_EXIT_HELP
)]
struct Cli {
    /// Which operation to run.
    #[command(subcommand)]
    command: Command,
}

/// The subcommand set. `mcp` is experimental: it works, but it is not hardened.
#[derive(Debug, Subcommand)]
enum Command {
    /// Compress input to a token-reduced rendering (and an optional recovery archive).
    #[command(after_help = STREAM_AND_EXIT_HELP)]
    Compress(CompressArgs),
    /// Reconstruct the exact original bytes from a recovery archive.
    #[command(after_help = STREAM_AND_EXIT_HELP)]
    Expand(ExpandArgs),
    /// Report what a compression pass achieves, without emitting the payload.
    #[command(after_help = STATS_STREAM_AND_EXIT_HELP)]
    Stats(StatsArgs),
    /// EXPERIMENTAL Model Context Protocol server on stdio. Unhardened; not for
    /// production secrets.
    Mcp,
}

/// Arguments for `compress`.
#[derive(Debug, Args)]
struct CompressArgs {
    /// Read input from this file instead of standard input.
    #[arg(short, long, value_name = "PATH")]
    input: Option<PathBuf>,

    /// Also write the binary recovery archive to this path.
    ///
    /// The path belongs to tokfold. A successful pass overwrites what is there when it
    /// can -- it replaces a regular file whatever it contains but refuses a FIFO, a
    /// socket or a device, refuses a path whose opening does not empty it (on macOS, a
    /// /dev/fd/N, /dev/stdin, /dev/stdout or /dev/stderr spelling of a descriptor
    /// already open on a file), and exits 2 on a path it cannot write -- and a pass the passthrough
    /// encoder wins is a successful pass: it still writes an archive. Only a pass the
    /// engine
    /// *rejects* writes none, and that pass deletes what is there when the file opens
    /// with the TKFD archive magic, so a rejected pass does not leave an earlier
    /// run's archive at the path it was asked to write; any other file it can inspect
    /// is left untouched and reported on stderr, and so, on macOS, is any file behind a
    /// descriptor spelling the archive write cannot open. That deletion can still fail
    /// the command: a slot that cannot be inspected or cleared exits 2 and emits
    /// nothing, as does a file of four bytes or more behind a write-only descriptor
    /// spelling, which cannot be read to tell. So can
    /// writing the passthrough to standard output. Because a successful pass
    /// overwrites the path, it may not name the same file as --input, nor (on Unix) the
    /// regular file standard input reads from or the regular file standard output
    /// writes to, whether spelled as its own path or as a /dev/stdin, /dev/stdout or /dev/fd/N
    /// path; each is refused with exit 2 before tokfold reads or writes anything. A
    /// `>` redirect has already emptied its file by then -- the shell truncates it
    /// before tokfold starts -- so the refusal saves a `>>` or `1<>` target and the rest
    /// of the run, not the bytes `>` removed. A pipe carries bytes and no file, so `cat
    /// log.json | tokfold compress --archive log.json` is not refused: a successful pass
    /// replaces log.json with its archive, and a rejected one leaves it untouched unless
    /// it opens with the archive magic, in which case it is deleted. Standard error redirected into the archive is not checked.
    ///
    /// What lands there is not a protective wrapper. An archive is a header of 43 to
    /// 46 bytes followed by the original bytes verbatim -- not encrypted, not
    /// encoded, not obfuscated -- so it is exactly as sensitive as the input it came
    /// from, and larger than it by exactly that header: the saving is in tokens, on
    /// stdout, never on disk. Choose its path with the care you would give the
    /// input's own.
    #[arg(long, value_name = "PATH")]
    archive: Option<PathBuf>,

    /// Which encoders may compete for the rendering.
    #[arg(long, value_enum, default_value = "balanced")]
    profile: ProfileArg,
}

/// Arguments for `expand`.
#[derive(Debug, Args)]
struct ExpandArgs {
    /// Read the archive from this file instead of standard input.
    #[arg(short, long, value_name = "PATH")]
    input: Option<PathBuf>,
}

/// Arguments for `stats`.
#[derive(Debug, Args)]
struct StatsArgs {
    /// Read input from this file instead of standard input.
    #[arg(short, long, value_name = "PATH")]
    input: Option<PathBuf>,

    /// Which encoders may compete when measuring the pass.
    #[arg(long, value_enum, default_value = "balanced")]
    profile: ProfileArg,
}

/// CLI mirror of [`Profile`], so the enum stays an implementation detail of the core
/// crate and gains its command-line spelling here.
#[derive(Debug, Copy, Clone, ValueEnum)]
enum ProfileArg {
    /// Minification only — the smallest, safest change to the text.
    Conservative,
    /// Minification plus tabular re-encoding. The default.
    Balanced,
    /// Every shipped encoder (identical to `balanced` in v0.0.1).
    Aggressive,
}

impl From<ProfileArg> for Profile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::Conservative => Self::Conservative,
            ProfileArg::Balanced => Self::Balanced,
            ProfileArg::Aggressive => Self::Aggressive,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the two id→name tables.
    //!
    //! Nearly everything else in this binary is covered end to end from
    //! `tests/cli.rs`, which is the better place to assert behaviour; the exceptions
    //! are here and in `reader_gone_tests` and `archive_identity_tests` below. The
    //! two tables are one exception:
    //! they translate *frozen wire ids*, and `stats` can only ever print the ids the
    //! shipped configuration produces. Nothing on the command line selects a token
    //! estimator, so ids 1–4 have no end-to-end path at all — yet an archive written by
    //! another build (or by a library caller that swapped the estimator) can carry them,
    //! which is precisely why the pairings must not drift. Testing them here is the only
    //! way to pin them. The exit-code line every screen that claims the contract
    //! repeats — all but `mcp`, which is excluded on purpose — is another:
    //! `exit_codes!()` spells its numbers inside a string literal, so
    //! `EXIT_BAD_INPUT` and `EXIT_CORRUPT` could change without a single screen
    //! following, and `tests/cli.rs` can only check the screen against the numbers
    //! the process returns one case at a time.

    use super::{EXIT_BAD_INPUT, EXIT_CORRUPT, encoder_name, tokenizer_name};
    use tokfold_core::estimator::ids;

    #[test]
    fn the_help_spells_the_exit_codes_the_binary_returns() {
        let help = exit_codes!();
        assert!(help.starts_with("Exit codes: 0 success"), "{help}");
        let bad_input = format!("; {EXIT_BAD_INPUT} usage, I/O, or an input rejected by `stats`; ");
        assert!(help.contains(&bad_input), "{help}");
        let corrupt = format!(
            "; {EXIT_CORRUPT} a corrupt, empty, or otherwise unrecoverable archive on `expand`."
        );
        assert!(help.ends_with(&corrupt), "{help}");
    }

    #[test]
    fn encoder_ids_keep_their_frozen_names() {
        assert_eq!(encoder_name(0), "passthrough");
        assert_eq!(encoder_name(1), "e1-minify");
        assert_eq!(encoder_name(2), "e2-tabular");
    }

    #[test]
    fn an_unknown_encoder_id_is_named_rather_than_hidden() {
        // A build that meets an archive from a newer writer must still print a report;
        // the id itself is always shown alongside, so nothing is lost.
        assert_eq!(encoder_name(3), "unknown");
        assert_eq!(encoder_name(u8::MAX), "unknown");
    }

    #[test]
    fn tokenizer_ids_keep_their_frozen_names() {
        assert_eq!(tokenizer_name(ids::HEURISTIC), "heuristic");
        assert_eq!(tokenizer_name(ids::BYTE_LEN), "byte-length");
        assert_eq!(tokenizer_name(ids::CL100K_BASE), "cl100k-base");
        assert_eq!(tokenizer_name(ids::O200K_BASE), "o200k-base");
        assert_eq!(tokenizer_name(ids::HUGGING_FACE), "hugging-face");
    }

    #[test]
    fn the_five_tokenizer_ids_are_five_distinct_names() {
        // A copy-paste that pointed two ids at one name would still satisfy the table
        // test above if the expectation were copied with it.
        let names = [
            tokenizer_name(ids::HEURISTIC),
            tokenizer_name(ids::BYTE_LEN),
            tokenizer_name(ids::CL100K_BASE),
            tokenizer_name(ids::O200K_BASE),
            tokenizer_name(ids::HUGGING_FACE),
        ];
        let mut sorted = names;
        sorted.sort_unstable();
        let mut deduped = sorted.to_vec();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            names.len(),
            "duplicate tokenizer name: {names:?}"
        );
    }

    #[test]
    fn an_unknown_tokenizer_id_is_named_rather_than_hidden() {
        assert_eq!(tokenizer_name(5), "unknown");
        assert_eq!(tokenizer_name(u16::MAX), "unknown");
    }
}

#[cfg(test)]
mod reader_gone_tests {
    //! Unit test for the one predicate that decides which standard-output failures
    //! end a run cleanly. A socket peer that resets instead of closing cannot be
    //! staged portably from `tests/cli.rs`, so the pairing is pinned here.

    use super::is_reader_gone;
    use std::io::ErrorKind;

    #[test]
    fn only_a_gone_reader_is_a_clean_end() {
        assert!(is_reader_gone(ErrorKind::BrokenPipe));
        assert!(is_reader_gone(ErrorKind::ConnectionReset));
        for kind in [
            ErrorKind::ConnectionAborted,
            ErrorKind::NotConnected,
            ErrorKind::PermissionDenied,
            ErrorKind::WriteZero,
            ErrorKind::Other,
        ] {
            assert!(!is_reader_gone(kind), "{kind:?}");
        }
    }
}

#[cfg(test)]
mod archive_identity_tests {
    //! Unit tests for the filesystem-identity guard on the archive slot.
    //!
    //! These live here rather than in `tests/cli.rs` because the race they pin —
    //! the target being swapped between the magic read and the unlink — cannot be
    //! staged from outside the process: it has to happen between two calls the
    //! binary makes back to back. The end-to-end behaviour they protect is covered
    //! from `tests/cli.rs`; what is unit-tested here is the predicate alone.

    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{file_identity, removal_is_still_the_inspected_file};
    use std::fs;
    use std::path::{Path, PathBuf};

    /// The identity of a file that exists, or `None` only where the platform hands
    /// out no identities at all.
    ///
    /// The tests below skip themselves on `None`, and that skip is legitimate on
    /// exactly one platform. Left unasserted it made every one of them pass in
    /// silence the moment `file_identity` stopped working — the module that claims to
    /// be the predicate's only unit coverage was the module that could disable itself
    /// without saying so.
    fn identity_or_skip(path: &Path) -> Option<(u64, u64)> {
        let identity = file_identity(path);
        assert!(
            identity.is_some() || cfg!(not(unix)),
            "a file that exists has an identity on this platform: {}",
            path.display()
        );
        identity
    }

    /// A private, empty directory to work in.
    ///
    /// `CARGO_TARGET_TMPDIR` is only set for integration tests and benches, so a
    /// binary crate's own unit tests have to build their scratch space by hand.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tokfold-cli-unit-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the scratch directory");
        dir
    }

    /// Where the platform hands out no identity there is nothing to compare, and
    /// refusing every removal would break the clearing on that platform rather than
    /// protect it. The guard has to be vacuous, and vacuous on purpose.
    #[test]
    fn a_platform_without_file_identity_refuses_nothing() {
        let dir = scratch_dir("identity_absent");
        let archive = dir.join("recovery.tkfd");
        fs::write(&archive, b"TKFD").expect("write the archive");

        assert!(
            removal_is_still_the_inspected_file(&archive, None),
            "an unknown identity must degrade to the unguarded removal"
        );
    }

    /// The ordinary case, and the one that must never be refused: nothing moved
    /// between the magic read and the unlink.
    #[test]
    fn an_untouched_target_is_still_the_inspected_file() {
        let dir = scratch_dir("identity_unchanged");
        let archive = dir.join("recovery.tkfd");
        fs::write(&archive, b"TKFD").expect("write the archive");

        let Some(identity) = identity_or_skip(&archive) else {
            return; // no identity on this platform; covered by the test above
        };
        assert!(
            removal_is_still_the_inspected_file(&archive, Some(identity)),
            "a file nobody touched was refused"
        );
    }

    /// The race the guard exists for: the path now resolves to a *different* file
    /// from the one whose opening bytes were read. Deleting it would destroy
    /// something that was never inspected, so it has to be refused.
    #[test]
    fn a_target_swapped_after_the_check_is_refused() {
        let dir = scratch_dir("identity_swapped");
        let inspected = dir.join("inspected.tkfd");
        let planted = dir.join("planted.tkfd");
        fs::write(&inspected, b"TKFD").expect("write the inspected file");
        fs::write(&planted, b"TKFD").expect("write the planted file");

        let Some(inspected_identity) = identity_or_skip(&inspected) else {
            return;
        };
        assert!(
            !removal_is_still_the_inspected_file(&planted, Some(inspected_identity)),
            "a different file at the target was accepted for removal"
        );
    }

    /// A target that disappeared is not the file that was inspected either. This is
    /// also the only case where the re-read itself fails, so it pins that a missing
    /// identity on the *target* side is a refusal rather than the vacuous arm.
    #[test]
    fn a_target_that_vanished_after_the_check_is_refused() {
        let dir = scratch_dir("identity_vanished");
        let archive = dir.join("recovery.tkfd");
        fs::write(&archive, b"TKFD").expect("write the archive");

        let Some(identity) = identity_or_skip(&archive) else {
            return;
        };
        fs::remove_file(&archive).expect("remove the archive");

        assert!(
            !removal_is_still_the_inspected_file(&archive, Some(identity)),
            "a target that no longer exists was accepted for removal"
        );
    }
}
