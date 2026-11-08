# Threat model

This document describes the threats tokfold faces in each of the three ways it can
be deployed, and names the code that answers each one. Where nothing answers a
threat, it says so. Its claims were read back against the **working tree**, not
against a commit, and **not all on the same day** — a measurement here carries its
own date, and an undated sentence was read back whenever it was last touched. The
claims measured against a *built binary* are the exception: each of those names the
commit whose bytes it was measured on, because a binary's SHA-1 rots on a commit the
working tree cannot name — see *Where this document can go stale*. The tree is not
clean: at the most recent read-back (2026-10-10) it held uncommitted changes on top
of `main` at `c6f800e`, and `git diff --shortstat` reported 16 files changed. Both of
those move with every commit, so re-derive them (`git log -1 --format=%h`, `git diff
--shortstat`) rather than trusting the two values here. This
document is tracked — `df6a0fc` added it, and that same commit carried a
sentence calling it untracked, false the moment it landed — but no gate leg checks it
against the code, so a sentence here can describe code that `main` does not contain. It is not an audit: the
`tokfold-mcp` crate in particular is declared **EXPERIMENTAL, unhardened and
unaudited** (`crates/tokfold-mcp/src/lib.rs`, `EXPERIMENTAL_NOTICE`), and nothing
here changes that status.

`main` is not the release that npm installs. The published `0.0.1` platform
binaries (registry `gitHead` `81c7681`; `git diff 81c7681 87fb332 -- crates` is empty)
predate the archive-path protections described under *Archive path handling*:
`clear_stale_archive`, `ensure_archive_is_not_the_input`,
`ensure_output_is_not_the_input` and `identity_of` do not exist in
`git show 81c7681:crates/tokfold-cli/src/main.rs`. Measured with the published
`tokfold-darwin-arm64` binary (`bin/tokfold` SHA-1 `8e2ed283…`): a rejected pass left
an earlier archive at the `--archive` path in place, and
`tokfold compress --input r.json --archive r.json` exited `0` and replaced `r.json`
with its archive.

For what tokfold does and does not do with the data it handles — network, files,
environment, telemetry — see [`privacy.md`](privacy.md). For how to report a
vulnerability, see [`SECURITY.md`](../SECURITY.md).

## What tokfold is, in one paragraph

tokfold takes a document (JSON today; anything it cannot parse is passed through)
and produces two things: a **rendering** — a shorter text meant to be read by a
language model — and an optional **recovery archive** — a header followed by the
original bytes verbatim, from which the original is reconstructed exactly. The
rendering is what enters the model's context. The archive is what lets a person
or a tool get back what was really there.

## Deployment modes and trust boundaries

### 1. Library (`tokfold-core`)

- **Shape.** An embedder calls `Compressor::compress` / `Compressor::decompress`
  (re-exported from `crates/tokfold-core/src/lib.rs`) on byte slices it already
  holds.
- **I/O.** None. Core has no file, socket, process or environment access in its
  non-test sources, and no error variant wraps `std::io::Error`
  (`crates/tokfold-core/src/error.rs`, module docs).
- **Trust boundary.** Everything core is handed is untrusted input. That covers the
  document on compress and the archive on decompress. Core trusts the `Config` it is
  built with, and the embedder chooses it.
- **Who writes the input.** The embedder, usually by forwarding tool output. That
  output can contain text written by anyone the tool read from.

### 2. CLI (`tokfold compress | expand | stats`)

- **Shape.** One process per invocation (`crates/tokfold-cli/src/main.rs`). It reads
  one input from `--input` or standard input, writes one payload to standard output,
  and writes diagnostics to standard error. `compress` can also write an archive to
  the path given by `--archive`.
- **Trust boundary.** The invoking user is trusted: they choose the paths and the
  flags. The input's *content* is not trusted.
- **Who writes the input.** Whatever the user redirects in, which is typically tool
  output.

### 3. MCP stdio server (`tokfold mcp`)

- **Shape.** A long-lived process speaking line-framed JSON-RPC on standard input and
  output (`crates/tokfold-mcp/src/stdio.rs`, `serve_stdio`). It exposes three tools:
  `tokfold_compress`, `tokfold_decompress` and `tokfold_estimate`
  (`crates/tokfold-mcp/src/tools.rs`). Nothing a request carries outlives it. The
  `Server` struct (`crates/tokfold-mcp/src/server.rs`) holds three fields, and only
  two are written while requests are handled: the handshake flag and the negotiated
  protocol revision, both set in `initialize` and nowhere else. The third,
  `max_message_bytes`, is a limit fixed at construction through
  `Server::with_max_message_bytes` — the only assignment to it — so no request moves
  it. It is not inert either: it is the bound every answer is rendered against —
  `handle_batch` and `render` in the same file, and the panic-rescue frame in
  `crates/tokfold-mcp/src/stdio.rs` through `Server::max_message_bytes` — so it is
  what decides whether a reply is refused, and two of the readings under *MCP
  transport and protocol* turn on its value: the non-monotonic batch, and the
  `-32602` for an oversized reply. The oversized *request* named in that same bullet
  does not, because the read side holds the incoming line to the `MAX_LINE_BYTES`
  constant in `stdio.rs`, which is this field's default rather than this field — so
  an embedder that raises the field moves one direction only. The server has no
  persistence and opens no connections. Until 2026-10-14 this bullet called the first
  two "the only state it keeps across requests" while citing a struct with three
  fields.
- **Trust boundary.** Every byte on standard input is untrusted. The MCP client (the
  agent host) writes it. The tool arguments inside it are chosen by the model, which
  may be steered by content it has read.
- **What reaches the model.** Nothing directly: the server answers the client, and
  the client decides what enters the prompt. A `tokfold_compress` reply carries the
  rendering twice, as `content` text and as `structuredContent.rendering`. When there
  is an archive, it also carries `structuredContent.archive`, the archive in base64,
  which holds the original bytes. So by default the full original goes back to the
  client beside the rendering (measured 2026-10-10 against a release build of commit
  `166873f` (`cargo build --release -p tokfold-cli`, SHA-1 `834c01f2…`) with `{"a":1}`:
  the reply's `structuredContent` held `compressed`, `stats`, `rendering` and
  `archive`, the `rendering` was the `⟦tkfd:v1:raw⟧` marker plus the input and equal to
  `content[0].text`, and the base64 `archive` decoded to 50 bytes opening `TKFD` and
  ending in `{"a":1}`).
- **Not implemented here.** The *proxy* shape — an upstream connection, a
  content-addressed archive store and a `retrieve` tool — is out of scope by design
  and needs a threat model of its own before any of it is written
  (`crates/tokfold-mcp/src/lib.rs`, module docs). This document does not cover it.

## Assets

