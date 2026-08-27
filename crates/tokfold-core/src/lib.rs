//! Reversible, structure-aware context compression for LLM agents.
//!
//! `tokfold-core` is the engine: a deterministic, sans-io library that re-encodes
//! bulky agent context (tool outputs, JSON, logs) into a denser rendering the model
//! reads, plus a recovery archive that reconstructs the original on demand.
//!
//! # What "reversible" means here
//!
//! Decompression reproduces a **semantically identical** document, not identical
//! bytes: object key order, duplicate keys and number lexemes are preserved exactly;
//! whitespace and escape style are canonicalized. This is a deliberate contract —
//! byte-identity would force us to preserve exactly the whitespace we are paid to
//! delete.
//!
//! The engine is never marketed as "lossless". The model reads the compressed
//! rendering, not the reconstruction, so byte-reversibility proves nothing about
//! what the model understood. See [`Fidelity`].
//!
//! # Guarantees
//!
//! * **Sans-io.** No file, network or clock access anywhere in this crate.
//! * **Deterministic.** Same logical input produces byte-identical output, always.
//!   Agent context is prompt-cached by providers; nondeterministic output silently
//!   invalidates that cache and *costs* money.
//! * **Total on valid JSON.** If no encoder reduces the estimated token count, the
//!   engine returns a passthrough artifact. "Couldn't compress" is a statistic,
//!   never an error.
//! * **Do no harm.** When an encoder wins, its rendering — sentinel frame included —
//!   costs fewer estimated tokens than the input. The passthrough fallback is the one
//!   exception: it re-emits the input behind an 18-byte `raw` sentinel, a constant
//!   overhead of about 10 estimated (11 real `cl100k`) tokens that
//!   [`Stats::token_ratio`] does not attribute to compression — it reports exactly
//!   `1.0`. The guarantee is therefore "compressing never makes it worse", not "the
//!   rendering is never longer than the input".
//! * **Fail closed.** Any integrity mismatch on decode returns an error rather than
//!   partially recovered bytes.
//!
//! # What an archive actually contains (v0.0.1)
//!
//! Every archive this version writes is a **passthrough recovery blob**: a header of
//! about 43 bytes (magic, version, encoder id, tokenizer id, flags, the original
//! length as a varint, and a `SHA-256` digest) followed by **the original input bytes,
//! verbatim**. Only the model-facing [`Artifact::rendering`] is ever re-encoded; the
//! archive payload is not. It is **not encrypted, not encoded and not obfuscated** —
//! plain bytes behind a short header any reader can skip.
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
pub mod never_compress;
pub mod tape;

pub use compressor::{Artifact, Compressor, Config, ConfigBuilder, EncoderId, Profile, Stats};
pub use error::{CompressError, DecompressError};
pub use estimator::{ByteLenEstimator, HeuristicEstimator, TokenEstimator};
#[cfg(feature = "tiktoken")]
#[cfg_attr(docsrs, doc(cfg(feature = "tiktoken")))]
pub use estimator::{Cl100kEstimator, O200kEstimator, TokenizerLoadError};
pub use fidelity::Fidelity;
