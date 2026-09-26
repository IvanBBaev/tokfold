# tokfold-cli

The command-line interface of [tokfold](https://github.com/IvanBBaev/tokfold):
reversible, structure-aware context compression for LLM agents. This crate ships
the single `tokfold` binary; the engine lives in
[`tokfold-core`](https://github.com/IvanBBaev/tokfold/tree/main/crates/tokfold-core).

## Status

**v0.0.1 — skeleton under active development.** The command line is unstable and
will change without deprecation windows.

**On npm.** The binary is published as the `tokfold` package at `0.0.1`:

```sh
npm i -g tokfold
```

The launcher package carries no binary of its own; it declares one package per
platform (`tokfold-darwin-arm64`, `tokfold-darwin-x64`, `tokfold-linux-x64-gnu`,
`tokfold-linux-arm64-gnu`, `tokfold-windows-x64`) as optional dependencies, so an
install pulls down only the binary matching the machine. All six are published at
`0.0.1` with npm provenance attestations.

**Not on crates.io.** That publication is still on hold, so `cargo install
tokfold-cli` does not work, and there is no crates.io page and no docs.rs page yet.
The `documentation` field in `Cargo.toml` already points at
`https://docs.rs/tokfold-cli`, but that URL 404s: docs.rs builds the page on first
publish, which has not happened. To build from source instead:

```sh
git clone https://github.com/IvanBBaev/tokfold
cd tokfold
cargo build --release -p tokfold-cli   # binary at target/release/tokfold
```

## Subcommands

- `tokfold compress` — read input, emit the token-reduced rendering, optionally
  persist a recovery archive (`--archive PATH`). Compression is an optimization,
  never a gate: an input the engine rejects is forwarded unchanged and the command
  exits `0`. That path also clears a stale archive left at `--archive` by an earlier
  run, and only such an archive — see below. Two steps on that path can still fail
  the command with `2`: clearing the slot, when it cannot be inspected or
  cleared, in which case nothing is emitted; and writing the passthrough to
  standard output, on any I/O error other than a reader closing the pipe early.
- `tokfold expand` — reconstruct the exact original from a recovery archive.
  Fail-closed: any integrity error exits `3` and emits nothing. An empty archive is
  reported as empty rather than as corruption, and every failure names the file (or
  standard input) it read.
- `tokfold stats` — report what a compression pass would achieve, without emitting
  the payload. On the passthrough path the `after` figures are *set* equal to the
  `before` figures rather than measured, so a reported ratio of `1.0000` is a floor,
  not a measurement: the rendering still carries the 18-byte `raw` sentinel. Measure
  the rendering itself if you need to account for every token.
- `tokfold mcp` — EXPERIMENTAL. Serves the engine as Model Context Protocol tools
  (`tokfold_compress`, `tokfold_decompress`, `tokfold_estimate`) over stdio, one
  JSON-RPC message per line, until the client closes the stream. A warning goes to
  stderr first: the server is unhardened, unaudited, and not covered by the
  reversibility guarantees. Do not use it with production secrets. It went out in
  the `0.0.1` npm release in exactly that state — labelled experimental at every
  layer a user can meet rather than withheld — so treat "experimental" as a current
  description, not a plan.

Exit codes are normative: `0` success, `2` bad input (usage, I/O, or an input the
compressor rejects on `stats`), `3` a corrupt, empty, or otherwise unrecoverable
archive on `expand`. A downstream that closes the pipe early (`… | head`) is treated
as a clean exit. So is a standard output that was already closed when the process
started (`tokfold compress >&-`): Rust reopens a closed descriptor 0, 1 or 2 on
`/dev/null` before `main` runs, so the payload is written there, nothing reports it,
and the exit is `0` — with `--archive` the archive is still written. A standard
output that is open but not writable (`tokfold compress 1<file`) is not that case:
on Unix the write fails and the exit is `2`, for every subcommand that has
something to write and for `--help` and `--version` written anywhere but a
terminal. A `compress` or `expand` whose payload is zero bytes writes nothing and
exits `0` even then. The same holds for input: a
standard input that is open but not readable (`tokfold compress 0>>file`) is a read
failure and exits `2` on Unix, not an empty document. A standard input that was
already *closed* when the process started (`tokfold compress 0<&-`) is different: it
has been reopened on `/dev/null` the same way, so it reads as an empty document,
which the compressor rejects — and a rejected pass with `--archive` clears a stale
archive at that path, exactly as `< /dev/null` would. A rejected `compress` announces
its passthrough on stderr only once the passthrough write has returned, so a run
that exits `2` never claims one; a reader that closed the pipe early counts as a
returned write, so behind `| head -c1` the announcement still says the input was
passed through, although the reader took only part of it.

Those are the codes the process *chooses*. A process killed by a signal returns none
of them, and tokfold cannot make it otherwise: under an `RLIMIT_FSIZE` low enough to
cut a file write short — the archive, or a standard output redirected to a file — the
binary dies on `SIGXFSZ` with an empty standard error, and a shell reports `153`,
the npm launcher `128 + N` for the same reason. That is the default disposition of
`SIGXFSZ`, and a process inherits whatever disposition its parent left: under a
parent that ignores the signal (`trap '' XFSZ` in a shell, or Python's
`os.system`, which runs its command with the interpreter's own ignored `SIGXFSZ`) the
same write fails with `File too large` instead, and tokfold reports it and exits `2`,
leaving the partly written file behind. A caller that treats any code outside
`0`/`2`/`3` as "tokfold misbehaved" will misread the signal death; treat it as what it
is, the operating system stopping the process before it could report anything.

`mcp` exits `0` when the client hangs up — on clean end-of-input and on a broken
pipe alike — and `2` when its stdio fails any other way, which on Unix includes a
standard input it cannot read and a standard output it cannot write. It also exits
`2` before reading anything when its standard output is a regular file it would
be writing into the same file its standard input reads from (`tokfold mcp <
requests >> requests`). Earlier in development this subcommand was an unimplemented
stub that always exited `69`.

## The size ceiling is 16 MiB, and reaching it is quiet

The engine refuses an input larger than **16 MiB** — 16 777 216 bytes, inclusive:
that exact size is accepted and one byte more is not. The binary exposes no flag to
raise it; only the library does, through `ConfigBuilder::max_input_bytes`. It also
refuses JSON nested deeper than **512** levels: 512 is accepted and 513 is not, and
again only the library can raise it, through `ConfigBuilder::max_depth`.

Neither refusal is loud. `compress` forwards the input unchanged, exits `0` and
explains itself on standard error, so a caller that pipes stdout onward and drops
stderr sees a successful run that saved nothing; if `--archive` was given, no
archive is written and that is reported on stderr too. `stats` exits `2` with the
same explanation. When a large payload seems not to compress at all, check its size
against this ceiling before looking anywhere else.

## Reading `archive corrupted at byte N`

`expand` reports a rejected archive by the offset at which decoding stopped, counted
from the first byte of the file. Every offset it can name is a **header** position,
never a payload byte, so the number is only useful next to the layout:

| Offset | Bytes | Field |
|---|---|---|
| 0 | 4 | magic, `TKFD` |
| 4 | 1 | format version (`1`) |
| 5 | 1 | encoder id (`0`, passthrough) |
| 6 | 2 | tokenizer id, little-endian (`0`) |
| 8 | 2 | flags, little-endian (`0`) |
| 10 | 1–10 | original length, ULEB128 |
| after the length | 32 | `SHA-256` of the original |
| after the checksum | rest | payload |

An archive of 4 to 9 bytes is cut short at or inside a fixed field and is reported
at the field it cuts short (4, 5, 6 or 8); one shorter than the 4-byte magic is reported as a
bad magic. Otherwise `archive corrupted at byte 8` is a flag bit this build knows
and never sets, not a damaged payload; byte 4 is a version `0`, and a *higher* version
reports `format version N > supported 1` instead; byte 5 or 6 is a metadata field
that disagrees with what `0.0.1` emits; byte 10 is the length field disagreeing with
the bytes that follow it, which is what a copy cut short in the payload looks like;
byte 11 to 20 is a copy cut short inside the checksum, reported where the checksum
should begin (cut the archive of `{"k":"hello"}` to 20 bytes and it says byte 11; to
43 and it says byte 10); and byte 10 to 19 is also where a length field mis-spelled
at that byte is reported — a byte missing, an overlong spelling, or one that would
overflow — with byte 20, one past the field, when the tenth byte is `0x80` or `0x81`,
the only two values that still ask for an eleventh byte without overflowing (overflow
is checked first: nine `0x80` bytes and a `0x82` say byte 19, nine and a `0x81` say
byte 20). An offset at or past the end of the file means the decoder needed a byte
the archive does not have — the value is a position in the format, not an index into
your file.

Three neighbouring failures are deliberately *not* this one. `not a recognized
payload (bad magic)` means the file is not a tokfold archive at all; `reserved header
bits are not zero` means a flag bit this build has no meaning for is set, which a
later format version might write and this one refuses rather than ignores; and
`integrity checksum mismatch` means the payload was exactly the recorded length and
still did not hash to the recorded digest.

## Not a security boundary

tokfold is **not** a prompt-injection filter and must not be placed in a threat
model as one. It does not inspect, sanitize, score or neutralize adversarial
content; it preserves content faithfully, hostile content included, because
faithful preservation is the entire point.

**A recovery archive is not a protective wrapper.** Every archive `--archive`
writes at v0.0.1 is a passthrough blob: the header above followed by the original
bytes verbatim — not encrypted, not encoded, not obfuscated. An archive is exactly
as sensitive as the input it came from, so choose its path with the same care you
would choose one for the input itself. It is not smaller than the input either: it
is the input plus a header of 43 to 46 bytes, so `--archive` puts the input's own
size on disk once more rather than saving anything there. The saving is in tokens,
on stdout.

## The `--archive` path belongs to tokfold

`--archive PATH` is written destructively, and how far that goes depends on the
outcome of the pass:

- A successful pass **overwrites** what is at `PATH` when it can: it opens the file
  for writing, so it replaces a document of any kind, but it refuses a path that is
  not a regular file or a directory, and it fails with exit `2` on a file it lacks
  write permission for. It also refuses, with exit `2`, a path whose opening does not
  empty it: on macOS `/dev/fd/N`, `/dev/stdin`, `/dev/stdout` and `/dev/stderr` hand back a
  duplicate of a descriptor that is already open, keeping its offset and its append
  mode, so the archive would land after whatever that file already held. It is not unconditional, and it is not a superset of what the
  rejected pass does to the same path — see "The rejected pass is not contained by
  the successful one" below.
  A pass the *passthrough encoder* wins is a successful pass and writes an archive
  like any other: the engine accepted the input and merely found no encoder that
  paid, so the archive it writes still reconstructs the document. Only an input the
  engine **rejects** writes none. The two read alike on stdout — both emit bytes
  that round-trip — but they differ at `PATH`, and `tokfold stats` tells them apart:
  the first prints `encoder: passthrough (id 0)`, the second exits `2`.
- A pass the engine rejects **deletes** what is at `PATH` only when it opens with the
  `TKFD` archive magic, and says so on standard error. That deletion is what keeps
  the archive honest: a rejected input is not a failure, so an archive left over
  from an *earlier*, successful run would still verify its own checksum,
  and a later `tokfold expand` would hand back a completely different document with
  no signal that it was stale. A damaged or truncated archive is deleted too: four
  bytes are all it takes for `expand` to *start* reading a file as a document, even
  though reading one back to the end takes a header of at least 43 bytes and a matching
  checksum. Forty-three is a floor, not a size: the length in the table above is a
  ULEB128, so the header gains a byte for every 7 bits the original grows — 43 bytes
  while the original is under 128, 44 under 16 384, 45 under 2 097 152, and 46 from
  there up to this binary's 16 MiB input ceiling, which is where the ladder ends. The
  field can occupy ten bytes, but the tenth needs an original of 2^63 bytes, and no path
  produces one: an embedder who raises the engine's `max_input_bytes` stops at 47,
  because the parser refuses anything past `u32::MAX`. Subtracting the input size from
  the archive size measures it — 43, 44, 45, 46 and 46 for originals of 127, 128,
  2 097 151, 2 097 152 and 16 777 216 bytes. The magic is a necessary condition for the
  hazard, not a sufficient one, and the check is over-inclusive in that direction on
  purpose.
- A pass the engine rejects **leaves any other readable file alone**, byte for byte,
  and reports that on standard error instead. "Readable" is load-bearing: a file the
  slot check cannot open at all — no read permission, an unsearchable parent — is
  equally untouched, but there is no report and no passthrough either, because the
  check fails before the payload is written and the command exits `2` (see the last
  bullet). A file whose opening bytes are not the
  magic, or that is too short to carry it, is not tokfold's to delete: `compress
  --archive notes.md` on a rejected input does not touch `notes.md`. No stale
  document survives that restraint, because `expand` fails closed with exit `3` on
  anything that is not an archive. A FIFO, a socket or a device is left alone without
  even being opened — stderr says it is not a regular file, not that it is not an
  archive, since nothing read it: it cannot hold bytes a later `expand` would read
  back, and opening a FIFO would block until a writer appeared. A directory is the one
  exception — it is opened on purpose, so the read fails with the platform's own
  diagnostic and the command exits `2`, rather than reporting nothing to clear at a
  path that plainly is not empty.
- Both the overwrite and the deletion follow symlinks to the **target**, because a
  successful pass writes through the link and it is the target that would go stale.
  A rejected pass therefore clears the archive behind the link and leaves the link
  itself (every link of a chain survives; the warning names the file that went, as
  the one the typed link *resolves to*); it re-checks that the file it is about to unlink is still the one it
  inspected, and refuses rather than deletes if something swapped it in between.
  That narrows the window, it does not close it: nothing here is atomic.
- If the path cannot even be read, or the deletion itself fails, nothing is emitted
  and the command exits `2` — the same rule as a failed archive write. The context
  then reads `the input was rejected (…) and not passed through`; `… and passing it
  through failed` is kept for a failed write to stdout, which may have emitted part
  of the input first.
- On macOS a `/dev/fd/N`, `/dev/stdin`, `/dev/stdout` or `/dev/stderr` spelling names
  a descriptor, not a file, so there is nothing to unlink (measured on macOS only;
  where the spelling resolves to the file behind it, that file is cleared like any
  other). A rejected pass inspects what is behind it without removing anything, and
  reads it at offset `0` without moving the offset the caller shares with it. It first
  asks whether the archive write could reach the file: a file behind a read-only
  descriptor (`--archive /dev/stdin < a.tkfd`) cannot be written by a successful pass
  either, so no archive of this run's would have stood there, and whatever it holds is
  left as it is, reported on stderr as not openable for writing, and the input is
  passed through. Behind a descriptor the write reaches, a file shorter than the magic
  — the one `--archive /dev/fd/3 3>fresh.tkfd` has just emptied included — or one that
  is readable and does not open with the magic is left as it is, reported as not a
  tokfold archive, and the input is passed through. A file that does carry the magic
  is refused with exit `2` and no output, because it would survive and still verify.
  A write-only descriptor (`3>>log`) on a file of four bytes or more cannot be read to
  find out, and exits `2` too, whatever the file holds. A pipe, socket or device is
  not read at all, and stderr says it is not a regular file; a directory exits `2`, as
  above.
- **The rejected pass is not contained by the successful one.** It is tempting to
  reason that deleting is safe because a successful pass would have overwritten the
  same path anyway. That is not true, because the two use different system calls with
  different requirements. Overwriting opens the file and needs write permission on
  *the file*; deleting needs write permission on *the directory*. So a `chmod 444`
  archive in a writable directory survives every successful pass with exit `2` and is
  deleted by the first rejected one at exit `0` — and, the mirror case, an archive in
  a read-only directory is rewritten in place by a successful pass while a rejected
  one cannot unlink it and exits `2` with the archive intact. The path itself is read the same way
  by both: `--archive a.tkfd/` names no file either pass can reach, so a successful
  pass is refused and a rejected one deletes nothing. The deletion is still the right
  default — an archive that outlives its run is a *silent* wrong answer, two clean
  exits and a different document, where this is a loud one that names the file on
  standard error — but if you keep an archive you cannot afford to lose, keep it
  somewhere `--archive` does not point.
- **The write is not atomic.** A successful pass truncates the path and then writes,
  so an overwrite that fails partway — a full volume, an `RLIMIT_FSIZE` — leaves a
  truncated or empty file where the previous run's good archive was, and that archive
  is not recoverable. `expand` fails closed on the remains (exit `3`, never a
  best-effort document), so this costs availability rather than correctness, but a
  slot you overwrite is not a slot you can treat as a backup.
- **The deletion removes a name, not the file.** A rejected pass unlinks the path it
  was given. A stale archive with a second hard link elsewhere survives under that
  other name and still verifies, so do not keep hard links to an `--archive` slot.
- **A successful pass writes only to a regular file.** `--archive /dev/null`,
  `--archive >(gzip > a.gz)` and a named FIFO are refused with exit `2` and nothing
  on standard output — the FIFO would otherwise block until something reads it.
  Write the archive to a file and pipe it from there.

Because both outcomes are destructive to that path, `--archive` may not name the
same file as `--input`. tokfold refuses such an invocation with exit `2`
before reading or writing anything — which is before it can know whether the pass
would succeed, so an input that would be rejected and left untouched (one that does
not open with the archive magic) is refused too, where `0.0.1` passed it through
with exit `0`. The refusal is a rule about the two paths, not a prediction of the
outcome, and its message says only that: it is also given where neither outcome could
have touched the file — a read-only (`chmod 444`) input, which `0.0.1` refused to
overwrite with "Permission denied" and, when it opened with the magic and was
rejected, passed through with exit `0`; and on macOS a non-empty input reached through
a `/dev/fd/N` or `/dev/stdin` spelling, which a successful pass refuses to write
because opening it does not empty it. The two are compared by the identity the
operating system gives the files (device and inode on Unix), so a symlink, a
relative spelling and a second hard link to the same file are all caught: `--input
./log.json --archive log.json` is refused, and so is `--archive` naming a hard link
to the input. Where no such identity is available the comparison falls back to the
two paths with symlinks and relative spellings resolved, which still catches the
spellings but no longer catches a hard link. On Unix, an input redirected onto
standard input is compared the same way, by the identity of the file behind the
descriptor, so `tokfold compress --archive log.json < log.json` is refused too;
elsewhere standard input has neither an identity nor a path to compare, so a
redirect is not refused, any more than a pipe is. What the guard
cannot see is a pipe: `cat log.json | tokfold compress --archive log.json` carries
the bytes of `log.json` and no file to compare, so it is not refused — a successful
pass then replaces `log.json` with its archive, which `tokfold expand` turns back into
the document, and a rejected one leaves it untouched unless it opens with the archive
magic, in which case it *deletes* it. Name the input with `--input` or redirect it;
do not pipe it to the path it lives at.

Standard output is guarded the same way on Unix: `tokfold compress --archive
a.tkfd >> a.tkfd` (or `> a.tkfd`) is refused with exit `2` before tokfold reads or
writes anything, because a successful pass would write the archive and the rendering
into one file (on macOS, through a `/dev/stdout` or `/dev/fd/N` spelling of a
non-empty file, it would refuse instead, and is refused all the same) and a rejected one would delete the file its own passthrough is being
appended to whenever that file opens with the archive magic. A rejected input over a
file without the magic would only have been appended, and is refused all the same:
the check runs before tokfold knows which outcome it will have. Elsewhere that redirect is not refused. A `>` redirect is truncated by
the shell before tokfold starts, so the refusal cannot bring back what `>` removed;
it saves a `>>` or `1<>` target, and it stops the run before it can do more.

The identities are those of the files behind the descriptors, so a `/dev/stdin`,
`/dev/stdout` or `/dev/fd/N` spelling of a redirected descriptor is the same file as
the redirect: `--input /dev/stdin --archive log.json < log.json` and `--archive
/dev/stdout >> a.tkfd` are refused like their plain spellings. Standard error
redirected into the archive path is not compared. Only regular files are compared,
because only a regular file can be destroyed: `--input /dev/null --archive
/dev/null` names one device twice and is not refused, a standard input opened
write-only on the archive (`0>> a.tkfd`) fails as the unreadable input it is, and a
standard output opened read-only fails at its first write. For the same reason each
side is asked the way its own operation reaches it — the input by a read, the archive
by a write — and on macOS, where opening a `/dev` spelling duplicates the descriptor
behind it, a spelling that operation cannot open is not compared: `--archive
/dev/stdin` over a read-only standard input passes a rejected input through, and
`--input /dev/stdin` over a write-only one fails as an unreadable input.

Every command also refuses, on Unix and with exit `2`, a standard output redirected
into its own input — `tokfold compress -i log.json >> log.json`, `tokfold expand <
a.tkfd >> a.tkfd`, `tokfold stats`, and `tokfold mcp < requests >> requests`, which
would otherwise read its own replies back as requests and answer them without end.
The check runs before anything is read, so it is given even where the run would have
written nothing: `tokfold expand < bad.tkfd >> bad.tkfd` over a corrupt archive exits
`2` with this refusal where `0.0.1` exited `3` with the decoding error, and `tokfold
stats < notes.txt >> notes.txt` over non-JSON is refused where `0.0.1` failed to
compute stats (exit `2` both times). The same `>` caveat applies: `tokfold compress -i log.json > log.json` has lost
`log.json` before tokfold runs, and the refusal only keeps a rejected empty input
from also deleting the archive at `--archive`.

## Licence

Dual-licensed under either of MIT ([LICENSE-MIT](https://github.com/IvanBBaev/tokfold/blob/main/LICENSE-MIT)) or Apache
License 2.0 ([LICENSE-APACHE](https://github.com/IvanBBaev/tokfold/blob/main/LICENSE-APACHE)) at your option. Both texts
also travel inside the published tarball, next to this file.

Minimum supported Rust version: 1.85 (edition 2024).
