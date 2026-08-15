//! `tokfold` — the one shipped binary for the reversible context-compression engine.
//!
//! Four subcommands sit on top of [`tokfold_core`]:
//!
//! * `compress` — read input, emit the token-reduced rendering, optionally persist a
//!   recovery archive. Compression is an optimization, never a gate: a
//!   [`CompressError`](tokfold_core::CompressError) forwards the original bytes
//!   unchanged and still exits successfully.
//! * `expand` — reconstruct the exact original from a recovery archive. Fail-closed:
//!   any integrity error exits `3` and emits nothing.
//! * `stats` — report what a compression pass would achieve, without emitting the
//!   payload.
//! * `mcp` — serve the engine as Model Context Protocol tools over stdio. It prints
//!   the crate's experimental notice to stderr before serving anything: the server
//!   sits in the secrets path and its hardening audit is a separate, launch-gating
//!   milestone.
//!
//! Exit codes are normative: `0` success, `2` bad input (usage, I/O, or an input the
//! compressor rejects on `stats`), `3` a corrupt or unrecoverable archive on `expand`.
//! `anyhow` is confined to this binary; the library crates never depend on it.
//!
//! `mcp` exits `0` when the client closes the stream — on clean end-of-input and on a
//! broken pipe alike, since a client that hangs up has ended the session rather than
//! failed it. Until v0.0.1 it instead exited `69` unconditionally as an unimplemented
//! stub; that code is gone now that the subcommand does something.

#![forbid(unsafe_code)]

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tokfold_core::{Compressor, Config, Profile, Stats};

/// Bad usage, an I/O failure, or an input the compressor rejects on `stats`.
const EXIT_BAD_INPUT: u8 = 2;

/// A corrupt or otherwise unrecoverable archive handed to `expand`.
const EXIT_CORRUPT: u8 = 3;

/// Left-hand column width for the aligned `stats` report.
const STATS_LABEL_WIDTH: usize = 20;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            // I/O and usage failures land here; `{err:#}` prints the whole context
            // chain on one line. Everything reaching this arm is a bad-input error.
            eprintln!("tokfold: {err:#}");
            ExitCode::from(EXIT_BAD_INPUT)
        }
    }
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
/// and the command still succeeds, because dropping an agent's tool output would be
/// worse than shipping it uncompressed.
fn cmd_compress(args: &CompressArgs) -> Result<ExitCode> {
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
            eprintln!("tokfold: passing input through uncompressed: {err}");
            if args.archive.is_some() {
                eprintln!("tokfold: no recovery archive written (compression did not run)");
            }
            write_stdout(&input)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// `expand`: reconstruct and emit the exact original bytes. Fail-closed — a
/// `DecompressError` exits `3` and never emits best-effort bytes.
fn cmd_expand(args: &ExpandArgs) -> Result<ExitCode> {
    let archive = read_source(args.input.as_deref())?;
    let compressor = Compressor::new(Config::default());

    match compressor.decompress(&archive) {
        Ok(original) => {
            write_stdout(&original)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            eprintln!("tokfold: cannot expand archive: {err}");
            Ok(ExitCode::from(EXIT_CORRUPT))
        }
    }
}

/// `stats`: report the pass without emitting the payload. Unlike `compress`, an input
/// the compressor rejects is a hard error here (there is nothing to measure): report
/// it and exit `EXIT_BAD_INPUT`.
fn cmd_stats(args: &StatsArgs) -> Result<ExitCode> {
    let input = read_source(args.input.as_deref())?;
    let compressor = Compressor::new(config_for(args.profile));

    match compressor.compress(&input) {
        Ok(artifact) => {
            let report = format_stats(&artifact.stats)?;
            write_stdout(report.as_bytes())?;
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            eprintln!("tokfold: cannot compute stats: {err}");
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
    eprintln!("{}", tokfold_mcp::EXPERIMENTAL_NOTICE);
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
        fs::read(path).with_context(|| format!("reading input file {}", path.display()))
    } else {
        let mut buf = Vec::new();
        io::stdin()
            .lock()
            .read_to_end(&mut buf)
            .context("reading standard input")?;
        Ok(buf)
    }
}

/// Write `bytes` to standard output and flush, so a downstream reader that consumes the
/// whole stream sees the entire payload before the process exits.
///
/// A reader that closes the pipe early (`… | head`, a model harness that stops reading)
/// surfaces as [`io::ErrorKind::BrokenPipe`], because Rust starts every process with
/// `SIGPIPE` ignored. That is a routine end of consumption, not a `tokfold` failure, so
/// it resolves to a clean exit — otherwise a `set -o pipefail` shell or an agent harness
/// that inspects the exit code would misread a normal early close as an error (exit 2).
fn write_stdout(bytes: &[u8]) -> Result<()> {
    let mut out = io::stdout().lock();
    match out.write_all(bytes).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e).context("writing to standard output"),
    }
}

/// Write `bytes` to `path`, replacing any existing file.
fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("writing archive to {}", path.display()))
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
#[command(name = "tokfold", version, long_about = None)]
struct Cli {
    /// Which operation to run.
    #[command(subcommand)]
    command: Command,
}

/// The subcommand set. `mcp` is experimental: it works, but it is not hardened.
#[derive(Debug, Subcommand)]
enum Command {
    /// Compress input to a token-reduced rendering (and an optional recovery archive).
    Compress(CompressArgs),
    /// Reconstruct the exact original bytes from a recovery archive.
    Expand(ExpandArgs),
    /// Report what a compression pass achieves, without emitting the payload.
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
    //! Everything else in this binary is covered end to end from `tests/cli.rs`, which
    //! is the better place to assert behaviour. These two tables are the exception:
    //! they translate *frozen wire ids*, and `stats` can only ever print the ids the
    //! shipped configuration produces. Nothing on the command line selects a token
    //! estimator, so ids 1–4 have no end-to-end path at all — yet an archive written by
    //! another build (or by a library caller that swapped the estimator) can carry them,
    //! which is precisely why the pairings must not drift. Testing them here is the only
    //! way to pin them.

    use super::{encoder_name, tokenizer_name};
    use tokfold_core::estimator::ids;

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
