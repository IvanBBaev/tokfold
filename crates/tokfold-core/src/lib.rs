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
//! * **Do no harm.** Estimated rendering tokens never exceed estimated input tokens.
//! * **Fail closed.** Any integrity mismatch on decode returns an error rather than
//!   partially recovered bytes.
//!
//! # Non-goals
//!
//! This crate is not a prompt-injection filter and must not be described as reducing
//! injection risk. It also performs no token counting against proprietary
//! tokenizers: Anthropic's is not public, so exact counts can only be obtained by
//! the CLI or proxy layer via an API call, never from here.

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
