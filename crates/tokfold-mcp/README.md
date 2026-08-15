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

Relatedly, the transport caps a line it *reads* at 32 MiB and caps nothing it writes.
A reply is always larger than the call it answers — the payload comes back both as
`content` and as `structuredContent`, plus a base64 archive when compression succeeds —
so a request of around 10 MB already produces a reply larger than this server would
accept as input.

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
