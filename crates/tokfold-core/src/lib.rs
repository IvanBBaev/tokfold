//! Reversible, structure-aware context compression for LLM agents.
//!
//! `tokfold-core` is the engine: a deterministic, sans-io library that re-encodes
//! bulky agent context (tool outputs, JSON, logs) into a denser rendering the model
//! reads, plus a recovery archive that reconstructs the original on demand.
//!
//! # What "reversible" means here
//!
//! Decompression is contracted to reproduce a **semantically identical** document,
//! not identical bytes: object key order, duplicate keys and number lexemes are
//! preserved exactly; whitespace and escape style may be canonicalized. This is a
//! deliberate contract — byte-identity would force us to preserve exactly the
//! whitespace we are paid to delete. It is the floor, not a description of this
//! version: because every archive this version writes holds the original bytes
//! verbatim (see *What an archive actually contains*),
//! [`Compressor::decompress`] is byte-exact today, and a later encoder is
//! allowed to give that up while keeping the value tree.
//!
//! The engine is never marketed as "lossless". The model reads the compressed
//! rendering, not the reconstruction, so byte-reversibility proves nothing about
//! what the model understood. See [`Fidelity`].
//!
//! # Guarantees
//!
//! * **Sans-io.** No file, network or clock access anywhere in this crate.
//! * **Deterministic.** The same input bytes under the same [`Config`] produce
//!   byte-identical output from one build; across 32- and 64-bit targets it is not
//!   established (see [`compressor`](mod@crate::compressor)). "The same" means bytes, not the same JSON value:
//!   the archive holds the input as written, so `{"a":1}` and `{ "a" : 1 }` give two
//!   archives even where their renderings agree.
//!   Agent context is prompt-cached by providers; nondeterministic output silently
//!   invalidates that cache and *costs* money.
//! * **Total on valid JSON within the configured limits** (16 MiB of input and 512
//!   levels of nesting by default). If no encoder reduces the estimated token count,
//!   the engine returns a passthrough artifact. "Couldn't compress" is a statistic,
//!   never an error.
//! * **Do no harm.** When an encoder wins, its rendering — the `#tkfd1 <tag>`
//!   sentinel line included — costs fewer estimated tokens than the input. When none
//!   does, the passthrough rendering is the input itself, byte for byte, with no
//!   sentinel, so the fallback costs exactly what the input costs and the `1.0`
//!   [`Stats::token_ratio`] reports for it is exact. The guarantee is "a rendering
//!   replaces the input only when the estimate prices it lower, sentinel included,
//!   and is otherwise the input" — a statement about estimated tokens only: a real
//!   tokenizer can count a winning rendering as more tokens than the input.
//! * **Fail closed.** Any integrity mismatch on decode returns an error rather than
//!   partially recovered bytes.
//!
//! # What an archive actually contains (v0.0.1)
//!
//! Every archive this version writes is a **passthrough recovery blob**: a header of
//! 43 to 46 bytes — 47 once the original is 2^28 bytes or larger, which needs a
//! raised [`ConfigBuilder::max_input_bytes`]; 47 is the ceiling of the compress
//! path, because the parser rejects an input longer than `u32::MAX` and so no
//! archive `compress` writes carries a sixth varint byte — (magic, version, encoder
//! id, tokenizer id, flags, the original length as a varint, and a `SHA-256` digest)
//! followed by **the original input bytes, verbatim**. Each of those widths is
//! pinned at the varint boundary that produces it by
//! `header_byte_count_at_every_varint_boundary` in `format`. That ceiling belongs to
//! the compress path and not to the format or to this crate's API:
//! [`format::Header::new`] takes a `u64`, so a caller writes a 48-byte header at
//! 2^35 and a 52-byte one at 2^63 — widths the same test pins — and `decompress`
//! refuses such an archive at its length field. Only the model-facing
//! [`Artifact::rendering`] is ever re-encoded; the archive payload is not. It is
//! **not encrypted, not encoded and not obfuscated** — plain bytes behind a short
//! header any reader can skip.
//!
//! An archive is therefore exactly as sensitive as the plaintext it wraps. Anything
//! that stores, logs, caches or forwards archives must handle them with the same care
//! as the original input; treating an archive as opaque because it is binary would be
//! a mistake. The header's `SHA-256` detects corruption, not tampering — it is unkeyed
//! and travels with the payload, so a MAC would be required for integrity against a
//! modifying adversary (see [`format`](mod@crate::format)).
//!
//! # Non-goals
//!
//! This crate is not a prompt-injection filter and must not be described as reducing
//! injection risk. It also performs no token counting against proprietary
//! tokenizers: Anthropic's is not public, so an exact count for such a model can
//! only come from that vendor's own API, never from here. What this crate offers
//! instead is [`TokenEstimator`], which an embedder implements to plug in whatever
//! counter it does have.

#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod compressor;
pub mod encoder;
pub mod error;
pub mod estimator;
pub mod fidelity;
pub mod format;
mod ndjson;
pub mod never_compress;
pub mod tape;

pub use compressor::{Artifact, Compressor, Config, ConfigBuilder, EncoderId, Profile, Stats};
pub use error::{CompressError, DecompressError};
pub use estimator::{ByteLenEstimator, HeuristicEstimator, TokenEstimator};
#[cfg(feature = "tiktoken")]
#[cfg_attr(docsrs, doc(cfg(feature = "tiktoken")))]
pub use estimator::{Cl100kEstimator, O200kEstimator, TokenizerLoadError};
pub use fidelity::Fidelity;
