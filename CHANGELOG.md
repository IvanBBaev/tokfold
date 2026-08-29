# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the version is below 1.0 the public API is unstable and may change in any
release, without a deprecation window.

## [0.0.1] - Partially released

First version of the workspace, and the first one to leave the repository. There
is no upgrade path from anything earlier because there is nothing earlier.

On **npm**, four of the five prebuilt-binary packages were published on
2026-08-27: `tokfold-darwin-arm64`, `tokfold-darwin-x64`, `tokfold-linux-x64-gnu`
and `tokfold-linux-arm64-gnu`. The fifth was refused: npm answered `PUT
tokfold-win32-x64` with "Package name triggered spam detection", twice, two days
apart, from the same token that had just published its four siblings. The package
is therefore named **`tokfold-windows-x64`**, which is the name esbuild, turbo and
git-cliff all use for the same reason. The `tokfold` launcher is published last by
design, so until the Windows package lands **`npm i -g tokfold` does not work**.
The release is not complete and this entry will get a real date when it is.

On **crates.io** nothing has been published, so there is no `cargo add tokfold`
and no docs.rs page.

### Added

#### `tokfold-core` — the engine

- Sans-io compression engine: `Compressor::compress` and `Compressor::decompress`,
  configured through `Config` / `ConfigBuilder` (`profile`, `max_input_bytes`,
  `max_depth`, `estimator`, `min_saving_bps`). No file, network or clock access.
- JSON parser over a flat tape that preserves object key order, duplicate keys,
  array order and number lexemes byte-for-byte. Numbers are never round-tripped
  through `f64`.
- Binary recovery archive format version 1: `TKFD` magic, a versioned header
  (`format::Header`, `format::Flags`) and a SHA-256 checksum verified on decode.
  Decoding is fail-closed — an integrity failure yields a `DecompressError` and no
  output.
- Encoders competing for the rendering: passthrough, whitespace minification (E1),
  and shape-deduplicated tabular re-encoding (E2, whose rows are rendered as
  minified JSON). Selection is by estimated token count.
- Token estimators behind the `TokenEstimator` trait: `HeuristicEstimator` (the
  default; a pure arithmetic scanner with no model weights) and `ByteLenEstimator`.
- Opt-in, non-default `tiktoken` feature adding exact GPT tokenizer estimators
  `Cl100kEstimator` (`cl100k_base`) and `O200kEstimator` (`o200k_base`). It embeds
  BPE tables, is off by default, and does not change the archive format.
  `HeuristicEstimator::MEASURED_OVER_CLAIM_BPS` records the heuristic's measured
  over-claim but is not applied by default.
- Opt-in minimum-saving margin (`ConfigBuilder::min_saving_bps`, basis points).
  Left unset it falls back to the estimator's declared `over_claim_bps`, which is
  `0` for both estimators shipped here — so the default behaviour is unchanged.
- `never_compress`: a versioned list of rules marking content that must be copied
  verbatim instead of re-encoded. This is a fidelity safeguard, not a security
  control and not an injection filter.
- `compress` is total on valid JSON: when no encoder improves on the input it
  returns a passthrough artifact rather than an error. Invalid input (including
  `NaN`/`Infinity`, truncation, trailing garbage) returns a `CompressError`; the
  engine never repairs input.
- `Fidelity`, `Stats` (`byte_ratio`, `token_ratio`), `EncoderId` and `Profile` in
  the public surface, plus a `roundtrip` property-test suite with committed
  proptest regression seeds and an oracle test suite.

#### `tokfold-cli` — the `tokfold` binary

- `tokfold compress` — emit the token-reduced rendering, optionally persisting a
  recovery archive with `--archive PATH`. An input the engine rejects is forwarded
  unchanged and the command still exits `0`.
- `tokfold expand` — reconstruct the original from a recovery archive; fail-closed.
- `tokfold stats` — report what a compression pass achieves without emitting the
  payload.
- `tokfold mcp` — start the experimental MCP stdio server (see below). It exits `0`
  when the client hangs up; earlier in development this subcommand was an
  unimplemented stub that always exited `69`.
- `--input PATH` on all three data subcommands, `--profile` on `compress` and
  `stats`.
- Normative exit codes: `0` success, `2` bad input (usage, I/O, or an input the
  compressor rejects on `stats`), `3` a corrupt or unrecoverable archive on
  `expand`. A downstream that closes the pipe early is treated as a clean exit.

#### `tokfold-mcp` — EXPERIMENTAL MCP server

- Model Context Protocol stdio server exposing `tokfold_compress`,
  `tokfold_decompress` and `tokfold_estimate` as tools, line-framed, one JSON-RPC
  message per line.
- Both protocol eras are served: the `initialize` handshake for clients on
  `2025-11-25` and earlier, and stateless per-request metadata plus
  `server/discover` for `2026-07-28`.
