//! Token estimation for encoder selection.
//!
//! Encoder selection is a comparison, not a measurement: the candidate rule keeps a
//! rendering only when [`TokenEstimator::estimate`] rates it below the original.
//! Two properties of this layer are therefore load-bearing.
//!
//! * **Purity.** `estimate` is a pure function of its argument — no clock, no global
//!   state, no hashing with a per-process seed. A nondeterministic estimator selects
//!   a different encoder from run to run, which produces different output bytes for
//!   the same input bytes and silently invalidates the provider's prompt cache.
//!   Determinism here is what keeps that cache warm.
//! * **Objective.** The estimator must count *tokens*, not bytes. Selecting an
//!   encoder to minimize bytes optimizes the wrong quantity — see
//!   [`ByteLenEstimator`], which exists only as a reference point and is never the
//!   default.
//!
//! The chosen estimator's [`TokenEstimator::tokenizer_id`] is reported out-of-band in
//! [`Stats::tokenizer_id`](crate::Stats::tokenizer_id); it is **not** written to the
//! archive. In v0.0.1 the recovery archive is a passthrough blob whose header
//! `tokenizer_id` field (`format` field 4) is a literal `0` written regardless of which
//! estimator ran, and the decoder fails closed on any other value. This version
//! therefore does not use that field to name a cost model at all: **no** id from
//! [`ids`] can be inferred from an archive — [`ids::HEURISTIC`] included, even though
//! it happens to be `0`. [`Stats::tokenizer_id`](crate::Stats::tokenizer_id) is the
//! only record of which model selected the encoding.
//!
//! # Declared calibration error
//!
//! An estimator may also declare how far it is known to over-claim, via
//! [`TokenEstimator::over_claim_bps`]. The candidate rule then refuses a rendering
//! whose claimed saving does not clear that margin. The mechanism is what lets an
//! inexact cost model state its own error budget once instead of every caller
//! re-deriving it; see [`ConfigBuilder::min_saving_bps`](crate::ConfigBuilder::min_saving_bps)
//! to override it per configuration.

/// A cost model that rates text in tokens for encoder selection.
///
/// Implementations MUST be pure and deterministic: the candidate rule calls
/// `estimate` on every encoder's output, so a nondeterministic estimator yields
/// nondeterministic selection and defeats prompt caching. The trait is the
/// only public extension point in the crate; a caller may supply its own exact
/// tokenizer, but a stateful or seeded one violates the contract.
pub trait TokenEstimator: Send + Sync {
    /// Estimate the token count of `text`. Pure: equal input yields equal output.
    fn estimate(&self, text: &str) -> usize;

    /// Stable id reported out-of-band in
    /// [`Stats::tokenizer_id`](crate::Stats::tokenizer_id) — the record of which cost
    /// model selected the encoding.
    ///
    /// It is **not** written to the archive: the header's `tokenizer_id` (`format`
    /// field 4) is always `0` in v0.0.1. Ids are frozen in [`ids`]; a given estimator
    /// must always return the same one.
    fn tokenizer_id(&self) -> u16;

    /// This model's declared one-sided over-claim, in basis points of the input
    /// estimate. Default: `0`.
    ///
    /// The candidate rule keeps a rendering only when its *claimed* saving exceeds
    /// this margin, so an estimator that is known to rate its own output too cheaply
    /// can state that error budget once rather than leaving every caller to re-derive
    /// it. `0` means "no declared error": the gate then keeps any strict token win,
    /// which is exactly the v0.0.1 rule. An estimator that is exact for its target
    /// tokenizer should keep the default — its comparison is already sound.
    ///
    /// The value is empirical, never a proof. A margin shrinks the band of inputs
    /// where a mis-estimate can keep a token-losing rendering; with an inexact model
    /// it cannot close that band, because the calibration error is two-sided and a
    /// margin large enough to cover the worst over-claim also discards genuine wins.
    ///
    /// This is a *provided* method on purpose. [`TokenEstimator`] is the crate's only
    /// public extension point, so a required method would break every third-party
    /// implementor on upgrade.
    fn over_claim_bps(&self) -> u32 {
        0
    }
}

/// Frozen ids naming the cost model that drove selection, reported in
/// [`Stats::tokenizer_id`](crate::Stats::tokenizer_id).
///
/// They are the reserved value space of the header's `tokenizer_id` field: once
/// shipped, a value's meaning never changes. v0.0.1 does not yet use that field to
/// name a cost model — the compressor writes a literal `0` whatever the estimator is,
/// and `decompress` rejects every other value — so none of these ids can be read back
/// out of an archive, `HEURISTIC` included: its `0` and the header's `0` are unrelated
/// values that happen to coincide. They are meanings the field may carry in a later
/// version. Ids `0` and `1` are always available;
/// `2` and `3` name an implementation that ships only under feature `tiktoken`; `4` is
/// a reserved name with no implementation in v0.0.1.
pub mod ids {
    /// [`super::HeuristicEstimator`] — the default cost model.
    pub const HEURISTIC: u16 = 0;

    /// [`super::ByteLenEstimator`] — reference only, never the default.
    pub const BYTE_LEN: u16 = 1;

    /// The `cl100k_base` tokenizer (GPT-3.5 / GPT-4). Implemented by
    /// `Cl100kEstimator` under feature `tiktoken`; the id itself is always defined.
    pub const CL100K_BASE: u16 = 2;

    /// The `o200k_base` tokenizer (GPT-4o). Implemented by `O200kEstimator` under
    /// feature `tiktoken`; the id itself is always defined.
    pub const O200K_BASE: u16 = 3;

    /// Reserved: a Hugging Face tokenizer (feature `hf`). Not implemented in v0.0.1.
    ///
    /// Any future implementation MUST embed its vocabulary at compile time, never load
    /// `tokenizer.json` from disk or network, to preserve the crate's sans-io guarantee.
    pub const HUGGING_FACE: u16 = 4;
}

// --- Heuristic model constants (FORMAT-AFFECTING) ---------------------------
//
// These constants are part of the encoding contract, not tuning knobs. Encoder
// selection consults the heuristic, so changing any of them can change which
// encoder wins and therefore the produced bytes for a given input. Recalibrating
// them is a format change: it must travel with an `encoder_id` semantics bump, never
// as a silent in-place edit.

/// Alphanumeric density in tenths of a character per token: 37 tenths = 3.7
/// ASCII alphanumeric characters per token, the approximate mean `cl100k` packs
/// into one BPE token for identifiers and prose.
const ALPHANUMERIC_CHARS_PER_TOKEN_TENTHS: usize = 37;

/// Fixed-point scale paired with [`ALPHANUMERIC_CHARS_PER_TOKEN_TENTHS`] so the
/// ratio is evaluated in integer arithmetic (no float rounding, fully
/// deterministic).
const TENTHS_SCALE: usize = 10;

/// Shortest whitespace run that costs a token. A lone separator (one space) is
/// free — `cl100k` folds it into the adjacent word token — while a run of two or
/// more (indentation, blank lines) maps to a dedicated indentation token.
const WHITESPACE_RUN_MIN_TOKENIZED: usize = 2;

/// Token cost of a single ASCII punctuation, symbol or control byte.
const PUNCTUATION_TOKEN_WEIGHT: usize = 1;

/// Bytes per token charged to non-ASCII runs. Multi-byte UTF-8 fragments into
/// several BPE tokens, so each such byte is dearer than an ASCII alphanumeric one.
const NON_ASCII_BYTES_PER_TOKEN: usize = 2;

/// Token contribution of an ASCII alphanumeric run of `len` characters.
///
/// `ceil(len / 3.7)`, computed in integer fixed point. An empty run costs nothing;
/// any non-empty run costs at least one token. `saturating_mul` keeps a
/// pathologically long run from overflowing rather than panicking.
const fn alphanumeric_run_tokens(len: usize) -> usize {
    len.saturating_mul(TENTHS_SCALE)
        .div_ceil(ALPHANUMERIC_CHARS_PER_TOKEN_TENTHS)
}

/// Token contribution of a whitespace run of `len` characters: one token once the
/// run reaches [`WHITESPACE_RUN_MIN_TOKENIZED`], nothing below it.
const fn whitespace_run_tokens(len: usize) -> usize {
    if len >= WHITESPACE_RUN_MIN_TOKENIZED {
        1
    } else {
        0
    }
}

