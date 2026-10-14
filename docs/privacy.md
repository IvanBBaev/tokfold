# Privacy

This document covers what tokfold does with the data it is given, and what it does
not do. Every statement about what the code *does* names the code that makes it true,
the measurement that showed it, or a section of this document that names one of those
— the *Summary* restates what the sections below hold and is anchored by them, and
where a sentence draws an inference it says "therefore" and is drawn from the
statement above it. Four sentences do: one in this header, and one each in *What the
CLI writes*, *What the archive contains* and *What the MCP server sends back*, each
read back against the statement above it on 2026-10-14. A grep for the word answers
six, not four — this clause names it, and the sentence quoted at the end of this
paragraph no longer stands. Until 2026-10-14 the rule reached only a sentence that
*opens* with the word, and none here does: unwrap every paragraph, split on sentence
ends, and 0 of 249 open with it, so the clause covered nothing — while the one
inference that did not follow sat mid-sentence outside it, "Outputs are therefore free
of timestamps, and a run leaves no record of when it happened", whose second half
`ac7c08c` measured false. For the attacks this design does and does not resist, see
[`threat-model.md`](threat-model.md).

A statement below marked **enforced** is checked by CI on every push to `main` and on
every pull request. A statement marked **true today** was read back against the
**working tree**, not against a commit, and the statements were not all read back on
the same day — a measurement here carries its own date. The tree is not clean: at the
most recent read-back (2026-10-10) it held uncommitted changes on top of `main` at
`c6f800e`, and `git diff --shortstat` reported 16 files changed; both of those move
with every commit, so re-derive them rather than trusting the two values here. This
document is tracked — `df6a0fc` added it, and that same commit carried a
sentence calling it untracked, false the moment it landed. A statement here can
therefore differ from what `main` contains, and for a statement marked **true today**
nothing fails the build if a change breaks it — review is the only guard. The one
marked **enforced** is the exception, which is what the mark means: a change that adds
a network-capable dependency fails `cargo deny --locked check` in the `deny` job.
Until 2026-10-14 this said "nothing fails the build" of every statement here, which
the sentence after it contradicted. No check *blocks* the change either: `main` has no
branch protection (`gh api repos/IvanBBaev/tokfold/branches/main/protection` answered
"Branch not protected" again on 2026-10-14), so a failing run reports a broken
**enforced** statement but does not stop it landing.

A SHA-1 or a byte size beside a build command below names **bytes**, not this tree. The
commit named with it is the recipe: `git archive <commit> | tar -x -C <dir>` followed by
the command quoted beside the hash reproduces those bytes on macOS Darwin 27.0.0 arm64
with rustc 1.97.0. Two commits appear below for one command, because the bytes moved
between the measurements. A release build does not depend on where it is built — one
commit built as `--workspace --all-features` in two directories outside the repository
gave `30e93939…` both times — while a *debug* build's size does, which is why no size
below is a debug build's. So re-derive a hash here before trusting it to describe the
binary in hand, and read a build command as naming a build only together with a commit.
Until 2026-10-14 the bullet below called `834c01f2…` "a release build of the **working
tree**" and *Token counting* measured "on this tree"; at `181ec00` that tree builds
`637795cf…`, and `834c01f2…` is what `166873f` builds.

The published npm `0.0.1` CLI predates some of what *Files* describes: it has no
stale-archive deletion and no same-file guards (see
[`threat-model.md`](threat-model.md), the paragraph on `main` and the release).

## Summary

- tokfold makes no network connections.
- It sends no telemetry or usage data anywhere.
- It keeps no cache, no history and no log file.
- The only file it opens *to write* is the one you name with `--archive` — or, when
  that name is a symlink, the file it resolves to. It opens one other file by name and
  only reads it: the one you name with `--input`, where a symlink is followed too. The
  archive is read as well as written — at most four bytes of it, on the one path that
  clears a stale archive, and on one spelling of that path none at all. Until
  2026-10-14 this bullet said "opens by name", which the input read falsifies, and
  neither it nor the two bullets `7a005e3` rescoped alongside it — *What the CLI
  writes* on "the only function that opens a file to write it", *Caches and temporary
  files* on "no file of its own other than the `--archive` path" — said the archive is
  read at all. The quoted phrase was this bullet's alone: the note that attributed it
  to all three was written by `7a005e3` itself, and only its last clause held of the
  other two. See *What the CLI reads*.
