# Privacy

This document covers what tokfold does with the data it is given, and what it does
not do. Every statement names the code that makes it true, or the measurement that
showed it. For the attacks this design does and does not resist, see
[`threat-model.md`](threat-model.md).

A statement below marked **enforced** is checked by CI on every push to `main` and on
every pull request. A statement marked **true today** was read back against the
**working tree**, not against a commit: when this file was last audited (2026-10-01)
the tree held uncommitted changes on top of `main` at `0f759f6` — `git diff
--shortstat` was not empty — and this document is itself untracked. A statement here
can therefore differ from what `main` contains, and nothing fails the build if a
change breaks it. Review is the only guard.
CI is not a required check either: `main` has no branch protection
(`gh api repos/IvanBBaev/tokfold/branches/main/protection` answers "Branch not
protected"), so a failing run reports a broken **enforced** statement but does not
block the change.

The published npm `0.0.1` CLI predates some of what *Files* describes: it has no
stale-archive deletion and no same-file guards (see
[`threat-model.md`](threat-model.md), the paragraph on `main` and the release).

## Summary

- tokfold makes no network connections.
- It sends no telemetry or usage data anywhere.
- It keeps no cache, no history and no log file.
- The only file it writes is the one you name with `--archive` — or, when that
  name is a symlink, the file it resolves to.
- That archive contains your input **verbatim**, unencrypted.
- The MCP server returns that same archive to its client by default.

## Network

**tokfold opens no network connections, in any mode.**

- **Dependencies — enforced, with one deliberate carve-out.** `deny.toml` bans a
  fixed list of *named crates* — HTTP, RPC and WebSocket stacks, socket wrappers and
  DNS resolvers — from the entire workspace dependency graph, and denies tokio's
  `net` feature. It evaluates the graph with `all-features = true`, so optional
  features are covered too, and it allows no source other than crates.io. Read "bans
  socket primitives" as the list and not as the capability: `deny.toml`'s own comment
  says `libc` and `nix` are deliberately *not* banned, because they are the substrate
  of ordinary non-network crates. `libc 0.2.186` is in the default graph today,
  through `sha2 → cpufeatures` (`cargo tree -p tokfold-cli -e normal -i libc`), so
  the raw socket syscalls are reachable from the graph and this gate does not see
  them. What answers that is the first-party bullet below and `nm -u` on the built
  binary, not `cargo deny`.
  The `deny` job in `.github/workflows/ci.yml` runs `cargo deny --locked check` on
  every push to `main` and on every pull request. It does not run on the scheduled
  build.
- **First-party code — true today, not enforced.** `std::net` is part of the standard
  library, and a dependency ban cannot reach it (`deny.toml`, header comment;
  `crates/tokfold-mcp/src/lib.rs`, module docs). No non-test source file in the
  workspace uses `std::net`, `TcpStream`, `UdpSocket` or `std::process::Command`.
  Process spawning lives entirely in the test targets, and it is more than "the
  binary under test": the only source file with a `Command::new` is
  `crates/tokfold-cli/tests/cli.rs` (`grep -rn 'Command::new' crates
  --include='*.rs'`), and it starts three programs — the binary under test
  (`env!("CARGO_BIN_EXE_tokfold")`), `/bin/sh`, to arrange descriptor redirections
  the Rust API cannot express, and `mkfifo`. A `cargo test` run also forks through a
  dev-dependency: `proptest`'s default features include `fork`, which pulls in
  `rusty-fork 0.3.1` (`cargo tree -p tokfold-core -e features --all-features`). None
  of that is compiled into a released binary.
- **The binary.** A macOS arm64 release build of the **working tree** (SHA-1
  `834c01f2…`, built 2026-10-01 with default features — `tokfold-cli` declares no
  `[features]` table of its own, so `--all-features` on that package adds nothing)
  imports none of these symbols: `socket`, `connect`, `getaddrinfo`, `sendto`,
  `bind`, `listen`, `fork`, `execv`, `posix_spawn`. The same holds for the binary npm
  ships for that platform (`tokfold-darwin-arm64` `0.0.1`, `bin/tokfold` SHA-1
  `8e2ed283…`). The measurement is `nm -u <binary>` filtered for those names; it
  matched nothing in either, while the unfiltered lists are 83 undefined symbols for
  the working-tree build and 76 for the published one, so the filter had something to
  reject. Both binaries are stripped, so this says what the binary *imports*, not
  what it contains. The other four platform binaries were not measured.
- **Token counting.** The optional `tiktoken` feature of `tokfold-core` (off by
  default, and not enabled in the release build) compiles its encoding tables into the
  binary through `tiktoken-rs`. It downloads nothing.
- **Installing through npm** uses the network, as any `npm install` does: npm fetches
  the launcher package and one platform package from the registry. That is npm's
  traffic, not tokfold's. See *The npm launcher*.

## Telemetry

**None — true today, not enforced.**

- No crate contains code that collects, stores or transmits usage data.
- No crate reads the clock: no non-test source uses `std::time`, `SystemTime` or
  `Instant`.
- Outputs are therefore free of timestamps, and a run leaves no record of when it
  happened.

## Files

### What the CLI reads

- The input, from `--input <path>` or standard input (`read_source` in
  `crates/tokfold-cli/src/main.rs`). It reads the whole input into memory.
- When `--archive` is given, the CLI also inspects the named path's metadata, and
  reads its first four bytes (the `TKFD` magic). It does this to decide whether a
  previous archive there must be cleared, and whether the path is the same file as
  the input or output (`clear_stale_archive`, `ensure_archive_is_not_the_input` and
  `ensure_output_is_not_the_input` in `main.rs`).

### What the CLI writes

- **Standard output:** the rendering (`compress`), the restored original (`expand`),
  the measurement report (`stats`), or the JSON-RPC replies (`mcp`).
- **Standard error:** diagnostics, all through one function (`warn` in `main.rs`).
  These can include the paths you passed and sizes. The `mcp` subcommand also prints
  its experimental notice there on start-up.
- **The `--archive` path, and nothing else on disk.** `write_file` in `main.rs` is the
  only function that writes a file. Read "the `--archive` path" as *the file that
  path resolves to*: both the write and the deletion below follow a symlink at that
  path to its target.
  - It opens the path with `write`, `create` and `truncate`, and sets no permissions,
    so a new archive gets your process umask's default mode. Measured on macOS: under
    umask `022` a new archive is `0644`, which other local users can read; under
    umask `077` it is `0600`.
  - An existing file keeps its mode when it is overwritten: a file at `0666` stayed
    `0666` under umask `077`.
  - If the archive must be private, set a restrictive umask (`umask 077`) or create
    the file with the mode you want before the first run.
- **One deletion.** When `compress` rejects its input and passes it through, it
  deletes a previous archive at the `--archive` path, so that a stale archive cannot
  be mistaken for this run's. It does this only if the path is a regular file that
  starts with the `TKFD` magic (`clear_stale_archive` in `main.rs`). Two things that
  "only the `--archive` path" does not cover:
  - **A symlink is followed.** The path is resolved first, so `--archive link`
    deletes the file the link resolves to, through however many links, and leaves the
    link itself in place and dangling. The file that goes is therefore not the one
    you named, and standard error says which one went, in full: `tokfold: removed
    <target>, the file the symlink <path> resolves to: it opens with the tokfold
    archive magic`. When the typed path is not itself a symlink the message is the
    shorter `tokfold: removed <path>: it opens with the tokfold archive magic`, and
    then the path named is the file that went.
    Removing the link instead would leave a readable
    archive behind while claiming it was gone, which is the silent substitution this
    deletion exists to prevent. `crates/tokfold-cli/README.md` states the same
    boundary for users; the rustdoc of `clear_stale_archive` derives it.
  - **The magic is the whole test.** A file you never thought of as an archive is
    deleted if it opens with those four bytes: `compress --archive notes.md` leaves
    `notes.md` alone only while `notes.md` does not start with `TKFD`.

### What the archive contains

- The archive is a header of 43–46 bytes followed by the original input
  **verbatim** — not encrypted, not encoded (`tokfold compress --help`, `--archive`).
  The range is the ULEB128 length field's ladder cut off by this binary's 16 MiB
  input ceiling: 43 bytes while the original is under 128, a byte more for every 7
  further bits, 46 from 2 097 152 up to the ceiling.
  `crates/tokfold-cli/README.md` derives that ladder and gives the five measured
  sizes. Do not read the range off `crates/tokfold-core/src/format.rs`'s module
  docs: they give the field layout, in which the length is 1 to 10 bytes, so the
  *layout* admits 43 to 52. Nothing tokfold writes reaches the top of that, and 48 to
  52 is unreachable: `compress` rejects an input longer than `u32::MAX`, whose
  ULEB128 encoding is five bytes, so 47 is the widest header a length can ever be
  written into. `format.rs` draws that distinction itself, in the rustdoc that pins
  `(u64::from(u32::MAX), 47)` as "the longest input the parser addresses".
- Any secret in the input is in the archive, readable with any text viewer. Treat the
  archive exactly as you would treat the input.
- The header's SHA-256 is a checksum of the original. It detects corruption, but it
  is not a signature and hides nothing.

### The library and the MCP server

- `tokfold-core` performs no I/O at all. It works on byte slices it is handed and
  returns byte vectors (`crates/tokfold-core/src/error.rs`, module docs: "Core
  performs no I/O").
- `tokfold-mcp` reads standard input and writes standard output. It persists nothing
  (`crates/tokfold-mcp/src/lib.rs`, module docs: "no persistence, no network").

### Caches and temporary files

- **None at run time.** The CLI writes no file other than the `--archive` path (or,
  through a symlink there, that path's target), and neither library crate writes
  files.
- **A `cargo test` run is a different story, and the bullet above is about the
  shipped binary.** The workspace's own sources name a temporary directory in two
  places: `std::env::temp_dir`, in the `#[cfg(test)]` module `archive_identity_tests`
  in `crates/tokfold-cli/src/main.rs`, and `env!("CARGO_TARGET_TMPDIR")` — a
  directory under `target/`, not the system one — in
  `crates/tokfold-cli/tests/cli.rs`. Neither is compiled into a released binary: one
  is behind `#[cfg(test)]`, the other is in a `tests/` target. Two more writers
  arrive with the dev-dependency graph, so "only in tests, in two places" would
  understate it: `proptest 1.11.0` and `rusty-fork 0.3.1` pull in `tempfile 3.27.0`
  (`cargo tree --workspace -i tempfile`), and proptest saves failing seeds **into the
  source tree** — `crates/tokfold-core/tests/hostile.proptest-regressions` and
  `crates/tokfold-core/tests/roundtrip.proptest-regressions` both exist today. All of
  it is `[dev-dependencies]`, so none of it reaches a published binary.

## What the MCP server sends back

Everything the MCP server returns goes to its client, the agent host. The host
decides what enters the model's context and what it logs.

- **`tokfold_compress`** returns the rendering twice, as `content` text and as
  `structuredContent.rendering`. When there is an archive, it also returns
  `structuredContent.archive`, the archive in base64, and that holds the **complete
  original**. Measured on `{"a":1}`: the reply carried a base64 archive that decodes
  to a `TKFD` header followed by `{"a":1}`. A client that logs or stores tool results
  therefore logs or stores the original too.
- **`tokfold_decompress`** returns the original **twice**, like the bullet above:
  once as `content[0].text` and once as `structuredContent.text`. Measured on the
  built binary (SHA-1 `834c01f2…`) with an archive of
  `{"secret":"hunter2","rows":[{"a":1},{"a":2}]}`: both fields carried that document
  in full. A client that stores only one of the two still stores the whole
  original.
- **`tokfold_estimate`** returns the statistics without the payload (pinned by the
  `estimate_reports_stats_without_the_payload` test in `tools.rs`).
- Every result also carries the server's name and version in `_meta`.
- **The experimental notice is not part of any reply, and the crate does not print
  it.** `EXPERIMENTAL_NOTICE` is a constant in `crates/tokfold-mcp/src/lib.rs`; the
  CLI's `cmd_mcp` (`crates/tokfold-cli/src/main.rs`) writes it to **standard error**
  before the loop starts, so it reaches the terminal or the host's log, not the
  JSON-RPC client. Measured on the built binary (SHA-1 `834c01f2…`) with the input
  closed: standard error held exactly one line, byte-identical to that constant,
  saying the server is unhardened and unaudited and must not be used with production
  secrets.

## Environment variables

**tokfold's own code reads no environment variable at run time** (true today). The
workspace has exactly two `std::env`/`env!` uses outside the integration-test
targets, and neither is a run-time read: `std::env::temp_dir` in
`crates/tokfold-cli/src/main.rs`, which is inside the `#[cfg(test)] mod
archive_identity_tests` and so not in a released binary at all, and
`env!("CARGO_PKG_VERSION")` in `crates/tokfold-mcp/src/protocol.rs`, which cargo
substitutes at compile time (`grep -rn 'std::env\|env!(' crates/*/src`). The test
targets add two more compile-time substitutions (`env!("CARGO_BIN_EXE_tokfold")`,
`env!("CARGO_TARGET_TMPDIR")`) and one run-time reader — `proptest` consults
`PROPTEST_CASES` and its siblings through `env::vars_os` in
`proptest-1.11.0/src/test_runner/config.rs`. The shipped binary does read some
variables through its dependencies and the standard library:

| Variable | Read by | Effect |
|---|---|---|
| `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE`, `TERM`, `CI` | `anstream` (via `anstyle-query`), used by `clap` | Whether help and error text is coloured. |
| `RUST_BACKTRACE`, `RUST_LIB_BACKTRACE` | the standard library, for `anyhow` errors and for panic messages | Whether an `anyhow` error captures a backtrace (the CLI prints errors with `{:#}`, which does not show one) and whether a panic message prints one. |

`clap` reads the command line (`std::env::args_os`). It does not read `COLUMNS` or
`LINES`, because its `wrap_help` feature is not enabled: the workspace `Cargo.toml`
adds `derive` to clap's default features, and `wrap_help` is not one of them
(`cargo tree -p tokfold-cli -e features -i clap`). None of these variables changes
what is compressed, and none is sent anywhere.

The table is an enumeration, so here is how it was derived and what it leaves out.
With default features the only crate in the CLI's graph that reads the environment at
run time is `anstyle-query 1.1.5`, and it names six variables: `CLICOLOR`,
`CLICOLOR_FORCE`, `NO_COLOR`, `TERM`, `CI` and `COLORTERM`. The table carries five of
them. `COLORTERM` is left out because it is read only by
`anstyle_query::truecolor()`, and nothing in this graph calls that function — grepping
`truecolor` across the `anstream`, `clap`, `clap_builder` and `anstyle*` sources
matches only anstyle-query's own definition. The two `RUST_*` variables are the
standard library's, not a dependency's. The optional `tiktoken` feature was not in the
measured graph, so this table does not cover it.

## The npm launcher

The `tokfold` npm package is a small Node.js launcher (`npm/tokfold/bin/tokfold`).
It locates the native binary for your platform and runs it.

- **No install scripts.** Neither the launcher's `package.json` nor any platform
  package's `package.json` has a `scripts` field, so installing runs no tokfold code.
  The platform binary arrives as an `optionalDependencies` entry that npm selects by
  `os` and `cpu`.
- **No network.** `bin/tokfold` requires only `node:fs`, `node:os`,
  `node:child_process` and `lib/resolve.js`; `lib/resolve.js` requires only
  `node:path` and the launcher's own `package.json`. Neither file opens a connection.
- **No environment reads.** Neither file reads `process.env`. The binary is spawned
  with Node's default environment handling, so it inherits the launcher's environment
  unchanged, as it would if run directly.
- **No files written.** `node:fs` is used only to write the launcher's own error
  messages to standard error (`fs.writeSync(2, …)`).
- **Arguments and streams pass straight through.** The binary receives
  `process.argv.slice(2)` and the caller's standard input, output and error
  (`stdio: "inherit"`). The launcher never sees the data.
- **One process report on Linux.** To tell glibc from musl, `isMusl()` in
  `npm/tokfold/lib/resolve.js` calls `process.report.getReport()`. It does this only
  on Linux. Node builds that report in memory, and it includes the process's
  environment variables. The launcher reads one field from it,
  `header.glibcVersionRuntime`, and discards the rest. Nothing is written or sent
  anywhere.

## Keeping this document true

Each statement above names the code that makes it true. A change to one of those
places can falsify a sentence here. Only the network ban on dependencies is checked
mechanically; everything marked "true today" needs a reviewer to re-read this file
when the named code changes.