/// Token contribution of a non-ASCII run of `bytes` UTF-8 bytes: `ceil(bytes / 2)`,
/// zero for an empty run.
const fn non_ascii_run_tokens(bytes: usize) -> usize {
    bytes.div_ceil(NON_ASCII_BYTES_PER_TOKEN)
}

/// The default cost model: a pure, dependency-free scanner approximating BPE.
///
/// The scan classifies each character into one of four runs and charges each run
/// as it ends:
///
/// * **ASCII alphanumeric** — `ceil(len / 3.7)` tokens (at least one), the density
///   `cl100k` achieves on identifiers and words.
/// * **Whitespace** — free for a lone separator, one token for a run of two or more
///   (`cl100k` has dedicated indentation tokens); see `WHITESPACE_RUN_MIN_TOKENIZED`.
/// * **ASCII punctuation, symbols and controls** — one token each.
/// * **Non-ASCII** — `ceil(bytes / 2)`, reflecting multi-byte UTF-8 fragmenting
///   into several BPE tokens.
///
/// The constants above are FORMAT-AFFECTING (see their docs): they influence
/// encoder selection, so they are named and frozen rather than inlined.
///
/// **Accuracy.** This is a *relative* signal, not an absolute token count, and the
/// two must not be confused. Measured against `cl100k` on the reference corpus of
/// agent tool output, it over-counts absolute tokens substantially: roughly +67%
/// size-weighted over the rows the engine compresses, and between +30% and +127% per
/// fixture. Every short run rounds up to a whole token, and natural-language prose is
/// over-counted the most, because `cl100k` packs many common words together with
/// their leading space.
///
/// The sign is not fixed, and a reader who reads the paragraph above as a ceiling will
/// be wrong by about a factor of two. Over-counting comes from content a BPE tokenizer
/// packs and this scanner does not. High-entropy string values go the other way: there
/// is no merge for a random hex digest, so the tokenizer spends far more than
/// `ceil(len / 3.7)` on one. Across the survey in
/// `the_heuristic_bias_is_two_sided_the_way_the_docs_say_it_is` — six content families,
/// four sizes, both tokenizers — exactly half of the 48 cases *under*-count, by 18% to
/// 49%: SHA-256 digests sit at −43%, base64 blobs at −37% to −49% and UUIDs at −18% to
/// −21%, while prose runs +63% to +76%, word-shaped key/value objects +87% to +89% and
/// file paths +122% to +124%. `estimate` is therefore neither an upper nor a lower
/// bound on real tokens.
///
/// What the engine relies on is narrower and does hold: selection compares two
/// estimates produced by this same model, so most of that bias cancels. The residual
/// one-sided error on the reported *saving* is measured and stated as
/// [`HeuristicEstimator::MEASURED_OVER_CLAIM_BPS`]. Do not use `estimate` anywhere an
/// absolute token count matters — bill, budget or context-window arithmetic — use an
/// exact tokenizer for that.
///
/// **Purity.** `estimate` reads only its argument and uses only integer arithmetic,
/// so it is deterministic across processes — a precondition for prompt-cache-safe
/// output.
#[derive(Debug, Default, Clone, Copy)]
pub struct HeuristicEstimator;

impl HeuristicEstimator {
    /// The measured one-sided over-claim of this model against real BPE tokenizers,
    /// in basis points: **600 bps = 6.00 percentage points**.
    ///
    /// Derived from the reference corpus of agent tool output (11 fixtures, real
    /// `cl100k_base` and `o200k_base` counts): size-weighted over the rows this
    /// engine actually compresses, the heuristic claims a 32.4% saving where the real
    /// saving is 26.4%. The floor is not a modelling accident — sweeping the cost
    /// constants bottoms out near +2.8 pp, so the residual is structural.
    ///
    /// The error is **two-sided**, which is why this is not simply "the" safe margin:
    /// per-fixture it ranges from −20.8 pp (the heuristic under-claims, i.e. a real
    /// win is discarded) to +18.9 pp. 600 bps is the aggregate over-claim, chosen
    /// because it rejects no fixture in the reference corpus while removing the
    /// decisions most at risk of flipping sign, and because a margin much above this
    /// would retire E1 outright on the commonest real shape.
    ///
    /// "The commonest real shape" is canonical 4-space-indented JSON, and it does not
    /// have one saving — it has a different plateau per shape, which is why 600 bps
    /// is already close to the edge rather than comfortably inside it. Measured
    /// through [`Compressor`](crate::Compressor) with this estimator, the minify-only
    /// [`Profile::Conservative`](crate::Profile::Conservative) and no margin
    /// (`min_saving_bps(0)`), claimed saving by shape:
    ///
    /// | shape | claimed saving |
    /// |---|---|
    /// | flat object, 40 keys (1 KB) | 5.35% |
    /// | flat object, 200–5 000 keys | 6.4%–7.1% |
    /// | array of row objects, 10–2 000 rows | 13.1%–16.0% |
    /// | nested 4 levels | 1.96% |
    /// | nested 8–30 levels | 13.1%–21.8% |
    ///
    /// The flat-object plateau is the binding one: it clears a 6.00 pp margin by
    /// 0.4–1.1 pp, and the 40-key document below it does not clear it at all — at
    /// 600 bps that input, and a shallow 4-level nesting, fall back to passthrough
    /// while every other row above keeps E1. Small documents never reach the
    /// question: they take passthrough at any margin, because minification loses the
    /// strict comparison by the width of the sentinel. Every "keeps E1" here is a
    /// statement about that profile: under the default
    /// [`Profile::Balanced`](crate::Profile::Balanced) the array-of-rows shape goes to
    /// E2 instead, and the margin weighs E2's saving, not E1's.
    /// `the_600_bps_bar_sits_just_above_the_flat_object_plateau` pins the table and
    /// this paragraph, including which rows keep E1 at the bar. The corpus figures
    /// above the table are measurements over the reference corpus, and no test
    /// recomputes them.
    ///
    /// Not applied by default: [`TokenEstimator::over_claim_bps`] returns `0` for
    /// this estimator so v0.0.1 selection is byte-for-byte unchanged. Opt in with
    /// [`ConfigBuilder::min_saving_bps`](crate::ConfigBuilder::min_saving_bps).
    /// Making it the default would change which encoder wins for a band of inputs
    /// and is therefore a format change, not a tuning tweak — see the FORMAT-AFFECTING
    /// note on the cost constants above.
    pub const MEASURED_OVER_CLAIM_BPS: u32 = 600;
}

impl TokenEstimator for HeuristicEstimator {
    fn estimate(&self, text: &str) -> usize {
        // At most one run accumulator is non-zero at a time (runs are mutually
        // exclusive by class). A branch that starts a run of one class flushes the
        // other two; flushing an empty accumulator contributes zero.
        let mut total: usize = 0;
        let mut alphanumeric_run: usize = 0;
        let mut whitespace_run: usize = 0;
        let mut non_ascii_bytes: usize = 0;

        for ch in text.chars() {
            if ch.is_ascii_alphanumeric() {
                total += whitespace_run_tokens(whitespace_run);
                whitespace_run = 0;
                total += non_ascii_run_tokens(non_ascii_bytes);
                non_ascii_bytes = 0;
                alphanumeric_run += 1;
            } else if ch.is_ascii_whitespace() {
                total += alphanumeric_run_tokens(alphanumeric_run);
                alphanumeric_run = 0;
                total += non_ascii_run_tokens(non_ascii_bytes);
                non_ascii_bytes = 0;
                whitespace_run += 1;
            } else if ch.is_ascii() {
                total += alphanumeric_run_tokens(alphanumeric_run);
                alphanumeric_run = 0;
                total += whitespace_run_tokens(whitespace_run);
                whitespace_run = 0;
                total += non_ascii_run_tokens(non_ascii_bytes);
                non_ascii_bytes = 0;
                total += PUNCTUATION_TOKEN_WEIGHT;
            } else {
                total += alphanumeric_run_tokens(alphanumeric_run);
                alphanumeric_run = 0;
                total += whitespace_run_tokens(whitespace_run);
                whitespace_run = 0;
                non_ascii_bytes += ch.len_utf8();
            }
        }

        total += alphanumeric_run_tokens(alphanumeric_run);
        total += whitespace_run_tokens(whitespace_run);
        total += non_ascii_run_tokens(non_ascii_bytes);
        total
    }

    fn tokenizer_id(&self) -> u16 {
        ids::HEURISTIC
    }
}

