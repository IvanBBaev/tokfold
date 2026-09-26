# tokfold-mcp

**EXPERIMENTAL.** The MCP stdio server crate of
[tokfold](https://github.com/IvanBBaev/tokfold).

## What it does

Exposes the compression engine as three Model Context Protocol tools —
`tokfold_compress`, `tokfold_decompress`, `tokfold_estimate` — over a line-framed
stdio transport, so an agent can shrink a large tool result before it enters the
prompt and recover the original bytes afterwards.

The server is reached through the `mcp` subcommand of the `tokfold` binary. That
binary ships on npm at `0.0.1` — `npm i -g tokfold` — and `tokfold mcp` is then
your MCP client's command. This crate itself is **not on crates.io**: there is no
`cargo add tokfold-mcp` and no docs.rs page, so embedding the library means
building from this repository.

```sh
npm i -g tokfold                       # the CLI, from the npm registry
tokfold mcp                            # the MCP server on stdio
```

From a git checkout instead:

```sh
git clone https://github.com/IvanBBaev/tokfold
cd tokfold
cargo build --release -p tokfold-cli   # binary at target/release/tokfold
target/release/tokfold mcp             # or: cargo run -p tokfold-cli -- mcp
```

Both protocol eras are served: the `initialize` handshake for clients on `2025-11-25`
and earlier, and stateless per-request metadata plus `server/discover` for
`2026-07-28`.

All protocol logic sits behind `Server::handle_line`, a pure text-to-text function;
the stdio loop only adds the streams. JSON, base64, and the JSON-RPC envelope are
written here rather than pulled in, so the crate adds nothing to the dependency tree.

### Reading a compress result

`tokfold_compress` returns the recovery archive in `structuredContent.archive` and
nowhere else. The `content` block carries the rendering alone, so **a client that
reads only `content` keeps a compressed rendering it can never decompress** — reading
`structuredContent` is required if the original is ever to be recovered. Reading only
`content` is spec-conformant and is the common shape for older clients, so the tool
description says this too; it is a limitation of the current wire shape, not a bug in
such a client. `tokfold_decompress` is unaffected: it puts the restored text in both
blocks.

## What the transport expects of a client

The server handles one request at a time and writes each reply before it reads the
next line, so **a client must drain stdout concurrently with writing stdin**. Every
mainstream MCP client SDK already does this — a stdio transport reads the server's
output on a separate task — so this is a precondition rather than a bug you are likely
to meet through a normal client.

A client that writes a burst of requests and only then starts reading will deadlock:
once its replies fill the stdout pipe the server blocks in its write and stops draining
stdin, and the client blocks in its own write once the stdin pipe and the server's
8 KiB read buffer are full too. The threshold is what those buffers hold together, not
a large payload: on macOS, with each call a 41-byte `ping` line answered with a 144-byte line,
2,195 pipelined calls completed and 2,200 deadlocked. Making the loop read and write concurrently would mean a second thread or
an async runtime in a crate that is deliberately sans-io and dependency-free, so the
requirement is documented rather than designed away.

One frame limit applies in both directions: 32 MiB is the largest message the transport
will read *and* the largest it will write, measured the same way on both sides — on the
message, with the newline that ends the line left out. The write side is the one that
binds, because the reply carries the payload more than once: it comes back both as
`content` and as `structuredContent`, plus a base64 archive when compression succeeds.
Against the whole **call**, a large payload tends to 2.00x when the input is passed
through (no archive) and to 3.33x when it compresses but the rendering barely shrinks —
two copies of a rendering nearly the input's size, plus a base64 archive a third larger
than the bytes it carries (at v0.0.1 the archive never shrinks: it is the input behind a
header). Those are limits approached from above, not ceilings: the reply's fixed
envelope is several times the call's, so a small call measures more — counting each
line's newline, `{"k":"z"}` answers a 121-byte call with 608 bytes, 5.02x, and the same
shape measures 3.53x at a 1,000-byte value and 3.35x at 10,000; a one-byte passthrough
measures 3.13x. What the
limit does bound is the part of the reply that grows with the payload, and escaping
can only pull that part *down*, because it inflates the call and both copies in the
reply together: with the call's payload inflated by a factor *e*, the payload terms
come to 2 + 4/(3*e*) times the call's, and *e* is 1 only when nothing needed escaping.
Only a large call can approach the frame limit, and there the envelope is noise. Against the **payload** there is no ceiling of that
kind, because only the reply's two copies are escaped: an array of 50,000 one-letter
strings, `["a","b",…]`, measured 4.34x its payload and 2.89x its call.

The threshold is measured rather than derived from those ratios, and it belongs to a
shape, not to the server: for the escape-free `{"k":"z"×n}` that nears 3.33x, the
largest call whose answer still fits is **10,066,259 bytes** — a 10,066,148-byte
payload, under a third of what this same reader admits as input — and it comes back as 33,554,431 bytes against a
33,554,432-byte cap (both messages, newline left out, as the cap counts them). One more
byte of payload is refused, and what the client gets is a JSON-RPC error (`-32602`)
addressed to its own request id, not a truncated frame and not silence. The exception is a
request whose id alone leaves no room for the
addressed error — a string id within about a hundred bytes of the cap. It gets the
id-less form of the same error: still a bounded frame, but that call is uncorrelated and
its client waits. A payload the engine passes through meets the cap much later — a 5,000,000-byte
run of `A`, which is not JSON, is answered with 10,000,338 bytes.

Note the "against" — a reply is not unconditionally larger than the call it answers, and
the reverse is easy to arrange. A client that escapes non-ASCII as `\uXXXX` (what
Python's `json.dumps` does by default) spends six bytes per character on the way in and
gets raw UTF-8 back, so a 152 KB Cyrillic payload measured here answered a 424 KB call
with a 304 KB reply, 0.72x. That shape has a floor rather than a ceiling, and the floor
depends on the width of what was escaped: a character spelled as an escape costs six
bytes in the call (twelve as a surrogate pair) and comes back at its UTF-8 width *w*,
twice, so on the no-archive path the ratio tends to 2*w*/6 — 2/3 for Cyrillic and for a
surrogate pair, 1 for a three-byte script, and **1/3 for ASCII spelled `\u00XX`**, which
nothing stops a client from sending: `\u0041` × 100 000 measured here answered a
600,107-byte call with a 200,338-byte reply, 0.3338x. When an archive is returned it
holds the raw bytes at 4/3 in base64, so a rendering that shrinks toward nothing pushes
the ratio toward (4/3)*w*/6, which is 2/9 for ASCII; a million `\u0020` spread over ten
thousand rows measured 0.2703x (a 6,128,998-byte call, a 1,656,953-byte reply). All of
it is harmless; it is only the other direction that meets the cap.

The limit belongs to `Server::handle_line`, not to the stdio loop, so an embedder that
writes its own transport gets it too; `Server::with_max_message_bytes` sets a different
one when the receiving side has a smaller budget. In a batch, a member whose answer does
not fit is replaced by that error alone — addressed to the member's id unless that id is
too wide for the error to fit, and naming the bytes the array had left
as well as the limit, since the member was measured against the remainder — and the
other members keep their real answers; only a batch whose per-member errors cannot
themselves be made to fit collapses to a single id-less error.

## Status

**Not hardened.** The server sits in the secrets path — a transcript passed through it
is fully visible to it — and the audit that would make that acceptable has not been
done. Shipping did not wait for it: the `0.0.1` npm release carries this server as it
stands, labelled experimental, and `tokfold mcp` prints that warning to stderr on
start-up. So being released is not evidence of being reviewed. Nothing here is
production-ready or audited, and nothing here is covered by the reversibility
guarantees of
[`tokfold-core`](https://github.com/IvanBBaev/tokfold/tree/main/crates/tokfold-core).
Do not use it with production secrets.

Deliberately out of scope: the *proxy* shape — an upstream connection, a
content-addressed archive store, a `retrieve` tool — which needs its own threat model
before any of it is written. What exists is the server: tools in, tools out, no
persistence, no network.

## Licence

Dual-licensed under either of MIT ([LICENSE-MIT](https://github.com/IvanBBaev/tokfold/blob/main/LICENSE-MIT)) or Apache
License 2.0 ([LICENSE-APACHE](https://github.com/IvanBBaev/tokfold/blob/main/LICENSE-APACHE)) at your option. Both texts
also travel inside the published tarball, next to this file.

Minimum supported Rust version: 1.85 (edition 2024).
