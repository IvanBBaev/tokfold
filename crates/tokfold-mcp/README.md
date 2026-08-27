# tokfold-mcp

**EXPERIMENTAL.** The MCP stdio server crate of
[tokfold](https://github.com/IvanBBaev/tokfold).

## What it does

Exposes the compression engine as three Model Context Protocol tools —
`tokfold_compress`, `tokfold_decompress`, `tokfold_estimate` — over a line-framed
stdio transport, so an agent can shrink a large tool result before it enters the
prompt and recover the original bytes afterwards.

The server is reached through the `mcp` subcommand of the `tokfold` binary. That
binary is **not distributed**: nothing is published to a package registry and
there are no release artifacts, so build it from the git checkout and run the
built binary (or `cargo run`) as your MCP client's command:

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
once its replies fill the OS pipe buffer (64 KiB on macOS) the server blocks in its
write and stops draining stdin, and the client blocks in its own write. The threshold
is one pipe buffer, not a large payload — a couple of thousand pipelined `ping` calls
is enough. Making the loop read and write concurrently would mean a second thread or
an async runtime in a crate that is deliberately sans-io and dependency-free, so the
requirement is documented rather than designed away.

One frame limit applies in both directions: 32 MiB is the largest line the transport
will read *and* the largest it will write. Both matter, because a reply is always larger
than the call it answers — the payload comes back both as `content` and as
`structuredContent`, plus a base64 archive when compression succeeds, which is roughly
2x on the passthrough path and up to 3.3x when an archive barely shrinks. A request of
around 10 MB is therefore already enough to produce an answer that does not fit, and
what the client gets then is a JSON-RPC error (`-32602`) addressed to its own request id,
not a truncated frame and not silence.

The limit belongs to `Server::handle_line`, not to the stdio loop, so an embedder that
writes its own transport gets it too; `Server::with_max_message_bytes` sets a different
one when the receiving side has a smaller budget. In a batch, a member whose answer does
not fit is replaced by that error alone and the other members keep their real answers;
only a batch whose per-member errors cannot themselves be made to fit collapses to a
single id-less error.

## Status

**Not hardened.** The server sits in the secrets path — a transcript passed through it
is fully visible to it — and the audit that would make that acceptable is a separate
milestone gating any public launch. Nothing here is production-ready or audited, and
nothing here is covered by the reversibility guarantees of
[`tokfold-core`](https://github.com/IvanBBaev/tokfold/tree/main/crates/tokfold-core).
Do not use it with production secrets.

Deliberately out of scope: the *proxy* shape — an upstream connection, a
content-addressed archive store, a `retrieve` tool — which needs its own threat model
before any of it is written. What exists is the server: tools in, tools out, no
persistence, no network.

## Licence

Dual-licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache License
2.0 ([LICENSE-APACHE](LICENSE-APACHE)) at your option.

Minimum supported Rust version: 1.85 (edition 2024).
