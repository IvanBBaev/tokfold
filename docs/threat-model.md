# Threat model

This document describes the threats tokfold faces in each of the three ways it can
be deployed, and names the code that answers each one. Where nothing answers a
threat, it says so. Its claims were read back against the **working tree**, not
against a commit, and **not all on the same day** — a measurement here carries its
own date, and an undated sentence was read back whenever it was last touched. The
tree is not clean: at the most recent read-back (2026-10-10) it held uncommitted
changes on top of `main` at `c6f800e`, and `git diff --shortstat` reported 16 files
changed. Both of those move with every commit, so re-derive them (`git log -1
--format=%h`, `git diff --shortstat`) rather than trusting the two values here. This
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
  (`crates/tokfold-mcp/src/tools.rs`). The only state it keeps across requests is the
  handshake flag and the negotiated protocol revision (the `Server` struct in
  `crates/tokfold-mcp/src/server.rs`). It has no persistence and opens no connections.
- **Trust boundary.** Every byte on standard input is untrusted. The MCP client (the
  agent host) writes it. The tool arguments inside it are chosen by the model, which
  may be steered by content it has read.
- **What reaches the model.** Nothing directly: the server answers the client, and
  the client decides what enters the prompt. A `tokfold_compress` reply carries the
  rendering twice, as `content` text and as `structuredContent.rendering`. When there
  is an archive, it also carries `structuredContent.archive`, the archive in base64,
  which holds the original bytes. So by default the full original goes back to the
  client beside the rendering (measured 2026-10-10 against the built binary
  (`cargo build --release -p tokfold-cli`, SHA-1 `834c01f2…`) with `{"a":1}`: the
  reply's `structuredContent` held `compressed`, `stats`, `rendering` and `archive`,
  the `rendering` was the `⟦tkfd:v1:raw⟧` marker plus the input and equal to
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

Out of scope: an attacker who already runs code as the same user as tokfold. They
can read the input without tokfold's help.

## Threats and mitigations

Each entry names the code that holds the line. "Open" means that nothing in the
code holds it today.

### Availability (A1, A3, A4)

| Threat | Mitigation in code |
|---|---|
| Oversized input to the engine | Core refuses input over `max_input_bytes`, which defaults to 16 MiB (`DEFAULT_MAX_INPUT_BYTES` in `crates/tokfold-core/src/compressor.rs`). The CLI then passes the input through unchanged and exits `0` **unless something on the `--archive` path stops it first**. Measured with a 17,825,802-byte input and no `--archive`: exit `0`, the input echoed byte for byte, and on standard error "tokfold: passing input through uncompressed: input 17825802 bytes exceeds limit 16777216". With `--archive` the passthrough can fail instead, and then nothing reaches standard output at all. Three measured with that same 17,825,802-byte input (2026-10-03): a directory at `--archive` is fatal (exit `2`, 0 bytes on standard output, "the input was rejected (...) and not passed through: inspecting the archive path adir: Is a directory (os error 21)"), a stale archive the process cannot unlink is fatal (`--archive` naming a `TKFD` file in a mode-`555` directory: exit `2`, 0 bytes on standard output, "the input was rejected (...) and not passed through: removing stale archive ...: Permission denied (os error 13)"), and an archive aimed at its own input is refused before the read (exit `2`, 0 bytes on standard output, the input left at its full size). The *shape* of all three — exit `2`, nothing on standard output, and the failing step named in the message — is pinned by a test: `a_directory_at_the_archive_path_of_a_rejected_pass_costs_the_passthrough`, `a_stale_archive_whose_removal_fails_is_named_and_left_intact` and `the_guard_stops_a_rejected_pass_from_deleting_its_own_input`, all in `crates/tokfold-cli/tests/cli.rs`. None of those three pins the case measured here, though: each drives a small invalid-JSON input, so what rejects the document in the test is the parse and not the 16 MiB ceiling, and the oversized reading above rests on the measurement alone. The clearing needs the *directory* writable, which a successful pass does not — see *Archive path handling*. **On the MCP path the input is not passed through, and the ceiling is usually not what the client is told about.** Measured against the built binary (`cargo build --release -p tokfold-cli`, SHA-1 `834c01f2…`) with a single JSON string argument: `tokfold_compress` is refused with `-32602` and "the reply exceeds the 33554432 byte frame limit" at **both** 16 777 217 bytes (over the engine ceiling) and 16 777 100 bytes (under it), because a passthrough reply carries the text twice — `content[0].text` and `structuredContent` — so the 32 MiB frame limit is reached before the 16 MiB engine ceiling can be reported, and a client never learns which of the two stopped it. Only `tokfold_estimate`, whose reply stays small, surfaces the ceiling: at 16 777 217 bytes it answered `isError: false` with the text "not compressed: input 16777217 bytes exceeds limit 16777216" — a refusal reported as a *successful* result — and at 16 777 100 bytes it answered with an ordinary estimate. |
| Deep nesting in the engine | Core's parser refuses nesting past `max_depth`, which defaults to 512 (`DEFAULT_MAX_DEPTH` in `compressor.rs`; the check is in `open_container` in `crates/tokfold-core/src/tape.rs`). |
| Oversized MCP frame | Messages over `MAX_MESSAGE_BYTES` (32 MiB) are refused and skipped, and the stream resynchronises on the next newline (`read_line` and `skip_to_newline` in `stdio.rs`). Reading goes through `take`, so an unterminated line cannot grow the buffer without bound. The same constant bounds every reply (`render_bounded` in `crates/tokfold-mcp/src/jsonrpc.rs`, applied by `Server::handle_line`), and has done since commit `36b771e`. Both directions compare the message without its newline. |
| A small frame that parses into a huge tree | The MCP JSON parser caps materialised values at `json::MAX_NODES` (`MAX_MESSAGE_BYTES / 8`) and nesting at `json::MAX_DEPTH` (128) (`crates/tokfold-mcp/src/json.rs`). The frame limit alone does not bound memory, and `MAX_MESSAGE_BYTES`'s rustdoc says so. |
| A panic takes down the server | Every inbound line runs inside `catch_unwind` (`handle` in `stdio.rs`) and a panic becomes an `INTERNAL_ERROR` reply addressed to the recovered id. Unwinding stays enabled for this: `panic = "abort"` is deliberately not set (workspace `Cargo.toml`, profile comment). An allocation failure aborts rather than unwinds, so it bypasses the bulkhead; that is why `MAX_NODES` exists. |
| Panics in the first place | Workspace lints: `unsafe_code = "forbid"`; clippy `unwrap_used`, `expect_used` and `panic` are `deny`, and `indexing_slicing` is `warn` (workspace `Cargo.toml`, `[workspace.lints]`). Each crate root also carries `#![forbid(unsafe_code)]`. |
| Regex denial of service | The `never_compress` table matches case-folded literal substrings, with no regex (`crates/tokfold-core/src/never_compress.rs`, module docs). No regex engine is in the default graph. Two arrive with the optional `tiktoken` feature of `tokfold-core` (off by default and not enabled in the release build), both through `tiktoken-rs`: `fancy-regex` and `regex` (`cargo tree --workspace --all-features -i regex`). |
| Invalid UTF-8 or malformed JSON on the MCP stream | Both are answered with `PARSE_ERROR` and the session continues (`serve` in `stdio.rs`; the `malformed_input_is_answered_and_the_session_continues` and `invalid_utf8_is_answered_and_the_session_continues` tests). |
| A client that stops reading | See *Known limitations*: the serve loop never reads while it writes. |

The CLI reads its whole input into memory before the engine's size ceiling applies
(`read_source` in `main.rs` uses `fs::read` or `read_to_end`). The 16 MiB ceiling
therefore bounds what the engine processes, not what the CLI reads. The CLI's input
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
  injection filter.** It names lines an agent most needs to read back accurately
  (compiler errors, `4xx`/`5xx` statuses, `EACCES`, `denied`, certificate failures)
  so that an encoder can leave them verbatim (`never_compress.rs`, module docs). Only
  the minify encoder (E1) consults it. The tabular encoder (E2) never reads it. A
  phrase spelled with `\u` escapes misses the match. Nothing about it inspects,
  blocks or neutralises hostile content, and it must not be relied on to.
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

### Confidentiality (A1, A5)

| Threat | Mitigation in code |
|---|---|
| A dependency opens a network connection | **Partly.** `deny.toml` bans a fixed list of *named crates* — HTTP, RPC and WebSocket stacks, socket wrappers and DNS resolvers — from the whole workspace graph, evaluated with `all-features = true`, and denies tokio's `net` feature. CI runs `cargo deny --locked check` on every push to `main` and every pull request (the `deny` job in `.github/workflows/ci.yml`). Only crates.io is an allowed source. Read "socket primitives" as that list and not as the capability: `deny.toml`'s own comment exempts `libc` and `nix` deliberately, as the substrate of ordinary non-network crates, and `libc 0.2.186` is in the default graph through `sha2 → cpufeatures`. The raw socket syscalls are therefore reachable from the graph, and this gate does not see them; what answers that is the *First-party code opens a network connection* row and `nm -u` on the binary. |
| First-party code opens a network connection | **Held by review, not by the build.** `std::net` is part of the standard library and cannot be banned (`deny.toml`, header comment; `crates/tokfold-mcp/src/lib.rs`, module docs). No `std::net`, `TcpStream`, `UdpSocket` or process spawn exists in the non-test sources today; the test sources do spawn (`/bin/sh`, `mkfifo` and the binary under test in `crates/tokfold-cli/tests/cli.rs`) and are not compiled into a released binary. A macOS arm64 release build of the working tree (`cargo build --release -p tokfold-cli`, SHA-1 `834c01f2…`, default features — that package selection is the one `release.yml` ships. The package selection is *not* what moves the hash, though: `cargo build --release --workspace` with default features is byte-identical to that `-p tokfold-cli` build, `834c01f2…` at 1 261 712 bytes (measured 2026-10-10). The feature set moves it, and `--all-features` moves two features at once — core's `tiktoken` and its unimplemented `hf` — so `--workspace --all-features` gives `ca2cfc4a…` while `tiktoken` alone gives `a65170a2…`, the same 1 263 392 bytes as each other and different bytes. It is not the shipped *build*: `release.yml` adds `--target aarch64-apple-darwin --locked` and the `strip`/`crt-static` `--config` pair, which on this tree give `2a17afd0…` at 1 002 896 bytes. Every symbol count in this row is identical across that axis — 2 645 total, 83 undefined, 440 `tokfold` names unstripped and 83 / 82 / 0 stripped, with or without `--target`) imports no `socket`, `connect`, `bind`, `listen`, `getaddrinfo`, `sendto`, `fork`, `execv` or `posix_spawn` symbol; so does the published `tokfold-darwin-arm64` `0.0.1` binary (`bin/tokfold` SHA-1 `8e2ed283…`). The measurement is `nm -u <binary>` filtered for those names **as whole symbols** (`grep -xE '_?(socket|connect|bind|listen|getaddrinfo|sendto|fork|execv|posix_spawn)'`, which is empty for both); filtering for them as substrings is *not* empty, because `bind` matches dyld's own `dyld_stub_binder`, so an unanchored re-run contradicts this row's "imports no `bind`" claim without finding anything a caller could use. Against unfiltered lists of 83 and 76 undefined symbols respectively, so the filter had something to reject. **Of the two binaries this row's symbol counts come from, only the published one is stripped** — not because a local build cannot be, but because nothing in the repository strips one by default: the workspace `Cargo.toml` declares no `[profile.release]` table, and `release.yml` passes `--config 'profile.release.strip="symbols"'` as a one-off so that a developer's build keeps its backtrace symbols. `nm` lists 77 symbols for the published binary — 76 undefined, 1 defined, none naming `tokfold` — against 2,645 for the working-tree build, of which 2,562 are defined and 440 are mangled Rust names containing `tokfold`. For the published binary this measurement therefore says what it *imports* and not what it contains; for the working-tree build that limit is the measurement's, not the binary's. Stripping moves the undefined count too, so 83 against 76 spans two variables at once: the same tree built with that override lists 82 undefined symbols, the one it drops being `__tlv_bootstrap`. The other four platform binaries were not measured. |
| The archive leaks through file permissions | **Open.** See *Archive path handling*. |
| Diagnostics leak content | Read this per crate and then per binary, because the two differ. **Crates:** `tokfold-core` writes to neither stream; `tokfold-mcp` writes nothing to standard error and to standard output only the protocol replies of `serve_stdio`. The derivation is `grep -rn 'eprint!\|eprintln!\|stderr()\|io::stderr\|Stderr' crates/tokfold-mcp/src`, which matches nothing; the same pattern plus `print!\|println!\|stdout()` matches nothing in `crates/tokfold-core/src`. Grepping the bare word `stderr` in `crates/tokfold-mcp/src` instead is *not* empty — it finds two module-doc lines, in `lib.rs` and `stdio.rs`, one of which states the rule that diagnostics belong on stderr; in `crates/tokfold-core/src` even the bare word matches nothing (measured 2026-10-03). Name the directory: this sentence read as a claim about `crates/tokfold-core/src`, the one it was written next to, where the count is zero. So match the macros and the handle, not the word. **The `tokfold` binary running `mcp`** does write one line to standard error, but the CLI does it, not the crate: `cmd_mcp` in `crates/tokfold-cli/src/main.rs` prints `EXPERIMENTAL_NOTICE` — a constant in `crates/tokfold-mcp/src/lib.rs` — before the loop starts. Measured with the input closed: standard error held exactly that one line and nothing else, so no content passes through it. The CLI's own diagnostics go to standard error through one function (`warn` in `main.rs`) and do quote paths and sizes. Some MCP error messages echo client-supplied text back to that same client, such as `unknown tool: <name>` in `tools.rs`. |

### Archive path handling (CLI)

The only file the CLI writes is the `--archive` path, through `write_file` in
`main.rs`. It deletes only a stale archive at that same path, through
`clear_stale_archive`. Both resolve a symlink at that path and act on its target, so
"that same path" names the file the path resolves to and not always the file you
typed: through a chain `l1 -> l2 -> real` it is `real` that is written or removed,
and both links survive. The guards around both are documented in
`crates/tokfold-cli/README.md` and in the rustdoc of the functions named here.

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
  !meta.is_dir()`), `File::open` succeeds on a directory, and the read of it fails. The
  `is_file() || is_dir()` spelling is `write_file`'s and governs the successful pass
  below, not the clearing. On a *successful* pass both are fatal, with
  different messages: a FIFO is refused before the write ("refusing to write the
  archive to <path>: it is not a regular file"), a directory reaches the write and
  reports the platform error ("writing archive to <path>: Is a directory"). All four
  measured 2026-10-03 against the macOS arm64 release build the *First-party code
  opens a network connection* row fingerprints (`cargo build --release -p
  tokfold-cli`, SHA-1 `834c01f2…`), and pinned in `crates/tokfold-cli/tests/cli.rs` by
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
  mode. Measured on macOS: under umask `022` a new archive is `0644`, which any local
  user can read, and under `077` it is `0600`. Overwriting an existing file keeps
  that file's mode: a pre-existing `0666` file stayed `0666` under umask `077`. The
  archive holds the input verbatim, so on a shared machine the archive's permissions
  are the user's responsibility.
- The archive write is not atomic (no temporary file and `rename`). An interrupted
  overwrite can leave a truncated file where a good archive was. `expand` refuses a
  truncated archive rather than decoding it wrongly, and the offset it names is the
  first byte of the field that came up short — which sometimes is where the cut fell
  and sometimes is not. Measured against the built binary (`cargo build --release -p
  tokfold-cli`, SHA-1 `834c01f2…`) on a 1664-byte archive of a 1620-byte JSON
  original. The layout is the one
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
  independent of the cut either. It equals the cut at 4, 5, 6, 8, 10 and 11, and lags
  it by one byte at 7 and at 9, because a two-byte field is reported at its own first
  byte and not at the byte that is missing.

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
  one of the 1620 cuts inside the payload. That band is the widest by three orders of
  magnitude on this fixture, so it is where an interrupted write of a large archive
  lands; the header bands are reachable only by a write that died inside its first 44
  bytes.

## Known limitations and open issues

These are known, measured and **deliberately not fixed**: each changes behaviour on a
binary already published, or is a wire-contract question for the project owner.
They are listed so that nobody mistakes them for undiscovered defects.

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
  `method` must be a string (measured). Two servers of this kind wired
  crosswise would answer each other's errors indefinitely.
- **Batches are accepted, and `ping` is answered** (measured: a two-`ping` batch
  returned a two-member array).
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
- **A request whose id alone is too wide for an addressed refusal** is refused
  without an id, so that one call cannot be correlated and waits
  (`MAX_MESSAGE_BYTES` rustdoc in `crates/tokfold-mcp/src/lib.rs`).
- **`additionalProperties: false` is declared but not enforced.** The tool schemas
  declare it (`compress_tool`, `decompress_tool` and the estimate schema in
  `tools.rs`), but an unknown argument key is accepted: a `tokfold_estimate` call
  carrying `"bogus":7` was answered normally (measured).
- **A valid-JSON input that saves nothing is reported as compressed.**
  `tokfold_compress` on `{"a":1}` returned `"compressed": true` with encoder
  `passthrough` and a rendering behind a `⟦tkfd:v1:raw⟧` marker (measured).
- **No `outputSchema`** is declared for the tools. **`serverInfo` is duplicated**:
  every result carries it in `_meta` (measured on the `ping` replies).
- **`initialize` never answers `params.protocolVersion` with `-32022`, and it does
  not answer a *supported modern* one with itself either.** It falls back to a
  supported legacy revision, as the older revisions specify. Saying only
  "unsupported" understates it: measured against the built binary (`cargo build
  --release -p tokfold-cli`, SHA-1 `834c01f2…`), an `initialize` carrying
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

- **F2.** The do-no-harm gate prices a framed candidate against an *unframed*
  baseline. Measured: 6.67% of compressions are strictly worse in real `cl100k`
  tokens, about two thirds of that from framing.
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
    glibc 2.39, Node v22.23.2) every signal from 32 to 64.
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

Every claim above names the function or file that holds it. A change to one of those
places can falsify a sentence here. When one changes, re-read the matching entry
rather than assuming it still holds.
