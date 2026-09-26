# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the version is below 1.0 the public API is unstable and may change in any
release, without a deprecation window.

Every date here is **UTC**, which is what the npm registry reports and therefore the
only timezone in which the release timestamps can be checked against anything.

## [Unreleased]

Nothing here is published. `0.0.1` on npm is unaffected until a release goes out.
Most behaviour changes here describe a difference from what an installed `0.0.1`
does today; an entry that fixes a defect introduced after `0.0.1` and never
released says so, and names what `0.0.1` does in the same case.

### Fixed

- **A rejected `compress` no longer leaves a stale recovery archive at
  `--archive`.** `compress` treats a `CompressError` as a passthrough and exits
  `0`; an archive an *earlier* successful run had written at the same path
  used to survive that, still verifying its own checksum. A pipeline that re-ran
  `compress` and then `expand` therefore saw two clean exits and reconstructed a
  *different* document, with nothing anywhere signalling that it was stale. A
  rejected pass now clears that archive.

  The path is first asked as typed, the way the successful write reaches it; a path
  that names no file that way is an empty slot. Otherwise it is resolved once, and
  the resolved target is what gets both inspected and removed — `File::open` follows a symlink and `fs::remove_file` does
  not, so clearing the link would leave the stale archive readable behind it. This
  matches the success path, where opening the path for writing follows the link and rewrites the
  target. Immediately before the unlink the target's identity is re-read and
  compared with the one it was inspected under, so a file swapped in between is
  refused (exit `2`) rather than deleted. That narrows the window; it does not close
  it, and none of it is a security boundary. Apart from a directory, only a regular
  file is opened for the inspection: a FIFO, socket or device cannot hold bytes a later `expand` would read
  back, so it is classified without being opened — opening a FIFO would block until
  a writer appeared — and left where it was. A directory is opened deliberately, so
  the read fails with the platform's own diagnostic.

  The deletion is keyed to the `TKFD` archive magic, which is a *necessary*
  condition for the hazard and not a sufficient one: only a file carrying it can be
  expanded into a stale document, but carrying it does not make a file expandable.
  The check is over-inclusive in that direction on purpose: a file that does **not**
  open with the magic is left byte-for-byte as it was and stderr says so, which is
  what makes `compress --archive notes.md` safe for an ordinary `notes.md`. (The
  original wording justified this as "never wider than what a successful pass does to
  the same path". That was later measured and it is false — see the `Documentation`
  entry on the invariant the deletion was justified by.) The check reads exactly as many bytes as the magic is long, and it reads
  `tokfold_core::format::MAGIC` itself rather than a copy, so a change to the
  archive header cannot leave the CLI looking for a header the engine no longer
  writes.
- **`compress --archive` aimed at its own input is refused** before the input is
  read and before anything is written. Both outcomes are destructive to the archive
  path — success truncates it, rejection now deletes it — so aiming it at the source
  destroys the very bytes the archive exists to recover. The refusal comes before
  tokfold knows which outcome the pass would have, so it also stops a rejected input
  that does not open with the archive magic, which `0.0.1` passed through with exit
  `0` and left in place; that now exits `2`. The two sides are compared
  by the identity the operating system gives the files (`st_dev`/`st_ino` on Unix),
  not by their spelling, so a symlink, a `..` detour, `--input ./log.json --archive
  log.json` and — on Unix — a **second hard link to the same file** are all
  recognised as one file. Path comparison alone could not see the hard link: `canonicalize` answers
  with the name it was handed, and two links to one inode are two legitimate names.
  Where the platform exposes no such identity the comparison falls back to the
  canonicalized paths — which still catches the spellings, and loses only the hard
  link — and then to the literal spelling, which loses nothing because a
  non-existent input fails on the next line and a non-existent archive has nothing
  to destroy. On Unix, an input redirected onto standard input is compared the same way:
  the descriptor is duplicated into an owned one through `AsFd`, wrapped in a
  `File` and asked for its metadata — no path and no `unsafe` — so `tokfold
  compress --archive log.json < log.json` is refused like `--input log.json`, and
  so is a hard link or symlink to the redirected file. What the guard cannot see is
  a pipe: `cat log.json | tokfold compress --archive log.json` carries the bytes and
  no file, so there is nothing to compare and the pass overwrites `log.json`. Off
  Unix, standard input has no identity to ask for, so a redirect is not refused
  either, and the `--input` comparison falls back to the canonicalized paths. Only
  a regular file can be destroyed here — a successful pass refuses to write anything
  else and a rejected one leaves it untouched — so two spellings of one device or
  pipe (`--input /dev/null --archive /dev/null`) are not compared and pass through as
  they did in `0.0.1`.
- **`compress --archive` aimed at its own standard output is refused (Unix).**
  `tokfold compress --archive a.tkfd > a.tkfd` (or `>>`) wrote the archive and the
  rendering into one file, so a later `expand` refused it as corrupt; with the
  rejected-pass clearing above, a rejected input would additionally delete the file
  its own passthrough was being appended to, losing both. The descriptor behind
  standard output is now compared with the archive path by the same identity as
  standard input, and a match exits `2` before tokfold reads or writes anything —
  a rejected input over a file without the magic included, which `0.0.1` appended to
  that file with exit `0`. A `>` redirect has truncated the file before tokfold starts, so the refusal cannot
  restore what `>` removed; it saves a `>>` or `1<>` target and stops the run. A pipe,
  a terminal and `/dev/null` are not regular files and are never refused.
- **Every command refuses a standard output redirected into its own input (Unix).**
  `0.0.1` compared nothing here: `tokfold compress -i log.json >> log.json` appended
  the rendering to the document it had just read, `tokfold expand < a.tkfd >> a.tkfd`
  appended the document to its own archive, and `tokfold mcp < requests >> requests`
  was a feedback loop — the server read its own replies back as requests and answered
  them, and the file grew until the process was killed. `compress`, `expand`, `stats`
  and `mcp` now compare the file behind standard output with the input, whether it
  arrives through `--input` or standard input, and exit `2` with
  `<input> is the file standard output is redirected to; output may not be written
  into the input it is read from` before reading or writing anything. `compress`,
  `expand` and `stats` read their whole input before the first write, so a `>>`
  appends after it rather than destroying it; the refusal still stands, because a
  document with its own rendering appended is not the document, and the message says
  what would happen rather than claiming a loss. With `>` the input is
  already empty by then; the refusal keeps a rejected empty input from also deleting
  the archive at `--archive`, which is the last copy of the document in that command
  line.
- **The same-file guards above see through `/dev/stdin`, `/dev/stdout` and
  `/dev/fd/N` on macOS.** `0.0.1` had no such guards at all. macOS reports the device
  of its descriptor filesystem, not of the file, for those paths, so a comparison of
  `(dev, ino)` alone would let `--input /dev/stdin --archive log.json < log.json` and
  `--archive /dev/stdout >> a.tkfd` through. A path that resolves under `/dev/fd` and
  is a regular file is compared by the identity of the file it opens, and both are
  refused.
- **The same-file guards ask each side the way its own operation reaches it.** A
  `/dev/stdin`, `/dev/stdout` or `/dev/fd/N` spelling opened on macOS duplicates a
  descriptor that is already open, so
  whether it opens at all depends on that descriptor's access mode, and a single
  read-only open used for both sides called things collisions that could not
  collide. The input side is now identified through a read-only open and the archive
  side through a write-only one (no create, no truncation); a side its own operation
  cannot open is never the same file as anything, and only two regular files are
  compared. So `--input nope --archive nope`, and a dangling symlink named twice,
  fail with `reading input file <path>: …` and exit `2` as in `0.0.1`, instead of
  being called one file; `--archive /dev/stdin` over a read-only standard input
  passes the rejected input through with exit `0` as in `0.0.1`, since a write
  cannot reach that file; and `--input /dev/stdin` or `--input /dev/stdout` over a
  write-only descriptor fails with `reading input file <path>: …` and exit `2`,
  since no read can reach it. Real collisions — `--input bad.txt --archive bad.txt`,
  `< bad.txt`, `3<> bad.txt` behind `/dev/fd/3`, `-i ok.json >> ok.json` — are still
  refused before anything is read or written. Measured on macOS; on Linux
  `/dev/fd/N` reopens the file through `/proc` with the requested mode rather than
  duplicating the descriptor, and the tests expect the descriptor spellings above to
  be refused there as before.
- **A rejected `compress` whose `--archive` is a descriptor spelling on macOS.**
  `realpath` of `/dev/fd/N` (or `/dev/stdin`, `/dev/stdout`, `/dev/stderr`) open on a
  regular file returns `/dev/fd/<the file's basename>`, which names nothing, so the
  stale-archive check cannot follow it to the file. Such a path is classified through
  the typed spelling instead: a device, pipe or socket — never read, even when a
  pipe already holds bytes — a file shorter than the magic, or a file behind a
  read-only descriptor, which the archive write cannot reach, is left untouched and
  the input passes through as in `0.0.1`
  (`compress --archive /dev/fd/3 3> a.tkfd`, `--archive /dev/stdin < a.tkfd`); a
  directory exits `2`; a stale archive behind a descriptor the write reaches, which no
  name reaches and nothing can remove, is refused at exit `2` with `a stale recovery
  archive is open behind <path>, which names a descriptor rather than a file, so it
  cannot be removed`; and a file of four bytes or more that the descriptor cannot read
  (`--archive /dev/stderr 2>> log`) is refused at exit `2` too, as the success path
  refuses it. When the refusal comes from the archive path the message says the input
  was `not passed through`, since nothing has reached stdout; `passing it through
  failed` is kept for a failed write, which may have emitted part of it. The magic is
  read at offset `0` with a positional read, so the check neither depends on nor moves
  the offset the descriptor shares with the caller.
- **A file left untouched at `--archive` is no longer called a "non-tokfold file".**
  The path may be a pipe, a device or a socket; the line now reads `left <path>
  untouched: it is not a regular file` for those and `left <path> untouched: it is
  not a tokfold archive` for a regular file.
- **A standard output redirected read-only onto the input is not reported as a
  collision.** `tokfold compress -i in.json 1< in.json` can destroy nothing — no
  write through that descriptor reaches the file — so it now fails with the `Bad file
  descriptor` the first write reports, like any other read-only stdout, instead of
  being called a collision. The guard asks a regular file's descriptor with a
  zero-length `write` before comparing identities, and sends nothing to anything
  else: a zero-length `write` is an empty record on a datagram socket and stops a
  background job on a terminal under `stty tostop`.
- **A standard input redirected write-only onto the output or the archive is not
  reported as a collision either.** `tokfold compress --archive a.tkfd 0>> a.tkfd`
  reads nothing through that descriptor, so it now fails with `reading standard
  input: Bad file descriptor` — the mirror of the read-only stdout above. The guard
  asks a regular file's descriptor with a zero-length `read`, which consumes
  nothing.
- **`--archive` refuses a path whose opening does not empty it.** On macOS, opening
  `/dev/fd/N`, `/dev/stdout` or `/dev/stderr` duplicates a descriptor that is already
  open (so is `/dev/stdin` over a descriptor opened for writing, `0<> file`): the
  truncation is ignored and the offset and append mode are shared. `0.0.1`
  wrote the archive through such a path after whatever the file held, so a
  successful pass with `--archive /dev/stderr 2>> log` exited `0` having appended an
  archive nobody could expand to the log. The archive is now opened, checked to be
  empty at offset `0`, and refused with `refusing to write the archive to <path>:
  opening it did not empty it` otherwise.
- **A standard output whose peer reset the connection is a reader that has gone.**
  `0.0.1`'s `compress`, `expand` and `stats` forgave only `BrokenPipe` on standard
  output; the `mcp` transport already treated `ConnectionReset` as a disconnect. A platform that reports a socket peer's early
  close as `ECONNRESET` — a reset sent because unread data was still queued — made
  the same early close exit `0` behind a pipe and `2` behind that socket. On macOS
  every such write was measured failing with `EPIPE` (exit `0` in `0.0.1` as now), so
  the difference there is nil; it is a change only where the reset is reported.
- **Each diagnostic line reaches stderr in one write.** Standard
  error is unbuffered, and `0.0.1`'s `eprintln!` issued one `write` per formatting fragment,
  so a reader that takes each `write` as a record (a datagram socket, a logger
  sharing the stream with other writers) received every diagnostic with an argument
  in pieces. The line is now formatted first and written once. clap's usage errors go
  the same way: off a terminal they are rendered to a string and written once, where
  clap's own printer sent `tokfold --bogus` as ten datagrams and `tokfold compress
  --profile nope` as fourteen, none of them a whole line. The bytes are unchanged,
  because clap renders without colour when the stream is not a terminal; a terminal
  keeps clap's printer, which is the path that has the colour (12 ANSI escapes for
  `--bogus` on a pty under `TERM=xterm-256color`, none under `TERM=dumb`). A usage
  error whose write fails still exits `2` rather than losing the code.
- **A stderr whose reader has gone no longer costs the run its exit code.** Every
  diagnostic went out through `eprintln!`, which *panics* when the write fails, and
  Rust starts a process with `SIGPIPE` ignored — so a caller that keeps stdout and
  drops stderr (a harness collecting only the payload, a `2>` redirect into a reader
  that has exited) turned a correct run into `101` and an "Uncaught panic" line. A
  `compress` that had already passed its input through, a `stats` that had correctly
  reported a rejection, an `expand` refusing a corrupt archive (exit `3`) and an `mcp`
  writing its start-up notice all reported the panic code instead of the one the
  caller branches on.
  Diagnostics now go through one `warn` helper that drops a line it cannot deliver:
  losing the text of a message nobody is reading is the right outcome, losing the exit
  code is not.
- `expand` on an empty archive reports `the archive is empty (0 bytes)` and names
  its source, instead of failing with the generic corrupt-archive message that gave
  a caller nothing to act on.
- `--input` pointing at a directory is rejected with a direct message rather than
  surfacing the platform's raw read error.
- **A release test build emitted two warnings, and no gate could see them.** Two
  imports in `server.rs`'s test module are reached only by its two
  `#[cfg(debug_assertions)]` assertions, so a release build of the tests reported them
  unused. The clippy gate is the only leg run under `-D warnings` and it compiles
  debug, while the release test leg carried the warnings in its log without failing.
  Both imports are now `#[cfg(debug_assertions)]` as well. Test-only; no shipped
  behaviour changes.
- The stream and exit-code contract is appended to the `--help` screen of the root
  command and of `compress`, `expand` and `stats`. It was documented only in the
  README, and the callers who most need it — shell pipelines and agent harnesses
  branching on `$?` — are exactly the ones who never open a README, and who reach
  for `tokfold compress --help` rather than the bare root screen. clap does not
  propagate `after_help` down a subcommand tree, so each screen opts in explicitly
  and a test pins the set. `mcp` is excluded on purpose: it speaks a JSON-RPC stream
  instead of reading a payload, takes no `--input`, and never exits `3`, so most of
  the text would be false there rather than merely repeated. It can still exit `2`:
  a stdio transport failure reaches the same error arm as any other. The `stats`
  screen says the split its own way: the pass runs, and neither the rendering nor
  the archive it builds reaches either stream — only the measurement report goes to
  standard output. The exit-code sentence is one shared literal, so the three
  screens cannot drift apart on the half that is identical.
- **A truncated archive is reported as framing corruption, not as a failed
  integrity check.** When the header's length field disagreed with the number of
  payload bytes that followed it, `decompress` returned `ChecksumMismatch` — a
  verdict on content, reached on a branch that returns *before* any digest is
  computed. A user who lost bytes to a truncated copy, a full disk or a killed
  `scp` was told the data was corrupt rather than the file was short, and the
  `byte_offset` that would have named the disagreeing field was discarded. That
  branch now returns `Corrupt { byte_offset }` aimed at the length field, and the
  CLI prints `archive corrupted at byte 10` instead of `integrity checksum
  mismatch`. A payload of exactly the right length whose digest does not match is
  untouched and still reports `ChecksumMismatch`; a test pins both directions, and
  the `# Errors` contract on `decompress` now states which failure is which. No
  archive changes: this is the classification of a rejection, and both variants
  were already rejections.
- **`compressor.rs` no longer keeps its own copy of the archive header's field
  offsets.** Four `const`s duplicated the arithmetic in `format.rs` so that a
  `Corrupt` error could name the field it stopped at. A copy stays right until the
  frozen layout moves, at which point both sides still compile while every offset
  an archive reports names the wrong field — a silent wrong answer, in the one
  error whose whole value is the number it carries. The offsets are now
  `pub(crate)` in `format.rs` and imported, so the layout has exactly one
  definition. A compile-time assertion also pins `ULEB128_MAX_BYTES` against the
  shift width `read_uleb128` derives from it, which would otherwise overflow a
  `u64` shift if the bound were ever raised.
- **The corrupt-archive message named the wrong half of the file.** `Corrupt`
  rendered as `payload corrupted at byte {byte_offset}`, and every offset it
  carries is counted from the start of the *archive*. Five fixed header fields
  raise it — byte 4 (a missing or zero version), byte 5 (encoder id), byte 6
  (tokenizer id), byte 8 (the flag word) and byte 10 (the ULEB128 original-length
  field) — and exactly one site names a position in the payload region. So the two
  ordinary causes, a bit-flip in the flags and a truncated copy whose length field
  disagrees with the bytes after it, printed `payload corrupted at byte 8` and
  `payload corrupted at byte 10` while the payload of a minimal archive does not
  begin until byte 43. A reader who took the word at face value looked for the
  damage in the wrong region, and one who used the number as an index into the
  payload landed on archive byte 51 — forty-three bytes past the byte that was
  actually wrong. It now reads `archive corrupted
  at byte N`, and the variant's rustdoc states which offsets exist and what they
  mean. Released `0.0.1` prints the old string, which is why the `[0.0.1]` section
  below still quotes it.
- **A `compress` that rejected the input said `no recovery archive written
  (compression did not run)`.** The compressor *did* run: it read the input and
  rejected it, which is the only way that path is reached — and now that a
  rejected pass can remove a stale archive, the denial would sit directly under the
  line reporting the removal. The line now says `(the input was passed through
  unchanged)`, which is what happened unless the reader closed early, and it is printed only after the passthrough
  write has returned — a reader that closed the pipe early counts as a returned
  write, so behind `| head -c1` the line still says the input was passed through.
- **`invalid literal, expected 'null'` was the answer to any unrecognised byte
  where a JSON value belongs.** The parser branched on the first byte, and the
  three literal arms named their literal unconditionally, so `nonsense` was
  reported as a malformed `null` and every input starting with a `t` or an `f` was
  reported as a malformed `true` or `false`. The suggestion is actively
  misleading — nothing in `nonsense` was trying to be `null` — and it points the
  reader at the wrong repair. The parser now names the literal only when the input
  is committed to it: at least two of its bytes matched, or the document ended
  inside it, which is the truncation case where naming the literal is the useful
  thing to say however little of it got written. Anything else reports `expected a
  JSON value`. Measured on the built binary: `nonsense` → `invalid JSON at byte 0:
  expected a JSON value`; `nul` and `nulX` → `invalid literal, expected 'null'`.
