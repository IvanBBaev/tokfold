# tokfold

[![ci](https://github.com/IvanBBaev/tokfold/actions/workflows/ci.yml/badge.svg)](https://github.com/IvanBBaev/tokfold/actions/workflows/ci.yml)

An embeddable Rust engine that **reversibly** compresses LLM agent context — tool
outputs, JSON, logs — to cut token cost.

It is an engine, not an app: a deterministic, sans-io library you call at ingestion
to turn bulky context into a denser rendering the model reads, plus a recovery
archive that reconstructs the original on demand. A single `tokfold` binary wraps it
for the command line.

---

## Status

**v0.0.1 — skeleton under active development.**

- The public API is **unstable** and will change without deprecation windows.
- **Not on crates.io.** There is no `cargo add tokfold`, no crates.io page and no
  docs.rs page, because none of those exist yet. To use the engine as a library,
  build from this repository.
- **On npm.** `npm i -g tokfold` installs the CLI. The launcher package carries no
  binary of its own; it declares one package per platform
  (`tokfold-darwin-arm64`, `tokfold-darwin-x64`, `tokfold-linux-x64-gnu`,
  `tokfold-linux-arm64-gnu`, `tokfold-windows-x64`) as optional dependencies, so
  an install pulls down only the package whose `os` and `cpu` match the machine.
  None declares `libc`, so on musl Linux (Alpine) the glibc build is pulled down
  and the launcher then refuses it; on a platform with no package (Windows on
  `arm64`, for one) nothing is pulled down. All six are
  published at `0.0.1` with npm provenance attestations. See `npm/README.md` for
  why the launcher is published last, and for why the Windows package is
  `tokfold-windows-x64` rather than the `win32` name Node itself uses.
  The `0.0.1` packages predate `tokfold hook`, `tokfold bench`, `stats --json` and
  `stats --tokenizer`: those are on `main` and unreleased — see
  [Command line](#command-line).
- The one badge above is the CI badge, and it is narrower than it looks. The
  workflow in `.github/workflows/ci.yml` really does run on every pull request and
  on every push to `main`, and pushes to other branches run nothing — open a pull
  request to get a gate. But the badge URL carries no `?event=` filter, so it shows
  the newest run on `main` whatever triggered it, and that workflow also runs on a
  weekly schedule in which every gate job but `fuzz` is skipped by design (`if:
  github.event_name != 'schedule'`), so only the weekly jobs and `fuzz` (600 seconds
  a target there, 60 on a push or a pull request) run. On 2026-10-05 the badge read
  *passing* for exactly such a run, on `0f759f6`, which predates the `fuzz` job:
  `audit`, `release-test` and `nightly` succeeded, and `actionlint`, `fmt`, `clippy`,
  `docs`, `doc-drift`, `deny`, `package`, `launcher` and `test` were all skipped.
  Since `df6a0fc` a scheduled run also carries `fuzz`, and it is still not the gate
  on the current commit. A green badge therefore says
  the newest run on `main` passed, not that the gate passed on the current commit.
  Adding `?event=push` to the badge URL would narrow it to the runs this bullet used
  to describe; it is not set.
- The engine crate is `tokfold-core`; the CLI binary is `tokfold`; the MCP
  integration (`tokfold-mcp`) serves the engine as tools over stdio and is
  **experimental and unhardened** — it sees everything passed through it. Its
  [Quickstart](crates/tokfold-mcp/README.md#quickstart) covers installing it,
  connecting Claude Code or another stdio client, and a first session.
- **A recovery archive is not a protective wrapper.** At v0.0.1 every archive is a
  passthrough blob: a `TKFD` header of 43 to 46 bytes (47 for an original of
  2^28 bytes or more, which needs a raised `max_input_bytes`) followed by **the original
  bytes verbatim** — not encrypted, not encoded, not obfuscated. An archive is exactly as
  sensitive as its plaintext; store it with the same care. Nor is the header's SHA-256
  a tamper check: it is unkeyed and travels inside the archive it covers, so it
  detects corruption, not tampering — anyone who can modify an archive can recompute
  the digest to match (`tokfold-core`'s `format` module docs).

This README describes what the project *is* and what it *refuses to claim*. It
contains no benchmark numbers on purpose — see [Benchmarks](#benchmarks).

---

## What "reversible" means here — precisely

Reversible means **semantic identity, not byte identity.** Decompressing an archive
reproduces a document that is *equal as a value tree* to the original, not one that
is equal byte-for-byte.

Preserved **exactly**:

- **Object key order** — never reordered.
- **Duplicate keys** — kept, and kept in their original order (never collapsed
  last-wins).
- **Array element order.**
- **Number lexemes**, byte-for-byte. `1.0` comes back as `1.0`, `1e3` as `1e3`,
  a 100-digit integer as those 100 digits. Numbers are never round-tripped through
  `f64`.

Canonicalized (deliberately **not** preserved):

- **Insignificant whitespace.** Reproducing the exact whitespace would mean
  preserving the very bytes we are paid to delete.
- **String escape style.** Strings are compared *after* unescaping, so `"é"` and
  `"\u00e9"` are the same string, and the contract lets a decoder canonicalize
  escapes. None does yet: every 0.0.1 archive is a passthrough blob, so the original
  escaping comes back.
  Lone-surrogate `\uXXXX` escapes are retained as raw lexemes so they survive a
  round trip even though they cannot be unescaped to valid UTF-8.

That is the whole reversibility contract. It is enough to feed the reconstructed
bytes to anything that consumes JSON as data; it is *not* a promise that the
formatting matches.

**What v0.0.1 actually does is stronger than that, and you must not depend on it.**
Every recovery archive this version writes is a passthrough blob — a `TKFD` header of
43 to 46 bytes (47 for an original of 2^28 bytes or more, which needs a raised
`max_input_bytes`) followed by the original bytes verbatim — so `tokfold expand` returns
the input byte-for-byte today, whitespace and escape style included, and
`expand --help` says so.
The reversibility contract is the *floor*: the part of that behaviour a future
encoder is not allowed to take away. An encoder that stores structure instead of
bytes would still have to return the same value tree, and would be free to return
different whitespace.
Code that diffs the reconstruction against the original will pass on 0.0.1 and is
still wrong; compare it as a value tree.

## Why this is not called "lossless"

The word "lossless" is avoided on purpose, and the distinction is the crux of the
project.

The model does not read the reconstruction. **The model reads the compressed
rendering.** So even a perfect byte-for-byte reconstruction of the archive would
prove nothing about whether the model understood the *rendering* it was actually
shown. Byte-reversibility is a property of the recovery path; comprehension is a
property of the read path; they are different things.

The honest claim is therefore two-part:

1. **Reversible** — the archive reconstructs the original value tree, verified by a
   header checksum on decode.
2. **Measured comprehension fidelity** — how well a model reads the rendering, to be
   established with a public corpus and a reproducible harness, not asserted.

"Lossless" would collapse those two into a single word that only covers the first.
We will not do that.

One place does print the word, and it is scoped to the first claim only: the
`fidelity` field in the MCP server's `stats` reads `lossless` (and the library's
`Fidelity::Lossless` behind it), which is the name of the recovery path's
class — the archive reconstructs the value tree — and says nothing about how well
a model reads the rendering.

---

## Guarantees

- **Zero network calls.** The engine never opens a socket. It is sans-io: no file,
  network, or clock access anywhere in the core.
- **Zero telemetry.** Nothing is phoned home, counted, or logged off-box.
- **No ML model.** The default token estimator is a pure arithmetic scanner, not a
  learned model — no weights to download, no inference to run, no megabytes of BPE
  data at rest in the default build. (The opt-in, non-default `tiktoken` feature
  embeds exact GPT BPE tables; see [What is in v0.0.1](#what-is-in-v001--and-what-is-not).)
- **Deterministic output.** The same input bytes under the same configuration produce
  byte-identical output, every time, from one build. Across 32- and 64-bit targets
  that is not established: E2 counts array shapes by an `FxHasher` value, which
  differs between them (see the "Determinism" section of the `tokfold-core`
  `compressor` module docs). Bytes, not the same JSON value:
  the archive keeps the input as written, so reformatting a document changes its
  archive.

### Why determinism is load-bearing, not a nicety

Providers cache prompt *prefixes*: once a run of context bytes has been seen, a
later request that repeats those exact bytes reuses the cached prefix and is billed
at a fraction of the price. If a compressor emits *different* bytes for the *same*
logical content — because a hash iterates in random order, or a timestamp leaks into
the output — the prefix no longer matches, the cache silently misses, and you pay
full price again. Nondeterministic output does not just fail to help; it actively
*costs money* by invalidating a cache you were relying on. Determinism is what makes
the savings survive a multi-turn agent loop.

---

## Compression ratio is a secondary axis

A reversible compressor keeps every byte of information recoverable. By construction
it cannot beat a *lossy* compressor on raw ratio, because a lossy compressor is
allowed to throw information away and tokfold is not. If your only metric is "how
small is the output," a lossy tool will win that column, and it will win it *by
design*. That is not a bug to be closed; it is the cost of the guarantee.

The axes tokfold actually optimizes for are:

1. **Latency** — it is embeddable and starts instantly; in the default build there is
   no model to load and no service to call. (The opt-in `tiktoken` estimator loads its
   embedded BPE tables once on construction.)
2. **Fidelity risk** — bounded, because the transform is reversible and the recovery
   path is checksum-verified.

Ratio is reported as a statistic (`byte_ratio`, `token_ratio`), never as the
headline.

---

## When NOT to use this

Reach for a different tool when its trade-off fits your problem better:

- **You can tolerate loss and want the maximum squeeze on prose → LLMLingua-2.**
  It runs an ML model to drop low-information tokens from natural-language text.
  Better ratio on prose, at the cost of a model and genuinely lossy output.
  tokfold is neither.
- **Your bulk is context you can stash out-of-band and re-fetch on demand →
  headroom (Python).** It pairs lossy compression with out-of-band cache retrieval:
  the payload lives in a cache and is pulled back when needed, rather than being
  reconstructed inline. A different architecture for a different shape of workload.
- **The tokens you want to cut are MCP tool *schemas / definitions*, not tool
  *output* → mcp-compressor.** It shrinks the schema surface an agent is handed.
  tokfold compresses the content flowing *through* those tools, not their
  definitions.
- **You just want noisy dev-command output filtered down (test runners, build
  logs) → rtk.** It filters and trims command output for humans at the terminal.
  That is a different job from reversibly re-encoding structured context for a model.
- **Your problem is context *isolation*, not compression → context-mode.** It
  partitions and scopes what context a step or subagent can see. It does not shrink
  payloads; tokfold does not isolate them.

If two of these describe you, you may want that tool *instead of*, or *alongside*,
tokfold — they are not all mutually exclusive.

---

## Not a security boundary

tokfold is **not** a prompt-injection filter and must not be placed in a threat
model as one.

It does not inspect, sanitize, score, or neutralize adversarial content. It
preserves content faithfully — including hostile content — because faithful
preservation is the entire point. Malicious text that goes in comes back out in the
rendering, unchanged. Compressing context does not reduce injection risk, and this
project will never claim that it does.

---

## Benchmarks

**None published yet.** Numbers will ship with a versioned public corpus and a
reproducible harness.

Until that corpus and harness exist, any ratio, latency, or "cuts tokens by N%"
figure would be a number nobody could reproduce — which is exactly the marketing
dishonesty this project positions itself against. So there are zero performance
numbers in this README, and that is deliberate. When numbers arrive, they will
arrive with the corpus and the code to regenerate them.

---

## Command line

The `tokfold` binary has six subcommands: `compress`, `expand`, `stats`, `mcp`,
`hook` and `bench`. `tokfold --help` and each subcommand's `--help` are the
reference; [`crates/tokfold-cli/README.md`](crates/tokfold-cli/README.md) documents
every subcommand with its exit codes. This section covers the subcommands and flags
added since `0.0.1`. JSON Lines input, which `compress` and `stats` also accept since
then, is described under [Shape of the API](#shape-of-the-api-current-unstable).

**Unreleased.** `tokfold hook`, `tokfold bench`, `stats --json` and
`stats --tokenizer` are on `main` and in no published package. The `0.0.1` binary on
npm refuses each of them as a usage error and exits `2` (`unrecognized subcommand` /
`unexpected argument`) — measured on the registry's `tokfold-darwin-arm64` build. To
use them, build the CLI from this repository:

```sh
cargo build --release -p tokfold-cli   # binary at target/release/tokfold
```

### `tokfold hook` — a Claude Code `PostToolUse` hook

It reads one hook event on standard input. An MCP tool's result reaches a hook as a
string; when the engine accepts that string, an encoder other than passthrough wins,
and the rendering is shorter in bytes than the string, the hook prints a
`hookSpecificOutput` object whose `updatedToolOutput` is the rendering, and Claude
Code shows the model the rendering instead of the result. In every other case it
prints nothing and the model sees the original. A passthrough rendering is the input
behind an 18-byte `⟦tkfd:v1:raw⟧` line, so it is never forwarded. A built-in tool's
result arrives as an object and is left alone, so in practice the hook acts on MCP
tools only. As a hook it exits `0` whatever it is given — a malformed event, an
unreadable standard input or an unwritable standard output leaves the result
unchanged — so it does not fail the tool call it sits in front of; a signal that
kills the process is the one way around that, as for any process. It writes no
recovery archive: what it replaces is what the model is shown.

The rule compares bytes, not tokens, so a replacement can cost more tokens than the
result it replaced. How often that happened on one workload, with the tokenizer it
was counted in, is in the CLI README's
[*Using it as a Claude Code hook*](crates/tokfold-cli/README.md#using-it-as-a-claude-code-hook);
measure your own with `tokfold bench`.

```sh
tokfold hook --print-config
```

prints the `settings.json` fragment that installs it for every MCP tool (event
`PostToolUse`, matcher `mcp__.*`, command `tokfold hook`); merge it into
`~/.claude/settings.json` or a project's `.claude/settings.json`. The command is
looked up on `PATH`, so it must resolve to a build that has `hook`: the npm `0.0.1`
binary exits `2` on it. `--profile` selects the encoders as it does for `compress`
(default `balanced`).

### `tokfold bench <PATH>` — what the hook would save, on your transcripts

`PATH` is a directory of Claude Code transcripts searched recursively for `*.jsonl`
(such as `~/.claude/projects`), or one transcript. For each tool name, `bench`
reports the main-chain tool results, their tokens before and after compression with
`--profile`, the ratio and how many results the hook's rule would replace, with sums
for all tools and for the hook's `mcp__.*` matcher. It then prices those results
against the bill the transcripts record — each request's usage counted once — and
reports the results' and the saving's share of it. The shares mix units (results in
the chosen tokenizer's tokens, the bill in the tokens Claude reported), and the
report says so. Every number is printed beside the tokenizer and the profile it was
measured with; the pricing model is in the CLI README.

It reads only: no standard input, no network, no file written. Symbolic links and
special files inside the tree are skipped and counted, never opened. It prints
aggregates and tool names, never a result, a message or a path inside the tree.
`--tokenizer` takes the values `stats` does, and `--json` prints the report as one
line. This README quotes none of its output (see [Benchmarks](#benchmarks)): a
transcript corpus is one person's workload, and `bench` exists so you can measure
yours.

### `tokfold stats --json` and `stats --tokenizer`

`--json` prints the pass as one line of JSON instead of the aligned text report,
which stays the default. The line is the `stats` object every MCP tool reply
carries — `bytesBefore`, `bytesAfter`, `estTokensBefore`, `estTokensAfter`,
`byteRatio`, `tokenRatio`, `encoder`, `encoderId`, `tokenizerId`, `minSavingBps`,
`fidelity` — with the MCP encoder names (`minify`, `tabular`, `jsonl`,
`passthrough`) and unrounded ratios. A rejected input still exits `2` with nothing on
standard output.

`--tokenizer cl100k` or `--tokenizer o200k` also counts the input and the rendering
with an exact GPT tokenizer — a GPT vocabulary, not Claude's — and adds four
rows to the text report (`real tokens before`, `real tokens after`, `real token
ratio`, `real tokenizer`) and four keys to the JSON line (`realTokensBefore`,
`realTokensAfter`, `realTokenRatio`, `realTokenizerId`); `heuristic`, the default,
adds nothing. It is reporting only: the do-no-harm gate still prices with the
heuristic estimate, so the encoder, every other row and the archive are the ones the
heuristic chose, and an exact ratio above `1.0000` is a rendering the heuristic
priced as a win. On the passthrough path `real tokens after` is set equal to `real
tokens before`, as the estimated `after` figures are (see
[Shape of the API](#shape-of-the-api-current-unstable)).

The exact tokenizers need the CLI's non-default `tiktoken` feature, which embeds
megabytes of BPE tables:

```sh
cargo build --release -p tokfold-cli --features tiktoken
```

A build without it — every default build, and the release workflow builds without
it — refuses `cl100k` and `o200k` with exit `2` before reading any input, for
`stats` and `bench` alike, and never falls back to the heuristic.

---

## Shape of the API (current, unstable)

The public surface today, for orientation only — it will change:

```rust
use tokfold_core::{Compressor, Config};

let engine = Compressor::new(Config::default());

let artifact = engine.compress(input_bytes)?;   // Result<Artifact, CompressError>
send_to_model(&artifact.rendering);              // the denser view the model reads
store(&artifact.archive);                        // versioned recovery blob

let original = engine.decompress(&artifact.archive)?; // Result<Vec<u8>, DecompressError>
```

`compress` is **total on valid JSON within the configured limits** — 16 MiB of input
and 512 levels of nesting by default, both raised through `ConfigBuilder` (the input
limit no further than 4 GiB less one byte, the most the parser's `u32` offsets can
address), and past either one `compress` returns a `CompressError` like any invalid input. If no
encoder reduces the estimated token count, it returns a passthrough artifact with
ratio `1.0`. "Couldn't compress" is a statistic, never an error. That `1.0` is a floor rather than a measurement: a
passthrough rendering still carries the 18-byte `raw` sentinel, which costs 10
estimated (11 real `cl100k`, 13 real `o200k`) tokens more than a bare input that
opens with a non-whitespace byte — leading whitespace merges with the sentinel's
newline and moves each figure by at most one token (10–11, 10–12 and 12–13, measured
over every prefix of up to six spaces, tabs, CRs and LFs before `{}`),
and on that path the `*_after` fields are *set* equal to their `*_before`
counterparts instead of being measured. If you need to account for every token,
measure `Artifact::rendering` directly. Invalid input (including `NaN`/`Infinity`, truncated
documents, or trailing garbage) returns a `CompressError`; the caller then forwards
the original bytes unmodified. The engine never repairs input.

JSON Lines is the one input that is not a single JSON document and is still
accepted (after `0.0.1`, which rejected it): two or more JSON values, one per line,
blank and CRLF lines allowed, each line under the nesting limit on its own. Under
the default `Balanced` profile and `Aggressive` the records are rendered as the
tabular encoder renders the array of them, behind a `⟦tkfd:v1:jsonl⟧` sentinel,
when that saves estimated tokens; `Conservative` passes them through. Either way
the archive holds the original bytes, so `decompress` returns the lines exactly.

### What is in v0.0.1 — and what is not

Present: the parser, the archive format, the token estimator, and the first encoders
(passthrough, whitespace minification, shape-deduplicated tabular re-encoding). An
opt-in, non-default `tiktoken` feature adds exact GPT tokenizer estimators
(`cl100k_base`, `o200k_base`) for the do-no-harm gate; it embeds megabytes of BPE
tables. Turning it on does change what the model reads — pricing candidates exactly
is the whole point of it, and it both suppresses transforms the heuristic over-rated
and enables wins it under-rated — but it never changes the archive, and not because
it is off by default: the archive in v0.0.1 is always a passthrough recovery blob
whose header hardcodes `tokenizer_id = 0` and whose payload is the original bytes
verbatim, whatever estimator priced the candidates, so it comes out byte-identical
with the feature on. `tokfold-core` pins the two halves in
`exact_estimator_changes_encoder_selection_in_both_directions` and
`estimator_choice_does_not_change_recovery_archive`. An experimental MCP server
(`tokfold mcp`) exposes the engine over stdio as three tools — `tokfold_compress`,
`tokfold_decompress` and `tokfold_estimate`. That is the whole catalogue:
`tools::NAMES` in `tokfold-mcp`, whose length and order the unit test
`the_catalogue_lists_every_tool_in_a_fixed_order` pins. The server is unhardened and
unaudited. It shipped in `0.0.1` in that state, labelled rather than held back —
hardening is a milestone still owed, not one this release passed.
Reserved but **not implemented**: legend folding, Hugging Face tokenizer backends,
language bindings, and the MCP *proxy* shape — an upstream connection and a
content-addressed archive store, which needs its own threat model. This is a skeleton;
treat every part of it as subject to change.

---

## Licence

Dual-licensed under either of

- MIT ([LICENSE-MIT](LICENSE-MIT))
- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

**Minimum supported Rust version: 1.85** (edition 2024). MSRV changes are treated as
breaking while the crate is pre-1.0.
