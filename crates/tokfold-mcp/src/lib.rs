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
//! has not been done. Shipping did not wait for it: the `0.0.1` npm release carries
//! this server as it stands, so release is not evidence of review. What the release
//! does carry is the warning. [`EXPERIMENTAL_NOTICE`] holds it in text; the
//! `tokfold mcp` subcommand prints it to stderr on start-up, and an embedder calling
//! [`Server`] directly is expected to surface it too.
//!
//! Specifically out of scope here, and deliberately so: the *proxy* shape — an
//! upstream connection, a content-addressed archive store, and a `retrieve` tool —
//! which needs its own threat model before any of it is written. What this crate
//! implements is the server: tools in, tools out, no persistence, no network.
//!
//! A `cargo-deny` policy holds one half of that boundary: no HTTP, socket, or DNS
//! *dependency* may enter the workspace graph, so the capability cannot arrive with a
//! crate. It reaches no further. `std::net` is standard library and no lint can ban it,
//! so first-party code opening a socket is caught by review, not by the build — and
//! `std::fs` and `std::time` are outside that policy's view entirely. The boundary is
//! real; only part of it is mechanised.
//!
//! # Shape
//!
//! All protocol logic lives behind [`Server::handle_line`], which maps a line of text
//! to a line of text and performs no I/O. [`stdio`] is the loop that adds the streams.
//! The split is what makes the awkward cases — a duplicate handshake, a truncated
//! line, an unsupported protocol revision — testable without spawning a process.
//!
//! Because the split is where the protocol rules live, the frame size limit lives there
//! too: a [`Server`] left at its default never returns a line longer than
//! [`MAX_MESSAGE_BYTES`] from [`Server::handle_line`], so an embedder that writes its own
//! transport gets the same bound the stdio loop does. An embedder that overrides the
//! limit owns both ends of it — [`Server::with_max_message_bytes`] documents the floor
//! of 95 bytes, under which the refusal frame no longer fits inside the limit it
//! reports, and the regimes above it. See [`MAX_MESSAGE_BYTES`] for what a client is handed when an answer
//! does not fit.
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
//! server blocks once its replies fill the stdout pipe and stops draining stdin, and the
//! client blocks once the stdin pipe and the loop's read buffer are full too. The
//! threshold is what those buffers hold together — on macOS 2,195 pipelined `ping` calls
//! completed and 2,200 deadlocked — not a large payload. [`stdio::serve`] has the
//! measurement and its fixture.
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

/// Largest message this crate will read or emit, in bytes.
///
/// Sized off the real ceiling rather than guessed: core accepts 16 MiB of input, and
/// base64 inflates an archive by a third, so a legitimate `tokfold_decompress` call can
/// approach 22 MiB. 32 MiB leaves room above that.
///
/// The margin is deliberately narrow. Anything core would refuse as too large is echoed
/// back through the passthrough path, so a line the engine can do nothing with still
/// costs several copies of itself in memory before the refusal reaches the client.
/// Sizing this well above what core accepts buys no capability and multiplies that cost.
///
/// # This bounds bytes, not memory
///
/// Worth stating plainly, because the opposite was once written here: capping the line
/// length is *not* on its own what keeps a hostile client from making the server
/// allocate without bound. A parsed value costs far more than the text it came from —
/// 32 bytes per `Value`, 56 per object member, against two input bytes for an array
/// element — so a line well inside this limit can still expand into hundreds of
/// megabytes of tree. The second half of the bound is [`json::MAX_NODES`], which caps
/// how many values one message may materialise; that constant carries the arithmetic.
/// Both limits are needed, and neither substitutes for the other.
///
/// # One number in both directions
///
/// This is the cap on what is *read* and the cap on what is *emitted*, and it is one
/// constant rather than two so the two can never drift. It is measured the same way on
/// both sides — inclusive, on the message, with the newline that frames it left out —
/// because one constant compared two ways is two limits: the read side used to charge
/// the newline, and a message of exactly this size was refused as exceeding it while a
/// reply of the same size went out. The asymmetry the constant removes was
/// real and measured: a reply can be several times the call it answers — the payload
/// goes back twice, as `content` and as `structuredContent`, plus a base64 archive on
/// the compress path. A large call tends to 2.00x on the passthrough path and to 3.33x
/// when the rendering barely shrinks (the archive, at v0.0.1, never does), that second
/// figure being 2 + 4/3; escaping inflates the call and both copies in the reply
/// together, so it can only pull the payload's share of the ratio down. Those are limits
/// approached from above, not ceilings — the reply's fixed envelope outweighs the
/// call's, so `{"k":"z"}` measures 5.02x counting each line's newline (a 121-byte call,
/// a 608-byte reply; 5.06x on the messages alone) — but only a large call nears this cap, and
/// there the envelope is noise. With only the read side capped, a 10,066,259-byte call — under a
/// third of what this admits — produced a 33,554,431-byte line this same reader would
/// have refused one byte later. Those two sizes are messages with the newline left out,
/// as the cap measures them.
///
/// That multiple is not a law, either. Which side is bigger depends on how the client
/// encoded its request: one that escapes non-ASCII as `\uXXXX` — what Python's
/// `json.dumps` does by default — spends six bytes per character on the way in and is
/// answered in raw UTF-8, so a Cyrillic payload measured here came back at 0.72x its
/// call. That direction is harmless, and it has a floor rather than a ceiling: an
/// escaped character costs six bytes in the call and returns at its UTF-8 width `w`,
/// twice, so the no-archive path tends to `2w/6` — 2/3 for Cyrillic, 1/3 for ASCII a
/// client chose to spell `\u00XX` (`\u0041` × 100 000 measured 0.3338x) — and with an
/// archive of raw bytes behind a rendering that shrinks toward nothing it tends to
/// `(4/3)w/6`, 2/9 for ASCII (a million `\u0020` over ten thousand rows measured
/// 0.2703x). The cap exists for the other direction.
///
/// [`Server::handle_line`] therefore never returns a longer string from a server left
/// at this default, and [`stdio::MAX_LINE_BYTES`] is this same constant. A request
/// whose answer would not fit is refused with a JSON-RPC error addressed to that
/// request's own id, so the call fails rather than hanging; `tests/session.rs` pins
/// both the multiplier and the refusal. The one exception is an id so wide that the
/// addressed refusal would not fit either — a string id within about a hundred bytes of
/// this cap, which the reader still admits. That request is refused with the id-less
/// form, a bounded frame its client cannot correlate, so that call alone waits.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

/// Warning shown when the experimental server is started.
pub const EXPERIMENTAL_NOTICE: &str = "the `mcp` subcommand is EXPERIMENTAL: unhardened, unaudited, and not covered \
     by the reversibility guarantees. Do not use it with production secrets.";
