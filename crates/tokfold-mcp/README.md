# tokfold-mcp

**EXPERIMENTAL.** The MCP stdio proxy crate of
[tokfold](https://github.com/IvanBBaev/tokfold).

## Status

**v0.0.1 — a placeholder.** No proxy is implemented. The crate exports only the
experimental notice that the `tokfold mcp` subcommand prints before exiting
non-zero.

The proxy sits in the secrets path — it would see full agent transcripts — so
hardening it is a separate milestone that gates any public launch. Nothing here is
production-ready, hardened, or audited, and nothing here is covered by the
reversibility guarantees of [`tokfold-core`](https://crates.io/crates/tokfold-core).
Do not use it with production secrets.

## Licence

Dual-licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache License
2.0 ([LICENSE-APACHE](LICENSE-APACHE)) at your option.

Minimum supported Rust version: 1.85 (edition 2024).
