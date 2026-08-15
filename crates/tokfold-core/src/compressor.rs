//! The public compression facade: [`Compressor`], its [`Config`] builder, and the
//! [`Artifact`] a `compress` call returns.
//!
//! This module is the only entry point a caller needs. It wires the frozen pieces
//! together — UTF-8 validation, the lexeme-preserving [`tape`] parser,
//! the sealed [`encoder`] candidate rule, and the
//! [`format`](mod@crate::format) archive framing — behind a small, sans-io, synchronous
//! surface.
//!
//! # Two outputs, two jobs
//!
//! [`compress`](Compressor::compress) produces two independent things:
//!
//! * [`Artifact::rendering`] — the token-reduced, sentinel-framed text the model
//!   reads. Whitespace and escape style are canonicalized, so it is deliberately
//!   *not* byte-identical to the input.
//! * [`Artifact::archive`] — a `TKFD` recovery blob that reconstructs the exact
//!   original bytes on demand.
//!
//! Because only the passthrough encoder is byte-exact reversible (the E1/E2
//! renderings canonicalize and so cannot reproduce the original byte for byte), the
//! archive in v0.0.1 is always a passthrough recovery blob: its header records
//! `encoder_id = 0`, the default `tokenizer_id = 0` and no flags, and its payload is
//! the original bytes verbatim. Which encoder actually shaped the *rendering* is
//! reported separately in [`Stats::encoder`], alongside the configured estimator's
//! [`Stats::tokenizer_id`]. This mirrors the frozen `passthrough_archive` /
//! `decode_full` pattern in [`format`](mod@crate::format) and lets
//! [`decompress`](Compressor::decompress) reconstruct the original byte-for-byte and
//! verify its `SHA-256` before returning anything.
//!
//! # Determinism
//!
//! The same logical input yields byte-identical `rendering` and `archive` on every
//! call and every build. Nothing here reads a clock, hashes with a per-process seed,
//! or iterates a `std` `HashMap`, so the provider's prompt cache stays warm.

use std::sync::Arc;

use crate::encoder::{self, Encoder};
use crate::error::{CompressError, DecompressError};
use crate::estimator::{HeuristicEstimator, TokenEstimator};
use crate::fidelity::Fidelity;
use crate::format::{self, Flags, Header};
use crate::tape;

/// Default input ceiling: 16 MiB. Inputs larger than this are rejected with
/// [`CompressError::InputTooLarge`] before any work is done.
const DEFAULT_MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;

/// Default nesting ceiling. The iterative parser trips
/// [`CompressError::DepthExceeded`] rather than overflowing the stack.
const DEFAULT_MAX_DEPTH: usize = 512;

/// Ceiling for the candidate-rule margin: 10 000 bps = 100%. A larger value asks a
/// rendering to save more than the whole input, which nothing can do, so it is
/// clamped rather than accepted as a way to disable compression by accident.
const MAX_SAVING_BPS: u32 = 10_000;

/// Encoders offered under [`Profile::Conservative`]: minification only, the
/// lowest-risk transform.
const CONSERVATIVE_ENCODERS: &[Encoder] = &[Encoder::E1Minify];

/// Encoders offered under [`Profile::Balanced`] and [`Profile::Aggressive`]. E3
/// legend folding is reserved and unimplemented in v0.0.1, so the two profiles
/// currently offer the same set; the split exists so aggressive codecs can be added
/// later without an API change.
const FULL_ENCODERS: &[Encoder] = &[Encoder::E1Minify, Encoder::E2Tabular];

// Header field offsets, derived from the frozen `TKFD` layout (see `format`). Used
// only to aim a `Corrupt` error at the offending field.
const ENCODER_ID_OFFSET: usize = format::MAGIC.len() + 1;
const TOKENIZER_ID_OFFSET: usize = ENCODER_ID_OFFSET + 1;
const FLAGS_OFFSET: usize = TOKENIZER_ID_OFFSET + 2;
const ORIGINAL_LEN_OFFSET: usize = FLAGS_OFFSET + 2;

/// How aggressively [`compress`](Compressor::compress) is allowed to re-encode.
///
/// A profile only selects which encoders may *compete* for the rendering; the
/// candidate rule and the do-no-harm guarantee are unconditional, and archive
/// reversibility never depends on the profile.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// Minification only — the smallest, safest change to the text.
    Conservative,
    /// Minification plus tabular re-encoding. The default.
    #[default]
    Balanced,
    /// Every shipped encoder. Identical to [`Profile::Balanced`] in v0.0.1 until
    /// legend folding lands.
    Aggressive,
}

/// Immutable configuration for a [`Compressor`].
///
/// Build one with [`Config::builder`]; the fields are private so new knobs can be
/// added without breaking callers. Cheap to clone — the estimator is shared behind
/// an [`Arc`].
#[derive(Clone)]
pub struct Config {
    profile: Profile,
    max_input_bytes: usize,
    max_depth: usize,
    estimator: Arc<dyn TokenEstimator>,
    min_saving_bps: Option<u32>,
}

impl Config {
    /// Starts a [`ConfigBuilder`] seeded with the defaults.
    #[must_use]
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::new()
    }
}

impl Default for Config {
    fn default() -> Self {
        ConfigBuilder::new().build()
    }
}

/// Fluent builder for [`Config`].
///
/// Every setter takes and returns `self`, so calls chain;
/// [`build`](ConfigBuilder::build) finalizes. Defaults: [`Profile::Balanced`], a
/// 16 MiB input ceiling, a depth ceiling of 512, and the [`HeuristicEstimator`].
pub struct ConfigBuilder {
    profile: Profile,
    max_input_bytes: usize,
    max_depth: usize,
    estimator: Arc<dyn TokenEstimator>,
    min_saving_bps: Option<u32>,
}

impl ConfigBuilder {
    fn new() -> Self {
        Self {
            profile: Profile::default(),
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_depth: DEFAULT_MAX_DEPTH,
            estimator: Arc::new(HeuristicEstimator),
            min_saving_bps: None,
        }
    }

