# tokfold-cli

The command-line interface of [tokfold](https://github.com/IvanBBaev/tokfold):
reversible, structure-aware context compression for LLM agents. This crate ships
the single `tokfold` binary; the engine lives in
[`tokfold-core`](https://github.com/IvanBBaev/tokfold/tree/main/crates/tokfold-core).

## Status

**v0.0.1 — skeleton under active development.** The command line is unstable and
will change without deprecation windows.

Nothing is published to a package registry and there are no release artifacts, so
the only way to get the binary today is to build it from the git checkout:

```sh
git clone https://github.com/IvanBBaev/tokfold
cd tokfold
cargo build --release -p tokfold-cli   # binary at target/release/tokfold
```

## Subcommands

- `tokfold compress` — read input, emit the token-reduced rendering, optionally
  persist a recovery archive (`--archive PATH`). Compression is an optimization,
  never a gate: an input the engine rejects is forwarded unchanged and the command
  still exits `0`.
- `tokfold expand` — reconstruct the exact original from a recovery archive.
  Fail-closed: any integrity error exits `3` and emits nothing.
- `tokfold stats` — report what a compression pass would achieve, without emitting
  the payload.
- `tokfold mcp` — EXPERIMENTAL. Serves the engine as Model Context Protocol tools
  (`tokfold_compress`, `tokfold_decompress`, `tokfold_estimate`) over stdio, one
  JSON-RPC message per line, until the client closes the stream. A warning goes to
  stderr first: the server is unhardened, unaudited, and sees whatever passes through
  it. Do not point it at production secrets.

Exit codes are normative: `0` success, `2` bad input (usage, I/O, or an input the
compressor rejects on `stats`), `3` a corrupt or unrecoverable archive on `expand`.

Before v0.0.1 `mcp` was an unimplemented stub that always exited `69`; now that it
serves, it exits `0` when the client hangs up.

## Licence

Dual-licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache License
2.0 ([LICENSE-APACHE](LICENSE-APACHE)) at your option.

Minimum supported Rust version: 1.85 (edition 2024).
