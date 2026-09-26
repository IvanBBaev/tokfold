# tokfold

Reversible, structure-aware context compression for LLM agents — the `tokfold`
command-line tool.

This package ships a prebuilt native binary. It is the command-line front end to
an embeddable Rust engine that compresses agent context (tool output, JSON, logs)
into a denser rendering a model reads, plus a recovery archive that reconstructs
the original on demand.

```
npm install -g tokfold
tokfold compress --input big.json --archive big.tkfd
tokfold expand   --input big.tkfd
tokfold stats    --input big.json
```

## Status — read this before using it

**v0.0.1. A skeleton under active development.** The interface is unstable and
will change without deprecation windows. There are no published benchmarks, and
the README in the repository explains at length why there are none yet.

The `tokfold mcp` subcommand starts an **experimental, unhardened, unaudited** MCP
stdio server. It sees everything passed through it. Do not put production secrets
through it.

## What "reversible" means here

Semantic identity, not byte identity. Decompressing an archive reproduces a
document equal to the original *as a value tree*. Object key order, duplicate keys,
array order and number lexemes are preserved byte-for-byte; insignificant
whitespace and string escape style are canonicalised, because reproducing the
exact whitespace would mean preserving the very bytes we are paid to delete.

That is the floor, not this version's behaviour. `0.0.1` stores the original bytes
verbatim inside the archive, so `tokfold expand` gives them back byte-for-byte
today. A later encoder may store structure instead and return different whitespace
without breaking the contract, so do not diff the reconstruction against the
original — compare it as a value tree.

The word "lossless" is avoided on purpose. The model does not read the
reconstruction — it reads the compressed rendering — so byte-reversibility of the
recovery path proves nothing about comprehension of the read path. Those are two
different claims and only the first one is proven today.

## Not a security boundary

tokfold is **not** a prompt-injection filter and must not be placed in a threat
model as one. It does not inspect, sanitize, score or neutralize adversarial
content; it preserves content faithfully, hostile content included, because
faithful preservation is the entire point.

**A recovery archive is not a protective wrapper.** At v0.0.1 every archive is a
passthrough blob: a header of 43 to 46 bytes followed by the original bytes verbatim
— not encrypted, not encoded, not obfuscated. An archive is exactly as sensitive as
its plaintext.

## The `--archive` path belongs to tokfold

`compress --archive PATH` writes that path destructively. A successful pass overwrites
what is there when it can — a regular file whatever it contains; a FIFO, a socket or a device is refused,
and so, on macOS, is a `/dev/fd/N`, `/dev/stdin`, `/dev/stdout` or `/dev/stderr` path whose opening
does not empty the file behind it; a path it cannot write exits `2`. A pass whose input the engine rejects — which
still forwards the input unchanged and exits `0` — deletes what is there when the file
opens with the `TKFD` archive magic, and says so on stderr: an archive left by an
earlier run would still verify its own checksum, so a later `expand` would hand back a
different document with nothing to signal that it was stale. Any other file is left byte
for byte as it was and reported instead, because only a file carrying the magic can be
expanded at all. On macOS a `/dev/fd/N` spelling names a descriptor that cannot be unlinked:
a file behind a read-only descriptor, which a successful pass could not write either,
is left and reported whatever it holds, while an archive behind a descriptor the write
can open is refused with exit `2` rather than kept — and so is a file of four bytes or
more behind a write-only one, which cannot be read to tell.

Through this npm wrapper on macOS, only descriptors `0`, `1` and `2` reach the binary:
Node starts it with every other descriptor closed, so `--input /dev/fd/3` or
`--archive /dev/fd/3` fails with `Bad file descriptor` and exit `2`. Measured on
macOS with Node v22.23.2, for this wrapper and the published `0.0.1` one alike; on
Linux (aarch64, glibc 2.39, Node v22.23.2 and v18.20.8) this wrapper's binary
likewise found descriptors `3` and `4` closed. Run the binary directly to name a
higher descriptor.

Two steps on the rejected path can still fail the command. If the path cannot be
inspected or cleared — the deletion fails, or the file was replaced between the check
and the removal — nothing is written to standard output and the
exit is `2`. If the slot is cleared but standard output cannot be written, the exit is
`2` as well, and stderr says that passing the input through failed — part of it may
already have been written.

