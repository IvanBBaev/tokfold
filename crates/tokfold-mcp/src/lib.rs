//! **EXPERIMENTAL.** MCP stdio server for tokfold.
//!
//! Exposes the compression engine as three Model Context Protocol tools —
//! `tokfold_compress`, `tokfold_decompress`, and `tokfold_estimate` — over a
//! line-framed stdio transport. An agent can compress a large tool result before it
//! enters the prompt and recover the original bytes later.
//!
//! ```no_run
//! let mut server = tokfold_mcp::Server::new();
//! tokfold_mcp::stdio::serve_stdio(&mut server)?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # Status
//!
//! This crate is **not hardened**. It sits in the secrets path — a transcript passed
//! through it is fully visible to it — and the audit that would make that acceptable
//! is a separate milestone gating any public launch. [`EXPERIMENTAL_NOTICE`] carries
//! that warning in text; the `tokfold mcp` subcommand prints it to stderr on start-up,
//! and an embedder calling [`Server`] directly is expected to surface it too.
//!
//! Specifically out of scope here, and deliberately so: the *proxy* shape — an
//! upstream connection, a content-addressed archive store, and a `retrieve` tool —
//! which needs its own threat model before any of it is written. What this crate
//! implements is the server: tools in, tools out, no persistence, no network.
//! `deny.toml` bans every HTTP, socket, and DNS crate in the workspace, so that
//! boundary is enforced by the build rather than by intent.
//!
//! # Shape
//!
//! All protocol logic lives behind [`Server::handle_line`], which maps a line of text
//! to a line of text and performs no I/O. [`stdio`] is the loop that adds the streams.
//! The split is what makes the awkward cases — a duplicate handshake, a truncated
//! line, an unsupported protocol revision — testable without spawning a process.
//!
//! The server answers both protocol eras: `initialize` for clients on `2025-11-25`
//! and earlier, and stateless per-request metadata plus `server/discover` for
//! `2026-07-28`. See [`protocol`] for the revision table.
//!
//! # What the transport expects of a client
//!
//! The loop handles one request at a time and writes each reply before it reads the
//! next line, so **a client must drain stdout concurrently with writing stdin**. Every
//! mainstream MCP client SDK already reads a stdio server's output on a separate task,
//! which is why this is a documented precondition and not a live bug. A client that
//! writes a burst of requests and only then starts reading deadlocks instead: the
//! server blocks once its replies fill the OS pipe buffer (64 KiB on macOS) and stops
//! draining stdin. The threshold is one pipe buffer — a couple of thousand small calls
//! reach it — not a large payload. [`stdio::serve`] has the measurements.
//!
//! # No new dependencies
//!
//! JSON, base64, and the JSON-RPC envelope are written here rather than pulled in.
//! The reasons are recorded in this crate's `Cargo.toml`; the short version is that
//! every off-the-shelf option either breaks the workspace's MSRV, trips its
//! dependency bans, or forces a value model this crate cannot use — an archive is
//! arbitrary bytes, and a JSON library that normalizes numbers or reorders keys
//! would break both reversibility and the byte-stable output the prompt cache needs.

#![forbid(unsafe_code)]

pub mod base64;
pub mod json;
pub mod jsonrpc;
pub mod protocol;
pub mod server;
pub mod stdio;
pub mod tools;

pub use server::Server;

/// Warning shown when the experimental server is started.
pub const EXPERIMENTAL_NOTICE: &str = "the `mcp` subcommand is EXPERIMENTAL: unhardened, unaudited, and not covered \
     by the reversibility guarantees. Do not use it with production secrets.";
