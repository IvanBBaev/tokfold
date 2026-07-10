//! **EXPERIMENTAL.** MCP stdio proxy for tokfold.
//!
//! This crate is a placeholder in v0.0.1. The proxy sits in the secrets path — it
//! sees full transcripts — and hardening it is a separate milestone that gates any
//! public launch. Nothing here is production-ready.

#![forbid(unsafe_code)]

/// Warning shown wherever the experimental proxy is invoked.
pub const EXPERIMENTAL_NOTICE: &str = "the `mcp` subcommand is EXPERIMENTAL: unhardened, unaudited, and not covered \
     by the reversibility guarantees. Do not use it with production secrets.";