- **The MCP `name` parameter reported a wrong type as a missing one.** `tools/call`
  answered `"name": 42` with `\`name\` must be a string`, the same text it produced
  when `name` was absent, so a client that had sent the key could not tell from the
  reply that it had. The message now names what arrived — `it was not supplied` or
  `a number was supplied` — and the same constructor is shared with the tool
  arguments' own string check, which had the identical gap.
- **MCP tool `arguments` that were not an object were read as no arguments.**
  `"arguments": []`, `"arguments": "{}"` or `"arguments": 7` reached the tool as if
  nothing had been sent, so the reply said the tool's first required key was
  missing — a client that had sent the whole document as a JSON-encoded string was
  told `text` was absent. Each tool now refuses such a value first, with `-32602` and
  `` `arguments` must be an object: a string was supplied `` (or `an array`, `a
  number`, `a boolean`). An absent or `null` `arguments` is unchanged.
- **The npm launcher answered an unsupported architecture on musl with only half
  the reason.** The musl check ran first, so a machine that has *neither* a musl
  build nor any build for its architecture was told about the libc and never about
  the architecture, and the remedy it was handed — use a glibc image — would not
  have helped. The architecture branch now runs first and, when the runtime also
  links musl, states both reasons in one message before pointing at the one remedy
  that answers both, `cargo install --git`. The musl-only message is unchanged, and
  the ordering decision it encodes is preserved: on a musl machine that *does* have
  a build for its architecture, the libc is still the whole reason.
- **The MCP transport refused a message of exactly its own limit.** The read side
  charged the newline against `MAX_MESSAGE_BYTES` and the write side did not, so
  the one constant documented as "the same limit in both directions" was two
  limits one byte apart: a 33,554,432-byte message — the size the docs, the README
  and the `-32600` refusal itself name as the largest accepted — was refused with a
  message saying it *exceeds* 33,554,432, and under CRLF framing the ceiling was two
  bytes short, because the `\r` was charged as well. The reader now takes two
  bytes more than the limit, strips the newline and an optional carriage return,
  and judges the message that is left; MAX-1 and MAX are read whole under `\n`,
  `\r\n` and an unterminated final line alike, and MAX+1 is refused under all three
  with the same text. The write side's `render_bounded` was already measured this
  way and is unchanged.
- **`tokfold_decompress` reported an empty archive as corruption.** Zero bytes fail
  the magic check, so the reply said `not a recognized payload (bad magic)` and sent
  a reader hunting for damage inside an argument that has no content. The reply now
  says `the archive is empty (0 bytes)`; the `bad_magic` code is kept, because
  callers branch on it and the empty archive still has no magic.
- **A batch member refused for size reported what was left of the frame as the
  frame limit.** A batch reply is one frame, so each member is measured against what
  the array has not yet spent — and that remainder was the number the member's
  refusal called `the N byte frame limit`. A one-member batch under the default limit
  said `33554430`, and a `tools/list` answer refused after 225 942 `ping` answers said
  `the reply exceeds the 177 byte frame limit` (a `ping` in that place fits and is
  answered), against a limit of 33 554 432; a client reading either
  would have taken it for the server's limit. No test asserted the number, so every
  batch ever answered got it wrong and nothing failed. A member's refusal now says
  `the reply does not fit the N bytes left of the M byte frame limit`, naming both,
  because either alone misleads — the limit on its own would say a 143-byte `ping`
  answer exceeded 32 MiB. A single request's refusal and the id-less refusal that
  replaces a whole batch are unchanged, since for those the number *is* the limit.
  The member's refusal is 24 bytes plus the remainder's digits longer than before,
  so a batch that could only just fit its per-member refusals may now collapse one
  member earlier; the collapse form is the same.
- **A rejected `compress` no longer deletes its own input through a trailing
  separator.** Never released. `--input x --archive x/` exited `0`, printed `removed a stale recovery
  archive at x/` and deleted `x` — the input it had just read — whenever `x` held a
  `TKFD` archive; so did `--archive x/ < x`, and `--archive x/ >> x` deleted the file
  its own output had been appended to. Every same-file guard saw nothing, because
  each asks the path the way its operation reaches it and `x/` reaches no file
  (`ENOTDIR`); only the clearing resolved it with `fs::canonicalize`, which normalises
  the separator away, and deleted `x`. `0.0.1` never deleted any of them. The clearing
  now asks the path as typed first, as the successful write does, and a path that
  names no file that way (`ENOENT` or `ENOTDIR`) is an empty slot: nothing is
  removed. `--archive a.tkfd/` is therefore refused by a successful pass and left
  alone by a rejected one; the `chmod 444` asymmetry is unchanged.
- **Only `/dev/fd` spellings are identified through the descriptor they duplicate.**
  The check matched any path resolving under `/dev`, which on Linux includes
  `/dev/shm` — a regular tmpfs file there would have been identified by a write-only
  open, so a read-only one was "not reachable" as an archive and a rejected pass
  naming it as its own input could delete it. Reasoned from the code and not
  measured: no Linux host was available, and on macOS the change is inert, since a
  regular file under `/dev` is only reached through `/dev/fd`. Never released:
  `0.0.1` had no descriptor check and no same-file guard, and passed a rejected
  input through without inspecting the path.
- **The stale-archive check behind a descriptor spelling read from the caller's
  offset.** Never released. On macOS opening `/dev/fd/N` or `/dev/stdin` duplicates
  the descriptor, and the duplicate shares its offset, so the four-byte magic read
  took four bytes from the caller's own stream — `{ tokfold compress --input bad.txt
  --archive /dev/stdin; cat; } < notes.txt` made `cat` print `o world notes` — and
  read from wherever that stream stood, so an archive behind a descriptor already read
  past its magic was reported `left … untouched: it is not a tokfold archive`. The
  magic is now read with `pread` at offset `0` and the caller's offset is untouched.
  A stale archive behind a descriptor the archive write reaches (`3<> a.tkfd`) is
  refused at exit `2`; one behind a read-only descriptor (`--archive /dev/stdin <
  a.tkfd`) passes the input through at exit `0`, as `0.0.1` did, because a
  successful pass cannot write there either, so no archive of this run's would stand
  at that path. `0.0.1` had no such check and passed through in every case.
- **A read-only descriptor over an archive is not a stale archive slot.** Never
  released. Once the magic was read from offset `0`, `compress --archive /dev/stdin <
  a.tkfd` on a rejected input was refused at exit `2` as a stale archive that could
  not be removed — where `0.0.1` passed it through and where the same-file guard
  already treated a read-only descriptor as unreachable by the write. The check now
  asks whether the archive write could open the file before reading it, and a file
  it cannot reach is an empty slot.
- **A directory behind `/dev/fd/N` is reported, not passed through.** A rejected
  `compress --archive /dev/fd/3 3< some-dir` exited `0` beside a path that plainly
  is not an archive slot; `0.0.1` did the same. It now exits `2` with `inspecting
  the archive path /dev/fd/3: Is a directory`, as a directory named by its own path
  already did.
- **A pipe, socket or device at `--archive` is no longer called "not a tokfold
  archive".** Nothing reads it — a FIFO would block — so nothing checked what it
  holds. A rejected pass now says `left <path> untouched: it is not a regular file`
  for it, on the plain path and behind a descriptor spelling alike, and keeps `it is
  not a tokfold archive` for a regular file whose opening bytes were read.
- **npm launcher: a binary killed by `SIGUSR1` no longer starts the Node debugger.**
  The launcher re-raises a child's fatal signal on itself so the caller sees the same
  death; for `SIGUSR1` Node does not die but opens its inspector, so the launcher
  exited `158` on macOS and, in about one run in three, printed `Debugger listening
  on ws://127.0.0.1:9229/…` on stderr while it did. `0.0.1` has the same code.
  `SIGUSR1` is now never re-raised and exits `128 + N` directly (`158` on macOS, `138`
  on Linux), with nothing on stderr.
- **npm launcher: a `SIGUSR1` sent to the launcher alone opened the Node inspector.**
  With no listener for it, Node does not die of `SIGUSR1`: it prints `Debugger
  listening on ws://127.0.0.1:9229/…` and opens a debugger on that port for as long
  as the process lives, while the binary keeps running. `0.0.1` does this (measured
  on Node 20 and 22). The launcher now listens for `SIGUSR1` and relays it like the
  other four signals, which suppresses the inspector; the binary dies of it as it
  would run directly, and the launcher exits `128 + N` (`158` on macOS).
- **npm launcher: a relayed signal the kernel refused ended the launcher and orphaned
  the binary.** `child.kill` does not throw `EPERM`; Node emits it as an `error`
  event on the child, and the launcher sent every `error` event to its
  failed-to-start exit. So a forwarded `SIGTERM` the binary may not receive exited
  `1` ("tokfold never ran") while the binary kept running on the caller's
  descriptors. `0.0.1` has the same code. Once the child has a pid, its `error`
  events are now ignored and its own exit ends the launcher. The launcher also lost
  a `pending` slot that replayed a signal arriving during the spawn: Node runs signal
  listeners on the event loop, never inside the synchronous `spawn` call, so the
  slot could never be filled.

- **A read-only descriptor at `--archive` on a rejected pass is reported whatever
  its file holds.** Never released. On macOS a rejected `compress --archive
  /dev/fd/3 3< f` treated a file of four bytes or more as an empty slot and said
  nothing, while a shorter one reached the length check first and was reported
  `left /dev/fd/3 untouched: it is not a tokfold archive` although nothing had read
  it. The write-reach question is now asked before the length, and both print
  `tokfold: left <path> untouched: it cannot be opened for writing`, pass the input
  through and exit `0`. `0.0.1` passes the input through in both cases and prints
  `tokfold: passing input through uncompressed: <reason>` followed by `tokfold: no
  recovery archive written (compression did not run)`, saying nothing about the file
  at `--archive`.
- **The rejected-path messages say only what was checked.** Never released. A
  removal printed `removed a stale recovery archive at <path>` and a descriptor
  refusal `a stale recovery archive is open behind <path>`, but the check is the
  four-byte `TKFD` magic, so a `note.md` that opens with that word was called a
  recovery archive. They now read `tokfold: removed <path>: it opens with the
  tokfold archive magic` and `a file opening with the tokfold archive magic is open
  behind <path>, …`. `0.0.1` never removed or refused anything on this path.
- **MCP: a line of non-JSON whitespace is a parse error, not a blank line.** The
  server skipped any line that `str::trim` emptied, and `trim` strips Unicode
  whitespace JSON does not allow — vertical tab, form feed, NBSP, NEL, U+2028,
  U+3000. Such a line got no answer at all, in `0.0.1` too. Only space, tab, CR and
  LF now count as blank; anything else gets `-32700` with no id, as any other
  unparsable line does. The message names the first byte JSON does not accept, so it
  reads `invalid JSON at byte 0: expected a value` only when that byte opens the line
  (a lone vertical tab); a space ahead of it moves the offset (`" \x0b "` reports
  byte 1).
- **npm launcher: a binary killed by `SIGUSR1` kills the launcher by `SIGUSR1`.**
  The exit handler now removes every listener it installed, `SIGUSR1` included,
  before re-raising. Adding a `SIGUSR1` listener replaces Node's inspector handler
  rather than stacking on it, so removing it leaves the kernel default, and the
  re-raise is a true death (`158` in a macOS shell) with nothing on stderr: measured
  20 of 20 runs on Node 22 and 12 of 12 on Node 20. `0.0.1` re-raised with Node's
  handler still installed, so it exited `1` and printed `Debugger listening on
  ws://127.0.0.1:9229/…` in 3 of 12 runs (macOS, Node 22). The never-released
  intermediate exited `128 + N` without re-raising.
- **npm launcher: signal listeners are installed before the binary lookup.** A
  signal that arrived during the lookup or the spawn used to find no listener and
  Node's disposition applied: a `SIGUSR1` opened the inspector, a `SIGTERM` killed
  the launcher before a binary existed. It is now queued and dispatched once
  `spawn` returns, or dropped if the lookup or spawn fails and the launcher exits
  `1`. What remains is Node's own boot and the `require`s: a signal there meets
  Node's handlers or the kernel default, before any binary exists, so no binary can
  be orphaned by it. `--disable-sigusr1` would close the `SIGUSR1` half of that
  window, and the launcher cannot set it: Node v18.20.8 and v20.20.2 refuse the flag
  with exit `9`, on the command line and in `NODE_OPTIONS` alike, while v22.23.x and
  v26.7.0 accept it in both places (measured on macOS). A caller on Node 22 or later
  can set it through `NODE_OPTIONS`.

- **The warning a rejected pass prints after clearing a stale archive through a
  symlink named the link.** Never released. The link is left in place and the file
  it points at is what gets removed, so `removed <link>` described the one path
  that survived. When the typed path is itself a symlink the line now names both:
  `removed <target>, the file the symlink <link> resolves to`. It says "resolves to",
  not "points at": through a chain `l1 -> l2 -> real` the typed link points at `l2`,
  which survives with `l1`, and it is `real` that goes. A symlink further up the path
  needs no such care, since the path as typed then names the removed file.
- **The same-file refusals predicted an outcome the guard had not checked.** Never
  released. The guard runs before tokfold knows whether the input will be accepted,
  so a rejected input without the archive magic is refused as well, and a prediction
  of what a successful pass "would have done" was false too: a read-only input cannot
  be overwritten (`0.0.1` stopped at `Permission denied`), and on macOS a successful
  pass refuses a non-empty file behind a `/dev/fd/N`, `/dev/stdin` or `/dev/stdout`
  spelling, because opening it does not empty it. The messages now state only the
  rule checked — `the archive may not be aimed at its own input`, `the archive may
  not be aimed at the output`, and for the output guard `output may not be written
  into the input it is read from` — the rustdoc and CLI README say the check is a
  rule, not a prediction, and the tests pin each line whole.

### Changed

- **A rejected `compress --archive PATH` can now exit `2`.** `0.0.1` exited `0` on
  the rejected path unless writing the passthrough itself failed (a file-size limit
  with `SIGXFSZ` ignored exits `2` there too): it wrote a line saying no recovery
  archive had been written and passed the input through. Clearing the slot now happens before the
  passthrough is written, so a slot that cannot be inspected — among others a
  directory at that path or behind `/dev/fd/N`, an unreadable parent, a symlink that
  loops, a name too long for the filesystem, a closed descriptor behind `/dev/fd/N`,
  a file whose mode denies reading (`chmod 000`, or a write-only `chmod 200`), and a
  write-only descriptor on a file of four bytes or more (`--archive /dev/fd/3
  3>> f`) — or an archive that cannot be removed — a deletion that fails (an archive
  in a read-only directory, which a successful pass rewrites in place), or a file
  opening with the magic behind a read-write descriptor (`3<> a.tkfd`), which names
  a descriptor and not a file — ends the command with `2`
  and no output at all, the same rule that already governed a failed archive write.
  The line announcing the passthrough is printed only once the slot is cleared and
  the passthrough written, so an exit `2` on this path never claims one. It carries
  the context "the input was rejected (…) and not passed through" when the slot
  could not be cleared, which is decided before anything reaches stdout, and "the
  input was rejected (…) and passing it through failed" when the write itself
  failed — worded so it stays true when part of the input was already emitted.
- **A successful `compress --archive` refuses a FIFO, a socket or a device.**
  `0.0.1` opened whatever was there: `--archive /dev/null` discarded the
  archive and exited `0`, `--archive >(gzip > a.gz)` fed it to a pipe, and a named
  FIFO with no reader hung the pass before it wrote anything. All three now exit `2`
  with `refusing to write the archive to <path>: it is not a regular file` and no
  output, because the one outcome that cannot be told apart from the others is the
  hang. An archive meant for a pipe can be written to a file and piped from there.
- **npm launcher: a signal this runtime cannot number now writes a line before it
  exits `1`.** It is the one `1` that does not mean "tokfold never ran", so a silent
  one sent the operator to debug the wrong half of the system.
- **npm launcher: a binary killed by a signal the launcher cannot re-raise now
  exits `128 + N` instead of `1`.** `0.0.1` shipped the `1`. That was wrong: `1` is
  reserved for "the launcher failed and tokfold never ran", and `128 + N` is the
  encoding every POSIX shell already uses for a signal death, so a `SIGPIPE` kill
  now reports `141`, and a `SIGXFSZ` kill `153` on macOS — exactly what the caller would have seen running the binary
  directly, which is the launcher's whole design goal. The fallback to `1` survives
  for one corner: a signal this runtime's `os.constants.signals` table does not
  name. That corner is documented rather than papered over, because the alternative
  — `128 + undefined`, i.e. `NaN` — makes `process.exit` report **`0`** on Node 18,
  this package's `engines` floor, turning a killed run into a reported success.
  Node 20 and newer throw `ERR_OUT_OF_RANGE` instead, so the guard is load-bearing
  precisely on the oldest runtime the package claims to support.
- `Node::depth` in `tokfold-core`'s `tape` module widened from `u16` to `u32`. No
  encoder, estimator or rendering path reads the field, so no output moved — but
  `tape` is a `pub mod` and `Node` a `pub struct` with public fields, so this is a
  source-breaking change to the public API. It costs nothing today because the crate
  is not on crates.io.
- **`CompressError::NotUtf8` carries the byte offset where the decoding failed**
  and renders as `input is not valid UTF-8 at byte N`. `0.0.1` printed the bare
  sentence, which is the least actionable diagnostic this engine produces: the
  input can be megabytes, the caller usually did not write the bytes, and the one
  fact needed to repair it — where the bad sequence starts — was already computed
  by `str::from_utf8` and thrown away. It is `Utf8Error::valid_up_to`, so the
  prefix below the offset is valid and the byte at it is not; a roundtrip test now
  proves both halves rather than accepting any number. Adding the field is
  source-breaking for a caller matching the variant without `..`, which costs
  nothing today because the crate is not on crates.io.

- **MCP: the `initialize` instructions no longer promise that input which does not
  compress "is returned unchanged".** `0.0.1` said so, and it holds only for input
  the engine declines — not JSON, nested deeper than 512, or too large — which comes
  back unchanged with `compressed: false` and a `reasonCode`. Valid JSON
  with nothing to save comes back with `compressed: true` and a rendering that is
  the input behind a `⟦tkfd:v1:raw⟧` marker line — not unchanged. The sentence now
  names both cases. A client that shows the instructions to a model sees a different
  string than `0.0.1` sent.
- **A standard stream that cannot be used now fails the command (Unix).** The
  standard library treats `EBADF` on stdout as a successful write and on stdin as
  end of input. So with fd 1 open read-only (`tokfold compress < in.json 1< in.json`,
  or a supervisor that hands a read-only descriptor) `0.0.1` exited `0` and the
  output was lost — on every subcommand, on `--help` and `--version`, and on `mcp`,
  which dropped every reply. With fd 0 open write-only (`tokfold compress 0>>file`)
  it read an empty document: `compress` passed nothing through and exited `0`,
  `expand` reported a bad magic, `stats` called the input invalid JSON, and `mcp`
  ended its session at once with `0`. Both streams are now used through duplicates of
  their descriptors, and a failed read or write exits `2`. Duplicating takes a
  descriptor each, so a process at its open-file limit now fails where `0.0.1` did
  not: under `ulimit -n 4`, `mcp` exits `2` with "Too many open files" instead of
  serving. `--help` and `--version`
  bound for a terminal still go through clap's own printer, so a terminal opened
  read-only as stdout (`1</dev/tty`) is not covered.
