# tokfold-cli

The command-line interface of [tokfold](https://github.com/IvanBBaev/tokfold):
reversible, structure-aware context compression for LLM agents. This crate ships
the single `tokfold` binary; the engine lives in
[`tokfold-core`](https://crates.io/crates/tokfold-core).

## Status

**v0.0.1 — skeleton under active development.** The command line is unstable and
will change without deprecation windows.

## Subcommands

- `tokfold compress` — read input, emit the token-reduced rendering, optionally
  persist a recovery archive (`--archive PATH`). Compression is an optimization,
  never a gate: an input the engine rejects is forwarded unchanged and the command
  still exits `0`.
- `tokfold expand` — reconstruct the exact original from a recovery archive.
  Fail-closed: any integrity error exits `3` and emits nothing.
- `tokfold stats` — report what a compression pass would achieve, without emitting
  the payload.
- `tokfold mcp` — EXPERIMENTAL stub. Prints a notice and exits non-zero; the stdio
  proxy is a separate, launch-gating milestone.

Exit codes are normative: `0` success, `2` bad input (usage, I/O, or an input the
compressor rejects on `stats`), `3` a corrupt or unrecoverable archive on `expand`,
`69` the unavailable `mcp` subcommand.

## Licence

Dual-licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache License
2.0 ([LICENSE-APACHE](LICENSE-APACHE)) at your option.

Minimum supported Rust version: 1.85 (edition 2024).