    /// Sets the encoding [`Profile`]. Default: [`Profile::Balanced`].
    #[must_use]
    pub fn profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        self
    }

    /// Sets the maximum accepted input length in bytes. Default: 16 MiB. Inputs
    /// above this yield [`CompressError::InputTooLarge`].
    #[must_use]
    pub fn max_input_bytes(mut self, max_input_bytes: usize) -> Self {
        self.max_input_bytes = max_input_bytes;
        self
    }

    /// Sets the maximum accepted nesting depth. Default: 512. Deeper input yields
    /// [`CompressError::DepthExceeded`].
    #[must_use]
    pub fn max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Overrides the token cost model used by the candidate rule. Default:
    /// [`HeuristicEstimator`]. Must be pure and deterministic.
    #[must_use]
    pub fn estimator(mut self, estimator: Arc<dyn TokenEstimator>) -> Self {
        self.estimator = estimator;
        self
    }

    /// Sets the smallest estimated token saving a rendering must show to be kept,
    /// in basis points of the input estimate (10 000 bps = 100%).
    ///
    /// Unset, the effective margin is whatever the configured estimator declares via
    /// [`TokenEstimator::over_claim_bps`] — `0` for both estimators shipped in
    /// v0.0.1, i.e. "keep any strict token win". Setting it explicitly overrides the
    /// estimator's declaration in both directions.
    ///
    /// Raise this when a mis-selection is expensive and the cost model is inexact:
    /// with the default [`HeuristicEstimator`] a claimed saving in the low single
    /// digits sits inside the model's measured error, so it may not be a real saving
    /// at all. [`HeuristicEstimator::MEASURED_OVER_CLAIM_BPS`] is that measurement.
    /// The trade is real in both directions — the error is two-sided, so a large
    /// margin also discards genuine wins the model under-rates, and anything much
    /// above ~1100 bps retires E1 on typical pretty-printed JSON.
    ///
    /// Values above 10 000 are clamped: no rendering can save more than everything.
    #[must_use]
    pub fn min_saving_bps(mut self, min_saving_bps: u32) -> Self {
        self.min_saving_bps = Some(min_saving_bps);
        self
    }

    /// Finalizes the configuration.
    #[must_use]
    pub fn build(self) -> Config {
        Config {
            profile: self.profile,
            max_input_bytes: self.max_input_bytes,
            max_depth: self.max_depth,
            estimator: self.estimator,
            min_saving_bps: self.min_saving_bps,
        }
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// The frozen id of the encoder that produced a rendering.
///
/// Exposed in [`Stats`] so callers can attribute a result to an encoder without the
/// sealed [`Encoder`](crate::encoder) enum leaking into the public API.
///
/// The type is `#[non_exhaustive]`, so downstream crates cannot construct one — a
/// later version may add fields. Compare against the associated constants instead
/// ([`PASSTHROUGH`](Self::PASSTHROUGH), [`E1_MINIFY`](Self::E1_MINIFY),
/// [`E2_TABULAR`](Self::E2_TABULAR)); without them the derived `PartialEq` would be
/// unusable outside this crate, because there would be no way to build the
/// right-hand side. The wire id stays readable through the public field for ids a
/// given build does not name.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderId(
    /// The frozen id: `0` = passthrough, `1` = E1 minify, `2` = E2 tabular. Reported
    /// out-of-band via [`Stats::encoder`]; v0.0.1's compressor always records
    /// `encoder_id = 0` in the archive header, since the archive is a passthrough blob.
    pub u8,
);

impl EncoderId {
    /// No encoder beat the input; the rendering is the original bytes (wire id `0`).
    pub const PASSTHROUGH: Self = Self(0);
    /// Whitespace minification (wire id `1`).
    pub const E1_MINIFY: Self = Self(1);
    /// Shape-deduplicated tabular re-encoding (wire id `2`).
    pub const E2_TABULAR: Self = Self(2);
}

/// What a compression pass achieved.
///
/// The `*_before` / `*_after` pairs describe the model-facing rendering; the ratios
/// are the headline numbers. A passthrough result reports both ratios as exactly
/// `1.0` — "couldn't compress" is a statistic, never an error.
///
/// That `1.0` is a floor, not a measurement: a passthrough rendering still carries the
/// 18-byte `raw` sentinel, which costs about 10 estimated (11 real `cl100k`) tokens
/// more than the bare input. The `*_after` fields are *set* equal to their `*_before`
/// counterparts on that path rather than measured, so the framing overhead is
/// deliberately not attributed to compression. Callers that must account for every
/// token should measure [`Artifact::rendering`] directly.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Stats {
    /// Byte length of the original input.
    pub bytes_before: usize,
    /// Byte length of the rendering; equals `bytes_before` for passthrough, so
    /// [`byte_ratio`](Stats::byte_ratio) is exactly `1.0` there.
    pub bytes_after: usize,
    /// Estimated tokens of the original input, per the configured estimator.
    pub est_tokens_before: usize,
    /// Estimated tokens of the rendering. On the passthrough path this is *set* to
    /// `est_tokens_before` rather than measured, so it under-reports the sentinel
    /// frame by about 10 tokens; see the type-level doc.
    pub est_tokens_after: usize,
    /// Which encoder shaped the rendering.
    pub encoder: EncoderId,
    /// Id of the estimator that drove selection. Reported here only — the archive
    /// header's `tokenizer_id` (`format` field 4) is always `0` in v0.0.1.
    pub tokenizer_id: u16,
    /// The margin, in basis points of `est_tokens_before`, that this pass actually
    /// applied — the configured override if there was one, otherwise the estimator's
    /// declared [`over_claim_bps`](TokenEstimator::over_claim_bps), clamped to
    /// 10 000. Reported so a passthrough result is explainable after the fact: it
    /// distinguishes "no encoder produced a win" from "a win was produced and
    /// refused for being inside the estimator's error budget".
    pub min_saving_bps: u32,
    /// Fidelity of the reconstruction. Always [`Fidelity::Lossless`] in v0.0.1.
    pub fidelity: Fidelity,
}

impl Stats {
    /// `bytes_after / bytes_before`; `1.0` for passthrough (and for empty input).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn byte_ratio(&self) -> f64 {
        if self.bytes_before == 0 {
            1.0
        } else {
            self.bytes_after as f64 / self.bytes_before as f64
        }
    }

    /// `est_tokens_after / est_tokens_before`; `1.0` for passthrough (and when the
    /// input estimates to zero tokens).
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn token_ratio(&self) -> f64 {
        if self.est_tokens_before == 0 {
            1.0
        } else {
            self.est_tokens_after as f64 / self.est_tokens_before as f64
        }
    }
}

/// The result of [`compress`](Compressor::compress): the model-facing rendering, the
/// recovery archive, and the [`Stats`] that describe the pass.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Artifact {
    /// Sentinel-framed text for model context. When an encoder won it is token-reduced
    /// and canonicalized, so not byte-identical to the input; on the passthrough path
    /// it is the input verbatim behind a `raw` sentinel, so it is neither.
    pub rendering: String,
    /// `TKFD` recovery blob that reconstructs the exact original via
    /// [`decompress`](Compressor::decompress).
    pub archive: Vec<u8>,
    /// What the pass achieved.
    pub stats: Stats,
}

/// The compression engine. Holds an immutable [`Config`]; every method is `&self`
/// and free of I/O, so one instance is safely shared across threads.
pub struct Compressor {
    config: Config,
}