/// Byte-length estimator: reports `text.len()`. Reference implementation only.
///
/// This is **never** the default and must not be used for encoder selection. Byte
/// reduction is not token reduction: selecting an encoder to minimize
/// [`estimate`](TokenEstimator::estimate) here would optimize byte count while the
/// engine is paid to cut *tokens*. Whitespace stripping shrinks bytes without
/// touching many tokens, and a legend fold can add bytes while removing tokens — so
/// a byte-length objective would both over- and under-reward the wrong encoders.
/// It is retained as a deterministic baseline for tests and as a worked example of
/// the objective the engine deliberately does not optimize.
#[derive(Debug, Default, Clone, Copy)]
pub struct ByteLenEstimator;

impl TokenEstimator for ByteLenEstimator {
    fn estimate(&self, text: &str) -> usize {
        text.len()
    }

    fn tokenizer_id(&self) -> u16 {
        ids::BYTE_LEN
    }
}

/// Exact tokenizer-backed estimators, compiled only under feature `tiktoken`.
///
/// These count tokens with a real BPE model instead of approximating one, so the
/// do-no-harm gate they drive cannot greenlight a real-token loser for the model
/// family they cover — the gap the [heuristic estimator](super::HeuristicEstimator)
/// can leave. They are opt-in: each constructor loads megabytes of embedded BPE data,
/// which is why the heuristic, not these, is the default (see the crate feature note).
/// Adding one changes only which rendering is selected and the reported
/// [`TokenEstimator::tokenizer_id`]; the recovery archive is unaffected, since v0.0.1
/// writes a literal `0` into the header's `tokenizer_id` field regardless of the
/// selector. That `0` names no cost model — an archive produced with `Cl100kEstimator`
/// is header-identical to one produced with the heuristic.
#[cfg(feature = "tiktoken")]
mod exact {
    use super::{TokenEstimator, ids};
    use tiktoken_rs::CoreBPE;

    /// The embedded BPE tables of an exact tokenizer failed to build.
    ///
    /// Construction is the only fallible step; [`TokenEstimator::estimate`] cannot
    /// fail once an estimator exists.
    #[derive(Debug, thiserror::Error)]
    #[error("failed to load the {tokenizer} tokenizer: {message}")]
    pub struct TokenizerLoadError {
        tokenizer: &'static str,
        message: String,
    }

    /// Count the tokens of `text` as ordinary content.
    ///
    /// `encode_ordinary` treats the whole input as literal text: a substring that
    /// looks like a special token (`<|endoftext|>`) is counted as the bytes a
    /// provider actually sends in message content, never collapsed to a single
    /// special token. That is both the honest count for a rendering and panic-free —
    /// there is no fallible path here at all, which is what the workspace's `panic`,
    /// `unwrap_used` and `expect_used` clippy denies ask for. Those are lints on
    /// explicit panicking constructs, not a proof that this crate cannot panic: no such
    /// proof exists, which is why the MCP stdio loop keeps a `catch_unwind` bulkhead.
    fn count(bpe: &CoreBPE, text: &str) -> usize {
        bpe.encode_ordinary(text).len()
    }

    /// Exact `cl100k_base` tokenizer (GPT-3.5 / GPT-4), [`ids::CL100K_BASE`].
    ///
    /// Ground truth for the GPT family rather than an approximation, so unlike
    /// [`HeuristicEstimator`](super::HeuristicEstimator) the gate it drives cannot
    /// over- or under-count that family. Opt-in only, under feature `tiktoken`.
    pub struct Cl100kEstimator {
        bpe: CoreBPE,
    }

    impl Cl100kEstimator {
        /// Load the `cl100k_base` BPE tables and build the estimator.
        ///
        /// # Errors
        ///
        /// Returns [`TokenizerLoadError`] if the embedded tables fail to build.
        pub fn new() -> Result<Self, TokenizerLoadError> {
            let bpe = tiktoken_rs::cl100k_base().map_err(|e| TokenizerLoadError {
                tokenizer: "cl100k_base",
                message: e.to_string(),
            })?;
            Ok(Self { bpe })
        }
    }

    impl core::fmt::Debug for Cl100kEstimator {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("Cl100kEstimator").finish_non_exhaustive()
        }
    }

    impl TokenEstimator for Cl100kEstimator {
        fn estimate(&self, text: &str) -> usize {
            count(&self.bpe, text)
        }

        fn tokenizer_id(&self) -> u16 {
            ids::CL100K_BASE
        }
    }

    /// Exact `o200k_base` tokenizer (GPT-4o), [`ids::O200K_BASE`].
    ///
    /// The newer GPT-4o vocabulary; the corpus aggregates it as a robustness
    /// cross-check against [`Cl100kEstimator`]. Opt-in only, under feature `tiktoken`.
    pub struct O200kEstimator {
        bpe: CoreBPE,
    }

    impl O200kEstimator {
        /// Load the `o200k_base` BPE tables and build the estimator.
        ///
        /// # Errors
        ///
        /// Returns [`TokenizerLoadError`] if the embedded tables fail to build.
        pub fn new() -> Result<Self, TokenizerLoadError> {
            let bpe = tiktoken_rs::o200k_base().map_err(|e| TokenizerLoadError {
                tokenizer: "o200k_base",
                message: e.to_string(),
            })?;
            Ok(Self { bpe })
        }
    }

    impl core::fmt::Debug for O200kEstimator {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("O200kEstimator").finish_non_exhaustive()
        }
    }

    impl TokenEstimator for O200kEstimator {
        fn estimate(&self, text: &str) -> usize {
            count(&self.bpe, text)
        }

        fn tokenizer_id(&self) -> u16 {
            ids::O200K_BASE
        }
    }
}

