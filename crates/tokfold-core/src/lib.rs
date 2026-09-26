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
//! verbatim (see below), [`Compressor::decompress`] is byte-exact today, and a
//! later encoder is allowed to give that up while keeping the value tree.
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
//! * **Do no harm.** When an encoder wins, its rendering — sentinel frame included —
//!   costs fewer estimated tokens than the input. The passthrough fallback is the one
//!   exception: it re-emits the input behind an 18-byte `raw` sentinel, an overhead
//!   of 10 estimated (11 real `cl100k`, 13 real `o200k`) tokens for an input that
//!   opens with a non-whitespace byte. It is not constant: leading whitespace can
//!   merge with the sentinel's newline, and over every prefix of up to six spaces,
//!   tabs, CRs and LFs before `{}` it measured 10–11 estimated, 10–12 `cl100k` and
//!   12–13 `o200k` tokens. [`Stats::token_ratio`] does not attribute it to
//!   compression — it reports exactly `1.0`. The guarantee is therefore "an encoder's rendering
//!   replaces the input only when the estimate prices it lower, frame included", not
//!   "the rendering is never longer than the input" — the passthrough rendering is
//!   longer, by the estimate too — and it is a statement about estimated tokens
//!   only: a real tokenizer can count a winning rendering as more tokens than the
//!   input.
//! * **Fail closed.** Any integrity mismatch on decode returns an error rather than
//!   partially recovered bytes.
//!
//! # What an archive actually contains (v0.0.1)
//!
//! Every archive this version writes is a **passthrough recovery blob**: a header of
//! 43 to 46 bytes — 47 for an original of 2^28 bytes or more, which needs a raised
//! [`ConfigBuilder::max_input_bytes`] — (magic, version, encoder id, tokenizer id,
//! flags, the original length as a varint, and a `SHA-256` digest) followed by **the
//! original input bytes, verbatim**. Only the model-facing [`Artifact::rendering`] is ever re-encoded; the
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