impl Compressor {
    /// Builds a compressor from a finished [`Config`].
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Compresses `input`, returning the rendering, its recovery archive, and stats.
    ///
    /// The pipeline is: reject oversize input, validate UTF-8, parse to a tape, run
    /// the encoder candidate rule to build the rendering, then frame a `TKFD`
    /// recovery archive around the original bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CompressError::InputTooLarge`] past the configured byte ceiling,
    /// [`CompressError::NotUtf8`] for non-UTF-8 bytes, and
    /// [`CompressError::InvalidJson`] / [`CompressError::DepthExceeded`] for input
    /// the parser rejects. Every variant is recoverable: forward the original bytes.
    pub fn compress(&self, input: &[u8]) -> Result<Artifact, CompressError> {
        // 1. Size guard — before any allocation or parsing.
        if input.len() > self.config.max_input_bytes {
            return Err(CompressError::InputTooLarge {
                size: input.len(),
                limit: self.config.max_input_bytes,
            });
        }

        // 2. UTF-8 validation. Core never repairs input.
        let text = core::str::from_utf8(input).map_err(|_| CompressError::NotUtf8)?;

        // 3. Parse to a lexeme-preserving tape (propagates InvalidJson / DepthExceeded).
        let parsed = tape::parse(text, self.config.max_depth)?;

        // 4. Candidate rule: pick the encoder whose framed rendering is the strictest
        //    token win, or fall back to passthrough at ratio 1.0.
        let estimator: &dyn TokenEstimator = &*self.config.estimator;
        let enabled = enabled_encoders(self.config.profile);
        //    An explicit override wins over the estimator's declared error budget;
        //    clamped because a margin above 100% is unsatisfiable by construction.
        let min_saving_bps = self
            .config
            .min_saving_bps
            .unwrap_or_else(|| estimator.over_claim_bps())
            .min(MAX_SAVING_BPS);
        let selection = encoder::select(&parsed, text, estimator, enabled, min_saving_bps);

        // 5. Recovery archive: a passthrough TKFD blob wrapping the original bytes.
        //    Only passthrough is byte-exact reversible, so the archive header always
        //    records encoder 0, the default tokenizer id, and no flags. The selected
        //    rendering encoder is reported in `Stats`, not here.
        let Ok(original_len) = u64::try_from(input.len()) else {
            // Unreachable where usize <= 64 bits; treat an unrepresentable length as
            // over-limit rather than panic.
            return Err(CompressError::InputTooLarge {
                size: input.len(),
                limit: self.config.max_input_bytes,
            });
        };
        let header = Header::new(
            Encoder::Passthrough.id(),
            0,
            Flags::default(),
            original_len,
            format::sha256(input),
        );
        let mut archive = Vec::with_capacity(input.len().saturating_add(64));
        header.encode_into(&mut archive);
        archive.extend_from_slice(input);

        // 6. Stats. Passthrough reports the input size as its "after" so both ratios
        //    are exactly 1.0; a winning encoder reports the framed rendering length.
        let is_passthrough = matches!(selection.encoder, Encoder::Passthrough);
        let bytes_before = input.len();
        let bytes_after = if is_passthrough {
            bytes_before
        } else {
            selection.rendering.len()
        };
        let stats = Stats {
            bytes_before,
            bytes_after,
            est_tokens_before: selection.est_tokens_before,
            est_tokens_after: selection.est_tokens_after,
            encoder: EncoderId(selection.encoder.id()),
            tokenizer_id: estimator.tokenizer_id(),
            min_saving_bps,
            fidelity: Fidelity::Lossless,
        };

        Ok(Artifact {
            rendering: selection.rendering,
            archive,
            stats,
        })
    }

    /// Reconstructs the exact original bytes from a `TKFD` recovery `archive`.
    ///
    /// Fail-closed: the header is decoded, the metadata is checked against the only
    /// values v0.0.1 writes (passthrough encoder, default tokenizer id, no flags),
    /// the payload length is cross-checked against the header, and the reconstructed
    /// bytes are verified against the header's `SHA-256` — only then are they
    /// returned. Any mismatch is an error; partially recovered bytes are never
    /// returned.
    ///
    /// # Errors
    ///
    /// Returns a [`DecompressError`]: [`BadMagic`](DecompressError::BadMagic),
    /// [`UnsupportedVersion`](DecompressError::UnsupportedVersion),
    /// [`ReservedBitsSet`](DecompressError::ReservedBitsSet),
    /// [`Corrupt`](DecompressError::Corrupt) for malformed framing or unexpected
    /// metadata, or [`ChecksumMismatch`](DecompressError::ChecksumMismatch) when the
    /// reconstruction does not match the recorded digest.
    pub fn decompress(&self, archive: &[u8]) -> Result<Vec<u8>, DecompressError> {
        let (header, payload_start) = Header::decode(archive)?;

        // v0.0.1 only ever writes a passthrough recovery blob: encoder 0, the default
        // tokenizer id, no flags. Anything else in a version-1 archive could not have
        // been produced by this build, so fail closed. Gating these fixed fields also
        // makes every header byte load-bearing against single-bit corruption.
        if header.encoder_id != Encoder::Passthrough.id() {
            return Err(DecompressError::Corrupt {
                byte_offset: ENCODER_ID_OFFSET,
            });
        }
        if header.tokenizer_id != 0 {
            return Err(DecompressError::Corrupt {
                byte_offset: TOKENIZER_ID_OFFSET,
            });
        }
        if header.flags != Flags::default() {
            return Err(DecompressError::Corrupt {
                byte_offset: FLAGS_OFFSET,
            });
        }

        let Some(payload) = archive.get(payload_start..) else {
            return Err(DecompressError::Corrupt {
                byte_offset: payload_start,
            });
        };

        let Ok(expected_len) = usize::try_from(header.original_len) else {
            return Err(DecompressError::Corrupt {
                byte_offset: ORIGINAL_LEN_OFFSET,
            });
        };
        if payload.len() != expected_len {
            return Err(DecompressError::ChecksumMismatch);
        }

        // Verify the reconstructed original against the header digest before returning.
        format::verify_checksum(&header.checksum, payload)?;
        Ok(payload.to_vec())
    }
}