`--archive` may not name the same file as `--input`. That is refused with exit `2`
before tokfold reads or writes anything, and the two are compared by the file's identity
rather than its spelling, so a relative spelling and a symlink are caught. On Linux
and macOS a second hard link to the same file is caught too, and so is the input
itself redirected onto standard input: `tokfold compress --archive log.json <
log.json` is refused the same way. On Windows neither is: the comparison falls back
to the resolved paths, and a redirect is not refused, any more than a pipe is. A
pipe carries bytes and no file, so `cat log.json | tokfold compress --archive
log.json` is not refused: a successful pass then replaces `log.json` with its
archive, which `tokfold expand` turns back into the document; a rejected one leaves
it untouched unless it opens with the `TKFD` magic, in which case it *deletes* it.
Name the input with `--input` or redirect it — do not pipe it to the path it lives
at. On Linux and macOS standard output redirected into the archive path
(`tokfold compress --archive a.tkfd >> a.tkfd`) is refused with exit `2` as well,
and so is a `/dev/stdin`, `/dev/stdout` or `/dev/fd/N` spelling of a redirected
descriptor that the archive write can open (on macOS, one the descriptor's own mode
allows). On Linux and macOS every command likewise refuses a standard output
redirected into its own input (`tokfold compress -i log.json >> log.json`, `tokfold
mcp < requests >> requests`, or the same with `1<>`). A `>` redirect empties its file before tokfold starts, so these refusals
save a `>>` target and the rest of the run, not what `>` has already removed.
Standard error redirected into the archive path is not compared.

## The size ceiling is 16 MiB

The engine refuses an input larger than 16 MiB (16 777 216 bytes, inclusive), and
the binary has no flag to raise it. JSON nested deeper than 512 levels is refused
the same way. `compress` then forwards the input unchanged
and exits `0`, writing no archive and explaining itself on standard error; `stats`
exits `2`. So a payload that seems not to compress at all is worth measuring
against this ceiling first — the refusal is reported, but only on stderr.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | success — including a downstream that closed the pipe early; see below for the one signal death that also reads as `0` |
| `2` | bad input: usage, I/O, or an input the compressor rejects on `stats` |
| `3` | a corrupt, empty, or otherwise unrecoverable archive on `expand` |
| `1` | **the launcher failed and tokfold never ran** — except in one documented corner, below |
| `128 + N` | the binary was killed by signal `N` and re-raising `N` did not kill the wrapper — a signal Node ignores, such as `SIGXFSZ` (`153` on macOS) |

`1` is not a tokfold exit code. It is emitted only by this npm wrapper, so a
script can tell an installation problem apart from a data problem. Three
situations produce it in ordinary use, and in all three no tokfold process ever
ran:

- the platform is unsupported, so no package name matches — or it is a musl
  Linux, where a package does match and the wrapper refuses to run it;
- the package holding the binary for this machine is not installed;
- the binary is installed but will not start — not executable, not a program this
  kernel can run, or a truncated or partly written download.

When the binary does run and is then killed by a signal, the wrapper re-raises
that same signal on itself, so the calling shell reports a signal death as it
would for the binary alone — `130` for `Ctrl-C`, and so on. `SIGUSR1` is
re-raised too: the wrapper drops its own listener first, which leaves the kernel
default behind rather than Node's debugger. `128 + N` is the fallback for a signal
re-raising cannot deliver, one the Node runtime ignores process-wide. Node ignores
`SIGPIPE` and `SIGXFSZ`, and the binary can die of the second — a write past
`ulimit -f` exits `153` on macOS, what a shell reports for the binary run
directly, though a parent that reads the signal itself sees an exit code here
where it would have seen a signal. It cannot die of the first: like every Rust
program it starts with `SIGPIPE` ignored. A `SIGUSR1` sent to the wrapper itself is passed
on to the binary like `SIGTERM`, so it ends the same way instead of opening a
debugger in the wrapper — once the wrapper's script is running. One that lands
while Node is still starting up opens the debugger or kills the wrapper, before any
binary exists; a debugger opened then stays open, because the wrapper goes on to
start the binary.