- All protocol logic sits behind `Server::handle_line`, a pure text-to-text
  function; the stdio loop only supplies the streams. JSON, base64 and the
  JSON-RPC envelope are implemented in-crate, so the crate adds no dependencies.
- Each inbound line is wrapped in a `catch_unwind` bulkhead, so one malformed
  message becomes an `INTERNAL_ERROR` reply instead of ending the session. This
  relies on panics unwinding; the workspace deliberately does not set
  `panic = "abort"`.
- An experimental notice is written to stderr on startup. The server is unhardened
  and unaudited, it sees everything passed through it, and it is not covered by the
  reversibility guarantees of the engine. Not for production secrets.

#### npm distribution

- `tokfold` on npm: a launcher package carrying no binary of its own, plus one
  package per platform (`tokfold-darwin-arm64`, `tokfold-darwin-x64`,
  `tokfold-linux-x64-gnu`, `tokfold-linux-arm64-gnu`, `tokfold-windows-x64`) each
  holding a single prebuilt executable. They are wired as `optionalDependencies`
  with exact versions, so an install downloads only the binary that matches the
  machine and an unsupported platform fails at run time with an explanation
  rather than failing the install.
- The launcher hands the binary the caller's own file descriptors, so streaming,
  broken pipes and terminal detection behave as they do for the binary run
  directly; it reproduces the child's exit code untouched and re-raises the
  signal the child died of. Its own failures — unsupported platform, a platform
  package that is not installed, a binary that will not start — all exit `1`,
  which is deliberately not one of tokfold's codes.
- A signal addressed to the launcher is forwarded to the binary and the launcher
  outlives it, so no supervisor can kill the launcher and leave the binary
  running on the caller's descriptors. `SIGKILL` is the exception it cannot
  cover.
- Alpine and other musl runtimes are refused by name instead of resolving a
  glibc binary and failing later with an error that mentions neither musl nor
  tokfold. The published Linux binaries reference glibc symbols up to
  `GLIBC_2.34`. The Windows binary links the C runtime statically, so it does
  not require the Visual C++ Redistributable; rustc's default for that target
  would have made `VCRUNTIME140.dll` a run-time dependency.
- Publication is ordered: every platform package first, the launcher last. A
  launcher whose `optionalDependencies` name a package that does not exist
  installs cleanly under npm and pnpm and then fails at run time on the one
  platform whose package is missing — but yarn treats a 404 on an optional
  dependency as fatal during resolution, before `os`/`cpu` filtering can rule
  the package out, so there a single missing platform package breaks the install
  on every platform. The ordering is what keeps a partial release invisible
  instead of broken for everyone.

#### Project-level

- Three-crate workspace (`tokfold-core`, `tokfold-cli`, `tokfold-mcp`), edition
  2024, minimum supported Rust version 1.85, dual-licensed MIT OR Apache-2.0.
- Workspace lints: `unsafe_code` forbidden, clippy `pedantic` and `nursery`
  warned, `unwrap_used` / `expect_used` / `panic` denied.
- CI workflow (`.github/workflows/ci.yml`): each gate is its own top-level job —
  workflow lint, format, clippy, rustdoc, a test pinning the wording of the
  EXPERIMENTAL notice, a `cargo-deny` supply-chain policy gate (vulnerabilities
  and unmaintained crates fail the build; a yanked crate only warns), a
  `cargo package` check, and a launcher gate running the npm suite on Linux,
  Windows and macOS — the shipped JavaScript is the one piece of this release no
  cargo command reads. That suite covers the platform table, the musl refusal,
  the launcher's exit-code and signal contract, and what each of the six npm
  packages would actually publish; Node 18 and 22 both run, because the package
  declares a Node 18 floor. All of it alongside a test matrix over
  Linux/macOS/Windows and two MSRV legs. No gate hangs off a matrix value, so none can be switched off by
  a renamed key; the only condition any of them carries is one that skips the
  gate on the weekly cron. Weekly jobs re-run the suite under the release profile
  and on nightly, and re-scan a fresh advisory database with `cargo-audit`. Every
  invocation that resolves a dependency graph is `--locked`; every action and
  installed tool is pinned by commit SHA or version.
- Crate manifests carry `repository`, `homepage`, `documentation`, `readme`,
  `keywords`, `categories` and an explicit `include` allow list; each publishable
  crate carries its own README and licence texts. The `documentation` URLs point
  at docs.rs pages that do not exist yet — docs.rs builds them on first publish,
  and nothing has been published to crates.io.

### Not included

- No crates.io release. There is no `cargo add`, no crates.io page and no
  docs.rs page. The npm side is partial; see the note at the top of this entry.
- Reserved but unimplemented: legend folding, Hugging Face tokenizer backends
  (the `hf` feature is a placeholder), language bindings, and the MCP *proxy*
  shape (upstream connection, content-addressed archive store, `retrieve` tool).
- No published benchmarks. Numbers will ship with a versioned public corpus and a
  reproducible harness, not before.

[0.0.1]: https://github.com/IvanBBaev/tokfold