/// The encoders a profile may put forward as candidates. [`Encoder::Passthrough`] is
/// always the fallback and is never listed.
fn enabled_encoders(profile: Profile) -> &'static [Encoder] {
    match profile {
        Profile::Conservative => CONSERVATIVE_ENCODERS,
        Profile::Balanced | Profile::Aggressive => FULL_ENCODERS,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::float_cmp,
        clippy::indexing_slicing,
        clippy::format_push_string
    )]

    use super::*;

    fn compressor() -> Compressor {
        Compressor::new(Config::default())
    }

    /// A homogeneous array of `n` same-shape objects — the E2 tabular target. The
    /// keys repeat `n` times in the input but are hoisted once in the rendering, so a
    /// modest `n` is a clear token win.
    fn homogeneous_array(n: usize) -> String {
        let mut s = String::from("[");
        for i in 0..n {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"id\":{i},\"name\":\"item{i}\",\"active\":true}}"
            ));
        }
        s.push(']');
        s
    }

    /// A pretty-printed object with `n` members — the E1 minify target. Each
    /// 4-space-indented line is a whitespace token the heuristic charges.
    fn pretty_object(n: usize) -> String {
        let mut s = String::from("{\n");
        for i in 0..n {
            s.push_str("    \"key");
            s.push_str(&i.to_string());
            s.push_str("\": ");
            s.push_str(&i.to_string());
            if i + 1 < n {
                s.push(',');
            }
            s.push('\n');
        }
        s.push('}');
        s
    }

    /// `depth` nested arrays around a single scalar, e.g. `[[[1]]]` for `depth == 3`.
    fn nested_arrays(depth: usize) -> String {
        let mut s = String::new();
        for _ in 0..depth {
            s.push('[');
        }
        s.push('1');
        for _ in 0..depth {
            s.push(']');
        }
        s
    }

    /// A spread of valid JSON: scalars, minimal and pretty objects, a homogeneous
    /// array, protected content, nesting, and varied number lexemes.
    fn corpus() -> Vec<String> {
        vec![
            "{\"a\":1}".to_owned(),
            "42".to_owned(),
            "\"hello\"".to_owned(),
            "[1,2,3]".to_owned(),
            "null".to_owned(),
            "true".to_owned(),
            "{\"log\":\"error[E0308]: mismatched types\"}".to_owned(),
            "{\"x\":[1,2,{\"y\":3}],\"z\":{\"w\":[true,false,null]}}".to_owned(),
            "[1.0,1e3,-0.5,2.5e10]".to_owned(),
            pretty_object(30),
            homogeneous_array(12),
        ]
    }

    #[test]
    fn passthrough_is_not_an_error_and_reports_ratio_one() {
        let c = compressor();
        // Each of these is already minimal: no encoder can beat it, so the candidate
        // rule returns passthrough at ratio 1.0 rather than an error.
        for input in ["{\"a\":1}", "42", "\"hi\"", "[1,2,3]", "null", "true"] {
            let art = c.compress(input.as_bytes()).unwrap();
            assert_eq!(
                art.stats.encoder,
                EncoderId(0),
                "expected passthrough for {input:?}"
            );
            assert_eq!(art.stats.byte_ratio(), 1.0, "byte_ratio for {input:?}");
            assert_eq!(art.stats.token_ratio(), 1.0, "token_ratio for {input:?}");
            // `fidelity` is set to `Lossless` unconditionally in v0.0.1, so asserting
            // the tag alone is true by construction and catches nothing. Assert what
            // the tag *claims* instead: the archive gives the input back byte for byte.
            assert!(matches!(art.stats.fidelity, Fidelity::Lossless));
            assert_eq!(
                c.decompress(&art.archive).unwrap(),
                input.as_bytes(),
                "Lossless was reported but the archive did not reproduce {input:?}"
            );
        }
    }

    #[test]
    fn do_no_harm_across_the_corpus() {
        let c = compressor();
        for input in corpus() {
            let art = c.compress(input.as_bytes()).unwrap();
            assert!(
                art.stats.est_tokens_after <= art.stats.est_tokens_before,
                "harm on {input:?}: {} > {}",
                art.stats.est_tokens_after,
                art.stats.est_tokens_before
            );
            assert!(art.stats.token_ratio() <= 1.0, "ratio > 1 on {input:?}");
        }
    }

    #[test]
    fn compress_then_decompress_reconstructs_every_sample() {
        let c = compressor();
        for input in corpus() {
            let art = c.compress(input.as_bytes()).unwrap();
            let restored = c.decompress(&art.archive).unwrap();
            assert_eq!(restored, input.as_bytes(), "roundtrip failed for {input:?}");
        }
    }

    /// The recovery archive is estimator-independent: the header hardcodes
    /// `tokenizer_id = 0` and copies the original bytes (see `compress`), so swapping
    /// the configured [`TokenEstimator`] — the *only* thing the opt-in `tiktoken`
    /// feature does — leaves the `TKFD` archive byte-identical even though the two
    /// estimators report different ids in [`Stats`]. This is a regression lock on the
    /// "non-format-affecting" contract, not a claim that the two estimators pick
    /// different encoders on this corpus (they need not): its value is that a future
    /// refactor leaking the estimator id into the archive would break it. The differing
    /// reported id (heuristic `0` vs cl100k `2`) is asserted below so archive-equality
    /// is a genuine invariant, not a config compared with itself.
    #[cfg(feature = "tiktoken")]
    #[test]
    fn estimator_choice_does_not_change_recovery_archive() {
        use crate::estimator::Cl100kEstimator;

        let heuristic = Compressor::new(Config::default());
        let exact = Compressor::new(
            Config::builder()
                .estimator(Arc::new(Cl100kEstimator::new().unwrap()))
                .build(),
        );
        for input in corpus() {
            let bytes = input.as_bytes();
            let a = heuristic.compress(bytes).unwrap();
            let b = exact.compress(bytes).unwrap();
            // The two configs really are different estimators: the reported id differs
            // (heuristic 0 vs cl100k 2). Without this the archive-equality assertion
            // would be vacuous — a config trivially matches itself.
            assert_eq!(a.stats.tokenizer_id, 0, "heuristic id for {input:?}");
            assert_eq!(b.stats.tokenizer_id, 2, "cl100k id for {input:?}");
            assert_eq!(
                a.archive, b.archive,
                "recovery archive diverged by estimator for {input:?}"
            );
            // Both archives must still reconstruct the exact original, and each must be
            // decodable by either compressor (recovery ignores the estimator entirely).
            assert_eq!(heuristic.decompress(&b.archive).unwrap(), bytes);
            assert_eq!(exact.decompress(&a.archive).unwrap(), bytes);
        }
    }

    /// The configured estimator genuinely drives *encoder selection*, not merely
    /// the reported id. On inputs sitting near the do-no-harm threshold, swapping
    /// the default heuristic for an exact `tiktoken` tokenizer changes which
    /// encoder wins — in both directions — which is the end-to-end payoff of the
    /// opt-in feature and the claim that
    /// `estimator_choice_does_not_change_recovery_archive` deliberately does *not*
    /// make. Two realistic shapes, verified against the exact `cl100k_base` /
    /// `o200k_base` tables (frozen vocabularies, as stable as the `"hello world"`
    /// anchor test):
    ///
    /// * **False positive** — a small homogeneous array. The heuristic over-counts
    ///   and picks E2 tabular; both exact tokenizers see that hoisting three short,
    ///   already-cheap key sets is not a token win and pass through. The exact gate
    ///   *suppresses* a transform the heuristic wrongly rated as helpful.
    /// * **False negative** — a small pretty-printed config object. The heuristic
    ///   under-counts the per-line indentation and passes through; both exact
    ///   tokenizers charge that whitespace and pick E1 minify. The exact gate
    ///   *enables* a real win the heuristic missed.
    ///
    /// Whichever encoder wins, every estimator still honors do-no-harm
    /// (`token_ratio <= 1.0`) and every archive still reconstructs byte-for-byte
    /// under any config, because recovery ignores the estimator entirely.
    #[cfg(feature = "tiktoken")]
    #[test]
    fn exact_estimator_changes_encoder_selection_in_both_directions() {
        use crate::estimator::{Cl100kEstimator, O200kEstimator};

        let heuristic = Compressor::new(Config::default());
        let cl100k = Compressor::new(
            Config::builder()
                .estimator(Arc::new(Cl100kEstimator::new().unwrap()))
                .build(),
        );
        let o200k = Compressor::new(
            Config::builder()
                .estimator(Arc::new(O200kEstimator::new().unwrap()))
                .build(),
        );

        // False positive: heuristic picks E2 (id 2); both exact tokenizers pass
        // through (id 0). Selection diverges by estimator.
        let pos_input = "[{\"id\":0,\"name\":\"item0\",\"active\":true},{\"id\":1,\"name\":\"item1\",\"active\":true},{\"id\":2,\"name\":\"item2\",\"active\":true}]";
        let pos_heur = heuristic.compress(pos_input.as_bytes()).unwrap();
        let pos_cl = cl100k.compress(pos_input.as_bytes()).unwrap();
        let pos_o2 = o200k.compress(pos_input.as_bytes()).unwrap();
        assert_ne!(
            pos_heur.stats.encoder,
            EncoderId(0),
            "heuristic should pick a real encoder on the small array"
        );
        assert_eq!(
            pos_cl.stats.encoder,
            EncoderId(0),
            "cl100k should suppress it back to passthrough"
        );
        assert_eq!(
            pos_o2.stats.encoder,
            EncoderId(0),
            "o200k should suppress it back to passthrough"
        );
        assert_ne!(
            pos_heur.stats.encoder, pos_cl.stats.encoder,
            "the exact gate must change the winning encoder"
        );

        // False negative: heuristic passes through (id 0); both exact tokenizers
        // pick E1 minify (id 1). Selection diverges the other way.
        let neg_input = "{\n    \"enabled\": true,\n    \"retries\": 3,\n    \"timeout\": 30,\n    \"verbose\": false,\n    \"region\": \"us-east-1\"\n}";
        let neg_heur = heuristic.compress(neg_input.as_bytes()).unwrap();
        let neg_cl = cl100k.compress(neg_input.as_bytes()).unwrap();
        let neg_o2 = o200k.compress(neg_input.as_bytes()).unwrap();
        assert_eq!(
            neg_heur.stats.encoder,
            EncoderId(0),
            "heuristic should pass through the pretty config object"
        );
        assert_eq!(
            neg_cl.stats.encoder,
            EncoderId(1),
            "cl100k should pick E1 minify"
        );
        assert_eq!(
            neg_o2.stats.encoder,
            EncoderId(1),
            "o200k should pick E1 minify"
        );
        assert_ne!(
            neg_heur.stats.encoder, neg_cl.stats.encoder,
            "the exact gate must change the winning encoder"
        );

        // Invariants that hold irrespective of which encoder won: do-no-harm under
        // each estimator, and byte-exact recovery from every archive decoded by
        // any config (recovery never consults the estimator).
        for (input, artifacts) in [
            (pos_input, [&pos_heur, &pos_cl, &pos_o2]),
            (neg_input, [&neg_heur, &neg_cl, &neg_o2]),
        ] {
            for artifact in artifacts {
                assert!(
                    artifact.stats.token_ratio() <= 1.0,
                    "do-no-harm violated for {input:?}"
                );
                for engine in [&heuristic, &cl100k, &o200k] {
                    assert_eq!(
                        engine.decompress(&artifact.archive).unwrap(),
                        input.as_bytes(),
                        "recovery must be byte-exact and estimator-independent for {input:?}"
                    );
                }
            }
        }

        // The reported id attributes each outcome to the estimator that produced it.
        assert_eq!(pos_heur.stats.tokenizer_id, 0);
        assert_eq!(pos_cl.stats.tokenizer_id, 2);
        assert_eq!(pos_o2.stats.tokenizer_id, 3);
    }

    #[test]
    fn a_compressible_input_selects_an_encoder_and_still_reconstructs() {
        let c = compressor();
        // A homogeneous array is already whitespace-free, so E1 cannot win; E2 must
        // hoist its repeated keys. Either way a real encoder wins, and the archive
        // still recovers the exact original.
        let input = homogeneous_array(12);
        let art = c.compress(input.as_bytes()).unwrap();
        assert_ne!(
            art.stats.encoder,
            EncoderId(0),
            "expected a real encoder to win, got passthrough"
        );
        assert!(
            art.stats.token_ratio() < 1.0,
            "expected a token win, ratio {}",
            art.stats.token_ratio()
        );
        assert_eq!(c.decompress(&art.archive).unwrap(), input.as_bytes());
    }

    #[test]
    fn minification_wins_on_pretty_printed_input() {
        let c = compressor();
        let input = pretty_object(30);
        let art = c.compress(input.as_bytes()).unwrap();
        assert_eq!(art.stats.encoder, EncoderId(1), "expected E1 minify to win");
        assert!(art.rendering.starts_with("\u{27E6}tkfd:v1:min\u{27E7}\n"));
        assert_eq!(c.decompress(&art.archive).unwrap(), input.as_bytes());
    }

    #[test]
    fn oversized_input_is_rejected() {
        let cfg = Config::builder().max_input_bytes(4).build();
        let c = Compressor::new(cfg);
        let err = c.compress(b"{\"a\":1}").unwrap_err(); // 7 bytes
        assert!(matches!(
            err,
            CompressError::InputTooLarge { size: 7, limit: 4 }
        ));
    }

    #[test]
    fn non_utf8_input_is_rejected() {
        let c = compressor();
        // 0xFF is never a valid UTF-8 lead byte.
        let err = c.compress(&[0xFF, 0xFE, 0x00]).unwrap_err();
        assert!(matches!(err, CompressError::NotUtf8));
    }

    #[test]
    fn too_deeply_nested_input_is_rejected() {
        let cfg = Config::builder().max_depth(3).build();
        let c = Compressor::new(cfg);
        // Eight nested arrays are comfortably past the depth-3 ceiling.
        let input = nested_arrays(8);
        let err = c.compress(input.as_bytes()).unwrap_err();
        assert!(
            matches!(err, CompressError::DepthExceeded { limit: 3, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn invalid_json_is_rejected() {
        let c = compressor();
        let err = c.compress(b"{\"a\":").unwrap_err();
        assert!(
            matches!(err, CompressError::InvalidJson { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn compression_is_deterministic() {
        let input = homogeneous_array(12);
        let a = compressor().compress(input.as_bytes()).unwrap();
        let b = compressor().compress(input.as_bytes()).unwrap();
        assert_eq!(a.archive, b.archive, "archive bytes are not deterministic");
        assert_eq!(a.rendering, b.rendering, "rendering is not deterministic");
        assert_eq!(a.stats.encoder, b.stats.encoder);
        assert_eq!(a.stats.est_tokens_after, b.stats.est_tokens_after);
    }

    #[test]
    fn decompress_fails_closed_on_corruption() {
        let c = compressor();
        let input = homogeneous_array(6);
        let art = c.compress(input.as_bytes()).unwrap();
        assert_eq!(c.decompress(&art.archive).unwrap(), input.as_bytes());

        // A flipped payload/checksum bit must not decode.
        let mut flipped = art.archive.clone();
        if let Some(last) = flipped.last_mut() {
            *last ^= 0x01;
        }
        assert!(c.decompress(&flipped).is_err(), "flipped bit accepted");

        // A truncated archive must not decode.
        let truncated = &art.archive[..art.archive.len() - 1];
        assert!(
            c.decompress(truncated).is_err(),
            "truncated archive accepted"
        );

        // Bad magic must not decode.
        assert!(matches!(
            c.decompress(b"NOPExxxxxxxx"),
            Err(DecompressError::BadMagic)
        ));
    }

    #[test]
    fn conservative_profile_offers_minify_only() {
        // A homogeneous array is E2's target, but Conservative does not enable E2, so
        // it stays passthrough (the array is already whitespace-free, so E1 can't win).
        let cfg = Config::builder().profile(Profile::Conservative).build();
        let c = Compressor::new(cfg);
        let input = homogeneous_array(12);
        let art = c.compress(input.as_bytes()).unwrap();
        assert_eq!(art.stats.encoder, EncoderId(0));
        // Balanced (default) does enable E2 and compresses the same input.
        let balanced = compressor().compress(input.as_bytes()).unwrap();
        assert_ne!(balanced.stats.encoder, EncoderId(0));
    }

    #[test]
    fn config_defaults_match_the_spec() {
        let cfg = Config::default();
        assert_eq!(cfg.profile, Profile::Balanced);
        assert_eq!(cfg.max_input_bytes, 16 * 1024 * 1024);
        assert_eq!(cfg.max_depth, 512);
        assert_eq!(cfg.estimator.tokenizer_id(), 0); // HeuristicEstimator
    }

    /// The estimator is a *selection* input, not an archive input. Swapping the cost
    /// model can change which encoder wins the candidate rule, but the archive is a
    /// passthrough recovery blob over the original bytes either way — so it stays
    /// byte-identical, and either compressor reconstructs the other's archive exactly.
    #[test]
    fn a_different_estimator_never_changes_the_archive() {
        let heuristic = compressor();
        let byte_len = Compressor::new(
            Config::builder()
                .estimator(Arc::new(crate::estimator::ByteLenEstimator))
                .build(),
        );

        for input in corpus() {
            let h = heuristic.compress(input.as_bytes()).unwrap();
            let b = byte_len.compress(input.as_bytes()).unwrap();

            assert_eq!(
                h.archive, b.archive,
                "archive diverged under a different estimator for {input:?}"
            );

            // The configured cost model is reported out of band on `Stats`; the header
            // records 0 whichever estimator drove selection.
            assert_eq!(h.stats.tokenizer_id, 0, "heuristic id for {input:?}");
            assert_eq!(b.stats.tokenizer_id, 1, "byte-len id for {input:?}");

            // Neither archive is bound to the compressor that produced it.
            assert_eq!(
                heuristic.decompress(&b.archive).unwrap(),
                input.as_bytes(),
                "heuristic could not expand the byte-len archive for {input:?}"
            );
            assert_eq!(
                byte_len.decompress(&h.archive).unwrap(),
                input.as_bytes(),
                "byte-len could not expand the heuristic archive for {input:?}"
            );
        }
    }

    /// `Compressor` is sans-io and shareable: callers are expected to hold one behind
    /// an `Arc` and compress from several threads. This fails to compile if either type
    /// loses the bound — for instance if the boxed estimator stopped requiring it.
    #[test]
    fn compressor_and_config_are_send_and_sync() {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Compressor>();
        assert_send_sync::<Config>();
    }

    /// The profile -> candidate-set map is part of the selection contract, so pin it
    /// directly instead of inferring it from one lucky input: swapping
    /// `Aggressive` onto the conservative set was previously invisible, because no
    /// test ever built an `Aggressive` compressor.
    #[test]
    fn enabled_encoders_are_frozen_per_profile() {
        assert_eq!(
            enabled_encoders(Profile::Conservative),
            [Encoder::E1Minify].as_slice()
        );
        assert_eq!(
            enabled_encoders(Profile::Balanced),
            [Encoder::E1Minify, Encoder::E2Tabular].as_slice()
        );
        assert_eq!(
            enabled_encoders(Profile::Aggressive),
            enabled_encoders(Profile::Balanced),
            "Aggressive is documented as identical to Balanced until E3 lands"
        );
    }

    /// The order of the candidate slice is pinned above; this proves the pin is a
    /// determinism guard and not a behavioural claim. `prefers` breaks an estimate
    /// tie on the lower encoder id, so reversing the slice must select the same
    /// encoder and emit the same bytes.
    #[test]
    fn selection_does_not_depend_on_candidate_order() {
        for input in corpus() {
            let parsed = tape::parse(&input, DEFAULT_MAX_DEPTH).unwrap();
            let estimator = HeuristicEstimator;
            let forward = encoder::select(
                &parsed,
                &input,
                &estimator,
                &[Encoder::E1Minify, Encoder::E2Tabular],
                0,
            );
            let reversed = encoder::select(
                &parsed,
                &input,
                &estimator,
                &[Encoder::E2Tabular, Encoder::E1Minify],
                0,
            );
            assert_eq!(forward.encoder, reversed.encoder, "on {input:?}");
            assert_eq!(forward.rendering, reversed.rendering, "on {input:?}");
        }
    }

    /// Aggressive must actually compress what Balanced compresses — the assertion the
    /// profile map above cannot make on its own.
    #[test]
    fn aggressive_profile_matches_balanced_byte_for_byte() {
        let input = homogeneous_array(12);
        let cfg = Config::builder().profile(Profile::Aggressive).build();
        let aggressive = Compressor::new(cfg).compress(input.as_bytes()).unwrap();
        let balanced = compressor().compress(input.as_bytes()).unwrap();
        assert_eq!(aggressive.stats.encoder, EncoderId::E2_TABULAR);
        assert_eq!(aggressive.rendering, balanced.rendering);
        assert_eq!(aggressive.archive, balanced.archive);
    }

    /// `Config::builder()` is documented as "seeded with the defaults", so a
    /// freshly built config must behave exactly like `Config::default()` — and the
    /// seed must be the *documented* values, not merely self-consistent ones.
    ///
    /// The suite builds configs both ways all over the place but never states that
    /// the two paths agree, and it never observes the shipped ceilings at all: only
    /// deliberately shrunk ones (`max_input_bytes(4)`, `max_depth(3)`) are probed,
    /// so the real 16 MiB / 512 defaults were unpinned.
    #[test]
    fn builder_seed_is_the_documented_default_config() {
        let built = Compressor::new(Config::builder().build());
        let defaulted = Compressor::new(Config::default());
        for input in corpus() {
            let a = built.compress(input.as_bytes()).unwrap();
            let b = defaulted.compress(input.as_bytes()).unwrap();
            assert_eq!(a.stats.encoder, b.stats.encoder, "encoder on {input:?}");
            assert_eq!(a.rendering, b.rendering, "rendering on {input:?}");
            assert_eq!(a.archive, b.archive, "archive on {input:?}");
        }

        // Seeded profile is Balanced, so E2 competes; Conservative would pick E1.
        let table = homogeneous_array(12);
        assert_eq!(
            built.compress(table.as_bytes()).unwrap().stats.encoder,
            EncoderId::E2_TABULAR,
            "the default profile must offer the tabular encoder"
        );

        // Seeded byte ceiling is 16 MiB; the error reports the limit it applied.
        let over = vec![b' '; (16 * 1024 * 1024) + 1];
        let err = built.compress(&over).unwrap_err();
        assert!(
            matches!(
                err,
                CompressError::InputTooLarge {
                    size: 16_777_217,
                    limit: 16_777_216
                }
            ),
            "got {err:?}"
        );

        // Seeded depth ceiling is 512: 512 levels parse, 513 do not.
        assert!(
            built.compress(nested_arrays(512).as_bytes()).is_ok(),
            "512 nested levels must be within the default ceiling"
        );
        let deep = built.compress(nested_arrays(513).as_bytes()).unwrap_err();
        assert!(
            matches!(
                deep,
                CompressError::DepthExceeded {
                    depth: 513,
                    limit: 512
                }
            ),
            "got {deep:?}"
        );
    }

    /// The associated constants are the only way a downstream crate can name an
    /// encoder (`EncoderId` is `#[non_exhaustive]`), so their numeric values are
    /// public API. The rest of the suite compares against `EncoderId(0)` / `(1)`
    /// literals, which leaves the constants themselves unpinned.
    #[test]
    fn encoder_id_constants_are_the_wire_ids() {
        assert_eq!(EncoderId::PASSTHROUGH.0, 0);
        assert_eq!(EncoderId::E1_MINIFY.0, 1);
        assert_eq!(EncoderId::E2_TABULAR.0, 2);

        let c = compressor();
        assert_eq!(
            c.compress(b"42").unwrap().stats.encoder,
            EncoderId::PASSTHROUGH
        );
        assert_eq!(
            c.compress(pretty_object(30).as_bytes())
                .unwrap()
                .stats
                .encoder,
            EncoderId::E1_MINIFY
        );
        assert_eq!(
            c.compress(homogeneous_array(12).as_bytes())
                .unwrap()
                .stats
                .encoder,
            EncoderId::E2_TABULAR
        );
    }

    #[test]
    fn the_applied_margin_is_reported_and_clamped_at_one_hundred_percent() {
        let input = pretty_object(30);
        let stats = |bps: u32| {
            Compressor::new(Config::builder().min_saving_bps(bps).build())
                .compress(input.as_bytes())
                .unwrap()
                .stats
        };
        // Below the ceiling the configured margin is reported verbatim.
        assert_eq!(stats(600).min_saving_bps, 600);
        // Above it the margin is clamped, never passed through: a margin over 100%
        // is unsatisfiable, so it would silently disable compression instead.
        assert_eq!(MAX_SAVING_BPS, 10_000);
        assert_eq!(stats(50_000).min_saving_bps, MAX_SAVING_BPS);
        assert_eq!(stats(50_000).encoder, EncoderId::PASSTHROUGH);
        // Unset, the reported margin is the estimator's declaration — 0 in v0.0.1,
        // which is what keeps E1's win on this input.
        let default_stats = compressor().compress(input.as_bytes()).unwrap().stats;
        assert_eq!(default_stats.min_saving_bps, 0);
        assert_eq!(default_stats.encoder, EncoderId::E1_MINIFY);
    }

    /// A custom estimator must be the one actually consulted, must have its declared
    /// over-claim adopted as the default margin, and must be named in `Stats`. None
    /// of the three was covered: no test in the workspace built a `Config` with a
    /// non-default estimator.
    #[test]
    fn the_configured_estimator_drives_and_labels_selection() {
        /// The shipped heuristic, but declaring a 90% error budget.
        #[derive(Debug)]
        struct CautiousHeuristic;
        impl TokenEstimator for CautiousHeuristic {
            fn estimate(&self, text: &str) -> usize {
                HeuristicEstimator.estimate(text)
            }
            fn tokenizer_id(&self) -> u16 {
                4242
            }
            fn over_claim_bps(&self) -> u32 {
                9_000
            }
        }

        let input = pretty_object(30);
        let cfg = Config::builder()
            .estimator(Arc::new(CautiousHeuristic))
            .build();
        let stats = Compressor::new(cfg)
            .compress(input.as_bytes())
            .unwrap()
            .stats;
        assert_eq!(
            stats.tokenizer_id, 4242,
            "Stats must report the configured estimator, not the default"
        );
        assert_eq!(
            stats.min_saving_bps, 9_000,
            "an unset margin must fall back to the estimator's declared over-claim"
        );
        assert_eq!(
            stats.encoder,
            EncoderId::PASSTHROUGH,
            "a 90% bar must refuse E1's ~11% win on this input"
        );
    }

    /// The size guard is `>`, so an input of exactly `max_input_bytes` is accepted.
    /// `oversized_input_is_rejected` only probes 7 bytes against a limit of 4, which
    /// leaves the boundary itself untested in both directions.
    #[test]
    fn input_of_exactly_the_ceiling_is_accepted() {
        let input = b"{\"a\":1}"; // 7 bytes
        let at_limit = Config::builder().max_input_bytes(input.len()).build();
        assert!(
            Compressor::new(at_limit).compress(input).is_ok(),
            "an input of exactly max_input_bytes must be accepted"
        );
        let below = Config::builder().max_input_bytes(input.len() - 1).build();
        assert!(matches!(
            Compressor::new(below).compress(input),
            Err(CompressError::InputTooLarge { size: 7, limit: 6 })
        ));
    }

    /// `decompress` gates each fixed header field separately and aims a `Corrupt` at
    /// the field it rejected. The suite proved the *rejection* but never the offset,
    /// so all three field offsets could be shifted silently.
    #[test]
    fn decompress_names_the_header_field_it_rejected() {
        // Frozen TKFD layout: magic[0..4] | version[4] | encoder_id[5] |
        // tokenizer_id[6..8] | flags[8..10] | original_len varint | checksum[32].
        assert_eq!(ENCODER_ID_OFFSET, 5);
        assert_eq!(TOKENIZER_ID_OFFSET, 6);
        assert_eq!(FLAGS_OFFSET, 8);
        // `ORIGINAL_LEN_OFFSET` aims the `Corrupt` for an `original_len` that does not
        // fit a `usize`. That conversion cannot fail where `usize` is 64 bits, so the
        // branch is unreachable on every target this workspace builds for and no test
        // can exercise it. Pinning the constant is a layout guard only — it is NOT
        // behavioural coverage of that branch.
        assert_eq!(ORIGINAL_LEN_OFFSET, 10);

        let c = compressor();
        let art = c.compress(b"{\"a\":1}").unwrap();
        let corrupt_at = |index: usize, value: u8| {
            let mut a = art.archive.clone();
            a[index] = value;
            c.decompress(&a)
        };
        assert!(
            matches!(
                corrupt_at(ENCODER_ID_OFFSET, 1),
                Err(DecompressError::Corrupt { byte_offset: 5 })
            ),
            "encoder_id"
        );
        assert!(
            matches!(
                corrupt_at(TOKENIZER_ID_OFFSET, 1),
                Err(DecompressError::Corrupt { byte_offset: 6 })
            ),
            "tokenizer_id, low byte"
        );
        assert!(
            matches!(
                corrupt_at(TOKENIZER_ID_OFFSET + 1, 1),
                Err(DecompressError::Corrupt { byte_offset: 6 })
            ),
            "tokenizer_id, high byte: the offset names the field, not the byte"
        );
        // Bit 0 of `flags` is a defined flag (`has_sidecar`), so the header decodes
        // and is then refused by the metadata gate rather than as a reserved bit.
        assert!(
            matches!(
                corrupt_at(FLAGS_OFFSET, 0b0001),
                Err(DecompressError::Corrupt { byte_offset: 8 })
            ),
            "flags"
        );
    }

    /// A payload that does not match the length the header claims is an integrity
    /// failure, not framing corruption. `decompress_fails_closed_on_corruption` only
    /// asserts `is_err()`, so the variant was free to change.
    #[test]
    fn a_payload_length_mismatch_is_reported_as_a_checksum_mismatch() {
        let c = compressor();
        let art = c.compress(homogeneous_array(6).as_bytes()).unwrap();

        let truncated = &art.archive[..art.archive.len() - 1];
        assert!(
            matches!(
                c.decompress(truncated),
                Err(DecompressError::ChecksumMismatch)
            ),
            "a short payload"
        );

        let mut overlong = art.archive.clone();
        overlong.push(0x00);
        assert!(
            matches!(
                c.decompress(&overlong),
                Err(DecompressError::ChecksumMismatch)
            ),
            "a long payload"
        );
    }

    /// `Stats` is never built with a zero denominator by `compress` (empty input is
    /// invalid JSON), so the ratio guards and their orientation were unreachable from
    /// the public path and completely untested. Build `Stats` directly.
    #[test]
    fn ratios_are_after_over_before_and_report_no_change_on_a_zero_denominator() {
        let mk = |bytes_before, bytes_after, est_before, est_after| Stats {
            bytes_before,
            bytes_after,
            est_tokens_before: est_before,
            est_tokens_after: est_after,
            encoder: EncoderId::PASSTHROUGH,
            tokenizer_id: 0,
            min_saving_bps: 0,
            fidelity: Fidelity::Lossless,
        };
        // Orientation: after / before, so a real saving reads below 1.
        assert_eq!(mk(100, 25, 80, 20).byte_ratio(), 0.25);
        assert_eq!(mk(100, 25, 80, 20).token_ratio(), 0.25);
        // A zero denominator reports "no change" — never 0.0, which would read as a
        // 100% saving, and never NaN.
        assert_eq!(mk(0, 0, 0, 0).byte_ratio(), 1.0);
        assert_eq!(mk(0, 0, 0, 0).token_ratio(), 1.0);
    }

    /// The passthrough rendering is the input *plus* an 18-byte sentinel line, so the
    /// text handed to the model is strictly more expensive than the input — while
    /// `Stats` reports both ratios as exactly `1.0`.
    ///
    /// This test pins that measurement; it takes no position on whether the reporting
    /// should change, which is an open product decision.
    #[test]
    fn passthrough_framing_costs_tokens_the_stats_do_not_report() {
        let c = compressor();
        let est = HeuristicEstimator;
        // The framing costs 10 estimated tokens, or 11 when the input opens with
        // exactly one whitespace character: it merges with the sentinel's trailing
        // newline into a two-character run, which the heuristic charges where a
        // one-character run is free.
        for (input, extra_tokens) in [("{\"a\":1}", 10), (" {\"a\":1}", 11)] {
            let art = c.compress(input.as_bytes()).unwrap();
            assert_eq!(art.stats.encoder, EncoderId::PASSTHROUGH, "{input:?}");
            assert_eq!(
                art.rendering,
                format!("\u{27E6}tkfd:v1:raw\u{27E7}\n{input}")
            );
            // What Stats says: nothing changed.
            assert_eq!(art.stats.est_tokens_after, art.stats.est_tokens_before);
            assert_eq!(art.stats.token_ratio(), 1.0);
            assert_eq!(art.stats.byte_ratio(), 1.0);
            // What the model is actually charged.
            assert_eq!(art.rendering.len(), input.len() + 18, "{input:?}");
            assert_eq!(
                est.estimate(&art.rendering),
                est.estimate(input) + extra_tokens,
                "framing cost for {input:?}"
            );
        }
    }
}