- **MCP: the `tokfold_compress` description and the `initialize` instructions now say
  that a reply larger than the message limit (32 MiB by default) is an error.** The
  description adds that the error carries no text, so a client keeps its own copy
  of a large input; the instructions say it is "an error rather than a truncated
  answer". `0.0.1` sent neither sentence; a client that shows either string to a
  model sees a different one.
- **npm launcher: the message for a missing platform package no longer suggests
  installing it by itself.** `0.0.1` said "Fix it with: npm install <package>@<version>",
  which helps only for a project-local install run from that project's root: the
  launcher resolves the package beside its own install, not from the current
  directory, so after a global install, or from any other directory, the advice
  installed a package the launcher never looks at. The message now says to reinstall the launcher
  with optional dependencies enabled, and why installing the package alone is not
  the fix.

- **`compress -i f --archive f` over a read-only `f` now exits `2`.** The same-file
  guard refuses it before the input is judged. `0.0.1` stopped at `Permission
  denied` (exit `2`) for an accepted input, and passed a rejected one that opens with
  the archive magic through with exit `0`.
- **`tokfold expand < bad.tkfd >> bad.tkfd` over a corrupt archive now exits `2`**
  with the output-guard refusal. `0.0.1` exited `3` with the decoding error and
  wrote nothing.
- `release.yml` reported "## Published `<version>`" on any non-dry run, including
  one in which all six packages took the "already published by us" skip branch and
  nothing was uploaded. The step now tallies the uploads and says so, naming what a
  fresh `npm i` would actually install when the count is zero.
- `ci.yml`'s clippy gate was the only gate without `--workspace`. Harmless while the
  root manifest is virtual, and silently narrowing to one crate the day anyone adds
  `default-members`.
- `release.yml`'s platform-table check destructured `key.split("-")` into exactly
  two names, so a third segment would have vanished and the manifest would have been
  checked against a key nobody validated. A key that is not `platform-arch` is now
  rejected outright.
- **`release.yml`'s dry run reported "is not on the registry" for a registry that
  never answered.** The live path prints a warning and publishes through a
  three-strike lookup failure on purpose; the dry run collapsed that case into a
  positive claim about the registry, in the one output a person reads to decide
  whether the real run is safe to start. It now says the existence of that version
  is unknown, and the found case says "was not found on the registry" rather than
  asserting absence.
- **The version-agreement check counted its own sites in a comment.** The comment
  said seven while the sentence in it enumerated twelve and the code performed
  twelve. The summary line now reports the number of checks it actually ran, so
  there is no hand-maintained tally to drift; `npm/tests/resolve.test.js` carried
  the same stale count and is corrected.

### Added

- `Stats::gate_rejected_candidate` — whether the do-no-harm gate refused a candidate
  rendering during the pass. `Stats` is `#[non_exhaustive]`, so the field is
  additively compatible.
- The release workflow's platform-table check now reads each package manifest's own
  `name` instead of reconstructing `tokfold-${dir}`. Reconstruction was a latent
  trap: `npm/platforms/windows-x64` publishes as `tokfold-windows-x64` only by
  coincidence, and any future renamed package would have been checked against a name
  that does not exist. The workflow also verifies that each package declares the `os`
  and `cpu` its table key promises, and it collects every problem before failing
  instead of stopping at the first.

### Removed

- The five `npm/platforms/*/.rust-target` files. They held Rust target triples that
  nothing read — not at `HEAD`, and not through any manifest's `files` list, so they
  never shipped — while duplicating `release.yml`'s build matrix as an unchecked
  second source of truth.

### Documentation

Corrections, each to a statement that had become false, or to an omission, where
the sentence that should have been there was never written at all.
Sources are listed because this repository's most reliable defect source is
documentation a previous fix falsified.

- **A batch's answer is not monotonic in the frame limit, and the `handle_batch`
  rustdoc said it was.** It said each step down "loses exactly one more thing" and
  that one outsized call "fails alone". Neither holds: each member's refusal takes
  the widest form that fits the room left at that member and reserves nothing for
  the members after it. With the running member budget added since `0.0.1`,
  `[ping, tools/list with a 300-byte id, ping]` is answered (both pings, id-less
  refusal) at a 576-byte limit and collapses to one id-less error at every limit
  from 577 to 700, because from 577 the addressed refusal (431 bytes) fits its own
  remainder and starves the last ping; at 701 the last ping's own id-less refusal
  fits again, and from 721 both pings are answered. The same step is taken a second
  time when the middle member's real answer first fits: the batch collapses again
  from 2 968 to 3 092, answers the middle member and the first ping from 3 093, and
  all three from 3 112. The rustdoc now derives the first window (143 + 431 + 3,
  143 + 431 + 123 + 4 and 143 + 431 + 143 + 4) and names the second. `0.0.1` has the
  same shape at other limits — measured through `handle_line` on the source of
  `87fb332`, the published `gitHead`: both pings answered from 386 to 549, one
  collapsed error from 550 to 645, one ping from 646, both from 694, collapsed again
  from 2 858 to 2 953, two results from 2 954 and three from 3 002 — so at 576 it
  collapses and at 700 it answers, the reverse of the numbers above. The same
  shape is reachable at the default 32 MiB limit, in `0.0.1` too, and it has both
  windows there as well; the steps around the later window are taken exactly where
  the reply would exceed the limit by one byte, while the early collapse starts one
  id byte after a 33 554 430-byte reply and was not derived. Through the published `tokfold-darwin-arm64` 0.0.1 binary
  (macOS arm64, ids of `x`), a middle id 33 551 779 to 33 551 874 bytes wide gets a
  single 101-byte error (the real catalogue answer starving the last ping) and one of
  33 551 875 an array again; one of 33 554 082 to 33 554 177 gets the single error
  once more (the addressed refusal doing so) and one of 33 554 178 an array holding
  both ping answers, up to 33 554 300. From 33 554 301 the batch line (the id's width
  plus 131 bytes) is exactly 33 554 432 bytes and `0.0.1` refuses it at the read
  side, which charged the newline, with `-32600` "message exceeds the 33554432 byte
  limit" — the Fixed entry "The MCP transport refused a message of exactly its own
  limit", not the batch code. The running member budget added since moves the two
  windows to 33 551 636 to 33 551 764 and 33 554 017 to 33 554 145. Behaviour
  unchanged; reserving room or
  reordering is an open contract question.

- **"Reading an archive back to the end takes a full 43-byte header" is true only
  for originals under 128 bytes.** The header is `TKFD` + version + encoder id +
  tokenizer id + flags + a **ULEB128** original length + a 32-byte SHA-256, so it
  gains a byte for every 7 bits the original grows: 43 bytes up to 127, 44 up to
  16 383, and 52 in the limit. Measured through the shipped binary by subtracting
  the input size from the archive size — 43 at an original of 16 bytes, 44 at 413,
  45 at 30 013, 46 at 3 000 013. The number appears in six places and four of them
  already hedged it (`~43-byte`, `about 43 bytes`); the two that did not are
  `crates/tokfold-cli/README.md` and the doc comment on
  `only_the_archive_magic_marks_a_file_the_reject_path_may_delete` in
  `crates/tokfold-cli/tests/cli.rs`, and both were using it as the *upper* half of
  an argument — four bytes to start reading, a full header to finish — where an
  understated size makes the gap look smaller than it is. The README site was also
  contradicting the byte-layout table earlier in the same file, which has always
  said the length field is `1–10` bytes. Both now say "at least 43" and give the rule
  that produces the rest. The four hedged sites are unchanged.

- **The `test` matrix in `ci.yml` was introduced by a banner reading "gates
  nothing" — the exact claim the same file's header exists to correct.** The header
  says so in as many words: *"That is not the same as 'no gate hangs off it', which
  this comment used to claim"*, and then names the three `test` legs that are on the
  branch-protection list and the `continue-on-error: ${{ !matrix.required }}`
  expression that decides whether a failing leg still reports success. The header was
  rewritten; the section banner over the job itself, far below it, saying the same false
  thing in four words, was not. A reader who skims to the job — the likelier path — met
  the refuted version first. The banner now states the split (three of five legs gate,
  two report for signal) and points at the header for the reasoning. Comment-only; no
  job, condition or matrix value changed, and `actionlint` is clean.
- **"All six names ... published on 2026-08-29" is wrong for four of the six.** The
  claim appears in `release.yml`, justifying which exposure the publisher check is
  and is not about, and in `npm/README.md` in the paragraph explaining the same
  check. `0.0.1` did not go out in one batch: the four Linux and macOS platform
  packages were published at `2026-08-27T19:03Z`, then `win32-x64` was refused three
  times, and `tokfold-windows-x64` landed at `2026-08-29T22:54:14Z` with the
  launcher 3.3 seconds behind it. The registry's own per-package timestamps say so;
  these two sites had flattened them to the launcher's date. Both now name both days, and `release.yml` names the
  command that re-derives them — `curl -s https://registry.npmjs.org/<name>`, whose
  `time` object is the only authority — because a date in a comment rots and the
  registry does not. The `npm/README.md` site was the worse of the two: earlier
  in the same paragraph it already says the first release "published four
  platform packages and was then refused on the fifth", so the file contradicted
  itself within one screen. *Not* changed: user-facing release sentences elsewhere
  still say 2026-08-29, which is the launcher's date and the settled convention,
  because that is what `npm i -g tokfold` resolves.

- **The `--archive` deletion was justified by an invariant that does not hold, and
  the justification had been repeated into three files.** The claim was that clearing
  a magic-bearing file on the rejected path "is never wider than what a successful
  pass does to the same path", because a successful pass would truncate it anyway. It
  is false, and it is false in two independent ways, because the two paths do not use
  the same system calls. **Permissions:** success `open`s the file for writing (`fs::write` in
  0.0.1), which needs write permission on *the file*; the clearing is `fs::remove_file`,
  which needs write permission on *the directory* and ignores the file's own mode — so
  a `chmod 444` archive in a writable directory survives every successful pass at exit
  `2` and is deleted by the first rejected one at exit `0`. **Path spelling:** success
  `stat`s the path as typed while the clearing resolves it with `fs::canonicalize`
  first, and a `realpath` that normalises a trailing separator away then hands the
  clearing a file `stat` refused — `--archive a.tkfd/` is an `ENOTDIR` on the winning
  path and a deletion on the rejected one. Both were measured against the built
  binary. The behaviour is unchanged and still the right default — an archive that
  outlives its run is a *silent* wrong answer, two clean exits and a different
  document, where this is a loud one that names the file on stderr — but it is now
  documented as a trade rather than as the absence of one, and both asymmetries are
  pinned by tests (below). Corrected in `crates/tokfold-cli/src/main.rs`,
  `crates/tokfold-cli/README.md` and this file.
- **Nothing said the archive write is not atomic.** `write_file` opens the path
  with truncation (`fs::write` in 0.0.1), which truncates and then writes, and the binary calls `fsync` nowhere. An overwrite
  that fails partway — a full volume, an `RLIMIT_FSIZE` — therefore leaves a truncated
  or empty file where the *previous* run's good archive was, and that archive is gone.
  Measured: under `ulimit -f 8` a 37,935-byte archive became a 4,096-byte stub. This
  costs availability rather than correctness, because `expand` fails closed on the
  remains (exit `3`, "archive corrupted at byte 10", never a best-effort document),
  but a slot you overwrite is not a slot you can treat as a backup, and the README now
  says so. The same measurement is why the "empty archive" diagnostic names the size
  instead of guessing the cause: a truncated overwrite and a genuinely empty write are
  indistinguishable after the fact.