- A redirect is the other way your data reaches disk: `tokfold expand > restored.json`
  writes your original bytes into `restored.json` verbatim. tokfold never names that
  file and never sets its mode; your shell does both.
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
  them. What answers that is this section's *The binary* bullet and `nm -u` on the
  built binary, not `cargo deny`.
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
  --include='*.rs'`), and it starts four programs — the binary under test
  (`env!("CARGO_BIN_EXE_tokfold")`), `/bin/sh`, to arrange descriptor redirections
  the Rust API cannot express, `mkfifo`, and `sh`, which the umask test spells
  without a path. It also re-runs *itself*: one test spawns two further copies of
  the suite binary through `std::env::current_exe()`. Two of those four names are
  bare, so `std::process::Command` searches `PATH` for them at run time, and the
  two spellings of the shell are not interchangeable. Under `PATH=/nonexistent` the
  suite binary fails `a_new_archives_mode_is_what_the_umask_leaves` with a
  `NotFound` panic and exits `101`, while
  `a_short_file_behind_a_read_only_descriptor_is_reported_like_a_long_one`, which
  spawns `/bin/sh`, passes unchanged; the two `mkfifo` tests take a third course and
  return early when the spawn fails, saying so in a comment, so a box without
  `mkfifo` on its `PATH` reports them as passes that assert nothing (measured
  2026-10-14, macOS Darwin 27.0.0 arm64). A `cargo test` run also forks through a
  dev-dependency: `proptest`'s default features include `fork`, which pulls in
  `rusty-fork 0.3.1` (`cargo tree -p tokfold-core -e features --all-features`). None
  of that is compiled into a released binary.
- **The binary.** A macOS arm64 release build of commit `181ec00` (SHA-1 `637795cf…`,
  `cargo build --release -p tokfold-cli` with default features, measured 2026-10-14).
  What moves that hash is the **feature set**, and the package selection only decides
  which packages a feature flag can reach; five builds of that commit, measured the
  same day, separate the two: `-p tokfold-cli` and `--workspace` both give
  `637795cf…` at 1 262 112 bytes
  with default features, so widening the selection alone changes nothing;
  `-p tokfold-cli --all-features` also gives `637795cf…`, because `--all-features`
  applies to the *selected* packages and `tokfold-cli` declares no `[features]` table
  of its own; `-p tokfold-cli --features tokfold-core/tiktoken` gives `d8910026…` at
  1 263 776 without `tokfold-core` being selected at all; and `--workspace
  --all-features` gives `30e93939…`, the same 1 263 776 bytes by a different route,
  because the wider selection puts `tokfold-core` in reach of `--all-features` and so
  turns on both `tiktoken` and the unimplemented `hf`. Selecting a package never
  enables its non-default features; `tiktoken` is not in core's `default` list. Those
  five builds of `166873f` give the values this bullet carried while it called them the
  working tree's: `834c01f2…` at 1 261 712 for the first three, then `a65170a2…` and
  `ca2cfc4a…`, both at 1 263 392. The count was "four" over the same enumeration of
  five until 2026-10-14.
  `637795cf…` imports none of these symbols: `socket`, `connect`, `getaddrinfo`, `sendto`,
  `bind`, `listen`, `fork`, `execv`, `posix_spawn`. The same holds for the binary npm
  ships for that platform (`tokfold-darwin-arm64` `0.0.1`, `bin/tokfold` SHA-1
  `8e2ed283…`). The measurement is `nm -u <binary>` filtered for those nine names as
  whole symbols rather than as substrings:
  `grep -E '^_?(socket|connect|getaddrinfo|sendto|bind|listen|fork)(\$.*)?$|^_(execv|posix_spawn)'`,
  where the last two match by prefix so that `execve`, `execvp` and `posix_spawnp`
  are caught as well, and the optional suffix admits macOS spellings like
  `_connect$UNIX2003`. It matches nothing in either binary. A filter that matches
  nothing proves nothing by itself, so it was run against controls: fed the thirteen
  underscored spellings those nine names generate — the nine bases each prefixed with
  `_`, plus `_execve`, `_execvp`, `_posix_spawnp` and `_connect$UNIX2003` — it matches
  all thirteen, and fed
  `dyld_stub_binder`, `_realpath$DARWIN_EXTSN`, `_pthread_get_stackaddr_np`, `_read`
  and `_write` it matches none (re-run 2026-10-14: 13 fed, 13 matched; 5 fed, 0
  matched). This sentence said "twelve" twice until that date, over the same
  enumeration of thirteen — count the bases rather than trusting either number.
  Spelled as a bare substring search instead, the same
  nine names do match one symbol in both binaries, and it is not a socket call:
  `bind` occurs inside `dyld_stub_binder`, dyld's lazy-binding helper, which every
  dynamically linked Mach-O imports, so a reader who re-runs the obvious spelling and
  sees that one hit has not found a regression (measured 2026-10-14, macOS Darwin
  27.0.0 arm64). The unfiltered lists are 83 undefined symbols for the `181ec00`
  build — and 83 for `166873f`'s, measured the same day — and 76 for the published one,
  so the filter had something to
  reject. Of those two, only the published one is stripped — not because a local build
  cannot be, but because nothing in the repository strips one by default: the workspace
  `Cargo.toml` declares no `[profile.release]` table, and `release.yml` passes
  `--config 'profile.release.strip="symbols"'` as a one-off so that a developer's
  build keeps its backtrace symbols. Passing that same override here does strip a
  local build, and with the rest of `release.yml`'s flags — `--target
  aarch64-apple-darwin` among them, which moves the bytes on its own — it gives
  `ab8315f3…` at 1 002 928 bytes for `181ec00` and `2a17afd0…` at 1 002 896 for
  `166873f` (both measured 2026-10-14). So for the published binary this says what
  it *imports* and
  not what it contains, while the `181ec00` build still carries 2 570 defined
  symbols, 443 of them mangled Rust names containing `tokfold`, that this measurement
  does not look at (`166873f`'s carries 2 562 and 440 — the two numbers this sentence
  held until 2026-10-14). Those names are Rust **v0** symbols, spelled `__R…`, so the
  legacy spelling `grep '_ZN.*tokfold'` answers 0 on the same binary and is not
  evidence of absence. The other four platform binaries were not measured.
- **Token counting.** The optional `tiktoken` feature of `tokfold-core` (off by
  default, and not enabled in the release build — `release.yml` builds `--package
  tokfold-cli` with no feature flags) embeds its encoding tables at compile time
  through `tiktoken-rs`, which `include_str!`s them, so nothing is fetched at run
  time. It does not follow that those tables reach *this* binary: nothing on the
  CLI's command line selects an exact estimator, so the linker drops them. Measured
  2026-10-14 on commit `181ec00` with one variable moved — the same `-p tokfold-cli`
  selection, the feature off and then on — `cargo build --release -p tokfold-cli
  --features tokfold-core/tiktoken` grows the binary by 1 664 bytes (1 262 112
  → 1 263 776, SHA-1 `637795cf…` → `d8910026…`) — a thousandth of the 1 681 126
  bytes of `cl100k_base.tiktoken` alone. The same pair at `166873f` grows it by 1 680
  (1 261 712 → 1 263 392), which is what this sentence reported for "this tree".
  Either way it downloads nothing.
- **Installing through npm** uses the network, as any `npm install` does: npm fetches
  the launcher package and one platform package from the registry. That is npm's
  traffic, not tokfold's. See *The npm launcher*.

## Telemetry

**None — true today, not enforced.**

- No crate contains code that collects, stores or transmits usage data. The three
  verbs are held in different places: *Network* above for transmitting, both by the
  enforced dependency ban and by the first-party `std::net` review, and *Files* and
  *Caches and temporary files* below for collecting and storing.
- **No clock is read — first-party review, and the binary's imports.** No non-test
  source in the workspace uses `std::time`, `SystemTime` or `Instant`. The one match a
  grep for those three returns in `crates/*/src` is a doc comment in
  `crates/tokfold-mcp/src/lib.rs`, which names `std::time` to say a dependency ban
  cannot reach it; "non-test" is load-bearing, because
  `crates/tokfold-cli/tests/cli.rs` imports `Duration` and `Instant` and polls a
  deadline with them. As with `std::net` above, no gate enforces this, and a grep over
  first-party sources says nothing about the dependency graph — so what answers for the
  graph is the measurement *Network* uses for sockets. `637795cf…` imports none of
  `time`, `times`, `clock`, `clock_gettime`, `clock_gettime_nsec_np`, `gettimeofday`,
  `mach_absolute_time`, `mach_continuous_time`, `getrusage`, `utimes`, `futimes` or
  `utimensat` as whole symbols, and neither does the binary npm ships for this platform
  (`tokfold-darwin-arm64` `0.0.1`, `bin/tokfold` SHA-1 `8e2ed283…`, taken from a
  tarball checked against the registry's `dist.shasum` `2ffdfaf3…`). The measurement is
  `nm -u <binary>` filtered with
  `grep -E '^_?(times?|clock(_gettime(_nsec_np)?)?|gettimeofday|mach_(absolute|continuous)_time|getrusage|f?utimes|utimensat)(\$.*)?$'`,
  and because a filter that matches nothing proves nothing it was run against controls:
  fed the thirteen underscored spellings those twelve names generate — the twelve bases
  each prefixed with `_`, plus `_time$UNIX2003` — it matches all thirteen, and fed
  `dyld_stub_binder`, `_realpath$DARWIN_EXTSN`, `_pthread_get_stackaddr_np`, `_read`
  and `_write` it matches none. Here the naive spelling agrees with the careful one: a
  case-insensitive substring search for `time` or `clock` over either list also answers
  0, so a reader who re-runs the obvious grep has nothing to explain away, which is not
  true of the socket filter and its `bind` inside `dyld_stub_binder` (83 undefined
  symbols in the local build, 76 in the published one; measured 2026-10-14, macOS
  Darwin 27.0.0 arm64, rustc 1.97.0).
- **What that grep would have missed.** Swept over the CLI's 34-crate graph, the three
  Rust tokens match four crates and no clock: every hit is the substring `Instant`
  inside "Instantiate", "Instantiates" or "Instantaneous", except a doc example in
  `syn`'s `src/mac.rs` that really does write `Instant::now()` — `syn` is a proc-macro
  dependency, so that file runs only during the build, and a doc example does not run
  at all — and `libc`'s `sceKernelGetSystemTime` declarations, which are PSP-only.
  `libc` declares 51 clock functions across its platforms, and no crate in the graph
  calls one. The npm launcher is not a crate, so the sentence above leaves it out, and
  over everything the launcher package contains the five JavaScript tokens `Date`,
  `hrtime`, `performance`, `setTimeout` and `setInterval` return exactly one match: a
  comment in `npm/tokfold/bin/tokfold` explaining why `pause` does *not* use
  `setTimeout`. **That comment names what it uses instead, and none of the five tokens
  can match it.** `pause` sleeps with
  `Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms)`, and a finite
  timeout is a duration the engine has to measure: the call returns only when the wait
  is over, and it returns the string `"timed-out"` to say which way it ended. Measured
  on Node v26.7.0, macOS Darwin 27.0.0 arm64, one run each under
  `node -e 'console.log(Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,N))'`:
  `"timed-out"` after 0.305 s, 0.551 s and 1.058 s for N of 250, 500 and 1000, against a
  0.075 s baseline for `node -e 'process.exit(0)'`, and the launcher's own budget --
  `EAGAIN_ATTEMPTS` (1000) pauses of `EAGAIN_PAUSE_MS` (1) -- cost 1.337 s. That budget
  is reachable without a debugger: `writeStderr` is the launcher's only stderr writer,
  and `fs.writeSync` raises EAGAIN whenever fd 2 is a non-blocking pipe whose reader has
  stopped, which is the caller's choice and not tokfold's. So the launcher reads no
  clock *value* -- `Atomics.wait` hands back a verdict, not a time -- but it does ask
  the engine to measure elapsed time, and a grep for those five tokens is a claim about
  those five tokens and not about the category in this bullet's heading. One case sits
  outside both: on Linux the process report `isMusl()` asks Node for, enumerated under
  *The npm launcher*, arrives with a wall-clock `header.dumpEventTime` and
  `header.dumpEventTimeStamp` filled in — a clock value the launcher causes to be read
  and never reads itself.
- **Outputs carry no timestamp; the filesystem records the run anyway.** Neither the
  rendering on standard output nor the archive holds a date. A twelve-row fixture
  compressed with `--archive` gives 323 bytes on stdout and an 831-byte archive, and
  searching both for `20[0-9]{2}-[01][0-9]-[0-3][0-9]` or a ten-digit epoch matches
  nothing. What is not free of timestamps is the filesystem. `write_file` opens the
  archive with `create` and `truncate`, so tokfold brings that file into being and the
  kernel stamps it: the same run left the archive an mtime and a birth time of the
  second it ran, seventeen seconds after the mtime of the input it read. A *rejected*
  pass leaves a record too, although it writes no archive at all — clearing a stale one
  unlinks it, and that moves the containing directory's mtime (measured 2026-10-14:
  `18:44:34Z` before the run, `18:44:37Z` after, exit `0`, nine bytes of passthrough on
  standard output). Until 2026-10-14 this bullet ended "and a run leaves no record of
  when it happened", which both of those falsify. tokfold reads no clock to do any of
  it: every timestamp here is the kernel's, set because a file was written or removed,
  which is why the first half of this bullet holds while the second did not.

## Files

### What the CLI reads

- The input, from `--input <path>` or standard input (`read_source` in
  `crates/tokfold-cli/src/main.rs`). It reads the whole input into memory.
- When `--archive` is given, the CLI inspects the named path's metadata on every pass,
  to decide whether that path is the same file as the input or the output
  (`ensure_archive_is_not_the_input` and `ensure_output_is_not_the_input` in `main.rs`).
  It reads *bytes* out of that file on one path only: when the input is rejected,
  `clear_stale_archive` has `inspect_archive_slot` decide whether a previous archive
  there must be cleared, and on the plain path that function opens the *resolved* target
  read-only and reads up to four bytes (the `TKFD` magic) sequentially, from a fresh
  handle — fewer if the file is shorter than the magic, which it answers as a foreign
  file and leaves alone (*Caches and temporary files* lists the measured slots). A
  successful pass reads nothing there — it truncates the file and writes over it.
  Measured against a release build of this tree (`cargo build --release -p tokfold-cli`,
  SHA-1 `637795cf…`, macOS Darwin 27.0.0 arm64, rustc 1.97.0) with a 12-byte
  `TKFD`-prefixed file at mode `0200` — writable, not readable — at the `--archive`
  path: a valid input exits `0` and overwrites it, while a rejected input exits `2` with
  "inspecting the archive path b.tkfd, which resolves to <path>: Permission denied (os
  error 13)" and nothing on standard output. Until 2026-10-14 this bullet said those
  four bytes are read "when `--archive` is given", which every successful pass
  falsifies.
- **On one spelling those four bytes may never be read at all.** On macOS a
  `--archive` path whose resolved form falls under `/dev/fd` is routed to
  `inspect_descriptor_slot` instead, which works on the path *as typed* — the only
  spelling that reaches the file, as the rustdoc of `inspect_archive_slot` derives —
  and asks the write question first: a slot it
  cannot open for writing is answered from that alone, with nothing read. Measured
  against the release build named above, with a 6 107-byte archive from a successful
  pass behind the descriptor and a 2-byte non-JSON input: `compress --input bad.txt
  --archive /dev/fd/3 3< b.tkfd` exits `0`, leaves `b.tkfd` byte-identical and says
  "left /dev/fd/3 untouched: it cannot be opened for writing" — the archive survives
  because the write could not reach it, not because its bytes said anything. The
  mirror case reads nothing either, because it cannot: with `3>> c.tkfd` the
  descriptor is write-only, the read-only open is refused, and the pass exits `2` with
  "inspecting the archive path /dev/fd/3: Permission denied (os error 13)", the
  rejected document lost. (The append form is what was measured; `3>` would have had
  the shell truncate the file before tokfold started.) Where the read does happen on
  this branch it is positional, at offset `0` (`read_head_at_start`), because opening
  a `/dev/fd` spelling on macOS duplicates the descriptor and a duplicate shares the
  caller's file offset. Until 2026-10-14 the bullet above said this inspection "opens
  the path read-only", naming neither the write open that precedes it nor the branch
  that answers without reading — while *What the CLI writes* recorded that same write
  open. Both halves were written by one commit, `b5aad2a`.

### What the CLI writes

- **Standard output:** the rendering (`compress`), the restored original (`expand`),
  the measurement report (`stats`), the JSON-RPC replies (`mcp`), or `--help` and
  `--version` text. The first three go through `write_stdout` in `main.rs`, the
  replies through `tokfold_mcp::stdio::serve_stdio`, and the help and version text
  through clap's own printer when standard output is a terminal and `write_stdout`
  otherwise — the two writers the rustdoc of the `--help`/`--version` branch in
  `main.rs` sets out against each other. That makes three writers in all, which is
  why a fix to one of them is not a fix to standard output. Until 2026-10-10 this
  bullet listed the four subcommand outputs only, which read as the whole of what
  reaches standard output.
- **Standard error:** diagnostics, all through one function (`warn` in `main.rs`).
  These can include the paths you passed and sizes. The `mcp` subcommand also prints
  its experimental notice there on start-up.
- **The `--archive` path, and nothing else tokfold opens to write.** `write_file` in
  `main.rs` is the only function that *writes* a file. Two others open one to write and
  write nothing: `archive_identity`, which asks the archive path the way the write
  reaches it, and the writability check in `inspect_descriptor_slot`. Both open
  write-only, without `create` and without `truncate`, so neither can bring a file into
  being, change its mode or shorten it, and neither is reached for anything but a
  regular file — `archive_identity` only for a `/dev/fd` spelling of one. That open is
  observable: against the release build named in *What the CLI reads*, `compress
  --input good.json --archive /dev/fd/3 3< good.json` is not called a collision,
  because a read-only descriptor refuses the open, and it fails on the write instead
  ("writing archive to /dev/fd/3: Permission denied (os error 13)"); the same command
  with `3<> good.json` is refused as one file, before anything is written. Until
  2026-10-14 this bullet called `write_file` the only function that *opens* a file to
  write it, which both probes falsify. A redirect still puts
  standard output on disk — the *Standard output* bullet says what goes there, and on
  `expand` it is your original bytes verbatim — but that file is the caller's: tokfold
  does not name it, create it or set its mode. Read "the `--archive` path" as *the file
  that path resolves to*: both the write and the deletion of a stale archive follow a
  symlink at that path to its target. tokfold does open one other file of its own, the
  `--input` path, and only reads it (*What the CLI reads*).
  - It opens the path with `write`, `create` and `truncate`, and sets no permissions,
    so a new archive gets your process umask's default mode. Measured on macOS: under
    umask `022` a new archive is `0644`, which other local users can read; under
    umask `077` it is `0600`.
  - An existing file keeps its mode when it is overwritten: a file at `0666` stayed
    `0666` under umask `077`.
  - If the archive must be private, set a restrictive umask (`umask 077`) or create
    the file with the mode you want before the first run. The same applies to a file
    you redirect standard output into, which gets the same mode from the same umask:
    measured under `022`, `077` and `000`, both files came out `0644`, `0600` and
    `0666`.
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
  *layout* admits 43 to 52. Nothing tokfold writes reaches the top of that:
  `compress` rejects an input longer than `u32::MAX`, whose ULEB128 encoding is five
  bytes, so 47 is the widest header a length `compress` can be made to write. The
  wider headers are not unreachable, only unreachable through compression —
  `Header::new` in `crates/tokfold-core/src/format.rs` takes a `u64`, so an embedder
  writes 48 bytes at 2^35 and 52 at 2^63, and `expand` refuses such an archive at its
  length field. Reading a plaintext out of an archive you did not write therefore
  means skipping the header by the payload offset `Header::decode` reports, not by a
  constant. `format.rs` draws the distinction between the two ceilings itself, in the
  rustdoc that pins `(u64::from(u32::MAX), 47)` as "the longest input the parser
  addresses".
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

- **None at run time.** The CLI opens no file of its own other than the two you name —
  the `--archive` path and the `--input` path, or, through a symlink at either, that
  path's target. The input is only ever read. The archive takes four verbs, not the two
  this bullet used to give it: a successful pass truncates it and writes the archive; a
  rejected pass opens it and reads **at most** four bytes, the `TKFD` magic (*What the
  CLI reads*); if those four bytes are the magic the file is **deleted** (*What the CLI
  writes*); and on the one spelling routed to `inspect_descriptor_slot` a regular file
  there is opened for *writing* first — a probe that writes nothing, and that answers an
  unwritable slot with nothing read at all. "Four bytes" is a ceiling and not a figure:
  `read_exact` fills what it can and then reports `UnexpectedEof`, which that function
  answers `Foreign`. Measured against the release build named under *What the CLI reads*
  (`637795cf…`) with a 2-byte non-JSON input, each slot a regular file at the
  `--archive` path: content of 0, 2 and 3 bytes is each left untouched at exit `0` ("it
  is not a tokfold archive"), a 4-byte `TKFD` is removed at exit `0`, and a *directory*
  there is opened and the read then fails outright, ending the pass at exit `2` ("Is a
  directory (os error 21)") with the rejected document lost. Until 2026-10-14 this
  bullet gave each path a single verb, "the `--archive` path it writes and the `--input`
  path it reads", which the archive's own read falsifies; the commit that corrected it,
  `b5aad2a`, wrote the read in as an exact four bytes and left the deletion and the
  write probe out, and wrote the same figure into the *Summary* bullet and into *What
  the CLI reads* as well; `146393b` corrected the second of those for the branch that
  reads nothing and left its figure unqualified. All three now say *at most*. Neither
  library crate opens a file by name at all — and not only under the three spellings
  this bullet used to cite (`File::open`, `OpenOptions`, `fs::write`), which is a claim
  about three spellings and not about opening a file: `grep -rn 'fs::'
  crates/tokfold-core/src crates/tokfold-mcp/src` returns one line, `use std::fs::File`
  in `stdio.rs`, and `File` over the same two paths returns that line plus the two
  `File::from(…)` calls beside it and nothing else, while the control that both paths
  reach anything at all is `pub fn`, which returns 5 files in the first path and 7 in
  the second. `serve_stdio` in `tokfold-mcp` does build two `std::fs::File` values, but
  from duplicates of the descriptors it inherits on standard input and standard output,
  not from a path. A file you redirect standard output into is not a temporary file of
  tokfold's — it is where you asked the output to go — but it is on disk, and on
  `expand` it holds your original bytes verbatim.
- **A `cargo test` run is a different story, and the *None at run time* bullet is
  about the shipped binary.** The workspace's own sources name a temporary directory in two
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
  original**. Measured on `{"a":1}` against a release build of commit `7827f72`
  (`cargo build --release -p tokfold-cli`, SHA-1 `637795cf…`, 1 262 112 bytes): the
  reply carried a 50-byte archive, base64-encoded, that decodes to a `TKFD` header
  followed by `{"a":1}` at offset 43. A client that logs or stores tool results
  therefore logs or stores the original too.
- **A reply with no archive is not a reply with nothing of yours in it.** That is the
  tool's other outcome, and it is the one a reader is likeliest to misread. When core
  declines to compress, `passthrough_outcome` in `crates/tokfold-mcp/src/tools.rs`
  answers `"compressed": false` with `reason` and `reasonCode` and **no `archive` at
  all** -- and the `rendering` it does return is your input byte for byte, as is the
  `content` text, so the complete original still leaves twice. Measured on a release
  build of commit `7827f72` (SHA-1 `637795cf…`) with the input
  `SECRET-MARKER-7c1d not json at all`: `structuredContent` held exactly `compressed`,
  `reason`, `reasonCode` and `rendering`, the marker appeared once in
  `structuredContent` and once in `content[0].text`, and `rendering` equalled the input.
  The refusal's own fields quote none of it -- `reason` is `CompressError`'s `Display`,
  which names an offset, a size, a depth or a limit and nothing else ("invalid JSON at
  byte 0: expected a JSON value" on that input), and the message it interpolates is one
  of 21 fixed literals in `crates/tokfold-core/src/tape.rs`, none of them built with
  `format!`. So on that path the double echo is the whole of the exposure, not part of
  it.
- **`tokfold_decompress`** returns the original **twice**, like `tokfold_compress`:
  once as `content[0].text` and once as `structuredContent.text`. Measured on a
  release build of commit `166873f` (`cargo build --release -p tokfold-cli`, SHA-1
  `834c01f2…`) with an
  archive of `{"secret":"hunter2","rows":[{"a":1},{"a":2}]}`: both fields carried
  that document in full. A client that stores only one of the two still stores the whole
  original.
- **`tokfold_estimate`** returns the statistics without the payload: neither the text
  block nor `structuredContent` carries any part of the input back. The
  `estimate_reports_stats_without_the_payload` test in `tools.rs` pins that over the
  whole reply rather than over two key names -- each of its three documents holds a
  marker string and reaches a different encoder, and the marker must appear in neither
  half. The compressing documents are the cases that matter: on a passthrough input
  there is no rendering for the reply to withhold in the first place.
- Every result also carries the server's name and version in `_meta`.
- **The experimental notice is not part of any reply, and the crate does not print
  it.** `EXPERIMENTAL_NOTICE` is a constant in `crates/tokfold-mcp/src/lib.rs`; the
  CLI's `cmd_mcp` (`crates/tokfold-cli/src/main.rs`) writes it to **standard error**
  before the loop starts, so it reaches the terminal or the host's log, not the
  JSON-RPC client. Measured on a release build of commit `166873f`
  (`cargo build --release -p tokfold-cli`, SHA-1 `834c01f2…`) with the input closed:
  standard error held exactly
  one line, byte-identical to that constant,
  saying the server is unhardened and unaudited and must not be used with production
  secrets.

## Environment variables

**tokfold's own code reads no environment variable at run time** (true today). The
workspace has exactly two `std::env`/`env!` uses outside the integration-test
targets, and neither is a run-time read: `std::env::temp_dir` in
`crates/tokfold-cli/src/main.rs`, which is inside the `#[cfg(test)] mod
archive_identity_tests` and so not in a released binary at all, and
`env!("CARGO_PKG_VERSION")` in `crates/tokfold-mcp/src/protocol.rs`, which cargo
substitutes at compile time (`grep -rn 'std::env\|env!(' crates/*/src`). That glob
reaches no test target, so it says nothing about them; the rest of this paragraph
comes from running it again over `crates/*/tests`. The test targets add four more
compile-time substitutions — `env!("CARGO_BIN_EXE_tokfold")` and
`env!("CARGO_TARGET_TMPDIR")`, and also `env!("CARGO_PKG_VERSION")` in
`crates/tokfold-cli/tests/cli.rs` and `env!("CARGO_PKG_DESCRIPTION")` in
`crates/tokfold-mcp/tests/experimental_notice.rs`. At run time they read two
variables, not one. `PROPTEST_CASES` is read by the test code itself, with
`std::env::var`, in both `crates/tokfold-core/tests/roundtrip.rs` and
`crates/tokfold-core/tests/hostile.rs`, over and above `proptest`'s own sweep of it
and its siblings through `env::vars_os` in
`proptest-1.11.0/src/test_runner/config.rs`. And `PATH` is searched by
`std::process::Command` on behalf of the two programs that
`crates/tokfold-cli/tests/cli.rs` spawns by bare name, as this document's
*First-party code* bullet records. One further `std::env` call in that file reads no
variable at all: `std::env::current_exe()` asks the loader. None of this reaches a
released binary, and none of it bears on the sentence this section opens with. The
shipped binary does read some variables through its dependencies and the standard
library:

| Variable | Read by | Effect |
|---|---|---|
| `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE`, `TERM`, `CI` | `anstream` (via `anstyle-query`), used by `clap` | Whether help and error text is coloured. |
| `RUST_BACKTRACE`, `RUST_LIB_BACKTRACE` | the standard library, for `anyhow` errors and for panic messages | Whether an `anyhow` error captures a backtrace (the CLI prints errors with `{:#}`, which does not show one) and whether a panic message prints one. |

`clap` reads the command line (`std::env::args_os`). It does not read `COLUMNS` or
`LINES`, because its `wrap_help` feature is not enabled: the workspace `Cargo.toml`
adds `derive` to clap's default features, and `wrap_help` is not one of them
(`cargo tree -p tokfold-cli -e features -i clap`). None of these variables changes
what is compressed, and none is sent anywhere.

The table is an enumeration, so here is how it was derived and what it leaves out. The
graph is the CLI's own, 34 crates (`cargo tree -p tokfold-cli`) — 33 of them with
`--edges normal`, the 34th being `version_check 0.9.5`, which enters only as the build
dependency of `generic-array`. It is the graph cargo resolves for this host
(`aarch64-apple-darwin`): the only target-conditional entries anywhere along the chain
from `tokfold-cli` to `anstyle-query` are `anstream`'s and `anstyle-query`'s own
`cfg(windows)` dependencies on `anstyle-wincon` and `windows-sys`, so a Windows graph
holds two crates this one does not, while none of the feature sets below is
target-conditional. Sweeping all 34 for `env::var`, `env::vars`, `env::set_var` and
`env::remove_var`, with each crate's `src/` tree counted separately from its build
script and with examples, benches and tests skipped, matches nine crates rather than
one, so here are the eight that do not reach the table. A second pass for
`use std::env` and `use core::env` imports — which is what would let an unqualified
call hide from the first — adds no crate and one file, `clap_builder`'s
`src/builder/command.rs`, whose import is for `env::args_os`, the command line.

Seven of the eight read the environment while the build runs, on the build machine, and
none of those reads is compiled into the binary: `clap_derive 4.6.1` is a proc macro
and reads `CARGO_PKG_NAME`, and any variable an attribute names, while it expands the
derive (`src/derives/parser.rs`, `src/item.rs`); `version_check 0.9.5` is a library
that build scripts call — `generic-array`'s does — which is how `RUSTC` and `RUSTFLAGS`
come to be read at all; and `anyhow 1.0.103`, `libc 0.2.186`, `proc-macro2 1.0.106`,
`quote 1.0.46` and `thiserror 2.0.18` read it only in their own `build.rs`. The eighth
crate, `clap_builder 4.6.0`, is the only one whose matches sit in `src/` beside code
that ships, and none of its eight matches is live: six are lines of doc examples, one
is `parse_env` in `src/output/help_template.rs` behind `#[cfg(feature = "wrap_help")]`,
and the last is `Arg::env` in `src/builder/arg.rs` behind `#[cfg(feature = "env")]` —
the one site in this graph that would read a variable named by the program rather than
a fixed one. The features enabled for that crate are `color`, `error-context`, `help`,
`std`, `suggestions` and `usage`
(`cargo tree -p tokfold-cli -e features -i clap_builder`), so neither function is
compiled, and nothing in `crates/tokfold-cli/src` calls `Arg::env` — with the feature
off, such a call would not compile.

That leaves `anstyle-query 1.1.5` as the only crate in the graph that reads the
environment at run time, and it names six variables at eight sites: `CLICOLOR`,
`CLICOLOR_FORCE`, `NO_COLOR`, `TERM` (three sites), `COLORTERM` and `CI`. The table
carries five of them. `COLORTERM` is left out because `anstyle_query::truecolor()` is
the only function that reads it and nothing in this graph calls that function. The
crate is reached only through `anstream`'s `auto` feature
(`cargo tree -p tokfold-cli -e features -i anstyle-query`), and neither `clap` nor
`clap_builder` names `anstyle-query` in its sources or its manifest, so `anstream` is
its only caller — and `anstream`'s sources name seven of its items: the six functions
that read the table's five variables, `clicolor`, `clicolor_force`, `no_color`,
`term_supports_color`, `term_supports_ansi_color` and `is_ci`, plus
`windows::enable_ansi_colors`, which reads none. `truecolor` is the one public function
they leave out. Grepping `truecolor` over `anstream`, `clap`, `clap_builder`,
`colorchoice` and the other `anstyle*` sources matches nothing; the three matches
inside anstyle-query itself are its doc line, its signature and the string it compares
against. The two `RUST_*` variables are the standard library's, not a dependency's. The
optional `tiktoken` feature was not in the measured graph, so this table does not cover
it.

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
  with Node's default environment handling, and every variable whose name and value
  are valid UTF-8 reaches it unchanged: with such an environment a child spawned
  through the launcher and the same child run directly see byte-identical entries
  (measured on macOS, 57 of 57). A variable whose bytes are *not* valid UTF-8 does
  not survive the trip, because `spawn` without an `env` option does not hand on the
  caller's `environ` -- it serialises `process.env`, a JavaScript object of decoded
  strings, back into `KEY=value` pairs. Measured: a value of `0xFF 0xFE 0x80` arrives
  as nine bytes, `0xEF 0xBF 0xBD` three times, one U+FFFD per undecodable byte; and a
  name that is not valid UTF-8 is absent from `Object.keys(process.env)` altogether,
  so the pair is never built and the variable does not reach the binary at all. None
  of the variables tokfold itself reads (*Environment variables*) has a name or a
  meaningful value outside ASCII, so this changes no tokfold behaviour -- it is
  recorded because "unchanged" otherwise reads as a promise about every environment.
- **No files written.** `node:fs` is used only to write the launcher's own error
  messages to standard error (`fs.writeSync(2, …)`).
- **Arguments and streams pass straight through, with one decoding caveat on the
  arguments.** The binary receives `process.argv.slice(2)` and the caller's standard
  input, output and error (`stdio: "inherit"`). The streams are inherited
  descriptors -- nothing is copied, decoded or buffered by the launcher, which is why
  it never sees the data. The arguments take the same UTF-8 round trip as the
  environment above, so an argument byte that does not decode is replaced: measured,
  an `--input` path ending `caf` `0xE9` `.json` reaches the binary with that `0xE9`
  rewritten as `0xEF 0xBF 0xBD`. Run directly the bytes survive, because the binary
  takes its paths as `PathBuf` from clap's `args_os`. On this platform no file is
  lost that way: APFS refuses to create a name holding an undecodable byte at all
  (`EILSEQ`, errno 92, measured), so there is nothing for the rewritten path to miss.
  On a filesystem that accepts arbitrary bytes in a name this has not been measured.
- **One process report on Linux.** To tell glibc from musl, `isMusl()` in
  `npm/tokfold/lib/resolve.js` calls `process.report.getReport()`. It does this only
  on Linux. Node builds that report in memory, and the environment variables are one
  of the eleven top-level keys it holds, so naming them alone understates it.
  Enumerated on Node v26.7.0 on this macOS box — where the call is not on the
  launcher's path, and is made here only to see the shape — the report is 45 843 bytes
  of JSON over `header`, `javascriptStack`, `javascriptHeap`, `nativeStack`,
  `resourceUsage`, `uvthreadResourceUsage`, `libuv`, `workers`,
  `environmentVariables`, `userLimits` and `sharedObjects`. Its `header` carries the
  process's whole `commandLine` with every argument, its `cwd`, its `processId` and
  `threadId`, the machine's `host` name, the OS and CPU inventory (10 entries here), a
  wall-clock `dumpEventTime` and `dumpEventTimeStamp`, and 17 `networkInterfaces`
  entries with their MAC and IP addresses — the first of them
  `{"name":"lo0","internal":true,"mac":"00:00:00:00:00:00","address":"127.0.0.1",…}`.
  `sharedObjects` lists 414 loaded library paths, and `environmentVariables` has one
  entry per `process.env` key: 51 here, and the control is that the two counts are
  equal. The byte figure moves run to run with the command line, the stacks and the
  environment it copies, so it is a scale and not a constant. The launcher reads one
  field out of all of that, `header.glibcVersionRuntime`, and discards the rest:
  nothing is written and nothing is sent anywhere. The Linux field set is not measured
  here, and the control that the difference between the platforms is real rather than
  assumed is that `'glibcVersionRuntime' in r.header` is **false** on this box, which
  is why the call sits behind a Linux check at all.

## Keeping this document true

Each statement above about what the code *does* names the code that makes it true,
the measurement that showed it, or a section of this document that names one of those
— the same rule the header states, and it is worth stating twice only if both say the
same thing. Until 2026-10-10 this one dropped the "or the measurement" half, which
made it the stricter of two rules the document gave for itself, and under it the two
archive-mode bullets under *What the CLI writes* read as violations: `write_file` sets
no permissions, so no crate's `src` holds `0644`, `0600` or `0666` —
`grep -rnE '0o?(644|600|666)' crates/*/src` returns nothing, and the control that the
glob reaches anything at all is `MAGIC`, which it finds in three files — and only the
measurement beside them does. A change to a named place can falsify a sentence here.

Two sentences of this section were false from the day `166873f` wrote them,
2026-10-10. One said a `git grep` found none of the three "in any crate's `src` or
`tests`". The same glob run over `crates/*/tests` returns six lines, all in
`crates/tokfold-cli/tests/cli.rs`: the literals `0o644`, `0o600` and `0o666` inside
`a_new_archives_mode_is_what_the_umask_leaves`, and the bare spelling this document
uses in that test's own rustdoc. Either spelling alone misses the other, which is how
the sweep came back empty. The other sentence said only the network ban on
dependencies is checked mechanically, three commits after `6d6659e` added that test,
which pins all three archive figures inside `cargo test --workspace`. What holds of
the ban is narrower: it is the one statement above marked **enforced** and the one
with a gate that exists for it — `deny.toml`'s `[bans]` list, run by the `deny` job in
`.github/workflows/ci.yml` — while the umask figures are pinned by a test answering
for a claim in [`threat-model.md`](threat-model.md), not for this file. No check
anywhere compares a sentence here with the fact behind it, because nothing in the
build reads this document: `git grep 'privacy\.md' -- crates .github fuzz Cargo.toml`
returns nothing, and so does the looser `privacy`, so that silence is not an artefact
of the spelling. The control that the pathspec set reaches code at all is `tokfold`,
and it has to be counted per element, because one total certifies no element of a
four-part pathspec: had `fuzz` been untracked, the total would still look healthy and
the claim about `fuzz` would be empty. It matches 28 of the 43 tracked files under
`crates`, 2 of 3 under `.github`, 16 of 1353 under `fuzz`, and `Cargo.toml` itself —
47 of 1400 in all, measured 2026-10-14 with git 2.54.0. Which matcher ran is worth
naming for this section's two shell greps and not for its two `git grep` runs, because
`git grep` is git's own: the shell `grep` here answers from ugrep 7.8.4 and from BSD
grep 2.6.0, with the same counts from both. This sentence read "four files" between
`f220e42` and this correction, both on 2026-10-14.
Everything marked "true today" needs a reviewer to re-read this file when the named
code changes.