/// Exact GPT tokenizer estimators, available under feature `tiktoken`.
#[cfg(feature = "tiktoken")]
#[cfg_attr(docsrs, doc(cfg(feature = "tiktoken")))]
pub use exact::{Cl100kEstimator, O200kEstimator, TokenizerLoadError};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{ByteLenEstimator, HeuristicEstimator, TokenEstimator, ids};
    use crate::{Compressor, Config, EncoderId, Profile};
    use std::sync::Arc;

    /// Canonical 4-space-indented JSON in the three shapes agent output actually
    /// takes: a flat object, an array of uniform row objects, and a deep nest.
    fn four_space_json(shape: &str, n: usize) -> String {
        match shape {
            "flat" => {
                let body: Vec<String> = (0..n)
                    .map(|i| format!("    \"key_{i}\": \"value_{i}\""))
                    .collect();
                format!("{{\n{}\n}}", body.join(",\n"))
            }
            "rows" => {
                let body: Vec<String> = (0..n)
                    .map(|i| {
                        format!(
                            "        {{\n            \"id\": {i},\n            \"name\": \"item {i}\",\n            \"ok\": true\n        }}"
                        )
                    })
                    .collect();
                format!("{{\n    \"rows\": [\n{}\n    ]\n}}", body.join(",\n"))
            }
            "nested" => {
                let mut out = String::new();
                for d in 0..n {
                    let pad = " ".repeat(4 * (d + 1));
                    out.push_str(&" ".repeat(4 * d));
                    out.push_str("{\n");
                    out.push_str(&pad);
                    out.push_str("\"level_");
                    out.push_str(&d.to_string());
                    out.push_str("\": \n");
                    out.push_str(&pad);
                }
                out.push_str("\"leaf\"");
                for d in (0..n).rev() {
                    out.push('\n');
                    out.push_str(&" ".repeat(4 * d));
                    out.push('}');
                }
                out
            }
            other => panic!("unknown shape {other}"),
        }
    }

    /// Claimed saving in percent, and the encoder that won, at a given margin.
    fn claimed(doc: &str, bps: u32) -> (f64, EncoderId) {
        let config = Config::builder()
            .profile(Profile::Conservative)
            .estimator(Arc::new(HeuristicEstimator))
            .min_saving_bps(bps)
            .build();
        let artifact = Compressor::new(config)
            .compress(doc.as_bytes())
            .expect("valid JSON must compress");
        (
            (1.0 - artifact.stats.token_ratio()) * 100.0,
            artifact.stats.encoder,
        )
    }

    /// Pins the table in the `MEASURED_OVER_CLAIM_BPS` doc, and the conclusion the
    /// constant is justified by.
    ///
    /// The sentence this replaced said E1's claimed saving on canonical 4-space JSON
    /// "asymptotes near 11.2%", and nothing measured it. It is not one number: the
    /// flat-object plateau is around 7% and the row-array plateau around 16%, so
    /// 11.2% is a value between two plateaus that no shape actually sits at — and,
    /// worse for the argument, it overstates the headroom by roughly 4 pp on the very
    /// shape that binds. The real margin over a 6.00 pp bar is under 1.1 pp, and a
    /// 1 KB flat object is already below the bar. Anyone raising the constant needs
    /// to see that here, not discover it in the field.
    #[test]
    fn the_600_bps_bar_sits_just_above_the_flat_object_plateau() {
        /// A measured band, spelled the way the table spells it: both ends to one
        /// decimal. The bands below are what the argument needs; this is what the
        /// reader is shown, and a figure a test only brackets can drift a whole
        /// point without a failure.
        fn spelled(values: &[f64]) -> String {
            let low = values.iter().copied().fold(f64::INFINITY, f64::min);
            let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            format!("{low:.1}%–{high:.1}%")
        }

        let bar = HeuristicEstimator::MEASURED_OVER_CLAIM_BPS;
        assert_eq!(bar, 600, "the numbers below were measured against 600 bps");

        // Small documents never reach the question: minification loses the strict
        // comparison by the width of the sentinel, so they take passthrough at any
        // margin. `EncoderId::PASSTHROUGH` is the id the header records for that.
        for n in [3usize, 10] {
            let (saving, encoder) = claimed(&four_space_json("flat", n), 0);
            assert!(
                (saving - 0.0).abs() < 1e-9,
                "a {n}-key flat object should report no saving, got {saving}"
            );
            assert_eq!(encoder, EncoderId::PASSTHROUGH, "{n}-key flat object");
        }

        // The flat-object plateau: this is what the bar is close to.
        let plateau: Vec<f64> = [200usize, 1000, 5000]
            .iter()
            .map(|n| claimed(&four_space_json("flat", *n), 0).0)
            .collect();
        for saving in &plateau {
            assert!(
                (6.0..=7.5).contains(saving),
                "flat-object plateau moved: {saving}"
            );
            assert!(
                *saving > 6.0,
                "the plateau must still clear a 6.00 pp bar, got {saving}"
            );
        }
        assert_eq!(
            spelled(&plateau),
            "6.4%–7.1%",
            "the flat-object row of the table"
        );

        // Row arrays and deep nests are the shapes with real headroom.
        let rows: Vec<f64> = [10usize, 200, 2000]
            .iter()
            .map(|n| claimed(&four_space_json("rows", *n), 0).0)
            .collect();
        for saving in &rows {
            assert!(
                (13.0..=16.5).contains(saving),
                "row-array saving moved: {saving}"
            );
        }
        assert_eq!(
            spelled(&rows),
            "13.1%–16.0%",
            "the row-array row of the table"
        );
        let nested: Vec<f64> = [8usize, 16, 30]
            .iter()
            .map(|n| claimed(&four_space_json("nested", *n), 0).0)
            .collect();
        for saving in &nested {
            assert!(
                (13.0..=22.5).contains(saving),
                "nested saving moved: {saving}"
            );
        }
        assert_eq!(
            spelled(&nested),
            "13.1%–21.8%",
            "the deep-nesting row of the table"
        );

        // And the conclusion: at the bar, two shapes above lose E1 outright.
        for (shape, n, row) in [("flat", 40usize, "5.35%"), ("nested", 4usize, "1.96%")] {
            let (saving, at_zero) = claimed(&four_space_json(shape, n), 0);
            assert_ne!(
                at_zero,
                EncoderId::PASSTHROUGH,
                "{shape}/{n} must win without a margin, or the case is not a case"
            );
            assert!(saving < 6.0, "{shape}/{n} was supposed to be under the bar");
            assert_eq!(
                format!("{saving:.2}%"),
                row,
                "{shape}/{n}: the table spells this row to two decimals"
            );
            let (_, at_bar) = claimed(&four_space_json(shape, n), bar);
            assert_eq!(
                at_bar,
                EncoderId::PASSTHROUGH,
                "{shape}/{n} must fall back to passthrough at {bar} bps"
            );
        }

        // "While every other row above keeps E1": clearing the bar by the numbers is
        // not the same as the selection keeping the encoder, so ask the selection.
        for (shape, sizes) in [
            ("flat", &[200usize, 1000, 5000][..]),
            ("rows", &[10, 200, 2000]),
            ("nested", &[8, 16, 30]),
        ] {
            for n in sizes {
                let (_, at_bar) = claimed(&four_space_json(shape, *n), bar);
                assert_eq!(
                    at_bar,
                    EncoderId::E1_MINIFY,
                    "{shape}/{n} must keep E1 at {bar} bps"
                );
            }
        }
    }

    #[test]
    fn estimate_is_pure() {
        let estimator = HeuristicEstimator;
        let input = "{\"user\":\"alice\",\"count\":42,\"tags\":[\"a\",\"b\"]}";
        let first = estimator.estimate(input);
        let second = estimator.estimate(input);
        assert_eq!(first, second, "same input must yield the same estimate");
    }

    #[test]
    fn empty_string_costs_nothing() {
        assert_eq!(HeuristicEstimator.estimate(""), 0);
        assert_eq!(ByteLenEstimator.estimate(""), 0);
    }

    #[test]
    fn shipped_estimators_declare_no_margin() {
        // Every estimator the crate ships leaves the candidate rule at "any strict
        // token win", so selection is unchanged from the release that predates the
        // margin. The two exact tokenizers behind feature `tiktoken` are covered by the
        // same fact without being constructed here (each ctor loads megabytes of BPE
        // data): they do not override `over_claim_bps`, so they inherit the trait's `0`.
        // `HeuristicEstimator::MEASURED_OVER_CLAIM_BPS` records the measurement
        // without applying it; promoting it to the default is a format change.
        assert_eq!(HeuristicEstimator.over_claim_bps(), 0);
        assert_eq!(ByteLenEstimator.over_claim_bps(), 0);
        assert_eq!(HeuristicEstimator::MEASURED_OVER_CLAIM_BPS, 600);
    }

    #[test]
    fn a_third_party_estimator_compiles_without_the_new_method() {
        // `TokenEstimator` is the crate's only public extension point, so
        // `over_claim_bps` must stay a *provided* method: an implementor written
        // against v0.0.1 has to keep compiling and inherit the zero default. This
        // test exists to fail at compile time if it is ever made required.
        struct Minimal;
        impl TokenEstimator for Minimal {
            fn estimate(&self, text: &str) -> usize {
                text.len()
            }
            fn tokenizer_id(&self) -> u16 {
                ids::BYTE_LEN
            }
        }
        assert_eq!(Minimal.over_claim_bps(), 0);
    }

    #[test]
    fn concatenation_is_monotone() {
        let estimator = HeuristicEstimator;
        // Includes a pair whose boundary merges two alphanumeric runs
        // ("world" + "foo") to exercise run coalescing across the seam.
        let pairs = [
            ("hello world", "foo bar baz"),
            ("", "anything at all"),
            ("prefix", ""),
            ("    indented", "line\nwith breaks"),
            ("café", "über"),
        ];
        for (left, right) in pairs {
            let joined = format!("{left}{right}");
            let joined_est = estimator.estimate(&joined);
            assert!(
                joined_est >= estimator.estimate(left),
                "estimate({joined:?}) < estimate({left:?})"
            );
            assert!(
                joined_est >= estimator.estimate(right),
                "estimate({joined:?}) < estimate({right:?})"
            );
        }
    }

    #[test]
    fn byte_len_estimator_returns_byte_length() {
        let estimator = ByteLenEstimator;
        // "café" is five bytes (é is two) but four characters: len() is bytes.
        assert_eq!(estimator.estimate("café"), "café".len());
        assert_eq!(estimator.estimate("café"), 5);
        assert_eq!(estimator.estimate("plain ascii"), "plain ascii".len());
    }

    #[test]
    fn ascii_prose_lands_in_a_sane_band() {
        // Plain lowercase prose, minimal punctuation: fewer tokens than bytes, and
        // never fewer than one token per six bytes. cl100k would count still fewer
        // here, because it merges a word with the space in front of it. That is a
        // fact about prose, not about this scanner, which is not an upper bound in
        // general: on high-entropy string values it under-counts real tokens by up
        // to half. See `the_heuristic_bias_is_two_sided_the_way_the_docs_say_it_is`.
        let prose = "the quick brown fox jumps over the lazy dog and then the dog runs \
             away into the deep dark forest";
        let len = prose.len();
        let est = HeuristicEstimator.estimate(prose);
        assert!(
            est >= len / 6,
            "estimate {est} implausibly low for {len} bytes"
        );
        assert!(
            est <= len / 2,
            "estimate {est} implausibly high for {len} bytes"
        );
    }

    /// Exact-value pins for every rule in [`HeuristicEstimator::estimate`].
    ///
    /// The cost constants are FORMAT-AFFECTING: they decide which encoder wins, so a
    /// silent recalibration is a format change. Yet before this table no test in the
    /// workspace pinned a single estimate — `ascii_prose_lands_in_a_sane_band` only
    /// bounds one string to `len/6 ..= len/2`, a band wide enough that any constant
    /// could be nudged, either `div_ceil` turned into a truncating divide, or a
    /// mid-loop run flush deleted, without a test going red.
    ///
    /// Each row is hand-computed from the documented model and names the rule it
    /// pins. A row that changes is a deliberate format decision, not a tuning tweak.
    #[test]
    fn estimate_pins_every_cost_rule_to_an_exact_value() {
        let e = HeuristicEstimator;
        let cases: &[(&str, usize, &str)] = &[
            ("", 0, "empty input costs nothing"),
            // Alphanumeric runs: ceil(len * 10 / 37).
            ("a", 1, "a one-character run still costs a whole token"),
            ("abcd", 2, "ceil(40/37) = 2"),
            (
                "abcdefg",
                2,
                "ceil(70/37) = 2; a wider tenths scale would say 3",
            ),
            (
                "abcdefghijklmno",
                5,
                "ceil(150/37) = 5; 3.8 chars per token would say 4",
            ),
            // Whitespace runs: charged one token only from length 2 up.
            ("a b", 2, "a single space between words is free"),
            (
                "a  b",
                3,
                "a two-character whitespace run costs exactly one token",
            ),
            (
                "a  ",
                2,
                "a trailing whitespace run is flushed after the loop",
            ),
            // ASCII, neither alphanumeric nor whitespace: one token each.
            (
                "{}",
                2,
                "each punctuation character costs exactly one token",
            ),
            (
                "\u{0}\u{0}",
                2,
                "every non-alphanumeric, non-whitespace ASCII byte is punctuation, \
                 control characters included",
            ),
            ("  ,", 2, "punctuation flushes a pending whitespace run"),
            // Non-ASCII runs: ceil(bytes / 2), counted in bytes, not characters.
            (
                "\u{e9}\u{e9}\u{e9}",
                3,
                "three 2-byte characters are ceil(6/2); they are not an alphanumeric run",
            ),
            ("\u{20ac}", 2, "a 3-byte character is ceil(3/2) = 2, not 1"),
            (
                "\u{a0}",
                1,
                "U+00A0 is Unicode whitespace but not ASCII whitespace, so it is \
                 charged as non-ASCII bytes",
            ),
            (
                "a\u{e9}",
                2,
                "a trailing non-ASCII run is flushed after the loop",
            ),
            (
                "\u{e9}a",
                2,
                "an alphanumeric character flushes the pending non-ASCII run",
            ),
            (
                "\u{e9} a",
                2,
                "a whitespace character flushes the pending non-ASCII run",
            ),
            (
                "a  \u{e9}",
                3,
                "a non-ASCII character flushes the pending whitespace run, and a run \
                 of two is charged: 1 (a) + 1 (run) + ceil(2/2)",
            ),
        ];
        for (input, expected, why) in cases {
            assert_eq!(e.estimate(input), *expected, "estimate({input:?}): {why}");
        }
    }

    #[test]
    fn tokenizer_ids_are_stable() {
        assert_eq!(HeuristicEstimator.tokenizer_id(), 0);
        assert_eq!(ByteLenEstimator.tokenizer_id(), 1);

        assert_eq!(ids::HEURISTIC, 0);
        assert_eq!(ids::BYTE_LEN, 1);
        assert_eq!(ids::CL100K_BASE, 2);
        assert_eq!(ids::O200K_BASE, 3);
        assert_eq!(ids::HUGGING_FACE, 4);
    }

    #[test]
    #[allow(clippy::default_constructed_unit_structs)]
    fn heuristic_is_the_default() {
        // The default estimator is the heuristic (id 0), not byte length (id 1):
        // selecting encoders on bytes would optimize the wrong objective (R6). The
        // explicit `default()` call is what exercises the derived `Default`.
        assert_eq!(HeuristicEstimator::default().tokenizer_id(), ids::HEURISTIC);
    }
}