- **"Exit codes are normative" did not admit that a signal death is not an exit
  code.** The same `RLIMIT_FSIZE` run above died on `SIGXFSZ` with an empty standard
  error — `128 + 25` through a shell, outside the documented `0`/`2`/`3` set, and with
  no diagnostic, because the signal arrives inside `write` rather than as an `Err` the
  binary can report. The sentence now scopes itself to the codes the process chooses,
  and the signal death to the default disposition: under a parent that ignores
  `SIGXFSZ` (`trap '' XFSZ`, Python's `os.system`) the same write fails with `File
  too large` and exit `2`, and a standard output redirected to a file dies the same
  way as the archive.
- **E2's module docs said a tab-separated row layout "spends most of the key-dedup
  saving straight back at the tokenizer", and nothing measured it.** The structural
  half of that sentence holds: a real BPE tokenizer merges `,"` and `":"` into single
  tokens but never merges a `\t"` boundary, so a tab layout can never be the cheaper
  of the two spellings. The magnitude half was wrong for the ordinary table. Over the
  288 shapes now surveyed — six value families, two to twenty fields, four to a
  hundred rows, both `cl100k` and `o200k` — the median give-back is **20%** of the
  key-dedup saving, and on **96** of them it is nothing at all. It exceeds half on
  **90**, and those are wide rows of long string values; the peak is **86%**, at
  twenty date-like fields. A tab layout is therefore a steady loss but usually a
  modest one, and the honest case for reusing JSON's punctuation is that it is free,
  not that a tab would be ruinous.

  The rewritten paragraph quotes only figures a test computes, so a tokenizer upgrade
  that moves them fails a test instead of quietly rotting a sentence. That test —
  `estimator::tiktoken_tests::the_tab_layout_is_priced_the_way_the_e2_docs_say_it_is`
  — also pins the structural half directly (`,"` = 1 token, `":"` = 1, `\t"` = 2) and
  asserts that a tab layout comes out cheaper on **zero** of the 288, which is the
  claim the format decision actually rests on. It follows the precedent of
  `minification_is_priced_the_way_the_e1_docs_say_it_is`, added earlier for the
  same reason.

  The `BLOCK` constant's own docstring carried a third wording of the same claim —
  JSON punctuation "tokenizes far cheaper" — and now states the measured relation
  instead. A comment line in the same header, left ragged by an earlier edit (the
  word `forbid` alone on a ten-character line in a file wrapping at 86), is rejoined.

- **The heuristic estimator's docs named one direction for an error that has two.**
  Every sentence about `HeuristicEstimator`'s accuracy described over-counting: the
  module paragraph, the corpus range it quotes (+30% to +127%, which contains no
  negative value), and a test comment closing with the unconditional "this scanner is
  an upper bound". That last one is a general property claim, and it is false. The
  scanner charges `ceil(len / 3.7)` for an alphanumeric run; a BPE tokenizer has no
  merge to offer a random hex digest, so on high-entropy values it spends far more
  than that. Over the survey now added — six content families, four sizes, both
  tokenizers — exactly **half** of the 48 cases *under*-count, by **18% to 49%**:
  SHA-256 digests at −43%, base64 blobs at −37% to −49% and UUIDs at −18% to −21%,
  against prose at +63% to +76%, word-shaped key/value objects at +87% to +89% and
  file paths at +122% to +124%. The under-counting families are staples of agent tool
  output, and the error runs in the direction a reader budgeting a context window
  would least like to be wrong.

  `estimate` is therefore neither an upper nor a lower bound on real tokens, and the
  docs now say that with figures a test computes —
  `estimator::tiktoken_tests::the_heuristic_bias_is_two_sided_the_way_the_docs_say_it_is`
  pins the survey size, the half that under-counts, the 18%–49% band and every
  per-family band. The corpus paragraph is left as it stands, because that corpus is
  not in this repository and its numbers cannot be re-measured here; it is now read in
  the company of a sentence that denies it is a ceiling. No behaviour changes, and the
  narrower claim the docs already made is untouched: selection compares two estimates
  from this same model, so most of the bias cancels.

- **`compress --help` credited the archive deletion to the wrong outcome.** It said
  "a pass that falls back to passthrough deletes what is there" — but the passthrough
  *encoder* winning is a success, and a successful pass **writes** an archive;
  `tokfold stats` prints `encoder: passthrough (id 0)` for exactly that case. Only an
  input the engine *rejects* deletes. A reader whose input took the passthrough
  encoder would have expected an empty slot and found a valid archive from that same
  run (43 header bytes plus the input: 50 for a 7-byte input). Both READMEs and the crate rustdoc already said "rejects"; only the
  help screen, which is what `--help` actually shows, did not. It now also names the
  exit-2 outcome the READMEs document.

  Nothing caught this because the tests searched the screen for a word list —
  "deletes", "overwrites", "Exit codes" — and every needle was present in the wrong
  sentence. A new test drives the case each claim describes and asserts the help
  against the observed result instead: the passthrough encoder winning really does
  leave a `TKFD` archive, a rejected input really does clear it and exit `0`, and
  `stats` really is the subcommand that exits `2` on one.

  Fixing the help exposed the gap that had allowed the mistake: **no shipping
  document ever said that a pass the passthrough encoder wins counts as a successful
  pass.** The CLI README's `--archive` section split the outcomes into "a successful
  pass overwrites" and "a pass the engine rejects deletes" and left the reader to
  work out for themselves which of those a passthrough selection is — and the help
  screen is the evidence that a reader does not. The section now says so outright,
  including how to tell the two apart from outside: `stats` prints
  `encoder: passthrough (id 0)` for the first and exits `2` on the second.
- **The exit-code line on the `compress` help screen contradicted the binary.** It
  read "2 bad input or usage", and it is appended to `compress`, where a rejected
  input exits `0` and forwards the bytes unchanged. `2` covers usage, I/O, and an
  input rejected by `stats` — as the crate rustdoc's exit-code table and the CLI
  README both already said. The audience the constant's own docstring names — "shell
  pipelines and agent harnesses that branch on `$?`" — would have written a `2` arm
  that never fires. The same line described `3` as "a corrupt or empty archive",
  a third wording of a fact stated two other ways in the same crate; all three now
  read "corrupt, empty, or otherwise unrecoverable".
- The CLI README described `tokfold stats` with no mention that the `after` figures
  are *set* equal to the `before` figures on the passthrough path rather than
  measured. The root README, the core README and the `Stats` rustdoc all carry that
  caveat; the README for the tool that prints `byte ratio: 1.0000` was the one place
  it was missing. Measured: a 7-byte input reports `bytes after: 7` while the
  rendering it describes is 25 bytes.
- The CLI README quoted the `mcp` stderr warning as saying the server "sees whatever
  passes through it" — a clause the notice does not contain — and dropped the clause
  it does: "not covered by the reversibility guarantees". That dropped clause is the
  only place stating the reversibility guarantee does not extend to the MCP path.

- `MAX_SAVING_BPS` and `ConfigBuilder::min_saving_bps` both said a margin at or above
  the 10 000 bps ceiling "makes the pass return passthrough for all inputs". That is
  a property of the four estimators this crate ships — each rates the sentinel line
  above zero — not of the gate's arithmetic. `TokenEstimator` is the crate's public
  extension point, and a model rating a framed rendering at zero clears a 100% bar
  with equality, so an encoder is selected. The same working tree already argued the
  opposite quantifier in `clears_margin`'s own rustdoc ("the gate must hold for *any*
  `TokenEstimator`"). Both sites now name the estimator as the reason, and the case is
  pinned by a test rather than left as an unverified claim.
- The `0.0.1` npm release falsified a family of "nothing has been published"
  sentences (`SECURITY.md` twice, both crate READMEs) and five "hardening gates any
  public launch" promises about `tokfold-mcp` — the crate shipped unhardened and
  labelled, so hardening is a milestone still owed, not a gate the release passed.
  Fixed at every site, including `crates/tokfold-mcp/tests/experimental_notice.rs`.
- `SECURITY.md` directed reporters to a "Report a vulnerability" button that is not
  there — Private Vulnerability Reporting is disabled on this repository — while in
  the same breath forbidding a public issue, leaving a reporter with no channel at
  all. A safe fallback is now documented: a public issue containing *only* the
  sentence "security report, requesting a private channel".
- The claim that esbuild and turbo publish unscoped `-windows-` names was false at
  both shipping sites. Both are scoped (`@esbuild/win32-x64`, `@turbo/windows-64`);
  `git-cliff` is the only real unscoped precedent, and scoping is the only measure
  observed to sidestep npm's spam classifier outright.
- The minimum-saving margin was described as falling back to a declared
  `over_claim_bps` "of the two estimators"; there are four, and all four declare `0`.
- `Stats::gate_rejected_candidate`'s own rustdoc claimed that a document with no
  whitespace to strip and no array to tabularize reaches passthrough reporting
  `false`. Experiment refuted it, and a test now pins the corrected claim through the
  public API.
- `encoder/mod.rs` stated that both `u128` products stay below `2^78`. Only the left
  one does: `est_before * bps` with a `u32` `bps` reaches under `2^96`. The
  arithmetic was already correct; only the bound was wrong.
- `ci.yml`'s "every push" is now "every pull request and every push to `main`".
- `npm/tokfold/README.md` asserted an exhaustive rule — "Three situations produce
  it, and in all three no tokfold process ever ran" — and then broke it later in
  the same section, where it describes the fourth case in which `1` does *not*
  mean that. Both the sentence and the exit-code table row now scope the claim to
  ordinary use.
- The same file's exit-code table still described `3` as "a corrupt or
  unrecoverable archive"; the binary's own help text had already been corrected to
  include the empty-archive case. The launcher's own exit-code comment carried the
  same stale wording immediately above the block this release rewrote, and was
  corrected with it.
- `npm/tokfold/README.md` documented nothing about `--archive` even though its own
  quick-start runs `--archive` and that flag can now delete a file and can now be
  refused outright. It is the only documentation inside the npm tarball, so the
  section was added there rather than only in the crate README.
- `npm/README.md` still said renaming "was free to do only because the launcher had
  not been published" and that the names "stop being free the moment `tokfold`
  itself goes out" — future tense about something that happened on 2026-08-29. The
  same file already said the opposite in an earlier paragraph.
- `npm/tests/resolve.test.js` justified itself with a description of `release.yml`
  that this release's own workflow change made false, and it reconstructed package
  names as `tokfold-${dir}` — the very trap the workflow change removed. It now
  reads each manifest's own `name`, and the comment states the real reason the
  tests exist: `release.yml` runs only on a release, these run on every pull
  request and every push to `main`.
- The CLI README said a rejected `compress` "still exits `0`" — the wording this
  release's own change made conditional. Every sentence on that path now names the
  steps that can end the command with `2`.
- `mcp` was said to "never exit `2` or `3`" — the stated reason for leaving it out
  of the exit-code help text. A stdio transport failure reaches the same error arm
  as any other and exits `2`. The reason now rests on `3` alone, which is true.
- `npm/tokfold/README.md` said `tokfold compress --archive log.json < log.json`
  "will overwrite `log.json`". On a rejected pass it *deletes* it. Both READMEs now
  say the source document is destroyed either way.
- The npm exit-code table's `2` row was missing the "on `stats`" qualifier that the
  crate README and the rustdoc both carry; on `compress` a rejected input exits `0`.
  It is the only exit-code documentation inside the npm tarball.
- `min_saving_bps`'s rustdoc claimed it distinguishes "no encoder produced a win"
  from "a win was refused inside the error budget". It is computed before selection
  runs and is identical in both cases; `gate_rejected_candidate` — added this
  release — is the field that answers that, and its own doc says so.
- `ci.yml`'s branch-protection to-do listed two required checks no job can report
  (`launcher (node 18)`, `launcher (node 22)`; the legs are `linux-node18`,
  `linux-node22`, `macos-node22`, `windows-node22`). A required check that never
  reports stays pending forever, so pasting the list verbatim would have made every
  pull request unmergeable on two phantom names while all four real launcher legs —
  the only gate covering the shipped JavaScript — went unrequired. The same list
  said the macOS and Windows legs are `continue-on-error`; that is true of the
  `test` matrix only. The `launcher` job blocks on all four legs.
- **`e1_minify`'s "why this is a candidate, not a guarantee" section was wrong about
  both constructs it prices, in opposite directions.** That section is the reasoning
  a reader uses to judge whether the encoder is worth enabling. It priced a post-key
  `": "` at one token, arguing that stripping the space "can leave the token count
  flat while the byte count drops"; measured against the crate's own cl100k and o200k
  tokenizers, `": "` is 2 tokens where `":"` is 1, so stripping it pays exactly one
  token per key — the opposite of what was written, in the direction that understates
  the encoder. The first correction then over-claimed the other half, asserting that a
  run of indentation spaces is a single token "however long the run is". It is not:
  indentation is a finite, **non-monotonic** lookup table. Of the run lengths
  `1..=400`, cl100k spends one token on 86 of them and o200k on 84 of them; the longest
  single-token run is 128 for both, while the first length that costs more is 82 under
  cl100k and 80 under o200k — so 128 spaces cost 1 token while 84 cost 2. A 512-space
  run, which `Config`'s default `max_depth` of 512 makes reachable, is 4 tokens and
  2048 spaces is 16. Both sections now say that bytes and tokens are priced by a table
  rather than a rule, and that a byte objective therefore misprices in *both* directions.
- **The same false `": "` claim stood in a second place.** `encoder/mod.rs`'s
  module docs carried its own copy, justifying the byte-blind selection rule with the
  identical wrong premise; fixing `e1_minify` alone would have left it standing. Found
  only by reading the whole module rather than the reported line.
- The `tiktoken`-gated test added with the first fix would not have caught the second.
  It sampled 2, 4, 8, 16 and 32 spaces to defend a universally quantified sentence —
  every one of those widths really is one token, so it passed while the sentence it
  existed to defend was false. A test that never crosses the edge of a lookup table
  cannot see the edge. It now crosses it deliberately and surveys the whole `1..=400`
  range, pinning the per-tokenizer counts, so a tokenizer upgrade that moves the
  boundary is a failure rather than silent doc rot.
- **`tools.rs` named a test as the reason not to copy the archive into `content`, and
  that test would stay green.** The module said doing so "lifts the compress path's
  reply multiplier to roughly 4.7x of the request and trips the 400% ratchet in
  `tests/session.rs`". Measured on that ratchet's own fixture, the compress reply is
  205% of the request and a third echo of the archive takes it to 316% — under the
  400% bound, which was set against a structural *ceiling* of about 3.3x rather than
  against the fixture. The reason not to make the change is its cost and that it is a
  wire-shape decision for the owner; it is not a guard rail already in place, and the
  docs no longer claim one.
- `stdio.rs` answered an oversize line with `INVALID_REQUEST` (`-32600`), whose own
  definition in `protocol.rs` is "valid JSON but not a valid JSON-RPC request object"
  — but a line over the frame limit is never parsed. The parser's node budget, which
  abandons a message the same way, surfaces as `PARSE_ERROR` instead. Which code to
  borrow is a wire-contract question and is left to the owner; the code is unchanged
  and the deviation is now stated at the site rather than left to be discovered.
- `protocol.rs` said "unknown revision strings are treated as legacy". They are not:
  `is_modern_version` is a lexicographic comparison against `FIRST_MODERN_VERSION`, so
  a future-dated revision is treated as *modern* — as the module's own
  `a_future_dated_revision_is_treated_as_modern` test asserts. No behaviour depends on
  it, because both call sites gate on `is_supported_version` first.
- `server.rs`'s `with_envelope` claimed "every key a result can hold ... was
  enumerated" and then listed only the tool results. The function wraps every result,
  so `supportedVersions`, `capabilities`, `serverInfo`, `instructions`, `ttlMs`,
  `cacheScope`, `protocolVersion` and `tools` were all missing from a list whose whole
  value was being exhaustive. None of them collides, so the claim stayed true by luck.
- `json.rs` documented "an integral float keeps a `.0` suffix" without its bound; the
  code drops the suffix at a magnitude of `1e16`, a threshold that existed only in a
  test's name and comment.
- The rationale for `notice_carries_no_stray_whitespace` described a hazard Rust makes
  unreachable: a rewrap that keeps the `\` line continuation cannot introduce a run of
  spaces, because the escape swallows the newline *and* all leading whitespace after
  it. The test's other justifications are real and it still guards them.
- `tests/contract.rs` and `tests/properties.rs` both gave "a `params` that is neither
  object nor array" as a ground for refusing an envelope. `decode_request` refuses
  anything that is not an object — an array included — and both files' own cases feed
  `"params":[]` and expect a refusal, so each contradicted itself. The production docs
  in `jsonrpc.rs` were already correct.

- **`fidelity.rs` described an API this crate does not have.** Its module docs said the
  report is emitted "per segment", so that a lossy segment could sit beside lossless
  ones inside one document — but `Stats` carries exactly one `fidelity` for the whole
  artifact, and nothing in the compression path has segments at all. The layout that
  sentence was reaching for is reserved in the *container format*, as the
  `truncation_tolerated` and `segment_lossy` flag bits, neither of which anything in
  this version sets. The docs now say where the granularity actually is, and say
  outright that the day a lossy codec ships this report has to grow a segment with it.
- **The release date was wrong by a day in four places.** `0.0.1` went out on
  **2026-08-29** UTC, which is what the registry reports and what the `[0.0.1]`
  heading below already said; one line in this section, a comment in `release.yml`
  and two sentences in `npm/README.md` said 2026-08-30. The preamble above now states
  that every date here is UTC, because the timezone is the only thing that makes a
  release timestamp checkable against anything.
- **Two documents told the reader to run the npm suite in a way that fails on
  Node 22.** `npm/README.md` and `ci.yml` both gave `node --test npm/tests/`. From
  the repository root, on Node v22.23.0 and v22.23.2, that hands the directory to
  the module loader and dies with `Cannot find module` before a single test runs;
  on v18.20.8 and v20.20.2 the same command, with or without the trailing slash,
  runs all 76 tests (all measured on macOS; the release where it changed was not
  bisected). So it depends on the Node version, and the node22 legs of the CI matrix
  could not have used it. Both now say `cd npm/tests && node --test`, name the glob
  form as the path-taking alternative, and give the versions measured.
- **Every reversibility statement gave the contract, and none said what this version
  actually does.** `0.0.1`'s recovery archive is a passthrough blob — a `TKFD` header
  followed by the original bytes verbatim — so `tokfold expand` returns the input
  byte-for-byte today, whitespace and escape style included. The contract promises
  only a semantically identical value tree, and a later encoder that stores structure
  instead of bytes would keep that promise while returning different whitespace. Code
  that diffs a reconstruction against its original therefore passes on `0.0.1` and is
  still wrong. All three READMEs now state both halves: the floor, and the stronger
  behaviour this version happens to have that nothing may depend on.
- **`MEASURED_OVER_CLAIM_BPS`'s docs priced E1 with a single number no shape
  produces.** They said minification's saving "asymptotes near 11.2%", which makes the
  600 bps opt-in bar look comfortably inside the plateau. Measured through the public
  API with this estimator, the minify-only `Conservative` profile and no margin, there
  is no one plateau: a flat object saves
  6.4%–7.1%, an array of row objects 13.1%–16.0%, deep nesting 13.1%–21.8%, a 40-key
  flat object 5.35% and a four-level nesting 1.96%. The binding case is the flat
  object, which clears the 6.00 pp bar by 0.4–1.1 pp — and the two shapes below it do
  not clear it at all, so at that bar they fall back to passthrough while every other
  shape keeps E1 (under the default `Balanced` profile the array of row objects goes
  to E2 instead). The docs now carry that table, and
  `the_600_bps_bar_sits_just_above_the_flat_object_plateau` computes every figure in
  it.
- **Five places priced the MCP reply multiplier without naming its denominator, and
  three of them asserted a reply is always larger than the call it answers.** That
  absolute is false, and the counterexample is ordinary: a client that escapes
  non-ASCII as `\uXXXX` — Python's `json.dumps` default — spends six bytes per
  character on the way in and is answered in raw UTF-8, so a Cyrillic payload driven
  through the shipped binary answered a 294 KB call with a 108 KB reply, 0.37x. The
  multiple is a ceiling, not a law. Against the whole *call* it is 2.00x on the
  passthrough path and 3.33x when an archive barely shrinks; against the *payload* there
  is no ceiling of that kind, because only the reply's two copies are escaped. Corrected
  in `crates/tokfold-mcp/README.md`, `src/lib.rs`,
  `src/jsonrpc.rs`, `tests/session.rs` and `tests/contract.rs`; the ratchet's fixtures
  are ASCII, which its comment now says, because that is why its direction assertion
  holds at all.
- **The reply-multiplier paragraph written earlier to fix this was itself
  wrong in three of its four numbers, and one of them was unreachable.** It gave 3.6x as
  the ceiling against the payload, 3.20x as the ceiling against the call, a 9.4 MB
  payload as the threshold, and 0.37x as the Cyrillic counterexample. Every figure has
  now been re-derived from the real binary, and the fixture that produces each is named
  in the source so it can be re-run. Against the *call*, 3.20x is not a
  ceiling and 3.33x is: an escape-free `{"k":"z"×n}` measures **3.3339x** at every size,
  and escaping can only pull the ratio down because it inflates the call and both copies
  in the reply together. Against the *payload* there is no ceiling at 3.6x or anywhere
  near it — ordinary JSON measures **3.68x** and a quote-dense fixture **4.16x**,
  because there only the reply is escaped. The threshold is exact rather than
  approximate: the largest call whose answer still fits is **10,066,259 bytes**,
  answered with 33,554,431 against the 33,554,432 cap, and one further byte of payload
  is refused with `-32602`. And 0.37x could not have been measured on any path this
  server has: a passthrough reply carries the payload exactly twice against a call that
  carries it three times escaped, a floor of 2/3, and the compress path adds a base64
  archive that lifts the floor to 4/9. The real figure is **0.72x** — a 152 KB Cyrillic
  payload, a 424 KB call, a 304 KB reply. The likeliest origin is a measurement taken
  before the reply echoed the payload twice, kept through the change that made it false.
- **`notifications/initialized` is 25 bytes, not 26.** `MAX_ECHO_BYTES` is 120 and the
  point of the sentence — that nothing real comes near it — is untouched, but a byte
  count that does not survive `wc -c` is not evidence of anything.
- **The `#[allow(clippy::indexing_slicing)]` count in `tape.rs` was wrong by seven.** It
  said four test modules take it; eleven files did when this was corrected, and more
  have been added since, which is why the count is gone rather than corrected. It was written when `tokfold-core` was the whole workspace
  and went stale the day `tokfold-mcp` arrived. The count is gone rather than corrected
  — it carries no weight the sentence needs and rots on every crate added. (Worth noting
  how it hid: `grep -rn indexing_slicing | grep allow` returns *nothing*, because the
  lint name sits on its own line inside a multi-line `#![allow(...)]`. Silence from grep
  is not absence.)
- **`tape.rs` said `Node::depth` "was a `u16` through v0.0.1" while the crate it is in
  is still `0.0.1`.** The widening is unreleased; the `0.0.1` on npm still has the `u16`
  and will until a release goes out. The doc now says so instead of dating the change to
  a version that has not happened.
- **`stdio::serve` documented that a write failure is not an error. Only a *disconnect*
  is.** `write_line` swallows exactly `is_disconnect` and returns `Err` for everything
  else — a full disk, an I/O fault on a redirect target — and `serve` propagates it. The
  distinction matters to anyone deciding whether an `Ok` from this loop means the
  replies were delivered. `serve_stdio`'s `# Errors` section named only the read side
  and now defers to `serve` for both.
- **`compress` claimed in four places — twice in its rustdoc, once in the `--archive`
  help and once in the crate README — that clearing the stale archive is the one step
  on this path that can still fail the command.** `write_stdout` can too: it forgives
  a reader that has gone (`BrokenPipe`, `ConnectionReset`) and returns an error for
  every other I/O failure. What a `CompressError`
  cannot fail is the *payload*, not the command — the distinction both sentences were
  reaching for and neither made.
- `tokfold stats` was pinned only by its labels. The CLI suite asserted that the
  report printed `encoder:` and `tokenizer:` and never what followed them, so every
  number on the screen could have changed without a failure. A test now recomputes the
  byte counts, the ratio and the selected encoder from the run itself and checks the
  report against them.
- **The stale-archive fix was described in four places as removing only files it can
  actually expand, which overstates what four bytes prove.** The entry above, the
  CLI README, `ArchiveSlot`'s docs and a test's comment each said the deletion is
  keyed to the archive magic "so only such a file is removed" and that `compress
  --archive notes.md` therefore leaves `notes.md` untouched. The magic is a
  *necessary* condition for the hazard, not a sufficient one: a four-byte `TKFD`
  file is enough for the reject path to delete it, while reading one back needs 43
  header bytes and a matching digest. The promise holds only for files that do not
  open with the magic, and all four now say so. Measured while checking: `--archive`
  is destructive on the **success** path for a regular file — it is opened with
  truncation whatever its bytes (a FIFO, a socket, a device, or a path whose
  opening does not empty it is refused at exit `2`) — so the reject path is still never the
  wider of the two, which is the claim that actually matters and the one the docs
  now make.
- **`Corrupt.byte_offset` did not say it can point past the end of the archive.** It
  is a position in the format, produced by fields the header itself declares, and a
  caller who took it for a buffer index and sliced with it could panic on a
  truncated archive. Its documentation now says which of the two it is.
- **The licence links in all three crate READMEs pointed at a symlink, not at a
  licence.** `crates/*/LICENSE-MIT` and `LICENSE-APACHE` are symlinks to the
  workspace-root texts, which is right for the tarball — `cargo package`
  dereferences them into real files — but wrong for every rendered view of the
  README. Measured: `raw.githubusercontent.com/.../crates/tokfold-core/LICENSE-MIT`
  returns the six bytes `../../LICENSE-MIT`, and GitHub's blob view marks the path
  `symlink_file` and renders no licence text at all. crates.io resolves a relative
  README link against `repository` plus the package's `path_in_vcs`, so it would
  land on that same stub. All three now link to the workspace-root files by
  absolute URL and say that both texts also travel inside the tarball.
- **The workspace manifest carried a `description` no crate inherits.** All three
  crates set their own — they describe different things — so `[workspace.package]
  description` shipped nowhere while looking like the place to edit. Removed, with
  the reason recorded where it was.
- **The manifest now records why the crates.io hold is *not* expressed as
  `publish = false`.** It cannot be: measured on cargo 1.97, `cargo package
  --workspace` substitutes a sibling workspace member for a registry dependency
  only while that sibling is publishable, so marking `tokfold-core` unpublishable
  makes `tokfold-mcp` resolve `tokfold-core = "0.0.1"` against the real index and
  fail — and that command is a CI gate step. The list form fails identically. Only
  `tokfold-cli`, which nothing depends on, could carry the key for free, and one
  guard of three is worse than none, because `cargo publish --workspace` would then
  still register the other two — the half that cannot be undone. What does hold the
  line is written down beside it: no workflow contains `cargo publish`, and lifting
  the hold is a documentation change before it is a command, since the "not on
  crates.io" paragraphs in the three crate READMEs ship *inside* the tarballs and
  would render as the crates.io front pages.
- **Nothing shipped told a reader how to read the number in `archive corrupted at
  byte N`.** The message is the only diagnostic whose entire value is an offset,
  and the layout that gives the offset meaning existed only in `format.rs` — a
  source file, not a document, and not one that travels with the binary. The CLI
  README now carries the header table (magic at 0, version at 4, encoder id at 5,
  tokenizer id at 6, flags at 8, the ULEB128 length at 10, then the SHA-256 and the
  payload) and says what each of the reachable offsets means in practice: byte 8 is
  a flag word no `0.0.1` archive sets, byte 4 is a version this format never wrote
  (a *higher* one is a different message, `format version N > supported 1`), and
  byte 10 is what a truncated copy looks like. It also separates the three
  rejections a damaged file can produce — `bad magic` for something that is not an
  archive at all, `archive corrupted at byte N` for framing, and `integrity
  checksum mismatch` for a well-framed archive whose digest disagrees.
- **"No gate hangs off a matrix value" was true of eight jobs and false of three
  legs.** `ci.yml`'s header and the `[0.0.1]` section below both stated it without
  qualification, and it is the whole justification for having hoisted each gate out
  of the `test` matrix. Three `test` legs *are* on the branch-protection list, and
  `continue-on-error` on that job is derived from `matrix.required` — so flipping
  `required: true` to `false` on one of them makes a failing leg report success,
  which is exactly the failure mode the hoisting removed everywhere else. Both sites
  now scope the claim to the eight jobs and name the exception, along with why
  closing it is not a quiet change: hoisting those legs renames the checks branch
  protection matches by exact string.
- **Two documents said every launcher failure path runs on Windows.** `ci.yml` and
  `npm/README.md` both took that from `launcher.test.js`'s own comment, which scopes
  itself to the shell-script skip and says nothing about the *second*, narrower
  Windows skip beside it: a cleared execute bit is not a failure path that starts a
  child, and it is skipped anyway, because Windows does not decide execution by mode
  bits. All three now say so, and the README names the resulting fixture counts
  (six on Linux and macOS, five on Windows).
- The launcher's guard against an unmappable signal name justified itself with
  Windows' smaller signal table. Parent and child run on the same host, so the table
  that named the signal is the table being read back, and that asymmetry cannot by
  itself produce a miss. The guard stays — the cost of a miss is `process.exit(NaN)`,
  which exits `0` on Node 18 — and now gives that as its reason.
- Two `release.yml` comment blocks had been fused by an earlier edit, so the
  paragraph explaining the publish flags introduced the tally variable instead.
  Split back apart, each above the line it describes.
- **The CLI README never said that a recovery archive is plaintext, and neither did
  `--archive` itself.** Three of the six committed READMEs carried the warning: the
  root README, `tokfold-core`'s and the npm launcher's all state that an archive is a
  `TKFD` header followed by the original bytes verbatim, and is therefore exactly as
  sensitive as the input it came from. The one README that actually documents
  `--archive` did not.
  `crates/tokfold-cli/README.md` spent a whole section on which file that flag may
  delete and never said what the file it writes contains — the crate that owns the
  flag was the crate missing the warning. It now carries the same "Not a security
  boundary" section as the launcher README, and adds what none of them said: the
  archive is the input *plus* a header, so `--archive` roughly doubles what a pass
  costs on disk rather than saving anything there. The saving is in tokens, on
  stdout. The same sentence went into the `--archive` long help, on the argument this
  repository already made for the exit-code contract: the surface where that path is
  chosen is `compress --help`, and the callers who most need the fact are the ones who
  will never open a README. A test pins it there.
- **The 16 MiB input ceiling was documented nowhere a user could reach it.** The
  limit is `DEFAULT_MAX_INPUT_BYTES` in `compressor.rs`, and the only prose naming
  it was a doc comment on an internal function — no README, no help text. Yet
  reaching it is the likeliest reason a compression appears to do nothing, and both
  ways of reaching it are quiet: `compress` forwards the input unchanged and exits
  `0`, writing no archive, and `stats` exits `2`. Each explains itself on standard
  error, which is exactly the stream a caller piping stdout onward tends to drop.
  Measured at the boundary rather than read off the constant: 16 777 216 bytes is
  accepted, 16 777 217 is refused. The CLI README and the npm launcher README now
  give the ceiling, its inclusivity, the absence of any flag to raise it, and what
  each subcommand does when it is hit.
- **`Artifact::rendering` never said where its frame ends, and the natural guess is
  wrong.** Every rendering opens with a `⟦tkfd:v1:<tag>⟧` sentinel line, and the body
  after it is copied through unescaped. A document that legitimately contains the
  sentinel's own text therefore carries it into the rendering: compressing the JSON
  string `"⟦tkfd:v1:raw⟧"` yields a two-line rendering in which the sentinel appears
  twice, once as the frame and once as the payload. A consumer that splits renderings
  apart by *searching* for the sentinel cuts wherever the body chose; the rule is to
  cut at the first newline, and a consumer that concatenates renderings must keep its
  own boundaries rather than re-derive them by scanning. The field's docs now state
  both, and a test pins that rendering, its two occurrences and its round trip.

- **`Server::with_max_message_bytes` recommended a limit that cannot be met.** Its
  example set 64 bytes and said the refusal fits; the shortest refusal the server
  can write is 93 bytes plus the digits of the limit, so 64 is answered with a
  95-byte line — over the limit it was asked to keep. The doc now derives the four
  regimes a limit falls into for a one-character-id `ping` (below 95 the
  last-resort refusal itself exceeds the limit; from 95 the id-less refusal fits;
  from 103 the addressed refusal fits; from 143 the answer does) and names the test
  that pins them. `handle_line`'s and the crate root's "never returns a line longer
  than the limit" gained the exception the floor imposes, and `render`'s summary no
  longer promises a bound the floor can break.
- **`tokfold-core` doc comments promised or omitted the wrong things.** `tape::parse`
  had no `# Errors` section although it returns three variants; `compress`'s
  `# Errors` said `InputTooLarge.limit` reports the configured ceiling, which is
  false past 4 GiB where the parser's own `u32::MAX` bound fires first; two
  modules cited a "no-panic contract" that exists nowhere — the real guard is the
  workspace clippy denies on `unwrap`, `expect` and `panic`, which are lints, not a
  proof, and the transport's `catch_unwind` bulkhead exists because of that
  difference; and `never_compress::is_protected` documented its unit as a line when
  the code matches a lexeme.
- **The `tokfold-mcp` README and crate root said one number bounds both
  directions**, which was true only after the read-side fix above; the sentence now
  says how the number is measured — on the message, with the framing newline left
  out — so the invariant it states is the one the code checks.
- **"0.37x could not have been measured on any path this server has" was itself
  false, and so was the floor it rested on.** The correction above derived the floor
  of the escaped-input direction as 2/3 from "a call that carries the payload three
  times escaped", which is the arithmetic of one shape only: a two-byte character
  spelled `\uXXXX`. An escaped character costs six bytes in the call and returns at
  its UTF-8 width *w*, twice, so the no-archive reply tends to 2*w*/6 — 2/3 for
  Cyrillic and for a surrogate pair, 1 for a three-byte script, and **1/3** for ASCII
  a client spells `\u00XX`, which no rule forbids. Measured through the shipped
  binary: `\u0041` × 1 000 is a 6,107-byte call answered by 2,338 bytes, 0.3828x, and
  × 100 000 is 600,107 answered by 200,338, 0.3338x, so 0.37x sits on that curve
  (near 1,350 characters) and was reachable all along — only not by the Cyrillic
  fixture the earlier entry named. The compress path does not "lift the floor to
  4/9" either: its base64 archive holds the raw bytes at 4/3, so behind a rendering
  that shrinks toward nothing the ratio tends to (4/3)*w*/6 — 4/9 for Cyrillic, 2/9
  for ASCII — which is *lower* than 2/3, not higher; a million `\u0020` spread over
  ten thousand rows measured 0.2703x (a 6,128,998-byte call, a 1,656,953-byte
  reply). Corrected in `crates/tokfold-mcp/README.md`, `src/lib.rs` and
  `src/jsonrpc.rs`, with the derivation and the fixtures that reach it, so they can
  be re-run instead of believed. Every figure in that direction
  remains harmless: the cap binds only a reply that is larger than its call.
- **Nothing said what happens when standard output is closed before `tokfold`
  starts.** The exit-code contract names the early-closed pipe and the write failure
  that is not one, and every sentence about it is true — but `tokfold compress >&-`
  exits `0` with the payload gone, and no failure is reported because none occurs:
  Rust's start-up code reopens a closed descriptor 0, 1 or 2 on `/dev/null` before
  `main` runs, so the write lands there and succeeds. Measured on macOS by comparing
  the device and inode of a duplicate of stdout under `>&-` with those of
  `/dev/null` — identical — and through the shipped binary for every subcommand.
  The CLI README's exit-code paragraph and the doc on `write_stdout` now say so; an
  `--archive` file is still written on that run.
  The same start-up step applies to standard input: `tokfold compress 0<&-` reads
  `/dev/null`, an empty document the compressor rejects, so with `--archive` it
  clears a stale archive at that path exactly as `< /dev/null` would. The CLI
  README now says that too.
- **"52 in the limit" was never reachable.** The entry in this list that corrected
  "a full 43-byte header" to a floor gave the ceiling as 52 — the 42 fixed bytes plus
  the ten a ULEB128 can occupy — and `crates/tokfold-cli/README.md` repeated it. The
  tenth byte needs an original of 2^63 bytes. This binary refuses input past 16 MiB,
  so the field never passes four bytes and the header never passes 46; an embedder who
  raises `max_input_bytes` stops at 47, because the parser refuses anything past
  `u32::MAX`. Measured through the shipped binary at the boundaries the arithmetic
  predicts — 43 at an original of 127 bytes, 44 at 128, 45 at 2 097 151, 46 at
  2 097 152 and 46 at 16 777 216 — and the README now gives the ladder with its
  ceiling and the fixtures that reach it. A correction is a claim like any other: the
  earlier entry measured four rungs of the ladder and then wrote the top of it from
  the field's width instead of from the input cap.
- **The `Corrupt` offsets were enumerated as "five fields plus one payload site",
  and both halves were wrong.** The `Fixed` entry in this section on the
  corrupt-archive message, and the rustdoc on `DecompressError::Corrupt` beside it,
  said five fixed header fields raise it — bytes 4, 5, 6, 8 and 10 — "and exactly
  one site names a position in the payload region". Measured through the shipped
  binary on the 56-byte archive of `{"k":"hello"}`: a copy cut at 11, 12, 20 or 42
  bytes says `archive corrupted at byte 11`, because a checksum the archive ends
  inside is reported at the offset the checksum should begin — the byte after the
  length varint, 11 for an original under 128 bytes and up to 20; a length spelled
  `0x8d 0x00` (overlong) says 11, one spelled `0x8d 0x80` and then nothing says 12,
  and ten continuation bytes say 20, since a varint fault is reported at the
  offending byte; a copy of exactly 43 bytes, one with a byte appended and one whose
  field claims 141 bytes say 10. The reachable set is 4, 5, 6, 8 and every offset
  from 10 to 20, all in the header. The one site that would name a position in the
  payload region — `decompress` finding no bytes at `payload_start` — cannot fire:
  `Header::decode` returns as `payload_start` the end of the checksum slice it has
  just required to exist, so the slice from there is at worst empty. A reserved flag
  bit raises `ReservedBitsSet`, not `Corrupt`; byte 8 is a missing flag word in
  `decode` or a known flag set in `decompress`. The rustdoc and the CLI README's
  "Reading `archive corrupted at byte N`" section — which said "most" offsets are
  header fields and read a truncated copy as byte 10 alone — now say all of this;
  the `Fixed` entry is left as written. The same entry ends "Released `0.0.1` prints
  the old string, which is why the `[0.0.1]` section below still quotes it" — that
  section never quoted it; its only sentence on the subject is that exit `3` means
  a corrupt or unrecoverable archive.
- **"24 bytes plus the remainder's digits" was the wrong digits.** The `Fixed` entry
  on the batch member's refusal says the new message is that much longer than the
  old one. The old form was `the reply exceeds the N byte frame limit` — 39 bytes
  plus the remainder's digits — and the new one is `the reply does not fit the N
  bytes left of the M byte frame limit` — 63 bytes plus the remainder's digits plus
  the limit's. The remainder's digits cancel; the growth is 24 bytes plus the
  *limit's* digits, 32 at the default limit. The sentence was written on the same
  day as the fix it describes, from the shape of the message rather than from its
  length. That entry is left as written.
- **Entries in this section said every site now carries a correction, and each had
  left one site as it was.** Checked by reading every site each entry names
  and the ones beside them. The stale-archive entry says "all four now say so" of the
  necessary-not-sufficient qualifier; `ArchiveSlot`, `clear_stale_archive` and the
  doc on `only_the_archive_magic_marks_a_file_the_reject_path_may_delete` did, and
  the doc on `a_non_tokfold_file_at_the_archive_path_is_left_untouched` — the test
  comment the entry counts — still opened "exactly as wide as the hazard and no
  wider" and said "only such a file may be removed". The `tape.rs` entry corrected
  the module doc's "through v0.0.1"; the comment inside
  `depth_past_u16_max_is_reported_exactly_not_saturated` still said it. The
  `node --test` entry says both documents "name the glob form as the path-taking
  alternative"; `npm/README.md` does, and `ci.yml`'s comment did not. The exit-code
  entry says "all three now read corrupt, empty, or otherwise unrecoverable"; the
  crate rustdoc, the help macro and the CLI README do, and the doc on `EXIT_CORRUPT`
  itself — the constant the help quotes — read "a corrupt or otherwise unrecoverable
  archive". The signal-death entry says "the sentence now scopes itself to the codes
  the process chooses"; the CLI README's does, and the crate rustdoc's copy of the
  same sentence was left unscoped. Each of those sites now says what its entry
  claims; the entries are left as written.
- **The `--archive` help entry said "the same sentence went into the `--archive` long
  help" and "a test pins it there", and neither had happened.** The sentence before
  that claim is the one about disk cost. The long help said an archive is "slightly
  larger than" the input and stopped; `compress_help_says_what_an_archive_actually_holds`
  pinned "original bytes verbatim", "not encrypted" and "as sensitive as the input",
  none of which is about size. The help now says the saving is in tokens, on stdout,
  never on disk, and the test requires those words. Read again while checking it, the
  README's own version — `--archive` "roughly doubles what a pass costs on disk" —
  holds only when standard output is itself a file; into a pipe, a pass without the
  flag costs nothing on disk and one with it costs the input's size. The README now
  says the flag adds the input's own size again to what a pass leaves on disk. The
  entry is left as written.
- **Two comments still carried the tally their entry said was gone, and one doc
  claimed a pin the test did not make.** The version-agreement entry says "there is
  no hand-maintained tally to drift"; the comment over that step in `release.yml`
  opened "Twelve places carry the version", and the `npm/tests/resolve.test.js`
  comment the entry says it corrected reads "all twelve version sites". The count is
  right — two manifests and two sites per platform package, and the script requires
  five of those — and it is still a count written by hand. Both now name the sites
  and leave the summary line as the only total. The `MEASURED_OVER_CLAIM_BPS` entry
  says `the_600_bps_bar_sits_just_above_the_flat_object_plateau` "computes every
  figure" in the table its doc carries, and the doc says the test "pins all of it";
  the test computed them and asserted bands a point or more wide around three rows
  and only "under the bar" for the other two, so `5.35%` could have become `4.1%`
  without a failure. It now compares each row at the precision the table spells it
  (see `Tests`). And `with_max_message_bytes`'s doc sent the reader to
  `tests/properties.rs` for the four regimes; it now names the test.
- **The `Documentation` entry on the stale-archive fix ends by asserting the
  invariant a later `Fixed` entry measured false.** "The reject path is still never
  the wider of the two, which is the claim that actually matters and the one the docs
  now make" — the `Fixed` entry on the deletion's justification shows a `chmod 444`
  archive and a trailing separator on which the reject path is the wider one, and
  counts three files that carried the claim. This entry was a fourth site and is not
  counted there. Both are left as written.
- **The `Corrupt` entry above says "a varint fault is reported at the offending
  byte", and one fault is not.** A byte that is missing, overlong, or would overflow
  the `u64` is reported at that byte — 10 to 19 — but a tenth byte that still carries
  the continuation bit is reported at 20, one past the field, because the decoder
  reports the position it needed and could not read. The entry measured "ten
  continuation bytes say 20" and then described the rule as if 20 were the byte at
  fault; ten `0xFF` bytes from offset 10, measured this time, say 19, ten `0x80` bytes
  say 20. The rustdoc on `DecompressError::Corrupt` and the CLI README's "Reading
  `archive corrupted at byte N`" section now separate the two, and the README's
  "two neighbouring failures" are three: it had omitted `reserved header bits are not
  zero`, the message for a flag bit this build has no meaning for — a *known* bit
  that is set is `Corrupt` at byte 8, a *reserved* one is not `Corrupt` at all. The
  entry is left as written.
- **The entry on corrections that each left one site as it was calls the
  `EXIT_CORRUPT` doc "the constant the help quotes", and the help quotes no
  constant.** `exit_codes!()` spells `0`, `2` and
  `3` inside a string literal, so the numbers on every help screen could drift from
  `EXIT_BAD_INPUT` and `EXIT_CORRUPT` without any screen following, and
  `tests/cli.rs` checks each screen against each exit one case at a time, which
  cannot see the pairing itself. It is now pinned by a unit test (see `Tests`)
  instead of by a sentence. The entry is left as written.
- **The `--archive` long help said the archive is "slightly larger" than the
  input, and the CLI README said `--archive` "adds the input's own size again".**
  Both now give the figure: the archive is the input plus a header of 43 to 46 bytes
  (the ladder the corrupt-archive entry above measured), so it is larger than the
  input by exactly that header and puts the input's own size on disk once more.
  `compress_help_says_what_an_archive_actually_holds` passes before and after, as the
  entry above on it says: it pins phrases, none of them a size.
- **The crate rustdoc of `tokfold-core` described `decompress` as canonicalizing
  whitespace and escape style, and it never has.** "What 'reversible' means here"
  said decompression "reproduces a semantically identical document, not identical
  bytes … whitespace and escape style are canonicalized" — a description of a decoder
  no commit has contained: since `9e1e1f5` the archive is the original bytes behind a
  header, and `Compressor::decompress` returns them verbatim. The crate README already
  said both halves; the rustdoc now does too — the value-tree contract is the floor a
  later encoder may fall to, not what this version does.
- **The `Corrupt` correction above says "a tenth byte that still carries the
  continuation bit is reported at 20", and its own measurement says otherwise.** Ten
  `0xFF` bytes carry the continuation bit and, as that entry records, say 19:
  `read_uleb128` checks for overflow before it looks at the bit, so any tenth byte
  with value bits above bit 63 is reported *at* the tenth byte. The only tenth bytes
  that reach 20 are `0x80` and `0x81` — the two that keep the bit without
  overflowing — and nine `0x80` bytes followed by a `0x82` say 19. Measured through
  the shipped binary on a 56-byte archive with its length field replaced: `0x80` and
  `0x81` say 20; `0x82`, `0x7F`, `0x02`, `0x00` and `0xFF` say 19; a `0x01` decodes
  as 2^63 and is refused at 10, where the payload disagrees. The rustdoc on
  `DecompressError::Corrupt` and the CLI README's "Reading `archive corrupted at byte
  N`" section now give that rule with the fixtures, and
  `leb128_overflow_rejected` pins every offset instead of matching on the variant
  (see `Tests`). The entry is left as written.
- **The `--archive` "slightly larger" entry above credits "the corrupt-archive entry
  above" with the 43-to-46 ladder, and that entry measured no ladder.** The ladder
  with its ceiling and fixtures is in the "52 in the limit" entry; the corrupt-archive
  entry measured which byte a fault is reported at. The entry is left as written.
- **The "full 43-byte header" entry above counted "six places" with "four of them
  already hedged", and on the day it was written there were seven and five.** The
  fifth hedged site is the root README's reversibility paragraph, added by the
  "Every reversibility statement gave the contract" entry above two days earlier
  with the same `~43-byte`. The count was one short in both halves, so it named
  every unhedged site correctly and still described a smaller job than there was.
  All five hedged sites — the root README twice, the npm README, the `tokfold-core`
  README and its crate rustdoc — now give 43 to 46 outright, and the two
  library-facing ones add the 47 an embedder reaches by raising `max_input_bytes`,
  which the CLI's 16 MiB cap never does. The entry is left as written.
- **The MCP README and the comment in `tests/session.rs` that explains the reply
  fixture wrote the reply ratio as `2 + 4/3e`, which reads as 2 + (4/3)·e and grows
  with *e*.** Never released. The ratio falls as the escaping factor grows: a reply of
  2·e·P + (4/3)·P over a call of e·P is 2 + 4/(3e), maximal at e = 1, which is why
  their "maximal" claim was right while the formula was not. Both now write 4/(3e).
- **`Fidelity::Lossless`'s rustdoc said whitespace and escape style "are
  canonicalized".** The variant's contract permits canonicalization, and this
  version performs none — every archive holds the original bytes, so the
  reconstruction is byte-exact today. The rustdoc now says "may be", names the
  floor and points at the crate-level docs that say what this version does, as the
  crate root and the READMEs already did.
- **`ci.yml`'s job-layout legend gave clippy a command cargo refuses to run.** The
  legend exists so that a reader can check each gate against the job below it, and
  its clippy line read `cargo clippy --all-targets --all-features -D warnings` — not
  an abbreviation of the step but a different, invalid command: `-D warnings` is an
  argument for the lint driver and has to follow a `--`, so cargo answers "unexpected
  argument '-D' found" and prints `cargo check`'s usage instead. The `deny` and
  `package` lines had each dropped the `--locked` their steps carry, and the `docs`
  line paraphrased. Every legend line that gives a command is now that job's `run:`
  verbatim; the `docs` line names its `RUSTDOCFLAGS` separately, because that one is
  an environment variable and not an argument; and the three lines that describe
  rather than quote say which job to read. A legend nobody can run cannot be checked
  against the thing it summarizes, which is the only work a legend does.
- **The doc comment on `leb128_overflow_rejected` stated the last two checks in
  `read_uleb128` the wrong way round** — "a missing byte, then overflow, then the
  continuation bit, then an overlong terminator" — and that order is what the whole
  table of offsets beneath it is derived from. In the decoder the overlong check sits
  inside the terminator branch and runs per byte, while the continuation-bit failure
  is not a per-byte check at all: it is raised after the loop has spent all ten bytes
  with the bit still set. That is exactly why one offset in the table falls outside
  the length field while every other names the byte that faulted — so stating the
  order wrongly withheld the reason for the one value a reader is most likely to
  doubt. The comment now separates the three checks that can fire while a byte is
  read from the one raised after the loop, and records that the `u32` conversion
  between them cannot be reached within ten bytes.

- **The job-layout legend in `ci.yml` said its describing lines "say so by pointing
  at their job", as the `ci.yml` entry above repeats, and one of them does not.**
  `doc-drift` and `launcher` point at their own job; `actionlint` describes what the
  job checks and names no job to read. The legend now says which is which. The
  paragraph after it opened "Each of those carries exactly one condition", where
  "those" read as the three describing lines; it now says the eight gate jobs. The
  entry is left as written.
- **The root README called `compress` "total on valid JSON" with no limits.** Past
  16 MiB of input or 512 levels of nesting — the defaults, both raised through
  `ConfigBuilder` — `compress` returns a `CompressError` exactly as it does for
  invalid input, so the claim held only inside the configured limits. The root and
  `tokfold-core` READMEs now say so, the CLI README's size-limit section adds the
  depth refusal (`compress` exits `0` with `nesting depth 513 exceeds limit 512` and
  writes no archive; `stats` exits `2`), and the npm README says deeper JSON is
  refused the same way as oversized input.
- **The root README's escape-style example compared `"é"` with `"é"`** — the same
  literal twice, so it illustrated nothing. It now compares `"é"` with `"\u00e9"`.
- **The root README declines to call the engine "lossless", and the MCP server's
  `stats` object prints `"fidelity": "lossless"`.** That field, and
  `Fidelity::Lossless` behind it, names the class of the recovery path — the archive
  reconstructs the value tree — and says nothing about how well a model reads the
  rendering. The root and `tokfold-core` READMEs now say where the word appears and
  what it is scoped to. The variant's name is unchanged.
- **Two root README sites gave the passthrough header as "43 to 46 bytes" without the
  47 a library caller reaches.** An original of 2^28 bytes or more needs a fifth
  length byte, and only a raised `max_input_bytes` admits one; the parser's 4 GiB
  ceiling means no header is ever longer than 47. Both sites now say so.
- **The MCP README, `tools.rs`, the crate rustdoc and a comment in
  `tests/session.rs` said the archive "barely shrinks" on inputs E1 cannot help.**
  The archive never shrinks at this version — it holds the original bytes plus a
  header — so what barely shrinks is the rendering. Each site now says rendering, and
  the session comment says the input grows the archive past the input and its header.
- **The `tokfold-core` E2 module doc described the tables on which a tab layout gives
  back more than half of the key-dedup saving as "wide rows of long string
  values".** The 90 shapes the survey counts are tables whose every value is a
  string — dates and short runs of one letter at each surveyed width from three
  fields up, three included, one-letter strings from five, each at some of the
  surveyed heights rather than all of them; no table with a number or a boolean in its
  rows is among them. The doc now says that, and the survey test holds it (see
  `Tests`).
- **The `MEASURED_OVER_CLAIM_BPS` rustdoc said its test "pins all of it".** The test
  pins the per-shape table and the paragraph beneath it; the corpus-wide figures
  above the table are measurements over the reference corpus that no test
  recomputes. The rustdoc now says which is which, and the test now also pins which
  rows keep E1 at the 600 bps bar.
- **The `Tests` entry on the ten-byte length field that decodes called it "the one
  offset no test held", and the printed `Corrupt` line was held by no test at all.**
  Several of the offsets the CLI README quotes were matched on the variant alone,
  and none was checked on what `tokfold expand` prints, so rewording the message or
  dropping the offset from it left the workspace green. The entry is left as
  written; the new pins are under `Tests`.
- **The doc comment on `leb128_overflow_rejected` said the `u32` conversion of the
  shift "cannot be reached" within ten bytes.** It runs on every byte, between the
  missing-byte and overflow checks; only its failure arm cannot fire. The comment now
  says that, and that the post-loop continuation-bit failure is the only *varint*
  fault reported past the field: a checksum cut short behind a ten-byte length is
  reported at 20 as well, where the checksum begins.
- **"Total on valid JSON"** in the `tokfold-core` crate doc and the encoder module
  doc stated no limits; both now say "within the configured limits", and the crate
  doc names the defaults (16 MiB of input, 512 levels of nesting).
- **The root and `tokfold-core` READMEs said the input limit can be raised through
  `ConfigBuilder` without saying how far.** It stops at 4 GiB less one byte, the
  most the parser's `u32` offsets can address; past that the parser reports its own
  ceiling.
- **The `tokfold-core` README said the word "lossless" is avoided on purpose**,
  while a public item carries it. It now says that `Fidelity::Lossless` is that one
  item, and that it names the recovery path's class.
- **`MEASURED_OVER_CLAIM_BPS` and the entries describing it said which encoder a
  shape keeps without naming the profile.** Every "keeps E1" was measured under the
  minify-only `Conservative` profile; under the default `Balanced` profile an array
  of row objects goes to E2, and the margin weighs E2's saving. The rustdoc says so,
  and an earlier entry here that called 600 bps a "default" now calls it the opt-in
  bar it is.
- **The E2 module doc described the over-half shapes as one set.** It now gives the
  widths per family, as measured: dates and short runs of one letter from three
  fields, one-letter strings from five.
- **The entry on the non-atomic archive write gave `ulimit -f 8` a single result.**
  The unit of `ulimit -f` belongs to the shell: zsh counts 512-byte blocks and bash
  1024-byte ones, so on macOS the same command leaves a 4,096-byte stub under zsh and
  an 8,192-byte one under bash (measured on a 49,964-byte archive). The entry, which
  measured under zsh, is left as written.
- **A piped input named as its own archive was described as always destroyed.** The
  `--archive` help said `cat log.json | tokfold compress --archive log.json`
  "overwrites log.json", and both READMEs said "either way the source document is
  gone" (or "destroyed"). Only a successful pass writes there, and what it writes is
  the archive, which `tokfold expand` turns back into the document byte for byte; a
  rejected pass — piped NDJSON, say — exits `0` and leaves `log.json` untouched, unless
  that file opens with the `TKFD` magic, in which case it is deleted. The help and
  both READMEs now say that. The stdin identity entry under `Fixed` ("the pass
  overwrites `log.json`") and the entry on the npm README's redirect sentence
  ("destroyed either way") repeat the old claim and are left as written.
- **`ci.yml` justified `actionlint` by saying cargo does not read JavaScript.** True,
  and not the point: actionlint reads only the workflow files, so no gate reads the
  JavaScript; the comment now says that.
- **The entry on the deletion's justification says the path-spelling asymmetry is
  "unchanged and still the right default" and pinned by a test.** It is neither now:
  the trailing-separator case deleted the input it was named after, and the `Fixed`
  entry on it removes the asymmetry. The `Tests` entry on the two archive-path
  asymmetries likewise describes a spelling test that no longer exists in that form.
  Only the `chmod 444` half of either still holds; both are left as written.
- **The npm launcher's header and README said only `SIGPIPE` falls back to `128 + N`.**
  They now name `SIGUSR1` as the second case.
- **A binary killed by a signal Node cannot name reads as a success.** Node names the
  child's signal before the launcher sees it, and a signal it has no name for —
  `SIGEMT` on macOS, measured — arrives as an exit with code `0` and no signal, so
  the launcher exits `0`. `0.0.1` does the same, and no public Node API tells the
  two apart. The launcher header, its README's exit table and the test that empties
  the signal table now say so; they had said an unnamed signal "never reaches the
  lookup", which is true, without saying where it goes instead.
- **`npm/README.md` said that apart from `SIGKILL`, "everything else is tested"
  about orphaned binaries.** The launcher relays five signals (four in `0.0.1`,
  which had no `SIGUSR1`); any other fatal
  signal sent to its pid alone — `SIGUSR2` and `SIGALRM` were measured — still kills
  it and leaves the binary running, as in `0.0.1`. The paragraph and the launcher's
  comment on the relayed set now say so. "Those tests fail against the previous
  `spawnSync` version" now names all four shutdown-signal tests and the version they
  fail against: the `spawnSync` launcher of commit `81c7681`, which was never
  published and, signalled on its pid alone with the binary held, left the binary
  running for `SIGTERM`, `SIGINT`, `SIGHUP` and `SIGQUIT` alike. The published
  `0.0.1` launcher (commit `87fb332`) already relayed those four and took the binary
  with it for each (both measured on macOS, Node v22.23.2).
- **The launcher's relay comment and `npm/README.md` said any other fatal signal
  sent to the launcher alone kills it.** It gets Node's disposition, not the
  kernel's: `SIGPIPE` and `SIGXFSZ` are ignored by Node, so the launcher lives on,
  and `SIGUSR1` opened the inspector. The `npm/README.md` entry on orphaned binaries
  repeats the old claim and is left as written. The README section headed "The
  launcher outlives the binary, never the other way round" described cases where the
  launcher dies first; it is now headed "The launcher relays shutdown signals to the
  binary".
- **The launcher README's one signal death that reads as `0` was stated without its
  platform.** `SIGEMT` was measured on macOS. On Linux (aarch64, glibc 2.39, Node
  v22.23.2) every signal from 32 to 64 arrives the same way, measured: the realtime
  signals from `SIGRTMIN` (34) up and the two below it that glibc reserves. The
  README now names both platforms and the range.
- **The `Tests` entry on the trailing-separator spelling says the new test asserts
  "the file's bytes unchanged".** For `--archive x/ >> x` the file grows by the
  appended passthrough, and the test asserts that it still starts with the bytes it
  was seeded with. The entry is left as written.
- **The launcher's stderr retry comment named "a terminal under flow control" as a
  case the bounded `EAGAIN` retry covers.** A blocking descriptor never returns
  `EAGAIN`; the write waits in the kernel, unbounded. The comment now limits the
  retry to non-blocking descriptors and says so.
- **`release.yml` said a release of "whatever happens to be on main" is not
  possible.** The version check pins the number, not the commit: the workflow builds
  whatever ref it is dispatched on, from any branch, with no check that its CI
  passed. The header now says that.
- **The launcher README said the other platforms' packages are "never downloaded"
  and that on musl "no package name matches".** The platform packages declare no
  `libc` field, so on a musl Linux npm installs the glibc package for that
  architecture and the wrapper refuses to run it. Both sentences now say so.

- **The same-file guard entry describes a fallback that does not exist.** It says
  that where the platform exposes no file identity the comparison falls back to the
  canonicalized paths "and then to the literal spelling". There is no literal-
  spelling step: off Unix a path that does not canonicalize is simply not the same
  file, and on Unix only file identities are compared. The entry is left as written.
- **The `Fixed` entry on a binary killed by `SIGUSR1` misdescribes `0.0.1`.** It
  says `0.0.1` exited `158` on macOS; `0.0.1` exits `1`, because the re-raise meets
  Node's inspector handler instead of killing the process. Its "`SIGUSR1` is now
  never re-raised and exits `128 + N`", the relay entry's "the launcher exits `128 +
  N` (`158` on macOS)", this section's "They now name `SIGUSR1` as the second case"
  and the `Tests` entries asserting `128 + N` for `SIGUSR1` all describe a
  never-released intermediate: the launcher now dies of `SIGUSR1` itself (see
  `Fixed`). All are left as written.
- **The `128 + N` entry says the fallback is "exactly what the caller would have
  seen running the binary directly".** That holds for what a shell reports. A
  parent that reads the signal itself — Node's `spawnSync(...).signal`, Python's
  negative `returncode` — sees an exit code from the launcher where the bare binary
  would have given it a signal. The npm README now says so; the entry is left as
  written.
- **Two descriptor archive-slot entries are out of date.** The entry on `realpath`
  of `/dev/fd/N` quotes the refusal as `a stale recovery archive is open behind
  <path>`, and the entry "A read-only descriptor over an archive is not a stale
  archive slot" calls a file the write cannot reach an empty slot. Both were
  superseded before any release: that file is now reported `left <path> untouched:
  it cannot be opened for writing`, not treated as empty, and the refusal names the
  magic (see `Fixed`). They are left as written, and so is the `Tests` entry's "a
  read-only one is now an empty slot".
- **The MCP `MAX_NODES` rustdoc said "not even the densest *legal* line reaches
  it".** False in `0.0.1`: a `ping` whose `_meta` holds 4 194 298 zeroes in an array
  (about 8 MiB, well under the 32 MiB line limit) exceeds it and is answered `-32700`
  with no id. The rustdoc now says a legal line can reach it and what the reply is.
- **The JSON-RPC `Id` rustdoc said an id is "echoed back verbatim".** It is echoed
  by value: an integer id is parsed and re-rendered, so `-0` comes back as `0`. The
  rustdoc now says so.
- **The `Corrupt` rustdoc and the CLI README said byte 4 is "a version `1` has never
  written".** Never released. A version above `1` has never been written either and
  is `UnsupportedVersion`, not `Corrupt`; the offset means a version byte `0`. Both,
  and the `format.rs` comment that listed the version faults, now say `0`.
- **Two comments said opening a FIFO blocks "until a writer appears".** That is the
  read side. The CLI's comment is about the archive *write*, which blocks until a
  reader appears; it now says so, and the test's comment names both ends.
- **The MCP `not_utf8` comment called the path unreachable through this server.**
  The server only ever compresses `&str`, but the archive argument is supplied by
  the caller, and an archive built elsewhere over bytes that are not UTF-8 decodes
  cleanly and reaches it — `0.0.1` included. The comment now says so, and that
  `tokfold expand` restores the same archive at exit `0`.
- **The npm READMEs and the launcher header did not say that only descriptors `0`,
  `1` and `2` reach the binary on macOS.** libuv spawns with every other descriptor
  closed, so `--input /dev/fd/3` or `--archive /dev/fd/3` through the wrapper fails
  with `Bad file descriptor` and exit `2` — measured on macOS with Node v22.23.2,
  for this launcher and the registry's `tokfold@0.0.1` launcher alike; on Linux
  (aarch64, glibc 2.39, Node v22.23.2 and v18.20.8) this launcher's binary found
  descriptors `3` and `4` closed as well. The
  launcher README and header now say so and point at running the binary directly;
  widening `stdio` is not safe, since Node's own descriptors cannot be told apart
  from the caller's.
- **The npm READMEs said a `SIGUSR1` sent to the launcher is always relayed.** It is
  relayed once the launcher's script is running; one that lands while Node is still
  starting opens the debugger or kills the launcher, before any binary exists. Both
  READMEs and the launcher's comment now say so.

- **Two `Changed` npm entries describe a corner production does not reach.** The
  entry on a signal "this runtime cannot number" and the `128 + N` entry's fallback
  to `1` both hang off a name missing from `os.constants.signals`. Node names the
  child's signal from its own list in C++, separate from that table but with the
  same coverage (measured on macOS, Node 22), before the `exit` event fires, and a signal
  it cannot name — `SIGEMT` on macOS, measured — arrives as `exit(0, null)`, so the
  launcher exits `0`, not `1`, and writes nothing. The `1` with its line is reached
  only when the table lacks a name Node itself produced, which the tests arrange by
  emptying it. Both entries are left as written.
- **The `128 + N` entry's example, "a `SIGPIPE` kill now reports `141`", cannot
  happen to tokfold.** Rust starts the binary with `SIGPIPE` ignored, so a closed
  reader is an `EPIPE` error the binary reports itself; `SIGXFSZ` (`153` on macOS)
  is the reachable example. The entry is left as written.
- **The launcher header said "tokfold streams".** Only `mcp` does — `compress`,
  `expand` and `stats` read their whole input before writing — and `0.0.1`'s header
  said the same. It now says `mcp` streams.
- **An inherited ignored signal was not documented.** The launcher's listeners
  replace an ignore the shell handed down, and removing them restores the kernel
  default, so `nohup tokfold mcp &` through the wrapper dies of a hangup the binary
  run directly survives. `0.0.1` behaves the same (macOS, Node 22). Node cannot read
  an inherited disposition, so the header now documents it.
- **The launcher header did not say that an inspector opened during Node's boot
  stays open.** A `SIGUSR1` that lands before the script runs meets Node's own
  handler; the listener installed afterwards does not close the inspector it
  opened. The header now says so.
- **The entry on how to read `archive corrupted at byte N` paraphrases the README
  as "byte 4 is a version this format never wrote".** The README now says a version
  byte `0`, as the entry on the `Corrupt` rustdoc records. The paraphrase is left
  as written.
- **The MCP README and rustdoc called the 2.00x and 3.33x reply ratios ceilings.**
  They are limits approached from above: a one-byte `{"k":"z"}` call is answered at
  5.02x and a 10,000-byte value at 3.35x, and the largest call still answered in one
  frame gives 3.3334x, which a `tests/session.rs` comment gave as 3.3339x. `2 +
  4/(3e)` is now described as the payload's share of the ratio, not the whole of it.
  The same README's call-size threshold held for the `{"k":"z"×n}` shape only; it
  now says so, and that a 5,000,000-byte run of `A` is answered with 10,000,338
  bytes. `0.0.1` carried the ceiling wording. The earlier `Documentation` entries
  that call the multiple "a ceiling, not a law", say 3.33x "is" the ceiling against
  the call and that `{"k":"z"×n}` measures 3.3339x "at every size", call the
  10,066,259-byte threshold "exact" without naming its shape, and describe the
  ratchet's bound as set against "a structural *ceiling* of about 3.3x", all repeat
  the old claims and are left as written.
- **A comment on `parse_number` in the MCP JSON reader did not say where widening
  stops.** A number past `i64` is kept as an `f64`, so the line stays parseable up
  to about 309 digits and is `-32700` "number is out of range" beyond; an id widened
  that way is refused `-32600` with no id. The comment now says so.
- **The core rustdoc said inputs "over 4 GiB" are refused.** The parser addresses
  at most `u32::MAX` bytes, one under 4 GiB, so an input of exactly 4 GiB is
  refused too. The rustdoc on `compress` and on the error variant now says "4 GiB or
  more".
- **The core determinism guarantee promised the same archive for the "same logical
  input".** It holds for the same input bytes under the same `Config`: `{"a":1}` and
  `{ "a" : 1 }` render identically and produce different archives. The crate
  rustdoc, the `Compressor` and estimator rustdoc and both READMEs now say bytes.
  `0.0.1` carried the old wording.
- **The core crate rustdoc called do-no-harm "never makes it worse".** The gate
  compares estimated tokens; a real tokenizer can count more for the chosen
  candidate. It now says "never worse by the estimate". `0.0.1` carried the old
  wording. (That correction was itself false — see the passthrough-sentinel entry
  below; it is left as written.)

- **E2 counts array shapes by hash, and the docs said it counts key lists.**
  `AnalyzeFrame::intern` looks a shape up by its `FxHasher` value and never compares
  the key text, so two key lists that collide are counted as one shape. Colliding
  two-key lists are found by a generalized-birthday search in under a minute
  (`["a03c67ad","b03bbddf"]` and `["c004d13f","d009908a"]` on 64-bit aarch64). On
  such an input the header can be a shape that occurs once while every other row
  deviates, and two elements with no key list in common can qualify. Output still
  round-trips, because rows are matched against the header by exact key text. The
  E2 module docs now say "shape hash". The determinism guarantee in the core rustdoc
  (crate, `Compressor`) and both READMEs now says "from one build": `FxHasher`
  computes different values on 32- and 64-bit targets (and on sparc64/wasm64), so
  cross-target byte identity is not established; only 64-bit aarch64 is measured.
  Comparing key text on a hash hit would change renderings of a binary on npm and is
  left to the owner.
- **The passthrough sentinel's overhead was called constant.** It costs 10
  estimated (11 cl100k, 13 o200k) tokens for an input that opens with a non-whitespace
  byte; leading whitespace merges with the sentinel's newline, and over every prefix
  of up to six spaces, tabs, CRs and LFs before `{}` it measured 10–11 estimated,
  10–12 cl100k and 12–13 o200k. The core rustdoc and both READMEs now say so. The
  "Do no harm" bullet no longer says compressing never makes a document worse by the
  estimate — the passthrough frame itself is longer by the estimate — and now says an
  encoder's rendering replaces the input only when the estimate prices it lower,
  frame included. `Stats::est_tokens_after` no longer says "about 10 tokens" for
  every estimator.
- **A tape test comment put the input limit at "larger than 4 GiB".** An input of
  exactly 2^32 bytes is refused; the comment now says 4 GiB or more.

- **The npm launcher said a child killed by a signal missing from
  `os.constants.signals` exits `1`.** Such a signal arrives from Node as `exit(0,
  null)` and exits `0`, as the README's next paragraph already said. The `1` needs
  Node to report a name its own table cannot map back; no signal does, measured on
  macOS with Node 22 for signals 1 to 31 and on Linux (aarch64, glibc 2.39, Node
  v22.23.2) for signals 1 to 64, so only the launcher test that empties the table
  reaches it. The README's exit-code table now says a
  `128 + N` means the signal was re-raised and did not kill the wrapper, rather than
  that it "could not be" re-raised, and comments that called Node's signal names
  "the same table" now say they come from a separate list whose coverage matches.

- **`tokfold-mcp` still called the reply ratio a ceiling in three places** — the
  `tools` module doc, the `render_bounded` doc and the `tests/session.rs` comments.
  Each now describes a large-call limit approached from above, and the test pinning
  it is renamed `the_documented_large_call_reply_limit_is_reached_by_the_fixture_the_docs_name`.
- **The batch "177 bytes left" example named the wrong member.** After 225 942
  `ping` answers it is a `tools/list` answer that is refused; a `ping` in that place
  fits and is answered.
- **`Server::with_max_message_bytes` said a limit under 143 "answers nothing but
  refusals".** At 142 an unknown method (`-32601`, 78 bytes), a parse error
  (`-32700`, 94), a request with no `method` and an empty batch (`-32600`, 86 and 79)
  still get their real answers; only answers at least as wide as a `ping`'s are
  refused.
- **A refusal was documented as always addressed to the request's own id.** An id
  too wide for the addressed refusal to fit — at the default limit a string id within
  about a hundred bytes of 32 MiB — is refused with the bounded id-less form (101
  bytes for a request of exactly 32 MiB), so that one call cannot be correlated. The
  `MAX_MESSAGE_BYTES`, `handle_line`, `handle_batch` and `with_max_message_bytes`
  docs and the MCP README now say so.
- **The pipelined-client deadlock was documented as "one pipe buffer", reached by
  2,000 pings.** The replies fill the stdout pipe, then the stdin pipe and the loop's
  8 KiB read buffer fill behind them. Measured on macOS with 41-byte pings answered
  with 144 bytes, 2,195 complete and 2,200 deadlock; the docs now give that fixture
  and platform.
- **`stdio::MAX_LINE_BYTES` said a reply is "always bigger" than its call.** A
  passthrough of escaped whitespace answers a 6,128,998-byte call with 1,656,953
  bytes (0.27x). The MCP README's unfixtured payload ratios (3.68x ordinary, 4.16x
  quote-dense) are replaced by a measured one with its fixture: `["a","b",…]` of
  50,000 one-letter strings is answered at 4.34x its payload. The earlier entry that
  quotes 3.68x and 4.16x is left as written. Small-call ratios are now marked as
  counting each line's newline (`{"k":"z"}`: 121 → 608 framed, 5.02x; 5.06x on the
  messages alone) and the boundary sizes as message sizes.
- **The npm launcher justified its Node 18 guard with a CI leg that cannot see the
  one Node 18 problem measured on it.** The launcher header ended "which is the
  reason to keep both it and the Node 18 CI leg". The guard's reason holds
  (`process.exit(NaN)` exits `0` on Node 18 and throws `ERR_OUT_OF_RANGE` on 20 and
  later), but the Node 18 leg runs on Linux only — the launcher matrix is
  `linux-node18`, `linux-node22`, `macos-node22`, `windows-node22` — and the one
  launcher fault measured on Node 18 was measured on macOS. One measurement (macOS
  arm64, Node v18.20.8, the fake binary held, 60 runs for each relayed signal, the
  harness killing the launcher after 8 s) found 32 of 300 runs in which the binary
  was dead and reaped and the launcher's `exit` event never fired, with nothing on
  stderr; the same session reproduced it with a minimal script spawning `/bin/sleep`
  and no tokfold (5 of 60), and saw none on macOS with Node v20.20.2, v22.23.2 or
  v26.7.0, nor on Linux with v18.20.8 or v22.23.2. A second attempt on the same host
  and Node version reproduced none in 300 runs. The published `0.0.1` launcher
  (commit `87fb332`) uses the same asynchronous `spawn`; whether it hangs was not
  measured. The launcher header and its README now document this as a known issue,
  and `npm/README.md` names the missing CI leg. The remedy — a `macos-node18` leg, a
  higher `engines` floor, or a watchdog in the launcher — is an owner decision; none
  has been taken. Under CPU load the fault reproduces through the suite itself: on
  the same host with four `yes` processes running, `node --test npm/tests/*.test.js`
  on Node v18.20.8 lost 1 to 3 of its 76 tests to the 30-second bound in 2 of 3
  runs, while Node v20.20.2 and v22.23.2 passed 76 of 76 under the same load and
  v18.20.8 passed 76 of 76 unloaded.
- **`npm/README.md` said the published `tokfold-linux-arm64-gnu` binary "has never
  been executed by anyone, anywhere."** CI never executed it — the step was gated on
  `if: matrix.native` when it was built — but on 2026-09-26 the registry tarball
  (`dist.shasum` `08af891b2e7a02bd822eb7b12b7c1fee5b30d4ae`, matched locally) was run
  by hand in a Linux aarch64 VM: `--version` printed `tokfold 0.0.1`, and a
  `compress --archive` then `expand` round trip exited `0` both times and reproduced
  the input byte for byte. The README now says "CI has never executed" it and records
  the one manual run as that, not as a gate.
- **The launcher's `EAGAIN` budget was described as "about a second of patience".**
  That is `EAGAIN_ATTEMPTS` times `EAGAIN_PAUSE_MS` at the nominal pause, and no test
  reaches the retry: it needs a full non-blocking pipe on fd 2. Both comments that
  gave the duration now call it a chosen budget that was never measured.

### Tests

Defects in the suites themselves that no passing run could have shown — each
established by mutation or by measurement before it was fixed, and re-established the
same way afterwards — followed by the tests that pin the fixes above.

- `a_larger_limit_can_collapse_a_batch_that_a_smaller_one_answered` pins the batch
  windows of the Documentation entry on batch monotonicity at every limit from 393 to
  3 200 with the wide member in the middle, and from 413 to 3 200 with it at the end
  (exact shapes up to 1 000, "no collapse" beyond). It also pins the tool catalogue at
  2 521 bytes as a batch member; in `0.0.1` it is 2 411.

- **The output-into-input refusal is pinned for all seven entry points** —
  `compress`, `expand` and `stats` each through `--input` and through standard input,
  and `mcp` — on the full message and on the file left byte-for-byte unchanged. The
  run is killed after ten seconds: with the guard removed, the `mcp` case is the
  unbounded feedback loop it exists to refuse, and an unbounded wait filled the disk
  instead of failing. A `>`-emptied input is pinned not to cost the archive; the
  `/dev/stdout`, `/dev/fd/1`, `/dev/stdin` and `/dev/fd/0` spellings are pinned as
  refusals; and `--archive /dev/stderr` is pinned either to write exactly the archive
  or to refuse and leave the earlier log line alone.
- **The closed-stderr test drove only `compress`.** It now drives `expand` (exit `3`),
  `stats` (exit `2`) and `mcp` as well, and a clap usage error (`--bogus`, exit `2`) —
  the one diagnostic tokfold does not format itself, and the one whose failed write is
  dropped by `usage_failure` rather than by `warn`. A datagram socket as stderr pins
  that each diagnostic is one `write` of one complete line: the exact text of all three
  lines a rejected `compress --archive` prints, and the whole of the `--bogus` usage
  error, which clap's own printer had sent as ten fragments. A unit test pins which
  `ErrorKind`s count as a reader that has gone. Hand-mutating every new condition, call site and arm in this group —
  fifteen mutants — killed all of them.
- **A rejected `compress` through a descriptor spelling is pinned from each side.**
  `--archive /dev/fd/3 3> a.tkfd` is pinned to pass the input through and leave the
  slot empty; on macOS a stale archive behind `3<>` is pinned to exit `2` with the
  whole message and the archive byte-for-byte intact, a non-empty log behind `3>>` to
  exit `2` untouched, and a pipe on descriptor 3 already holding the magic to be
  passed through without one byte of it consumed. A read-only standard output on the
  input is pinned to fail on the write, not as a collision. Hand-mutating the new
  branches killed each one but two: the arm for an archive that ends before the magic
  is reachable only if the file shrinks between `stat` and `read`, and the arm for a
  descriptor path that does not exist only if `realpath` resolved a path `stat` then
  cannot find.

- **Neither archive-path asymmetry was asserted anywhere.** The closest existing
  tests vary the archive's *bytes* against the rejected path, or a missing *parent*
  against both; none varies one file's mode, or one path's spelling, across the two
  outcomes — which is the only way the split above is observable. Two `cfg(unix)`
  tests now do, each running both halves on one file: a `0o444` archive is asserted
  byte-for-byte intact after the successful pass and gone after the rejected one, and
  a trailing-separator spelling is asserted refused by the one and resolved by the
  other. Both carry a canary rather than a platform assumption — the mode test returns
  early under a uid that ignores mode bits, and the spelling test probes
  `fs::canonicalize` first, so on a libc whose `realpath` enforces the POSIX
  trailing-slash rule the test skips instead of asserting a divergence that platform
  does not have. Both failure messages name the documentation that goes stale if the
  behaviour changes.
- **The `--archive` identity tests cannot disable themselves in silence.** Three of
  them skip when `file_identity` returns `None` — legitimate on exactly one platform,
  the non-Unix fallback where the operating system hands out no file identities at
  all. The skip runs through a helper that asserts `cfg!(not(unix))` before taking
  it, so a `file_identity` that has simply stopped working on Unix fails three tests
  instead of skipping them.
- **Two MCP dispatcher properties held for a dispatcher that answers nothing.** Both
  wrapped every assertion in `if let Some(reply) = server.handle_line(&line)`, so a
  `handle_line` returning `None` for every input satisfied them vacuously — the
  mutation leaves both green. The suite now carries a deterministic
  generator-liveness meta-test, in the shape `tokfold-core`'s `roundtrip.rs` already
  uses: 1500 draws through `TestRunner::deterministic()`, counting lines that were
  answered, lines answered with silence, answers that fit the frame limit and
  answers the limit refused, and failing if any of the four is zero. Under the same
  mutation the meta-test is the thing that fails, which is what it is for.
- **...and one of those properties was reaching its stated target 0.07% of the
  time.** `no_answer_ever_exceeds_the_frame_limit` drives the frame limit down to
  128-512 bytes, and its own docs claimed that at that size "almost every answer
  overruns it", so almost every draw exercises the refusal machinery rather than the
  happy path. Instrumenting the new meta-test measured the opposite: **1 refusal in
  1495 draws**. A malformed line — which is nearly all the junk alphabet produces —
  is answered by a compact parse error that fits any limit in that range. The
  generator now mixes in three requests that have real answers, one of them a
  `tools/list` whose catalogue overruns every limit it can draw, which lifts
  refusals to **271 in 1493**, and the paragraph now reports the measurement instead
  of the hope. The meta-test holds it there with a *proportional* floor rather than
  the `refusals > 0` it was first written with — that assertion would have passed on
  the broken generator, and a liveness check a 0.07% hit rate satisfies certifies
  nothing. Restoring the old junk-only generator now fails it: 0 refusals against a
  required 74.
- **Two tests named for a request being *served* accepted an empty catalogue.**
  `a_modern_request_is_served_without_a_handshake` and
  `a_legacy_declared_version_needs_no_client_capabilities` asserted only that the
  reply carried no `error` member. A `tools/list` answered with an empty `tools`
  array is a successful reply *and* an empty catalogue, and both tests pass for it,
  so neither could tell a served request from a served-but-empty one. Both now go
  through a helper that requires all three tools — the assertion two other
  `tools/list` tests already made inline. `a_legacy_session_runs_end_to_end` now uses
  the helper too; `tools_list_carries_the_cache_hints_the_spec_requires` still makes
  the assertion inline.
- `the_read_bound_is_the_message_and_the_newline_is_not_charged_against_it` feeds
  `read_line` a message of exactly `MAX_LINE_BYTES` under `\n`, `\r\n` and no
  terminator and requires each to come back whole, then one byte more under each
  and requires `Line::TooLong` — the case the old reader got wrong.
- `a_limit_below_a_real_answer_refuses_in_three_measurably_different_ways` pins the
  regimes `with_max_message_bytes` now documents: a limit of 64 is answered with the
  95-byte last-resort refusal that exceeds it, 95 is the floor at which the id-less
  refusal fits, 103 is where the addressed refusal fits, and 143 is where a
  one-character-id `ping` is answered rather than refused.
- `an_empty_archive_keeps_the_bad_magic_code_and_says_it_is_empty` calls
  `tokfold_decompress` with an empty string and requires the `bad_magic` code
  together with the `the archive is empty (0 bytes)` text.
- `the_same_file_guard_sees_the_input_redirected_onto_standard_input` (Unix) runs
  `compress --archive log.json` with `log.json` itself as the child's standard
  input, and then with a hard link to it, and requires exit `2`, the "names the
  file on standard input" refusal, an empty stdout and an unchanged file; it then
  redirects a *different* file and requires the pass to succeed and write the
  archive, so the guard cannot be satisfied by refusing every redirect.
- `every_literal_is_ascii` in `never_compress` pins the premise the module doc's
  "ASCII-only case folding is sufficient" argument rests on. Nothing checked it: a
  non-ASCII literal would compile, be matched byte-for-byte, and silently stop being
  case-insensitive in the letters that are not ASCII.
- `a_batch_member_refused_for_size_names_the_bytes_left_and_the_limit` derives the
  remainder the way `handle_batch` does and requires the member's message to carry it
  and the limit, and pins the single-request message beside it so the two forms cannot
  drift into each other. The existing three-member batch test moved its limit from 400
  to 480: the longer refusal for the middle member left the third `ping` too little
  room at 400.
- `the_600_bps_bar_sits_just_above_the_flat_object_plateau` compares every row
  of the table in the `MEASURED_OVER_CLAIM_BPS` doc at the precision the table spells
  it — `6.4%–7.1%`, `13.1%–16.0%`, `13.1%–21.8%`, `5.35%` and `1.96%`.
- `the_tab_layout_is_priced_the_way_the_e2_docs_say_it_is` requires every case
  that reaches the 86% peak to be the shape the E2 module docs name for it, the
  date-like family at twenty fields — the place as well as the number.
- `compress_help_says_what_an_archive_actually_holds` requires, among the rest,
  "the saving is in tokens".
- `the_help_spells_the_exit_codes_the_binary_returns` formats `EXIT_BAD_INPUT` and
  `EXIT_CORRUPT` into the two clauses of the exit-code line and requires
  `exit_codes!()` to contain them, which ties the literal's `2` and `3` to the
  constants the process actually returns; `tests/cli.rs` checks each screen
  against each exit one case at a time and could not see the pairing itself.
- `arb_num` in `tests/roundtrip.rs` generates the `f64` boundaries its doc has
  claimed since `9e1e1f5` — `1.7976931348623157e308` and its negative with an
  upper-case `E+`, `2.2250738585072014e-308`, `5e-324`, `-0.0` and `1e309`, the
  last a valid JSON number no `f64` holds — spelled as literals because the property
  under test is that the *lexeme* survives, and `f64::MAX.to_string()` is a 309-digit
  integer no producer emits. The generator had integers at the `i64`/`u64` edges and
  a 100-digit literal, and no `f64` edge at all. All roundtrip properties pass.
- **`leb128_overflow_rejected` matched three ten-byte length fields on `Corrupt { .. }`
  and never read the offset**, so the documented rule for which byte a varint fault
  names — the one the `Corrupt` correction got wrong — was pinned by nothing. It now
  derives the tenth byte (19) and one-past-the-field (20) from `ORIGINAL_LEN_OFFSET`
  and `ULEB128_MAX_BYTES` and asserts each fixture's offset: nine `0x80` bytes then
  `0x02`, `0x82`, `0x00`, and ten `0xFF`, all at 19; nine then `0x80` or `0x81` at
  20; and nine then `0x01` decoding to 2^63 with no fault at all, because a
  well-formed ten-byte length is `decompress`'s complaint, not the header's.
- **The `--archive` help's "a header of 43 to 46 bytes" is pinned.**
  `compress_help_header_size_is_the_measured_ladder`
  in `tokfold-cli/tests/cli.rs` requires the phrase on the flattened screen and
  derives the figure rather than trusting it: `Header::encode_into` is measured at
  every step of the varint ladder — 43 bytes at 0 and 127, 44 at 128 and 16 383, 45 at
  16 384 and 2 097 151, 46 at 2 097 152 and at 16 MiB, 47 at 2^28 — and the top of the
  ladder on the binary itself, where an original of exactly 16 MiB, the largest
  `compress` accepts, leaves an `--archive` file 46 bytes longer than the original.
  The fixture is 16 MiB of spaces ending in a digit, because a rejected input passes
  through with no archive and a 16 MiB string costs the debug binary half a minute.
- **Of every `Corrupt` offset the documentation publishes, the one no test held was
  the ten-byte length field that decodes.** The rustdoc on `DecompressError::Corrupt`
  and the CLI README's section on `archive corrupted at byte N` both
  say that nine continuation bytes followed by `0x01` are not a varint fault: the
  value 2^63 decodes, and the refusal comes from `decompress`'s length gate, at the
  field's own offset. Nothing asserted it. `leb128_overflow_rejected` builds that
  exact field but stops at `Header::decode`, which succeeds and therefore never
  reaches the gate, and the framing test reaches the same offset by truncating and by
  appending — neither of which widens the field.
  `a_well_formed_ten_byte_length_is_refused_at_the_length_field` in `compressor.rs`
  splices the ten bytes into a real archive and asserts both halves: first that the
  header decodes to 2^63, so the refusal cannot be coming from the varint layer, then
  that `decompress` returns `Corrupt` at `ORIGINAL_LEN_OFFSET`. Its fixture is
  deliberately under 128 bytes, so the field it replaces is one byte wide, and it
  holds that property with an assertion rather than trusting the arithmetic.

- **`tokfold expand` is now held to the offsets the CLI README quotes.**
  `expand_names_the_corrupt_offsets_the_readme_quotes` compresses a 13-byte document
  with `--archive`, splices each fixture into the real archive and asserts exit `3`,
  empty stdout and a stderr line ending `archive corrupted at byte N`: cut to 20
  bytes at 11, cut to 43 at 10, version `0` at 4, nine `0x80` then `0x82` at 19, nine
  `0x80` then `0x81` at 20, and ten `0xFF` at 19. A reworded `Corrupt` message fails
  it; before it, every test matched the exit code or the variant.
- **`format.rs` matched three header faults on `Corrupt { .. }` or `is_err()` and
  never read the offset.** `truncation_rejected_at_every_header_boundary` now states
  the offset for every prefix length — nothing under four bytes, 4 and 5 at
  themselves, 6 and 7 at 6, 8 and 9 at 8, 10 at 10, and every checksum cut at 11 —
  and asserts each; `overlong_leb128_rejected` asserts 11 and `version_zero_rejected`
  asserts 4. A new test,
  `truncation_behind_a_wide_length_field_is_reported_where_the_field_ends`, holds the
  two cases a one-byte length cannot reach: a checksum cut behind a ten-byte length at
  20, and a length field that ends mid-varint at the byte it lacks, 12.
- **MCP: `json_with_nothing_to_save_comes_back_verbatim_behind_the_raw_marker`**
  holds the half of the `initialize` promise the existing test did not reach: valid
  JSON E1 cannot shrink comes back `compressed: true` with the input behind the raw
  marker, and the instructions say so and no longer say "returned unchanged".
- **The E2 tab-layout survey pins which shapes give back more than half, not only
  how many.** It asserts that every one is an all-string table and, family by
  family, the full set of surveyed widths at which some height reaches it — dates and
  short runs from three fields up, one-letter strings from five — the description the
  module doc gives.
- **The 600 bps bar test pins the rows that do not move as well as the ones that
  do.** It asserts that E1 is still chosen at the bar for flat objects of 200, 1 000 and
  5 000 keys, tables of 10, 200 and 2 000 rows, and nesting 8, 16 and 30 deep —
  under the minify-only `Conservative` profile the test runs; under the default
  `Balanced` profile those tables go to E2.
- **No test read the whole message of any engine error.** The CLI prints a
  compression error after "passing input through uncompressed:" (a `compress`
  that passed its input through), inside "the input was rejected (…) and not passed
  through" (one that could not) or after "cannot compute stats:" (`stats`) and a decompression error after "cannot expand
  <source>:", and the MCP server returns either as a reason, but
  tests matched the variant, so a message could drop or swap its numbers with every
  test green. Every `CompressError` and `DecompressError` message is now pinned in
  full on an input the engine really rejects, and the CLI test pins two of them as
  printed, including the default depth limit ("nesting depth 513 exceeds limit 512").
- **`tokfold expand`'s printed offset is now pinned for the encoder id (5), the
  tokenizer id (6), a named flag bit (8) and an overlong length (11)**, alongside the
  offsets already held.
- **The encoder-pairing contract test never saw E1.** Its inputs reached the tabular
  and passthrough encoders only, so the name and wire id of minify could disagree
  unnoticed. It now drives all three and asserts the set it saw.
- **The modern-envelope test covered one handler.** Each handler adds the envelope
  itself, so it now also covers `initialize`, `server/discover`, `tools/list` and
  both tools, a tool error included.
- **The MCP server's `encoder_name` had no direct test** (the CLI's had one in
  0.0.1); each id this build names now maps to its frozen name.
- **The single-bit-flip test drove a test-only decoder.** It proved the format admits
  no silent flip, not that `Compressor::decompress` rejects one, which was checked at
  the last byte only. A new test flips every bit of a real archive through the public
  entry point.
- A read-only standard output is covered by CLI tests (Unix): a rejected
  `compress` with and without `--archive`, a successful `compress`, `expand`,
  `--help`, `--version`, `compress --help` and an `mcp` session. The rejected pass
  with `--archive` asserts that neither line claiming the input reached standard
  output is printed, so printing "no recovery archive written" ahead of the write
  fails a test.
- The rejected-pass announcements are pinned in full: an empty archive slot is named
  by its path, and a file at `--archive` shorter than the magic (zero or three bytes)
  is reported as a file that is not a tokfold archive, left untouched, not as an empty slot.
- **A standard input that cannot be read is covered by a CLI test (Unix)** for
  `compress --archive`, `expand`, `stats` and `mcp`: each exits `2`, prints nothing on
  standard output, names the operating system's error, and a `compress` leaves the
  archive already at `--archive` unchanged.
- **No test told the three MCP `profile` strings apart.** A call that mapped
  `aggressive` or `conservative` to the wrong profile passed every test. Each string
  is now checked against the profile the argument parser returns for it, and each
  also selects the encoder its profile allows on the same four-row table:
  passthrough for `conservative`, tabular for the other two. The parser check is
  the one that holds `aggressive`: in this release it enables exactly the encoders
  `balanced` does, so no output can tell the two apart.
- **The MCP `profile` fallback test compared outputs no profile changes.** Its
  input, `{"a":1}`, is passed through under every profile, so mapping an absent or
  `null` profile to `aggressive` or to `conservative` passed it. The parser is now
  checked directly for no arguments, an empty object and `"profile": null`, and the
  call is checked on a table that `conservative` compresses differently.
- **No MCP test read a decompression failure's message.** The tests checked `code`
  only, so replacing the message with a constant passed them and would have dropped
  the byte offset. Each deliberate kind of damage now asserts the engine's own
  message, and a header cut after its length field asserts
  `archive corrupted at byte 11`.
- **The MCP flipped-archive test flipped one bit, in the last byte.** It now flips every bit of the
  archive and asserts, for each, an error result that carries no `text` and does not
  contain the original document.
- The request-id test now covers a `tools/call` reply and an error reply as well as a
  result, and the stdio test that every written line is one JSON message now also
  asserts how many lines were written.
- The same-file guard is held by four new CLI tests: a missing input and a dangling
  symlink named as their own archive, a read-only `/dev/stdin` archive spelling, and
  a write-only `/dev/stdin` or `/dev/stdout` input spelling. Each asserts the exit
  code, the first words of stderr and that the file behind the descriptor is
  unchanged; the descriptor cases assert the macOS behaviour on macOS and the refusal
  elsewhere.
- **The trailing-separator spelling is asserted on its own input.** The spelling test
  now asserts that a successful pass is refused and a rejected one leaves the archive
  intact, and a new test drives `--input x --archive x/`, `--archive x/ < x` and
  `--archive x/ >> x`, asserting exit `0`, the file's bytes unchanged and no removal
  line. Both fail with the typed-path check removed.
- **The descriptor-slot check is asserted to read from offset `0`** (macOS): a stdin
  standing just past an archive's magic is refused as a stale archive, and its offset
  is still `4` afterwards. It fails with a plain `read_exact` in place of the
  positional read. The unopenable-archive test's always-running half is now a
  self-referencing symlink (`ELOOP`), since `ENOTDIR` on the typed path is an empty
  slot.
- **The launcher's `SIGUSR1` fallback is asserted eight times per run**, each for exit
  `128 + N`, no signal and an empty stderr — the debugger line was a race, and one run
  missed it.
- **The launcher's signal tests cover every relayed signal.** The re-raise and
  the forward-to-the-binary loops ran `SIGTERM` and `SIGINT` only; `SIGHUP` and
  `SIGQUIT` are relayed by the same list and are now run too, and `SIGUSR1`, which
  the launcher also relays and whose exit differs, has tests of its own. A new test makes
  `child.kill` fail with `EPERM` through the preload and asserts the launcher outlives
  the refusal and then dies of the binary's own signal; it fails against the previous
  `error` handler. A macOS-only test pins the `SIGEMT` exit `0` described under
  Documentation. Each launcher run through `spawnSync` is now bounded at 30 seconds,
  so a hang fails one test instead of stalling the suite, and the preload's
  `TOKFOLD_TEST_LIBC` legend says that any value but `musl` or `absent` means glibc.
- **The descriptor archive slot is asserted the way the write reaches it** (macOS).
  A stale archive behind a read-only `/dev/stdin` passes the input through at exit
  `0`, with the archive unchanged; a directory behind `/dev/fd/3` exits `2` with `Is
  a directory` and nothing on stdout; the offset test opens its archive read-write,
  since a read-only one is now an empty slot. The pipe and `/dev/null` tests assert
  `it is not a regular file`. Each fails with its branch removed or the message
  reverted.
- **The launcher's `SIGUSR1` relay is asserted**: a `SIGUSR1` to the launcher's pid
  alone ends the binary, the launcher exits `128 + N`, and stderr carries no
  `Debugger` line. It fails with `SIGUSR1` dropped from the relayed list. The
  `EPERM` test now asserts the relay was attempted — the preload announces each
  refusal on stderr — so a launcher that relayed nothing no longer passes it.
- Two tests were renamed to what they check: `conservative_profile_keeps_the_tabular_encoder_out`
  and `a_ping_result_carries_the_envelope_under_its_named_keys`.

- **MCP: `an_archive_of_non_utf8_bytes_is_refused_as_not_utf8`** builds a valid
  archive over `FF FE`, checks the engine restores it, and asserts the `decompress`
  tool answers `isError` with code `not_utf8` and its message. No test reached that
  branch before.
- **MCP: `a_line_of_non_json_whitespace_is_a_parse_error_not_a_blank_line`** sends
  vertical tab, form feed, NBSP, NEL, U+2028 and U+3000 lines and asserts a `-32700`
  for each.
- **CLI: `a_short_file_behind_a_read_only_descriptor_is_reported_like_a_long_one`**
  (macOS) pins the `it cannot be opened for writing` line for a two-byte file, and
  the existing read-only-descriptor test now pins the same line where it matched
  only a substring of the old one.
- **npm: the `SIGUSR1` launcher tests assert a signal death.** A binary killed by
  `SIGUSR1` must leave the launcher dead of `SIGUSR1` with no exit status and an
  empty stderr, eight times per run; a `SIGUSR1` relayed to the launcher must end it
  the same way. With the listener left installed across the re-raise both fail.

- **CLI: the symlink-clearing test pins the whole warning** — the resolved target,
  the link, and that the link still exists after the pass.
- **MCP: the non-JSON-whitespace test pins the byte offset and the missing id** for
  each line (`" \u{b} "` at byte 1, `"\t \u{a0}"` at byte 2), where it asserted
  only the code. `a_blank_line_is_ignored` now includes a bare `"\n"` and a mixed
  `" \r\n\t\n "`.

- **A launcher test for a `SIGXFSZ` death**, the one signal path to `128 + N` the
  real binary can reach (a write past `ulimit -f`). It pins `128 + SIGXFSZ` (`153`
  on macOS) with no signal and no "cannot map" line; the `0.0.1` launcher fails it
  with `1`.
- **The CLI same-file and output-guard refusals are pinned whole.** Every test had
  matched a prefix only, so the clauses after it could change with every test
  green. New tests cover a symlink chain at `--archive` and a read-only input named
  as its own archive.

- **An id too wide for the addressed refusal is pinned.**
  `an_id_too_wide_for_the_addressed_refusal_is_refused_without_it` walks id widths 0
  to 120 under a 200-byte limit and asserts, for each, the exact frame length, whether
  the id is present, and that the frame fits; all three rungs (answer, addressed
  refusal, id-less refusal) are reached.

- **An E2 shape-hash collision is pinned** (64-bit targets other than sparc64 and
  wasm64): the minority key list wins the header and all six majority rows deviate,
  and a colliding two-element pair with no shared key list is tabulated where its
  control declines. When key text is compared on a hash hit, this test flips.
- **The sentinel's cost is pinned across leading whitespace**, and the first
  whitespace run that costs two tokens (82 cl100k, 80 o200k) and the exact one-token
  tail (83, 87, 91, 95, 128) are pinned where the tests had checked membership only.

## [0.0.1] - 2026-08-29

First version of the workspace, and the first one to leave the repository. There
is no upgrade path from anything earlier because there is nothing earlier.

Released on **npm**: `npm i -g tokfold`. All six packages — the launcher plus the
five prebuilt-binary packages — are live at `0.0.1`, published from CI with
provenance attestations.

Four of the platform packages went out on 2026-08-27; the fifth was refused. npm
answered `PUT tokfold-win32-x64` with "Package name triggered spam detection",
three times over two days, from the same token that had just published its four
siblings. The package is therefore named **`tokfold-windows-x64`**. The one exact
precedent is `git-cliff`, which hit this same refusal in 2023 and renamed the same
day rather than fight it. esbuild and turbo are *not* precedents for the unscoped
name — they publish `@esbuild/win32-x64` and `@turbo/windows-64`, both scoped, and
a scope is what actually sidesteps the classifier. Renaming was free only because
the launcher had not published yet; the platform table in
`npm/tokfold/lib/resolve.js` keys on Node's `win32-x64` and always did, so only the
name on the registry moved.

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
  `0` for all four estimators shipped here — the two in the default build and the
  two behind `tiktoken` — so the default behaviour is unchanged.
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
  unchanged and the command still exits `0`. (Unchanged in `0.0.1`; see
  [Unreleased] for the case that now exits `2`.)
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
  Linux/macOS/Windows and two MSRV legs. None of those gates hangs off a matrix
  value, so none can be switched off by a renamed key; the only condition any of
  them carries is one that skips the gate on the weekly cron. The three `test`
  legs on the branch-protection list are the exception, and the workflow says
  so: their `continue-on-error` is derived from `matrix.required`, so that key
  does still decide whether one of them can block. Weekly jobs re-run the suite
  under the release profile and on nightly, and re-scan a fresh advisory
  database with `cargo-audit`. Every
  invocation that resolves a dependency graph is `--locked`; every action and
  installed tool is pinned by commit SHA or version.
- Crate manifests carry `repository`, `homepage`, `documentation`, `readme`,
  `keywords`, `categories` and an explicit `include` allow list; each publishable
  crate carries its own README and licence texts. The `documentation` URLs point
  at docs.rs pages that do not exist yet — docs.rs builds them on first publish,
  and nothing has been published to crates.io.

### Not included

- No crates.io release. There is no `cargo add`, no crates.io page and no
  docs.rs page. Only npm distributes this version.
- Reserved but unimplemented: legend folding, Hugging Face tokenizer backends
  (the `hf` feature is a placeholder), language bindings, and the MCP *proxy*
  shape (upstream connection, content-addressed archive store, `retrieve` tool).
- No published benchmarks. Numbers will ship with a versioned public corpus and a
  reproducible harness, not before.

[Unreleased]: https://github.com/IvanBBaev/tokfold
[0.0.1]: https://github.com/IvanBBaev/tokfold