| Asset | Why it matters |
|---|---|
| **The original bytes** | Tool output, logs and transcripts routinely contain secrets and personal data. tokfold sees all of it. |
| **The archive** | It holds the original bytes verbatim — not encrypted, not encoded, not obfuscated (`tokfold compress --help`, `--archive`). It is exactly as sensitive as the input. |
| **The rendering's meaning** | The model acts on the rendering. A rendering that reads differently from the original misleads the agent, even if the archive is intact. |
| **Availability of the host** | tokfold runs inside, or next to, an agent. A crash, a hang or a runaway allocation takes the agent's tool path down with it. |
| **Files at the `--archive` path** | `compress` overwrites that path, and can delete it (see *Archive path handling*). |
| **The file standard output is redirected to** | All four subcommands write there: `expand` the original bytes verbatim, `compress` the rendering, `mcp` its JSON-RPC replies, and `stats` a report whose eight lines carry six numbers and a name-with-id for the encoder and the tokenizer (`format_stats` in `crates/tokfold-cli/src/main.rs`) — the one of the four that puts nothing from the input there. `--help` and `--version` land there too, through clap's printer or `write_stdout` depending on the stream (`parse_failure` in the same file). The CLI does not name that file and does not set its mode (see *Archive path handling*). Until 2026-10-14 this row named three of the four subcommands, omitting the only one whose output is content-free. |

## Attacker capabilities considered

- **A1 — hostile document content.** The attacker controls some or all of the text
  that tokfold compresses. For example, they wrote a web page, a log line or an
  issue body that a tool later returned. This is the common case in every mode.
- **A2 — a compression-aware attacker.** A1, plus knowledge of how tokfold renders.
  The attacker shapes content so that it reads differently once folded.
- **A3 — a hostile or confused MCP client.** The attacker controls the bytes on the
  server's standard input: framing, sizes, batch shape and tool arguments. That
  includes a model that has been steered into calling a tool with chosen arguments,
  such as a forged archive.
- **A4 — a hostile archive.** The attacker supplies an archive to `expand` or to
  `tokfold_decompress`. The archive may be corrupted, truncated, or deliberately
  forged with a recomputed checksum.
- **A5 — a compromised dependency.** Code that arrives through the dependency graph.
- **A6 — another local user on the same machine.** The attacker has an account on the
  host and no more: they read, and where the mode allows it write, the files whose
  permissions let them, but they do not run code as tokfold's user. This is the
  shared-machine case that *The archive leaks through file permissions* and *Archive
  path handling* reason about.

Out of scope: an attacker who already runs code as the same user as tokfold. They
can read the input without tokfold's help, which is the reason that case is excluded
and A6 is not — a different local user has no such shortcut, so what tokfold leaves
readable beside the input is a question about tokfold. Until 2026-10-15 this list
stopped at A5 and this note covered only the same-user attacker, while the two
sections named in A6 reasoned about "any local user" and "a shared machine" — a
capability no entry on the list named, under a *Confidentiality* heading that
labelled its rows A1 and A5.

## Threats and mitigations

Each entry names the code that holds the line. "Open" means that nothing in the
code holds it today.

### Availability (A1, A3, A4)

| Threat | Mitigation in code |
|---|---|
| Oversized input to the engine | Core refuses input over `max_input_bytes`, which defaults to 16 MiB (`DEFAULT_MAX_INPUT_BYTES` in `crates/tokfold-core/src/compressor.rs`). The CLI then passes the input through unchanged and exits `0` **unless something on the `--archive` path stops it first**. Measured with a 17,825,802-byte input and no `--archive`: exit `0`, the input echoed byte for byte, and on standard error "tokfold: passing input through uncompressed: input 17825802 bytes exceeds limit 16777216". With `--archive` the passthrough can fail instead, and then nothing reaches standard output at all. Three measured with that same 17,825,802-byte input (2026-10-03): a directory at `--archive` is fatal (exit `2`, 0 bytes on standard output, "the input was rejected (...) and not passed through: inspecting the archive path adir: Is a directory (os error 21)"), a stale archive the process cannot unlink is fatal (`--archive` naming a `TKFD` file in a mode-`555` directory: exit `2`, 0 bytes on standard output, "the input was rejected (...) and not passed through: removing stale archive ...: Permission denied (os error 13)"), and an archive aimed at its own input is refused before the read (exit `2`, 0 bytes on standard output, the input left at its full size). The *shape* of all three — exit `2`, nothing on standard output, and the failing step named in the message — is pinned by a test: `a_directory_at_the_archive_path_of_a_rejected_pass_costs_the_passthrough`, `a_stale_archive_whose_removal_fails_is_named_and_left_intact` and `the_guard_stops_a_rejected_pass_from_deleting_its_own_input`, all in `crates/tokfold-cli/tests/cli.rs`. None of those three pins the case measured here, though: each drives a small invalid-JSON input, so what rejects the document in the test is the parse and not the 16 MiB ceiling, and the oversized reading above rests on the measurement alone. The clearing needs the *directory* writable, which a successful pass does not — see *Archive path handling*. **On the MCP path the input is not passed through, and the ceiling is usually not what the client is told about.** Measured against a release build of commit `166873f` (`cargo build --release -p tokfold-cli`, SHA-1 `834c01f2…`) with a single JSON string argument: `tokfold_compress` is refused with `-32602` and "the reply exceeds the 33554432 byte frame limit" at **both** 16 777 217 bytes (over the engine ceiling) and 16 777 100 bytes (under it), because a passthrough reply carries the text twice — `content[0].text` and `structuredContent` — so the 32 MiB frame limit is reached before the 16 MiB engine ceiling can be reported, and a client never learns which of the two stopped it. Only `tokfold_estimate`, whose reply stays small, surfaces the ceiling: at 16 777 217 bytes it answered `isError: false` with the text "not compressed: input 16777217 bytes exceeds limit 16777216" — a refusal reported as a *successful* result — and at 16 777 100 bytes it answered with an ordinary estimate. |
| Deep nesting in the engine | Core's parser refuses nesting past `max_depth`, which defaults to 512 (`DEFAULT_MAX_DEPTH` in `compressor.rs`; the check is in `open_container` in `crates/tokfold-core/src/tape.rs`). |
| Oversized MCP frame | Messages over `MAX_MESSAGE_BYTES` (32 MiB) are refused and skipped, and the stream resynchronises on the next newline (`read_line` and `skip_to_newline` in `stdio.rs`). Reading goes through `take`, so an unterminated line cannot grow the buffer without bound. The same constant bounds every reply (`render_bounded` in `crates/tokfold-mcp/src/jsonrpc.rs`, applied by `Server::handle_line`), and has done since commit `36b771e`. Both directions compare the message without its newline. |
| A small frame that parses into a huge tree | The MCP JSON parser caps materialised values at `json::MAX_NODES` (`MAX_MESSAGE_BYTES / 8`) and nesting at `json::MAX_DEPTH` (128) (`crates/tokfold-mcp/src/json.rs`). The frame limit alone does not bound memory, and `MAX_MESSAGE_BYTES`'s rustdoc says so. |
| A panic takes down the server | Every inbound line runs inside `catch_unwind` (`handle` in `stdio.rs`) and a panic becomes an `INTERNAL_ERROR` reply addressed to the recovered id. Unwinding stays enabled for this: `panic = "abort"` is deliberately not set (workspace `Cargo.toml`, profile comment). An allocation failure aborts rather than unwinds, so it bypasses the bulkhead; that is why `MAX_NODES` exists. |
| Panics in the first place | Workspace lints: `unsafe_code = "forbid"`; clippy `unwrap_used`, `expect_used` and `panic` are `deny`, and `indexing_slicing` is `warn` (workspace `Cargo.toml`, `[workspace.lints]`). Each crate root also carries `#![forbid(unsafe_code)]`. |
| Regex denial of service | The `never_compress` table matches case-folded literal substrings, with no regex (`crates/tokfold-core/src/never_compress.rs`, module docs). No regex engine is in the default graph. Two arrive with the optional `tiktoken` feature of `tokfold-core` (off by default and not enabled in the release build), both through `tiktoken-rs`: `fancy-regex` and `regex` (`cargo tree --workspace --all-features -i regex`). |
| Invalid UTF-8 or malformed JSON on the MCP stream | Both are answered with `PARSE_ERROR` and the session continues (`serve` in `stdio.rs`; the `malformed_input_is_answered_and_the_session_continues` and `invalid_utf8_is_answered_and_the_session_continues` tests). |
| A client that stops reading | **Open.** Nothing in the code holds this one: the serve loop never reads while it writes, and that is recorded as a documented precondition rather than fixed (see *Known limitations*). Until 2026-10-14 this row carried the cross-reference alone, with no **Open**, so the legend above read it as a mitigation in code. |