All of that assumes the caller left each signal at its default. A signal the
caller has set to be *ignored* is not ignored through the wrapper: Node replaces
the inherited ignore with the wrapper's own listener and starts the binary with
every signal at its default. So `nohup tokfold mcp &` dies on a hangup that the
binary run directly survives, and so does a background job whose shell started it
with `SIGINT` ignored (measured on macOS; the published `0.0.1` wrapper does the
same). Node gives the wrapper no way to see an inherited ignore. Where one
matters, run the binary from the platform package directly.

There is a fourth `1` in the code, and it is the one case where `1` would **not**
mean "tokfold never ran": Node reports the child's signal under a name that this
runtime's `os.constants.signals` table cannot map back to a number, so `128 + N`
has no `N` to add. No production input is known to reach it. Node produces the
name from a list of its own, separate from that table, and every name it produced
was in the table — measured on macOS with Node 22 for signals 1–31, and on Linux
(aarch64, glibc 2.39, Node v22.23.2) for signals 1–64. The signals it could not
name arrive with no name at all, and exit `0`, below. Only the launcher's own
test, which empties the table, reaches this `1`. The launcher writes a line naming the signal before it exits, so this
`1` can be told apart from the three that mean the binary never started. Nothing
better than `1` is available at that point — a made-up number would be worse than a
known-imprecise one — and the alternative, `128 + undefined`, is `NaN`, which
`process.exit` turns into **`0`** on Node 18, this package's `engines` floor. Newer
runtimes throw `ERR_OUT_OF_RANGE` instead, which is loud but still not an exit code
a script can read. That is the failure this fallback exists to avoid.

And there is one way a killed binary reads as **`0`**, which no launcher built on
Node's public API can prevent. Node names the child's signal before handing it to
the launcher, and a signal Node has no name for is reported as an ordinary exit
with code `0` and no signal. Measured: `SIGEMT` on macOS, and on Linux (aarch64,
glibc 2.39, Node v22.23.2) every signal from 32 to 64 — the realtime signals from
`SIGRTMIN` (34) up, and the two below it that glibc reserves for itself. The launcher cannot
tell that from success, so it exits `0`. tokfold never raises such a signal
itself; it takes an outside `kill` to reach this. If that matters to you, run the
binary directly rather than through this wrapper.

## Known issue: an intermittent hang on Node 18 under macOS

One measurement found the wrapper hanging after the binary had already exited:
on macOS arm64 with Node v18.20.8, of 300 runs ended by a signal (five signals,
60 runs each), 32 left the binary dead and the wrapper waiting, with nothing on
stderr, until the test harness killed it. The same measurement reproduced it
with a minimal Node script starting `/bin/sleep` and no tokfold at all, and saw no
hang on macOS with Node 20, 22 or 26, nor on Linux with Node 18 or 22. A second
attempt on the same machine and Node version reproduced none in 300 runs, but
under CPU load (four busy processes) the wrapper's own test suite on Node v18.20.8
lost one to three of its 76 tests to a 30-second bound in two of three runs, where
Node 20 and 22 passed all 76 under the same load. The
published `0.0.1` wrapper starts the binary the same way; whether it hangs too was
not measured. The continuous-integration suite does not run Node 18 on macOS, so
it cannot see this. No fix has been chosen. Running the binary from the platform
package directly bypasses the wrapper.

## Supported platforms

macOS arm64 and x64, Linux x64 and arm64 (**glibc**), Windows x64. The binary for
your machine arrives as an optional dependency; the other four are never
downloaded. On a musl Linux the glibc package for your architecture is downloaded
anyway — the platform packages declare no `libc` field, so npm has nothing to
skip it on — and the wrapper then refuses to run it.

musl systems (Alpine and `-alpine` images) are **not** supported — the wrapper
detects musl and says so rather than failing with a confusing loader error. Use a
glibc image, or build from source:

```
cargo install --git https://github.com/IvanBBaev/tokfold tokfold-cli
```

## Licence

MIT OR Apache-2.0, at your option.

Source, full documentation and the honest-benchmarks discussion:
<https://github.com/IvanBBaev/tokfold>