#[cfg(all(test, feature = "tiktoken"))]
mod tiktoken_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{Cl100kEstimator, HeuristicEstimator, O200kEstimator, TokenEstimator, ids};

    /// The sentinel's real cost, pinned per tokenizer.
    ///
    /// The passthrough overhead is quoted in three places of published documentation
    /// (`lib.rs`, `Stats`, and the crate README) as "about 10 estimated (11 real
    /// `cl100k`, 13 real `o200k`)". Those were prose with nothing holding them to the
    /// tokenizers, and the `o200k` figure was absent entirely until it was measured
    /// here — 18% above the `cl100k` number the docs quoted, on the tokenizer a GPT-4o
    /// caller is actually billed on.
    ///
    /// The cost is measured the way the docs state it: as the *delta* a prepended
    /// sentinel adds to a real payload, not as the line's count in isolation. The two
    /// could differ — BPE merges across the boundary — and this asserts they do not,
    /// across payloads whose first character is `{`, `[`, `"` and a digit.
    #[test]
    fn the_sentinel_costs_what_the_docs_say_it_costs() {
        let cl100k = Cl100kEstimator::new().unwrap();
        let o200k = O200kEstimator::new().unwrap();
        let payloads = [
            "{\"a\":1}",
            "[{\"id\":0,\"name\":\"item0\"},{\"id\":1,\"name\":\"item1\"}]",
            "{\n    \"alpha\": 1,\n    \"beta\": 2\n}",
            "\"plain string with words in it\"",
            "[1,2,3,4,5,6,7,8,9,10]",
            "1234567890",
        ];
        // (tag, heuristic, cl100k, o200k). Only `raw` is quoted in the docs — it is
        // the tag the passthrough path always writes. `min` and `tbl` are measured
        // alongside it so a future retag cannot move the published number unnoticed;
        // `min` happens to cost one token less under both exact tokenizers.
        let expected = [
            ("raw", 10, 11, 13),
            ("min", 10, 10, 12),
            ("tbl", 10, 11, 13),
        ];

        for (tag, heur, cl, o2) in expected {
            let sentinel = format!("\u{27E6}tkfd:v1:{tag}\u{27E7}\n");
            assert_eq!(sentinel.len(), 18, "the sentinel line is 18 bytes");
            for payload in payloads {
                let framed = format!("{sentinel}{payload}");
                assert_eq!(
                    HeuristicEstimator.estimate(&framed) - HeuristicEstimator.estimate(payload),
                    heur,
                    "heuristic cost of the {tag} sentinel before {payload:?}"
                );
                assert_eq!(
                    cl100k.estimate(&framed) - cl100k.estimate(payload),
                    cl,
                    "cl100k cost of the {tag} sentinel before {payload:?}"
                );
                assert_eq!(
                    o200k.estimate(&framed) - o200k.estimate(payload),
                    o2,
                    "o200k cost of the {tag} sentinel before {payload:?}"
                );
            }
        }
    }

    /// The sentinel's cost is not constant: it depends on the input's first bytes.
    ///
    /// The test above feeds only payloads that open with a non-whitespace byte, and
    /// the docs once called the overhead "constant". Valid JSON may open with
    /// whitespace, and the passthrough path renders it verbatim, so the input's
    /// leading whitespace sits directly after the sentinel's newline, where BPE (and
    /// the heuristic's whitespace-run rule) can merge the two. This pins single cases
    /// and the whole range the crate docs quote: over every prefix of up to six
    /// spaces, tabs, CRs and LFs before `{}`, 10–11 estimated, 10–12 `cl100k`, 12–13
    /// `o200k`. (A survey of the same prefixes before ten other bodies — `[]`, `1`,
    /// `null`, strings, objects — found the same six bounds; only `{}` is swept here
    /// because the exact tokenizers are slow in a debug build.)
    #[test]
    fn the_sentinel_cost_moves_with_leading_whitespace() {
        let cl100k = Cl100kEstimator::new().unwrap();
        let o200k = O200kEstimator::new().unwrap();
        let sentinel = "\u{27E6}tkfd:v1:raw\u{27E7}\n";
        let cost = |est: &dyn TokenEstimator, input: &str| {
            est.estimate(&format!("{sentinel}{input}")) - est.estimate(input)
        };

        // (input, heuristic, cl100k, o200k)
        for (input, heur, cl, o2) in [
            (" {}", 11, 11, 13),
            ("\n{}", 11, 10, 12),
            ("\r\n{}", 10, 11, 12),
            ("\r\n\t\t\r\n{}", 10, 12, 13),
        ] {
            assert_eq!(
                cost(&HeuristicEstimator, input),
                heur,
                "heuristic {input:?}"
            );
            assert_eq!(cost(&cl100k, input), cl, "cl100k {input:?}");
            assert_eq!(cost(&o200k, input), o2, "o200k {input:?}");
        }

        let mut prefixes = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..6 {
            frontier = frontier
                .iter()
                .flat_map(|p| [" ", "\t", "\r", "\n"].map(|w| format!("{p}{w}")))
                .collect();
            prefixes.extend(frontier.iter().cloned());
        }
        assert_eq!(prefixes.len(), 5461, "4^0 + 4^1 + ... + 4^6 prefixes");

        for (name, est, lo, hi) in [
            (
                "heuristic",
                &HeuristicEstimator as &dyn TokenEstimator,
                10,
                11,
            ),
            ("cl100k", &cl100k, 10, 12),
            ("o200k", &o200k, 12, 13),
        ] {
            let costs: Vec<usize> = prefixes
                .iter()
                .map(|p| cost(est, &format!("{p}{{}}")))
                .collect();
            assert_eq!(costs.iter().min(), Some(&lo), "{name}: cheapest sentinel");
            assert_eq!(costs.iter().max(), Some(&hi), "{name}: dearest sentinel");
        }
    }

    /// What minification is actually worth per construct, pinned per tokenizer.
    ///
    /// `e1_minify`'s "why this is a candidate, not a guarantee" section is the
    /// reasoning a reader uses to decide whether the encoder is worth enabling, and
    /// it used to say cl100k/o200k tokenize "a post-key `": "` as a single token, so
    /// stripping them can leave the token count flat". Measured, `": "` is *two*
    /// tokens under both, so stripping it pays one token per key — the section
    /// asserted the opposite of the truth, in the direction that understates the
    /// encoder. Nothing held that prose to a tokenizer, so this does.
    ///
    /// The indentation half is pinned with it, and pinned at its edges. The first
    /// version of this test sampled only 2, 4, 8, 16 and 32 spaces to defend the
    /// claim that a run costs one token "whatever its length" — every one of those
    /// widths really is one token, so the test passed while the universally
    /// quantified sentence it existed to defend was false. Indentation is a lookup
    /// table, not a rule: it is finite (82 spaces already cost 2 under cl100k, 80
    /// under o200k) and it is **not monotonic** (128 spaces cost 1 while 84 cost 2).
    /// A test that never crosses the edge of a table cannot see the edge, so this
    /// one crosses it deliberately, and surveys the whole table so that a tokenizer
    /// upgrade moving the boundary is a failure rather than a silent doc rot.
    #[test]
    fn minification_is_priced_the_way_the_e1_docs_say_it_is() {
        let cl100k = Cl100kEstimator::new().unwrap();
        let o200k = O200kEstimator::new().unwrap();

        // The widths real documents indent at are one token however many bytes they
        // are, so a line sheds bytes without shedding tokens in proportion.
        for spaces in [2usize, 4, 8, 16, 32, 128] {
            let run = " ".repeat(spaces);
            assert_eq!(cl100k.estimate(&run), 1, "cl100k: {spaces} spaces");
            assert_eq!(o200k.estimate(&run), 1, "o200k: {spaces} spaces");
            let line = format!("\n{run}");
            assert_eq!(
                cl100k.estimate(&line),
                2,
                "cl100k: newline + {spaces} spaces"
            );
            assert_eq!(o200k.estimate(&line), 2, "o200k: newline + {spaces} spaces");
        }

        // But the table runs out, at a different width per tokenizer, and it is not
        // monotonic — 128 spaces are cheaper than 84. Neither fact survives being
        // stated as "whatever its length".
        assert_eq!(
            cl100k.estimate(&" ".repeat(80)),
            1,
            "cl100k still pays 1 at 80"
        );
        assert_eq!(cl100k.estimate(&" ".repeat(82)), 2, "cl100k pays 2 from 82");
        assert_eq!(o200k.estimate(&" ".repeat(80)), 2, "o200k pays 2 from 80");
        for est in [&cl100k as &dyn TokenEstimator, &o200k] {
            assert_eq!(est.estimate(&" ".repeat(84)), 2, "84 spaces cost more");
            assert_eq!(est.estimate(&" ".repeat(128)), 1, "yet 128 cost one");
            // The whole of the non-monotonic tail, because `encoder`'s module docs
            // name these five widths one by one to say that "past the table" is not
            // a threshold. Stating that and pinning only its endpoints is how the
            // monotonic phrasing survived in that file after this test was written.
            for width in [83usize, 87, 91, 95, 128] {
                assert_eq!(
                    est.estimate(&" ".repeat(width)),
                    1,
                    "{width} spaces are past the first 2-token width and still cost 1"
                );
            }
            assert_eq!(est.estimate(&" ".repeat(400)), 4, "400 spaces");
            // `Config`'s default `max_depth` is 512, so both of these are reachable
            // by a real document, and stripping such a run is worth far more than
            // the two tokens the shallow case suggests.
            assert_eq!(est.estimate(&" ".repeat(512)), 4, "512 spaces");
            assert_eq!(est.estimate(&" ".repeat(2048)), 16, "2048 spaces");
        }

        // The survey `e1_minify` quotes, so its figures cannot drift from the crate.
        for (name, est, singles) in [
            ("cl100k", &cl100k as &dyn TokenEstimator, 86usize),
            ("o200k", &o200k, 84),
        ] {
            let one_token: Vec<usize> = (1..=400)
                .filter(|n| est.estimate(&" ".repeat(*n)) == 1)
                .collect();
            assert_eq!(one_token.len(), singles, "{name}: single-token run lengths");
            // `encoder`'s module docs name "the first run that costs 2" and list the
            // widths past it that are "back to 1". Membership alone would not hold
            // either: a single-token width appearing below 82, or a sixth one in the
            // tail, would pass every per-width assertion above.
            let first_dearer = (1..=400)
                .find(|n| est.estimate(&" ".repeat(*n)) != 1)
                .expect("some run costs more than one token");
            assert_eq!(
                first_dearer,
                if name == "cl100k" { 82 } else { 80 },
                "{name}: first run that costs 2"
            );
            let tail: Vec<usize> = one_token
                .iter()
                .copied()
                .filter(|n| *n > first_dearer)
                .collect();
            assert_eq!(
                tail,
                [83, 87, 91, 95, 128],
                "{name}: the non-monotonic tail"
            );
            assert_eq!(
                one_token.last().copied(),
                Some(128),
                "{name}: longest single-token run"
            );
        }

        // The post-key separator: two tokens spaced, one token tight.
        for est in [&cl100k as &dyn TokenEstimator, &o200k] {
            assert_eq!(est.estimate("\": \""), 2, "a spaced post-key separator");
            assert_eq!(est.estimate("\":\""), 1, "a tight post-key separator");
        }

        // And in context, where BPE could have merged across the boundary: exactly
        // one token per key, with nothing else changed between the two spellings.
        let spaced = "{\n  \"a\": 1,\n  \"bb\": 2,\n  \"ccc\": 3\n}";
        let tight = "{\n  \"a\":1,\n  \"bb\":2,\n  \"ccc\":3\n}";
        for est in [&cl100k as &dyn TokenEstimator, &o200k] {
            assert_eq!(
                est.estimate(spaced) - est.estimate(tight),
                3,
                "three keys must save three tokens, one each"
            );
        }
    }

    #[test]
    fn exact_tokenizer_ids_are_stable() {
        let cl100k = Cl100kEstimator::new().expect("cl100k_base tables load");
        let o200k = O200kEstimator::new().expect("o200k_base tables load");
        assert_eq!(cl100k.tokenizer_id(), ids::CL100K_BASE);
        assert_eq!(o200k.tokenizer_id(), ids::O200K_BASE);
        assert_eq!(cl100k.tokenizer_id(), 2);
        assert_eq!(o200k.tokenizer_id(), 3);
    }

    #[test]
    fn empty_string_costs_nothing() {
        assert_eq!(Cl100kEstimator::new().unwrap().estimate(""), 0);
        assert_eq!(O200kEstimator::new().unwrap().estimate(""), 0);
    }

    #[test]
    fn cl100k_matches_known_bpe_count() {
        // "hello world" is a fixed two-token sequence in cl100k_base (`hello`,
        // ` world`). This anchors the wrapper to ground truth: a wrong method or a
        // silent tokenizer swap would move it off 2.
        assert_eq!(Cl100kEstimator::new().unwrap().estimate("hello world"), 2);
    }

    #[test]
    fn o200k_matches_known_bpe_count() {
        // The o200k_base twin of the cl100k anchor above: "hello world" is `hello` +
        // ` world`, two tokens. This pins the wrapper to ground truth — a broken encode
        // path or a silent heuristic fallback (the heuristic scores "hello world" as 4)
        // would move it off 2. It does NOT by itself catch an o200k that loaded cl100k's
        // tables (cl100k also returns 2 here); that swap is caught by
        // `o200k_and_cl100k_are_distinct_tokenizers`. Together the two tests cover both
        // mis-wirings.
        assert_eq!(O200kEstimator::new().unwrap().estimate("hello world"), 2);
    }

    #[test]
    fn o200k_and_cl100k_are_distinct_tokenizers() {
        // The two exact estimators must not be one table behind two names. Rather than
        // bet on a single hand-picked string (o200k's larger, more multilingual vocab
        // diverges from cl100k in ways that are easy to guess wrong), scan a diverse
        // battery and require divergence on at least one — enough to prove
        // `O200kEstimator` is not a relabeled `Cl100kEstimator`.
        let cl100k = Cl100kEstimator::new().unwrap();
        let o200k = O200kEstimator::new().unwrap();
        let battery = [
            "你好世界，这是一段中文测试文本",
            "🎉🎊✨🥳 emoji then prose",
            "supercalifragilisticexpialidocious antidisestablishmentarianism",
            "def foo(x):\n    return x + 1  # inline comment",
            "Ĉ Ĝ Ĥ Ĵ Ŝ Ŭ Esperanto diacritics",
        ];
        let diverges = battery
            .iter()
            .any(|s| cl100k.estimate(s) != o200k.estimate(s));
        assert!(
            diverges,
            "o200k and cl100k produced identical counts across the whole battery — \
             they appear to be the same tokenizer"
        );
    }

    #[test]
    fn exact_estimators_debug_as_a_named_opaque_struct() {
        // `CoreBPE` implements no `Debug`, so these are the crate's only hand-written
        // `Debug` impls — every other type derives one. Both halves of what they print
        // are load-bearing and pinned here: the type name, without which the estimator
        // is unidentifiable in a `{:?}` dump of a config or an error, and the `{ .. }`
        // elision that stands in for the megabyte-scale merge table.
        let cl100k = Cl100kEstimator::new().unwrap();
        let o200k = O200kEstimator::new().unwrap();
        assert_eq!(format!("{cl100k:?}"), "Cl100kEstimator { .. }");
        assert_eq!(format!("{o200k:?}"), "O200kEstimator { .. }");
    }

    #[test]
    fn exact_estimate_is_pure() {
        let cl100k = Cl100kEstimator::new().unwrap();
        let input = "{\"user\":\"alice\",\"count\":42,\"tags\":[\"a\",\"b\"]}";
        assert_eq!(cl100k.estimate(input), cl100k.estimate(input));
    }

    #[test]
    fn heuristic_over_counts_dense_json() {
        // The reason feature `tiktoken` exists (roadmap #2): the heuristic charges
        // one token per ASCII punctuation byte, but a JSON-trained BPE merges pairs
        // like `,"`, `":`, `{"`. On punctuation-dense JSON the exact count is
        // therefore never above the heuristic — the exact estimator closes the
        // over-claim the heuristic can leave in the do-no-harm gate.
        let json = "{\"user\":\"alice\",\"count\":42,\"tags\":[\"a\",\"b\"],\"ok\":true}";
        let exact = Cl100kEstimator::new().unwrap().estimate(json);
        let heuristic = HeuristicEstimator.estimate(json);
        assert!(
            exact <= heuristic,
            "exact cl100k {exact} unexpectedly above heuristic {heuristic} on dense JSON"
        );
    }

    /// The tab-layout comparison E2's module docs rest on, pinned.
    ///
    /// Two claims live in that paragraph and only one of them was ever true. The
    /// structural half — a BPE tokenizer merges `,"` and `":"` but never a `\t"`
    /// boundary, so a tab-separated row can never be the cheaper spelling — holds.
    /// The magnitude half did not: the paragraph claimed a tab layout spent *most* of
    /// the key-dedup saving straight back at the tokenizer, and nothing measured it.
    /// It is wrong for the typical table. A third of the shapes surveyed here give
    /// back nothing whatever, the median gives back a fifth, and "most" is reached
    /// only on tables whose every value is a string, from three fields wide. The
    /// figures the module docs now quote are computed here, so a tokenizer upgrade
    /// that moves them fails a test instead of rotting a sentence.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn the_tab_layout_is_priced_the_way_the_e2_docs_say_it_is() {
        /// Build (keys repeated on every element, E2's `+[…]` rows, hypothetical
        /// tab-separated rows) for one value family and table size. The header is
        /// identical in the last two, so the only difference measured is the row
        /// separator.
        fn build(family: usize, fields: usize, rows: usize) -> (String, String, String) {
            let key = |j: usize| format!("\"field{j}\"");
            let val = |i: usize, j: usize| match family {
                0 => match j % 3 {
                    0 => format!("{}", i * 10 + j),
                    1 => format!("\"value{i}_{j}\""),
                    _ => String::from("true"),
                },
                1 => match j % 3 {
                    0 => format!("{i}"),
                    1 => format!("\"item{i}\""),
                    _ => String::from("true"),
                },
                2 => format!("\"{}\"", char::from(b'a' + u8::try_from(i % 26).unwrap())),
                3 => format!("{}", i * 1_000_003 + j),
                4 => format!("\"2026-09-0{}T12:{:02}:00Z\"", (i % 9) + 1, j % 60),
                _ => format!("\"{}\"", "x".repeat(1 + (i + j) % 12)),
            };

            let mut repeated = String::from("[");
            for i in 0..rows {
                if i > 0 {
                    repeated.push(',');
                }
                repeated.push('{');
                for j in 0..fields {
                    if j > 0 {
                        repeated.push(',');
                    }
                    repeated.push_str(&key(j));
                    repeated.push(':');
                    repeated.push_str(&val(i, j));
                }
                repeated.push('}');
            }
            repeated.push(']');

            let header: Vec<String> = (0..fields).map(key).collect();
            let mut tabular = format!("\n#{rows}[{}]\n", header.join(","));
            let mut tabbed = tabular.clone();
            for i in 0..rows {
                let vs: Vec<String> = (0..fields).map(|j| val(i, j)).collect();
                tabular.push_str("+[");
                tabular.push_str(&vs.join(","));
                tabular.push_str("]\n");
                tabbed.push('+');
                tabbed.push_str(&vs.join("\t"));
                tabbed.push('\n');
            }
            (repeated, tabular, tabbed)
        }

        let cl100k = Cl100kEstimator::new().unwrap();
        let o200k = O200kEstimator::new().unwrap();

        // The structural half, stated as the module docs state it.
        for est in [&cl100k as &dyn TokenEstimator, &o200k] {
            assert_eq!(est.estimate(",\""), 1, "`,\"` merges into one token");
            assert_eq!(est.estimate("\":\""), 1, "`\":\"` merges into one token");
            assert_eq!(est.estimate("\t\""), 2, "`\\t\"` is never merged");
        }

        // Percentage of E2's key-dedup saving that a tab layout would hand back, over
        // six value families, six widths, four heights and both tokenizers.
        // (give-back, family, fields, rows), one per tokenizer.
        let mut surveyed: Vec<(i64, usize, usize, usize)> = Vec::new();
        for family in 0..6 {
            for fields in [2usize, 3, 5, 8, 12, 20] {
                for rows in [4usize, 8, 25, 100] {
                    let (base, tabular, tabbed) = build(family, fields, rows);
                    for est in [&cl100k as &dyn TokenEstimator, &o200k] {
                        let b = i64::try_from(est.estimate(&base)).unwrap();
                        let e = i64::try_from(est.estimate(&tabular)).unwrap();
                        let t = i64::try_from(est.estimate(&tabbed)).unwrap();
                        let saved = b - e;
                        assert!(saved > 0, "E2 must save on family{family} {fields}x{rows}");
                        surveyed.push(((saved - (b - t)) * 100 / saved, family, fields, rows));
                    }
                }
            }
        }
        let mut given_back: Vec<i64> = surveyed.iter().map(|s| s.0).collect();
        given_back.sort_unstable();

        let cases = given_back.len();
        let median = given_back.get(cases / 2).copied().unwrap();
        let worst = given_back.last().copied().unwrap();
        let free = given_back.iter().filter(|v| **v == 0).count();
        let most = given_back.iter().filter(|v| **v > 50).count();
        assert_eq!(
            given_back.first().copied().unwrap(),
            0,
            "a tab layout must never come out cheaper than E2's own row spelling"
        );
        assert_eq!(cases, 288, "the survey size the module docs quote");
        assert_eq!(
            free, 96,
            "shapes on which a tab layout costs exactly the same"
        );
        assert_eq!(median, 20, "median give-back, in percent of the saving");
        assert_eq!(most, 90, "shapes on which a tab really does give back most");
        assert_eq!(worst, 86, "worst give-back, in percent of the saving");

        // The docs say which shapes give back more than half, too: tables whose every
        // value is a string -- families 2, 4 and 5 in `build` -- at three fields or
        // more. The count alone let that description say "wide rows of long string
        // values" about date strings three fields wide.
        let over_half: Vec<_> = surveyed.iter().filter(|s| s.0 > 50).collect();
        assert!(
            over_half
                .iter()
                .all(|s| matches!(s.1, 2 | 4 | 5) && s.2 >= 3),
            "a shape the E2 docs do not describe gives back more than half: {over_half:?}"
        );
        // And which widths, family by family: dates and short runs from three fields
        // up, one-letter strings only from five. Asserting only that *some* shape three
        // fields wide got there let a status note credit it to the one-letter tables.
        let widths = |family: usize| {
            let mut w: Vec<usize> = over_half
                .iter()
                .filter(|s| s.1 == family)
                .map(|s| s.2)
                .collect();
            w.sort_unstable();
            w.dedup();
            w
        };
        assert_eq!(widths(2), [5, 8, 12, 20], "one-letter strings");
        assert_eq!(widths(4), [3, 5, 8, 12, 20], "dates");
        assert_eq!(widths(5), [3, 5, 8, 12, 20], "short runs of one letter");

        // The docs also say *where* the peak is — twenty date-like fields, which is
        // family 4 at 20 fields in `build` — and a number pinned without its place
        // lets the place rot. Every case that reaches the peak must be that shape.
        let at_peak: Vec<_> = surveyed.iter().filter(|s| s.0 == worst).collect();
        assert!(
            at_peak.iter().all(|s| s.1 == 4 && s.2 == 20),
            "the peak give-back is not where the E2 docs put it: {at_peak:?}"
        );
    }

    /// One document of each content family, sized by `n`. The families are chosen
    /// to be ordinary agent tool output, not adversarial: prose, structured
    /// key/value rows, file paths, digests, UUIDs, base64.
    fn survey_document(family: usize, n: usize) -> String {
        let hex = |i: usize, w: usize| -> String {
            (0..w)
                .map(|j| {
                    char::from_digit(u32::try_from((i * 7 + j * 11) % 16).unwrap(), 16).unwrap()
                })
                .collect()
        };
        let join = |v: Vec<String>| v.join(",");
        match family {
            0 => format!(
                "{{\"text\":\"{}\"}}",
                "the quick brown fox jumps over the lazy dog and then runs away ".repeat(n)
            ),
            1 => format!(
                "{{{}}}",
                join(
                    (0..n * 4)
                        .map(|i| format!("\"field_name_{i}\":\"reasonable value {i}\""))
                        .collect()
                )
            ),
            2 => format!(
                "[{}]",
                join(
                    (0..n * 4)
                        .map(|i| format!("\"/usr/local/lib/node_modules/pkg{i}/dist/index.js\""))
                        .collect()
                )
            ),
            3 => format!(
                "{{{}}}",
                join(
                    (0..n)
                        .map(|i| format!("\"d{i}\":\"{}\"", hex(i, 64)))
                        .collect()
                )
            ),
            4 => format!(
                "[{}]",
                join(
                    (0..n * 2)
                        .map(|i| {
                            format!(
                                "\"{}-{}-4{}-a{}-{}\"",
                                hex(i, 8),
                                hex(i + 1, 4),
                                hex(i + 2, 3),
                                hex(i + 3, 3),
                                hex(i + 4, 12)
                            )
                        })
                        .collect()
                )
            ),
            _ => format!(
                "{{{}}}",
                join(
                    (0..n)
                        .map(|i| format!(
                            "\"b{i}\":\"{}\"",
                            "aGVsbG8gd29ybGQgdGhpcyBpcyBiYXNlNjQ=".repeat(2)
                        ))
                        .collect()
                )
            ),
        }
    }

    /// The direction of the heuristic's absolute error, pinned.
    ///
    /// Every doc sentence about this model used to name one direction. The module
    /// paragraph says it over-counts, the corpus range it quotes (+30% to +127%)
    /// contains no negative value, and `ascii_prose_lands_in_a_sane_band` used to
    /// close with "this scanner is an upper bound" — a general property claim
    /// defended by one string of English prose. It is not an upper bound. The scanner
    /// charges `ceil(len / 3.7)` for an alphanumeric run, and a BPE tokenizer has no
    /// merge to offer a random hex digest, so on high-entropy values it spends far
    /// more than that. Half the survey below under-counts, and the worst of it is off
    /// by nearly a factor of two — in the direction a reader budgeting context would
    /// least like to be wrong. The figures the module docs quote are computed here.
    #[test]
    fn the_heuristic_bias_is_two_sided_the_way_the_docs_say_it_is() {
        let cl100k = Cl100kEstimator::new().unwrap();
        let o200k = O200kEstimator::new().unwrap();

        // Signed relative error of the heuristic against each exact tokenizer, in
        // percent, kept per family so the docs can quote a per-family band.
        let mut per_family: Vec<Vec<i64>> = vec![Vec::new(); 6];
        for (family, bucket) in per_family.iter_mut().enumerate() {
            for n in [1usize, 2, 4, 8] {
                let s = survey_document(family, n);
                let h = i64::try_from(HeuristicEstimator.estimate(&s)).unwrap();
                for est in [&cl100k as &dyn TokenEstimator, &o200k] {
                    let e = i64::try_from(est.estimate(&s)).unwrap();
                    assert!(e > 0, "family{family} n={n} tokenized to nothing");
                    bucket.push((h - e) * 100 / e);
                }
            }
        }

        let all: Vec<i64> = per_family.iter().flatten().copied().collect();
        assert_eq!(all.len(), 48, "the survey size the module docs quote");
        assert_eq!(
            all.iter().filter(|v| **v < 0).count(),
            24,
            "exactly half the survey under-counts real tokens"
        );
        let under: Vec<i64> = all.iter().copied().filter(|v| *v < 0).collect();
        assert_eq!(
            (
                -under.iter().copied().min().unwrap(),
                -under.iter().copied().max().unwrap()
            ),
            (49, 18),
            "the 18%-to-49% under-count band the module docs quote"
        );

        // Per-family bands, in the order the module docs list them.
        let bands: [(&str, i64, i64); 6] = [
            ("prose", 63, 76),
            ("word-shaped key/value", 87, 89),
            ("file paths", 122, 124),
            ("SHA-256 digests", -43, -43),
            ("UUIDs", -21, -18),
            ("base64 blobs", -49, -37),
        ];
        for (bucket, (name, lo, hi)) in per_family.iter().zip(bands) {
            assert_eq!(
                (
                    bucket.iter().copied().min().unwrap(),
                    bucket.iter().copied().max().unwrap()
                ),
                (lo, hi),
                "{name}: the band the module docs quote"
            );
        }
    }
}