The CLI reads its whole input into memory before the engine's size ceiling applies
(`read_source` in `main.rs` uses `fs::read` or `read_to_end`). The 16 MiB ceiling
therefore bounds what the engine compresses, not what the CLI reads — and not what
it expands: `decompress` applies no size ceiling, so `expand` reads and decodes an
archive of any size, holding the archive and the decoded copy in memory together. The CLI's input
is chosen by the trusted invoking user, so this is recorded here rather than treated
as a threat.

### Integrity of reconstruction (A4)

| Threat | Mitigation in code |
|---|---|
| A corrupted or truncated archive decodes to wrong bytes | The header carries SHA-256 of the *original*, and it is verified against the reconstructed bytes before anything is returned. Bad magic, an unknown version, a set reserved bit, a malformed varint and a checksum mismatch each fail closed (`crates/tokfold-core/src/format.rs`, module docs). `tokfold expand` exits `3` on an unrecoverable archive (`tokfold --help`). Measured through `tokfold_decompress` with the archive's last byte changed: `isError: true` and `"code":"checksum_mismatch"`. |
| A **forged** archive | **Not mitigated, by design.** The checksum is unkeyed and travels inside the archive it covers, so anyone who can modify an archive can recompute it. It detects corruption, not tampering (`format.rs`, module docs). An archive is only as trustworthy as the place it was stored. In particular, an archive that passed through the model's context (for example as an argument the model supplies to `tokfold_decompress`) can be substituted, and it will decode "successfully" to whatever the forger chose. |

### Semantic integrity of the rendering (A1, A2)

This is the threat reversibility does not answer.

**The model acts on the compressed view.** Reversibility — an archive that restores
the original exactly — protects **auditability**: afterwards, a person or a tool can
compare what the model was shown with what was really there. It does not protect
**decision integrity**. Whatever the model did with the rendering, it did before
anyone decompressed anything. If a rendering reads differently from its source,
whether through a defect or because an attacker shaped the input, the agent may act
on the difference, and a perfect archive does not undo that action.

What the code does, and does not do, about it:

- **`never_compress` is a fidelity safeguard. It is not a security control and not an
  injection filter.** It names case-folded literal substrings of the output an agent
  most needs to read back accurately — among them compiler errors, `EACCES`, `denied`,
  certificate failures, panic and stack-trace markers, and `warning:`. Two things about
  that are narrower than they sound. The unit protected on a hit is the JSON string
  lexeme that contains the substring, not the line: E1 copies that one lexeme
  byte-for-byte and minifies everything around it, so a sibling lexeme on the same
  source line still has its escapes canonicalized (`never_compress.rs`, "What v0.0.1
  actually implements"; pinned by
  `a_protected_lexeme_keeps_its_escapes_while_its_neighbour_loses_theirs`). And the HTTP
  literals are reason phrases rather than codes, so `500 Internal Server Error` is
  matched while a bare `500` is not, and neither is `HTTP/1.1 500 Oops` (the
  `http-status` rows of the table, pinned by
  `http_rules_are_reason_phrases_and_never_bare_codes`). Only the minify encoder (E1)
  consults the table. The tabular encoder (E2) never reads it. A phrase spelled with
  `\u` escapes misses the match. Nothing about it inspects, blocks or neutralises
  hostile content, and it must not be relied on to.
- **Known fidelity gaps an A2 attacker could aim at** (all recorded, all
  owner-gated, none fixed):
  - E2 hoists an array's dominant key set into the table header once. A protected
    string among those keys is therefore written once rather than per row
    (`never_compress.rs`, "What v0.0.1 actually implements").
  - E2 interns shapes by a 64-bit `FxHasher` hash and never compares the key text,
    so two colliding key lists are counted as one shape. The rendering still
    round-trips, but the header can be a shape that occurs once. The
    `a_shape_hash_collision_merges_two_shapes_into_one_count` test in
    `crates/tokfold-core/src/encoder/e2_tabular.rs` pins a colliding pair.
- **No injection defence.** tokfold is not a prompt-injection filter and does not
  claim to reduce injection risk (`SECURITY.md`). Hostile text in the input reaches
  the rendering, and therefore the model, like any other text.

### Confidentiality (A1, A5, A6)

| Threat | Mitigation in code |
|---|---|
| A dependency opens a network connection | **Partly.** `deny.toml` bans a fixed list of *named crates* — HTTP, RPC and WebSocket stacks, socket wrappers and DNS resolvers — from the whole workspace graph, evaluated with `all-features = true`, and denies tokio's `net` feature. CI runs `cargo deny --locked check` on every push to `main` and every pull request (the `deny` job in `.github/workflows/ci.yml`). Only crates.io is an allowed source. Read "socket primitives" as that list and not as the capability: `deny.toml`'s own comment exempts `libc` and `nix` deliberately, as the substrate of ordinary non-network crates, and `libc 0.2.186` is in the default graph through `sha2 → cpufeatures`. The raw socket syscalls are therefore reachable from the graph, and this gate does not see them; what answers that is the *First-party code opens a network connection* row and `nm -u` on the binary. |
| First-party code opens a network connection | **Held by review, not by the build.** `std::net` is part of the standard library and cannot be banned (`deny.toml`, header comment; `crates/tokfold-mcp/src/lib.rs`, module docs). No `std::net`, `TcpStream`, `UdpSocket` or process spawn exists in the non-test sources today; the test sources do spawn (`/bin/sh`, `sh`, `mkfifo`, the binary under test and — through `std::env::current_exe()` — two further copies of the suite binary, all in `crates/tokfold-cli/tests/cli.rs`; the two bare names are resolved through `PATH`) and are not compiled into a released binary. A macOS arm64 release build of commit `166873f` (`cargo build --release -p tokfold-cli`, SHA-1 `834c01f2…`, default features — that package selection is the one `release.yml` ships. The package selection is *not* what moves the hash, though: `cargo build --release --workspace` with default features is byte-identical to that `-p tokfold-cli` build, `834c01f2…` at 1 261 712 bytes (measured 2026-10-10). The feature set moves it, and `--all-features` moves two features at once — core's `tiktoken` and its unimplemented `hf` — so `--workspace --all-features` gives `ca2cfc4a…` while `tiktoken` alone gives `a65170a2…`, the same 1 263 392 bytes as each other and different bytes. It is not the shipped *build*: `release.yml` adds `--target aarch64-apple-darwin --locked` and the `strip`/`crt-static` `--config` pair, which on that commit give `2a17afd0…` at 1 002 896 bytes. Every symbol count in this row is identical across that axis — 2 645 total, 83 undefined, 440 `tokfold` names unstripped and 83 / 82 / 0 stripped, with or without `--target`) imports no `socket`, `connect`, `bind`, `listen`, `getaddrinfo`, `sendto`, `fork`, `execv` or `posix_spawn` symbol; so does the published `tokfold-darwin-arm64` `0.0.1` binary (`bin/tokfold` SHA-1 `8e2ed283…`). The measurement is `nm -u <binary>` filtered for those names **as whole symbols, with `execv` and `posix_spawn` matched by prefix** (`grep -E '^_?(socket|connect|getaddrinfo|sendto|bind|listen|fork)(\$.*)?$|^_(execv|posix_spawn)'`, which is empty for both). The prefix is load-bearing rather than cosmetic: a binary that really does spawn imports `_execvp` and `_posix_spawnp`, and imports neither `_execv` nor `_posix_spawn` at all, so anchoring on the whole names alone sees one of the eleven process-spawn symbols this workspace's own `cli` integration-test binary imports — `_fork`, missing `_execvp`, `_posix_spawnp`, three `_posix_spawn_file_actions_*` and five `_posix_spawnattr_*` — where the spelling above sees all eleven (measured 2026-10-14, macOS Darwin 27.0.0 arm64; locate that binary with `cargo test -p tokfold-cli --test cli --no-run --message-format=json` and read the `executable` field, because a `target/debug/deps/cli-*` glob also matches stale builds). Filtering for the nine names as bare substrings is *not* empty either, because `bind` matches dyld's own `dyld_stub_binder`, so an unanchored re-run contradicts this row's "imports no `bind`" claim without finding anything a caller could use. Against unfiltered lists of 83 and 76 undefined symbols respectively, so the filter had something to reject. **Of the two binaries this row's symbol counts come from, only the published one is stripped** — not because a local build cannot be, but because nothing in the repository strips one by default: the workspace `Cargo.toml` declares no `[profile.release]` table, and `release.yml` passes `--config 'profile.release.strip="symbols"'` as a one-off so that a developer's build keeps its backtrace symbols. `nm` lists 77 symbols for the published binary — 76 undefined, 1 defined, none naming `tokfold` — against 2,645 for the commit-`166873f` build, of which 2,562 are defined and 440 are mangled Rust names containing `tokfold`. For the published binary this measurement therefore says what it *imports* and not what it contains; for the commit-`166873f` build that limit is the measurement's, not the binary's. Stripping moves the undefined count too, so 83 against 76 spans two variables at once: the same commit built with that override lists 82 undefined symbols, the one it drops being `__tlv_bootstrap`. The other four platform binaries were not measured. |
| The archive leaks through file permissions | **Open.** See *Archive path handling*. |
| Diagnostics leak content | Read this per crate and then per binary, because the two differ. **Crates:** `tokfold-core` writes to neither stream; `tokfold-mcp` writes nothing to standard error and to standard output only the protocol replies of `serve_stdio`. The derivation is `grep -rn 'eprint!\|eprintln!\|stderr()\|io::stderr\|Stderr' crates/tokfold-mcp/src`, which matches nothing; the same pattern plus `print!\|println!\|stdout()` matches nothing in `crates/tokfold-core/src`. Grepping the bare word `stderr` in `crates/tokfold-mcp/src` instead is *not* empty — it finds two module-doc lines, in `lib.rs` and `stdio.rs`, one of which states the rule that diagnostics belong on stderr; in `crates/tokfold-core/src` even the bare word matches nothing (measured 2026-10-03). Name the directory: this sentence read as a claim about `crates/tokfold-core/src`, the one it was written next to, where the count is zero. So match the macros and the handle, not the word. **The `tokfold` binary running `mcp`** does write one line to standard error, but the CLI does it, not the crate: `cmd_mcp` in `crates/tokfold-cli/src/main.rs` prints `EXPERIMENTAL_NOTICE` — a constant in `crates/tokfold-mcp/src/lib.rs` — before the loop starts. Measured with the input closed: standard error held exactly that one line and nothing else, so no content passes through it. The CLI's own diagnostics go to standard error through one function (`warn` in `main.rs`) and do quote paths and sizes. Some MCP error messages echo client-supplied text back to that same client, such as `unknown tool: <name>` in `tools.rs`. |

### Archive path handling (CLI)

The only file the CLI *writes* by name is the `--archive` path, through `write_file` in
`main.rs`. It deletes only a stale archive at that same path, through
`clear_stale_archive`. Both resolve a symlink at that path and act on its target, so
"that same path" names the file the path resolves to and not always the file you
typed: through a chain `l1 -> l2 -> real` it is `real` that is written or removed,
and both links survive. That is a claim about both operations, and until 2026-10-10
only the removing half was held, by
`a_symlink_chain_at_the_archive_path_names_the_file_it_resolves_to`. The writing half
is pinned by
`a_successful_pass_writes_through_a_symlink_chain_and_keeps_both_links`, which drives
the chain twice — once with a file already at the end of it, and once with nothing
there, where `create` decides where the file is born — and reads the original back
through `expand`. `write_file` resolves the link by asking `fs::metadata`, which
follows symlinks, so a link aimed at something that is not a regular file is refused
exactly as its target would be: exit `2`, nothing on standard output, and the refusal
names the path the caller typed, because that is the path `metadata` was asked about
and no write ever happened
(`a_symlink_aimed_at_a_fifo_is_refused_by_the_write_path`). The guards around both are
documented in `crates/tokfold-cli/README.md` and in the rustdoc of the functions named
here.

It is not the only file the CLI *opens* by name. The `--input` path is opened
read-only, through `fs::read` in `read_source`, by each of the three subcommands that
take the flag — `compress`, `expand` and `stats`, while `mcp` takes no arguments — and
a symlink there is followed exactly as one at the archive path is. Until 2026-10-14
the sentence this section opens with said "opens by name", and that wording was itself
a fix: it replaced "the only file the CLI writes", which the redirected standard output
falsifies and which the paragraph below answers. The input read falsifies the
replacement instead, and nothing in that edit looked at it. Measured against a release
build of commit `181ec00` (`cargo build --release -p tokfold-cli`, SHA-1 `637795cf…`,
macOS Darwin 27.0.0 arm64, rustc 1.97.0): `compress --input missing.json` exits `2` with
"reading input file missing.json: No such file or directory (os error 2)", `expand` and
`stats` name the same step for their own missing paths, an input at mode `000` exits
`2` with "Permission denied (os error 13)", and `--input link` with `link -> real`
compresses the bytes of `real`. The same-file guards reach that path a second way, by
`stat` rather than by name, opening it only for a `/dev/fd` spelling of a regular file
(`input_identity` and `reached_identity` in `main.rs`).

It is not the only file the CLI *writes*. Every command writes to standard output, and
a redirect puts that on disk: the CLI then writes a file it never names, through a
descriptor it inherits, and it neither creates the file, nor chooses its mode, nor can
refuse the write. `expand` puts the original bytes there verbatim, so that file is as
sensitive as the archive — measured on macOS arm64 against `cargo build --release`, a
1,551-byte input round-tripped through `compress --archive a.tkfd` and `expand >
restored.json` left `restored.json` equal to the input byte for byte, at `0644` under
umask `022`. Its mode is the shell's: under `022`, `077` and `000` it came out `0644`,
`0600` and `0666`, the same three figures as the archive, so *The archive leaks through
file permissions* reads the same way for this file — except that nothing in tokfold
could set it. `ensure_output_is_not_the_input` refuses a run whose standard output is
the file it reads, first thing in all four commands (`cmd_compress`, `cmd_expand`,
`cmd_stats`, `cmd_mcp`) and Unix-only like the other descriptor guards; its rustdoc
states what the refusal saves. With `>>` the input survives (measured: exit `2`, the
1,551 bytes intact); with `>` the shell emptied the file before tokfold started
(measured: exit `2`, 0 bytes), so there the refusal saves the rest of the run and not
the bytes — including a `compress --archive` that would otherwise read the emptied
input as a rejected document and clear the stale archive beside it.

- A rejected pass deletes a previous archive at the path only if that path is a
  regular file that opens with the `TKFD` magic. This keeps a stale archive from
  verifying against a new document. Foreign files are left alone.
- "Not a regular file" is a category, and its members do not behave alike on a
  rejected pass. A FIFO is left where it is and the passthrough still runs: exit `0`,
  the input echoed, "tokfold: left <path> untouched: it is not a regular file". A
  **directory** is fatal: exit `2`, nothing on standard output, "inspecting the
  archive path <path>: Is a directory (os error 21)" — the clearing lets a directory
  through to the magic probe on purpose (the check it delegates to,
  `inspect_archive_slot`, settles "not a regular file" on `!meta.is_file() &&
  !meta.is_dir()`), and on Unix `File::open` succeeds on a directory and the read of it
  fails. On Windows the open itself fails, since `File::open` passes no
  `FILE_FLAG_BACKUP_SEMANTICS`, so the message there is an access denial that names
  nothing about the path being a directory. The
  `is_file() || is_dir()` spelling is `write_file`'s and governs the successful pass
  below, not the clearing. On a *successful* pass both are fatal, with
  different messages: a FIFO is refused before the write ("refusing to write the
  archive to <path>: it is not a regular file"), a directory reaches the write and
  reports the platform error ("writing archive to <path>: Is a directory"). All four
  measured 2026-10-03 against the macOS arm64 release build the *First-party code
  opens a network connection* row fingerprints (commit `166873f`, `cargo build
  --release -p tokfold-cli`, SHA-1 `834c01f2…`), and pinned in
  `crates/tokfold-cli/tests/cli.rs` by
  `a_fifo_at_the_archive_path_neither_hangs_nor_is_removed` (both FIFO legs),
  `a_directory_at_the_archive_path_of_a_successful_pass_reports_the_platform_error`
  and `a_directory_at_the_archive_path_of_a_rejected_pass_costs_the_passthrough`.
  Two of the four were unpinned when this bullet first named two tests as covering all
  of them: no test drove a directory on a rejected pass at all, and the FIFO test
  asserted a message only on its winning leg — the "left <path> untouched" wording was
  held for `/dev/fd/3` and `/dev/null`, never for a FIFO.
- The CLI refuses to use the same regular file for the input and the archive, or for
  the output and the input (`ensure_archive_is_not_the_input` and
  `ensure_output_is_not_the_input`). On Unix these guards compare device and inode
  numbers. Off Unix `identity_of` returns `None`, so **the guards that ask a
  descriptor — standard input or standard output against the archive, standard
  output against the input — do not apply on Windows**. What remains there is the
  `--input`/`--archive` pair, which `is_same_file` compares by `fs::canonicalize`
  (read from the source; not measured on Windows).
- **The archive is created with the process umask, not with a restrictive mode.**
  `write_file` uses `OpenOptions` with `write`, `create` and `truncate` and sets no
  mode. Measured on macOS against `cargo build --release --workspace --all-features`
  (SHA-1 `ca2cfc4a…`, the bytes commit `166873f` builds — that hash-to-commit mapping is
  its own measurement, taken 2026-10-14): under umask `022` a new archive is `0644`,
  which any local user can read; under `077` it is `0600`; and under `000` it is `0666`,
  which any local user can also **write** — the member this bullet did not name before
  the 2026-10-10 re-measurement. Overwriting an existing file re-applies nothing and
  keeps that file's own mode: a pre-existing `0666` file stayed `0666` under umask
  `077`. The mode is the umask's and not the input's: re-measured 2026-10-15 with the
  input narrowed to `0600`, the same three passes still gave `0644`, `0600` and `0666`,
  so under `022` tokfold writes a world-readable copy of a document only its owner could
  read, which is what A6 reaches. The archive holds the input verbatim, so on a shared
  machine the archive's permissions are the user's responsibility. All four figures and
  that independence are pinned in `crates/tokfold-cli/tests/cli.rs` by
  `a_new_archives_mode_is_what_the_umask_leaves`, which drives each pass under `sh -c
  'umask N; …'` because std cannot set a umask and which until 2026-10-15 left the input
  at whatever the suite's own umask gave -- under the usual `022` that is `0644`, the
  very mode the first pass expects of the archive, so no pass exhibited a widening and
  the `0600`-to-`0644` case this bullet describes was absent from it; the three passes
  refuted a copied mode then as now, because a copy would give one archive mode for all
  three and they give three; before it, no test in that file read a produced file's mode
  at all — its eight `set_permissions` calls all write a mode onto a fixture.
- The archive write is not atomic (no temporary file and `rename`). An interrupted
  overwrite can leave a truncated file where a good archive was. `expand` refuses a
  truncated archive rather than decoding it wrongly, and the offset it names is
  usually the first byte of the field that came up short — but not always, and not
  always where the cut fell either. Measured against a release build of commit
  `166873f` (`cargo build --release -p tokfold-cli`, SHA-1 `834c01f2…`) on a 1664-byte
  archive of a 1620-byte JSON original. The layout is the one
  `crates/tokfold-core/src/format.rs` owns as named `*_OFFSET` constants: the 4-byte
  magic at 0, `version` at 4, `encoder_id` at 5, the little-endian `tokenizer_id` at
  6–7, the little-endian `flags` at 8–9, the `original_len` ULEB128 at 10–11 (1620
  needs two bytes), the 32-byte SHA-256 at 12–43, and the 1620 verbatim payload bytes
  from 44. Truncating to every length from 1 to 1664 and running `expand` on each cut
  gives ten bands and no other answer:

  | cut to | exit | message | the field that is short |
  |---|---|---|---|
  | 0 | `3` | `the archive is empty (0 bytes)` | — (spell it `: > cut`; macOS `head -c 0` refuses) |
  | 1–3 | `3` | `not a recognized payload (bad magic)` | the magic itself |
  | 4 | `3` | `archive corrupted at byte 4` | `version` |
  | 5 | `3` | `archive corrupted at byte 5` | `encoder_id` |
  | 6–7 | `3` | `archive corrupted at byte 6` | `tokenizer_id` |
  | 8–9 | `3` | `archive corrupted at byte 8` | `flags` |
  | 10 | `3` | `archive corrupted at byte 10` | the varint, absent entirely |
  | 11 | `3` | `archive corrupted at byte 11` | its second byte, which the first asks for |
  | 12–43 | `3` | `archive corrupted at byte 12` | the checksum |
  | 44–1663 | `3` | `archive corrupted at byte 10` | no field — the payload is short |
  | 1664 | `0` | — | the original, byte for byte |

  So do not read the offset as "where the file ends" — but do not read it as
  independent of the cut either. It equals the cut at 4, 5, 6, 8, 10, 11 and 12 —
  at 4, 5, 6, 8, 10 and 12 because the cut fell on the first byte of the field that
  came up short, and at 11 because a varint missing its continuation byte is reported
  at that missing byte and not at the field's start. It lags the cut by exactly one
  byte at 7, 9 and 13, the second byte of `tokenizer_id`, of `flags` and of the
  checksum, because a multi-byte field is reported at its own first byte and not at
  the byte that is missing; inside a wider field that lag widens with it, so a cut to
  43 is reported at 12, thirty-one bytes behind. A cut below 4 names no offset at
  all: at 1 to 3 the magic is what fails, and 0 is refused as empty.
  `expand_offsets_equal_the_cut_only_where_a_field_opens` in
  `crates/tokfold-cli/tests/cli.rs` builds an archive of this shape and pins both
  sets, the checksum band's 12, the payload band's 10 and that silence below 4.

  The rule is the format's; the band boundaries in that table are this fixture's. They
  move with the width of the `original_len` varint, because every field after it moves.
  `expand_names_the_corrupt_offsets_the_readme_quotes` in
  `crates/tokfold-cli/tests/cli.rs` is the one-byte counterpart — a 13-byte original,
  so the checksum starts at 11 and the payload at 43 — and it pins a cut to 20 bytes
  at offset 11 where this fixture reports 12, and a cut to 43 at offset 10 where this
  fixture is still inside the checksum. Quote a band only together with the original's
  size.

  The last band is the one that differs in kind. There the whole frame reads and only
  the payload is short, so no digest is computed and the error is aimed at the
  `original_len` field at offset 10 — the field the payload disagrees with — for every
  one of the 1620 cuts inside the payload. That makes it the widest band by far —
  fifty times the next widest, the 32 cuts inside the checksum — so it is where an
  interrupted write of a large archive lands; the header bands are reachable only by a
  write that died inside its first 44 bytes.

## Known limitations and open issues

These are known, measured and **not fixed**. Most are one of two things: a change of
behaviour on a binary already published, or a wire-contract question for the project
owner. Two are neither: they are limitations of the runtime the npm launcher sits on,
no owner decision is pending on either, and both are in *Process and platform*. A
signal Node cannot name arrives as the same `exit(0, null)` Node reports for a clean
exit, so the launcher has nothing to tell the two apart by; and the Node 18 hang
reproduces with a plain Node script starting `/bin/sleep` and no tokfold at all.
Whether either could be worked around was not measured. Until 2026-10-14 this
paragraph offered the first two as an exhaustive pair and called every entry
deliberate, which made those two read as held pending an owner decision. They are
listed so that nobody mistakes any of them for an undiscovered defect.

### MCP transport and protocol

- **The serve loop never reads while it writes.** It is one thread running
  read → handle → write → flush, so a client must drain standard output while it
  writes standard input. A client that pipelines a burst and only then reads will
  deadlock. Measured on macOS against `serve_stdio`, with 41-byte `ping` calls:
  2,195 pipelined calls completed and 2,200 deadlocked (`serve` rustdoc in
  `stdio.rs`). The crate docs treat this as a documented precondition rather than a live
  bug, on the grounds that mainstream MCP client SDKs read a stdio server's output on
  a separate task (`crates/tokfold-mcp/src/lib.rs`, "What the transport expects of a
  client").
- **A JSON-RPC *response* arriving on the input is answered with an error.**
  `{"jsonrpc":"2.0","id":2,"result":{}}` receives a `-32600` error saying that
  `method` must be a string (measured). The message is the one `decode_request` in
  `crates/tokfold-mcp/src/jsonrpc.rs` raises for an envelope whose `method` is absent
  or not a string; nothing on that path looks for a `result` at all, so a response is
  not recognised as one and is reported as a malformed request. Two servers of this
  kind wired crosswise would answer each other's errors indefinitely.
- **Batches are accepted, and `ping` is answered** (measured: a two-`ping` batch
  returned a two-member array). Both are deliberate in the code the revision does not
  ask for: `handle_batch` walks a top-level array, and `dispatch` answers
  `method::PING` with an empty result object, both in
  `crates/tokfold-mcp/src/server.rs`.
- **A batch's answer is not monotonic in the frame limit.** Each member's refusal
  takes the widest form that fits the room left at that member and reserves nothing
  for later members, so a larger limit can collapse a batch that a smaller one
  answered. The derivation is in the `handle_batch` rustdoc in
  `crates/tokfold-mcp/src/server.rs`, and the small-limit windows are pinned by
  `a_larger_limit_can_collapse_a_batch_that_a_smaller_one_answered`. The same shape
  is reachable at the default 32 MiB limit through a wide request id.
- **An oversized reply is refused with `-32602` ("invalid params")** even though the
  parameters were valid (`jsonrpc.rs`, "Why `INVALID_PARAMS`"). An oversized *request*
  is refused with `-32600` instead (`serve` in `stdio.rs`). Which code a "too large"
  refusal should borrow is open.
- **A refusal the parser produces carries no id, so that one call waits.**
  An id can be echoed only off a line that parsed: `read_id` in
  `crates/tokfold-mcp/src/jsonrpc.rs` takes an already-parsed value, and `recover_id` in
  `stdio.rs` — the one best-effort path, reached when handling a panic — re-parses the
  line, so neither can address a refusal the parser itself raised. Both `PARSE_ERROR`
  sites pass `None` for that reason: the parse failure in `handle_line` and the
  non-UTF-8 line in `serve`. Measured against `cargo build --release -p tokfold-cli` on
  macOS arm64 (commit `ae065a8`, SHA-1 `637795cf…`, rustc 1.97.0), with a narrow and
  plainly readable id in the text of every fixture: a `tools/list` carrying a duplicate
  key, a trailing comma, an unterminated string, a lone surrogate in an argument, a
  leading-zero number, and one with an invalid UTF-8 byte each came back `-32700` with
  no `id` member, while
  the same server echoed the id on the two faults that do parse — a response envelope
  answered `-32600`, and an unknown tool answered `-32602`. The absence rather than
  `"id":null` is deliberate: MCP admits only a string or a number as a `RequestId`, so a
  null id is not an address, and `properties.rs` pins the absence as what id-less means.
  A request whose id alone is too wide for an addressed refusal reaches the same outcome
  by the other route (`MAX_MESSAGE_BYTES` rustdoc in `crates/tokfold-mcp/src/lib.rs`).
- **`additionalProperties: false` is declared but not enforced.**
  All three tool schemas declare it — `compress_tool`, `decompress_tool` and
  `estimate_tool` in `tools.rs`, which are also the only three sites in that file that
  set the key — and all three tools accept an unknown argument key. That holds by
  construction rather than by accident: `tools::call` routes every tool through
  `object_arguments`, which rejects only an `arguments` that is neither an object nor
  null, and each tool then reads what it needs by key (`required_str`,
  `optional_profile`). The key set is not unexamined on the way in — `parse` sorts every
  object's keys and refuses a duplicate, so an `arguments` carrying `text` twice is
  answered `-32700` with "duplicate key in object" after the byte offset, before any
  tool runs. What no site asks is whether a key is one the schema names, and that is the
  only question that would reject an extra one. Measured on all three tools, each beside
  the same call without the key, against `cargo build --release -p tokfold-cli` on macOS
  arm64 (commit `ae065a8`, SHA-1 `637795cf…`, rustc 1.97.0): adding `"bogus":7` to
  `tokfold_estimate`, `tokfold_compress` and `tokfold_decompress` gave a result equal to
  its control in every field, and the `tokfold_decompress` call still restored its
  archive. The declaration itself is held on every tool —
  `every_catalogue_entry_carries_the_fields_a_client_needs` asserts it over the whole
  catalogue, which is those three functions, and
  `the_catalogue_lists_every_tool_in_a_fixed_order` ties that catalogue's length to
  `NAMES` — but nothing pins the acceptance, because enforcing it is the open question
  and a test would entrench the answer.
- **A valid-JSON input that saves nothing is reported as compressed.**
  `tokfold_compress` on `{"a":1}` returned `"compressed": true` with encoder
  `passthrough` and a rendering behind a `⟦tkfd:v1:raw⟧` marker (measured).
- **No `outputSchema`** is declared for the tools. **`serverInfo` is duplicated**:
  every result carries it in `_meta` (measured on the `ping` replies).
- **`initialize` never answers `params.protocolVersion` with `-32022`, and it does
  not answer a *supported modern* one with itself either.** It falls back to a
  supported legacy revision, as the older revisions specify. Saying only
  "unsupported" understates it: measured against a release build of commit `166873f`
  (`cargo build --release -p tokfold-cli`, SHA-1 `834c01f2…`), an `initialize` carrying
  `"protocolVersion":"2026-07-28"` — the
  revision this server lists **first** in the `supported` array of its own `-32022`
  errors, measured as `["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26",
  "2024-11-05"]` — was answered with `"protocolVersion":"2025-11-25"`. Adding a
  `_meta` declaration of `2026-07-28` to the same request did not change that
  answer, so no `initialize` reply ever names the revision the server advertises
  first. `LATEST_LEGACY_VERSION`'s rustdoc in `protocol.rs` says this is deliberate:
  naming the modern revision there would promise handshake semantics the server does
  not implement. What `params._meta` selects is the revision the *per-request*
  checks run under, and that check does fire — a `ping` declaring `1999-01-01` in
  `_meta` was answered `-32022` (measured). Whether
  `initialize` should be held to the modern check is an open contract question
  (rustdoc of `Server::initialize` in `server.rs`).

### Engine and output

- **F2.** The do-no-harm gate prices a framed candidate against an *unframed* baseline.
  In `select` in `crates/tokfold-core/src/encoder/mod.rs` the gate is `clears_margin`,
  and the two numbers handed to it are `estimate(&candidate)` — the rendering with its
  `⟦tkfd:v1:…⟧` framing — and `est_before`, which is `estimate(input)` on the raw input.
  Measured: 6.67% of compressions are strictly worse in real `cl100k` tokens, about two
  thirds of that from framing. Nothing in the repository records how that was reached:
  no test carries the figure, no crate declares a bench target and there is no `benches`
  directory, and no benchmark corpus is tracked. That last one is checked by filtering
  `git ls-files` on the path, and it is the one check of this kind whose answer
  recording it cannot change: `git ls-files` lists paths, not contents, so the sentence
  you are reading adds no match to it. An earlier note here tried to pin the absence of
  the figure the other way, by quoting what a repository-wide search for it returns;
  writing the note down added occurrences to the search it quoted, which is why the
  check on the figure is stated as what it is about instead. What the tree does hold is
  fuzzing inputs rather than benchmarks — `fuzz/seeds/<target>` is committed,
  `fuzz/corpus/` is ignored by `fuzz/.gitignore` — and neither can re-derive a
  token-saving percentage. `crates/tokfold-core/Cargo.toml` reads like the evidence and
  is not it: the comment above its `include` list does name "local benchmark corpora"
  among what the list excludes by omission, but an `include` list governs the `.crate`
  tarball and says nothing about what git tracks. So re-reading the gate will not
  re-derive the number, and re-measuring or hedging it is owner-gated.
- **F6.** E2 can hoist a `never_compress`-protected key (see *Semantic integrity*).
- **E2 shape interning by hash** (see *Semantic integrity*).
- **Forged archives** decode successfully (see *Integrity of reconstruction*).

### Process and platform

- **The descriptor-based same-file guards are Unix-only** (see *Archive path
  handling*).
- **The archive's mode follows the umask** (see *Archive path handling*).
- **The npm launcher cannot always reproduce a signal death.** Each item below is a
  separate measurement and carries the platform it was made on; none of them is
  known to hold on Windows, which was not measured.
  - A signal Node cannot name reaches the launcher as `exit(0, null)`, so the
    launcher exits `0` for a killed binary. `SIGEMT` on macOS; on Linux (aarch64,
    glibc 2.39, Node v22.23.2) every signal from 32 to 64. That pair is also what
    Node reports for a clean exit, so nothing distinguishes the two at the launcher:
    measured on macOS Darwin 27.0.0 arm64 with Node v22.23.2, `child_process.spawn`
    reports `[0, null]` for `/usr/bin/true`, `[0, null]` for a shell killed by
    `kill -EMT`, and `[null, "SIGTERM"]` for one killed by `kill -TERM`.
  - A signal Node *ignores* process-wide cannot be re-raised, so the launcher exits
    `128 + N` instead of dying. Node ignores `SIGPIPE` and `SIGXFSZ`; `SIGXFSZ` is
    reachable (a write past `ulimit -f`), `SIGPIPE` is not, because a Rust binary
    starts with it ignored.
  - A signal the *caller* has set to be ignored is not ignored through the launcher:
    Node replaces the inherited disposition with its own listener and starts the
    binary with every signal at its default. Measured on macOS; the published
    `0.0.1` launcher does the same.
  - Descriptors above 2 do not reach the binary on macOS (Node v22.23.2), so
    `--archive /dev/fd/3` fails with `Bad file descriptor`. On Linux (aarch64,
    glibc 2.39) the measurement was narrower: descriptors 3 and 4 were found closed,
    which is not the same claim as "only 0–2 reach it".

  All four are in the header comments of `npm/tokfold/bin/tokfold`. Three of them
  are also in `npm/tokfold/README.md` under *Exit codes*, at more length. The
  descriptor one is not an exit-code matter and is not in that section: it is under
  *The `--archive` path belongs to tokfold* there, which is also where the paths it
  affects are described.
- **The npm launcher can hang after the binary has exited, on Node 18 under macOS.**
  `npm/tokfold/bin/tokfold` labels this "Known issue, not fixed", so it belongs on
  this list. One measurement on macOS arm64 with Node v18.20.8 saw 32 of 300
  signal-killed runs leave the binary dead and the launcher waiting with nothing on
  stderr; a second attempt on the same machine and runtime reproduced none in 300,
  but under CPU load the launcher's own suite lost one to three of its 76 tests to a
  30-second bound in two of three runs, where Node 20 and 22 passed all 76. It
  reproduces with a plain Node script starting `/bin/sleep` and no tokfold at all.
  Node 18 is this package's `engines` floor, and the only Node 18 CI leg is Linux
  (`.github/workflows/ci.yml`), so CI cannot see it. Whether the published `0.0.1`
  launcher hangs too was not measured.

## Where this document can go stale

Every claim above about what the code *does* names the function, file or `--help` entry
that holds it, or a section of this document that does. Nine are anchored only through a
section rather than directly: the *Availability* row for a client that stops reading,
the *Confidentiality* row for file permissions, the *Assets* rows for files at the
`--archive` path and for the file standard output is redirected to, F6, E2 shape
interning, forged archives, the Unix-only same-file guards, and the archive's mode
following the umask. `grep -nE '[Ss]ee \*' docs/threat-model.md` finds those nine plus
the *Availability* row for oversized input, which carries direct anchors as well; match
`see \*` rather than a section name, because one of the nine wraps mid-reference. A
change to one of those places can falsify a sentence here. When one changes, re-read the
matching entry rather than assuming it still holds.

The rule does not reach *Attacker capabilities considered* or three of the six
*Assets* rows, and cannot. A1 to A6, the out-of-scope note, and the rows for the
original bytes, the rendering's meaning and the availability of the host say why
something matters, or what an attacker is assumed to control — not what the code
does — so no change to the code falsifies them, and nothing triggers a re-read of
them. Until 2026-10-10 this section claimed the rule for *every* claim above, which
made those sentences look guarded when review is all that holds them.

The rule does not reach a binary's bytes either. Several claims above name the build
they were measured against by its SHA-1, and that hash identifies bytes rather than a
tree: the commit beside it names the tree those bytes were built from, not a claim
that the current tree builds them — and not a guarantee that the recipe below rebuilds
them, since the build reads inputs the commit does not pin. Two hashes appear above for one build command, because
the bytes moved between the commits the measurements were taken at. The recipe is `git
archive <commit> | tar -x -C <dir>` followed by `cargo build --release -p tokfold-cli`,
and on macOS Darwin 27.0.0 arm64 with rustc 1.97.0 it gives
`834c01f2be12ebacab13057ed40aa570742dcddc` at 1 261 712 bytes for `166873f` and
`637795cf1c33ddb91c414c04c4a8409633d4765e` at 1 262 112 for `ae065a8` (both measured
2026-10-14, each in its own directory outside the repository and with its own
`target/`). On 2026-11-08 the same recipe on the same machine and the same rustc
1.97.0 gave the same sizes and different hashes: `6f8f7c86…` for `166873f` and
`4f4e7d98…` for `ae065a8`. The input that moved is not identified. One input that
did move is the `rust-src` component, installed into the stable toolchain on
2026-10-15: with it present the release binary embeds 21 standard-library paths under
`~/.rustup/toolchains/` where it would otherwise carry `/rustc/<hash>/`. It is not the
only one — a build of `166873f` with a copy of the toolchain that lacks `rust-src`
gave a third hash, `f70e7f2e…`, still at 1 261 712 bytes. The release binary also
embeds dependency paths under `~/.cargo/registry/src/`, so it depends on the home
directory even where it does not depend on the build directory. The hashes cited in
this document therefore identify the bytes each measurement ran against; they are not
something this recipe is known to reproduce. That a value does not depend on where it is built takes a second
measurement of *one* commit, not one measurement each of two: `--workspace
--all-features` gave `30e9393951f14769ecd0b2279a064d86aac4097a` at 1 263 776 bytes in
two different directories, and that is the support for it. It holds for the release
profile only — a **debug** binary embeds the absolute path it was built at, so `cargo
build -p tokfold-cli` at `181ec00` gave four sizes: 5 198 424 bytes in the repository,
5 245 736 and 5 247 416 in two scratch directories, and 5 267 816 in a third whose path
is 34 characters longer than the first of those (all measured 2026-10-14). Until that
date this paragraph argued path-independence from "each in its own directory", which
does not reach it. What moves a hash is the
compiled code and not the commit count: `bfc47ec`, `980ff80` and `ae065a8` all build to
`637795cf…`, across 94 changed lines in four files under `crates/*/src`, because every
one of those lines is a doc comment or sits after its file's `#[cfg(test)]`, and
neither is linked into the binary. So re-derive a hash here before trusting it to
describe the binary in hand, and read a build command as naming a build only together
with a commit. Until 2026-10-14 six of these sites called their binary "the built
binary" or "a release build of the working tree" and named `834c01f2…`, while two
others named `637795cf…` for the same command, so the document contradicted itself
about what one command produces.

The other three *Assets* rows do say what the code does, and carry an anchor: **The
archive** cites the `--archive` entry of `tokfold compress --help`, which holds the
verbatim-and-unencrypted wording itself, and **Files at the `--archive` path** and **The
file standard output is redirected to** both cite *Archive path handling*. Nor is *What
tokfold is, in one paragraph* free of code claims: the passthrough of anything the
engine cannot parse, and an archive that is a header followed by the original bytes
verbatim from which the original is reconstructed exactly, are both behaviour. It
restates them rather than anchoring them — the passthrough is held by `cmd_compress` in
`crates/tokfold-cli/src/main.rs` and, on the MCP path, by the `compressed: false` reply
built in `crates/tokfold-mcp/src/tools.rs`; the archive half by *Integrity of
reconstruction* — so re-read the summary when either changes. Until 2026-10-14 this
section counted that paragraph and the whole *Assets* table as beyond the rule's reach,
and named two of the then eight indirect anchors as though they were all of them.
