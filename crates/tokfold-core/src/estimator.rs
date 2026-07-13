//! Token estimation for encoder selection.
//!
//! Encoder selection is a comparison, not a measurement: the candidate rule keeps a
//! rendering only when [`TokenEstimator::estimate`] rates it below the original
//! (§7 candidate rule). Two properties of this layer are therefore load-bearing.
//!
//! * **Purity.** `estimate` is a pure function of its argument — no clock, no global
//!   state, no hashing with a per-process seed. A nondeterministic estimator selects
//!   a different encoder from run to run, which produces different output bytes for
//!   the same logical input and silently invalidates the provider's prompt cache
//!   (§10). Determinism here is what keeps that cache warm.
//! * **Objective.** The estimator must count *tokens*, not bytes. Selecting an
//!   encoder to minimize bytes optimizes the wrong quantity — see
//!   [`ByteLenEstimator`], which exists only as a reference point and is never the
//!   default.
//!
//! The chosen estimator's [`TokenEstimator::tokenizer_id`] is reported out-of-band in
//! [`Stats::tokenizer_id`](crate::Stats::tokenizer_id); it is **not** written to the
//! archive. In v0.0.1 the recovery archive is a passthrough blob whose header
//! `tokenizer_id` field (`format` field 4) is always `0`, and the decoder fails closed
//! on any non-zero value — so ids `2..=4` name cost models that never appear in a valid
//! archive.

/// A cost model that rates text in tokens for encoder selection.
///
/// Implementations MUST be pure and deterministic: the candidate rule calls
/// `estimate` on every encoder's output, so a nondeterministic estimator yields
/// nondeterministic selection and defeats prompt caching (§10). The trait is the
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
}

/// Frozen ids naming the cost model that drove selection (§3), reported in
/// [`Stats::tokenizer_id`](crate::Stats::tokenizer_id).
///
/// They are the reserved value space of the header's `tokenizer_id` field: once
/// shipped, a value's meaning never changes. A valid v0.0.1 archive header always
/// carries `0` (passthrough) regardless of the selector, so `1..=4` are meanings the
/// field may name but this build never writes. Ids `0` and `1` are always available;
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
// them is a format change (§2.4 / §5): it must travel with an `encoder_id`
// semantics bump, never as a silent in-place edit.

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
///   (`cl100k` has dedicated indentation tokens); see [`WHITESPACE_RUN_MIN_TOKENIZED`].
/// * **ASCII punctuation, symbols and controls** — one token each.
/// * **Non-ASCII** — `ceil(bytes / 2)`, reflecting multi-byte UTF-8 fragmenting
///   into several BPE tokens.
///
/// The constants above are FORMAT-AFFECTING (see their docs): they influence
/// encoder selection, so they are named and frozen rather than inlined.
///
/// **Accuracy.** Within roughly ±10% of `cl100k` on the target corpus — structured
/// tool output, JSON and logs — which is what the engine compresses. Natural-language
/// prose is over-counted (short words each round up to a whole token, whereas
/// `cl100k` packs many common words together with their leading space), so a prose
/// estimate is an upper bound, not a match. Absolute accuracy is secondary anyway:
/// selection compares two estimates from this same model, and any consistent bias
/// cancels.
///
/// **Purity.** `estimate` reads only its argument and uses only integer arithmetic,
/// so it is deterministic across processes — a precondition for prompt-cache-safe
/// output (§10).
#[derive(Debug, Default, Clone, Copy)]
pub struct HeuristicEstimator;

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
/// family they cover — the gap [`HeuristicEstimator`](super::HeuristicEstimator) can
/// leave (§7). They are
/// opt-in: each constructor loads megabytes of embedded BPE data, which is why the
/// heuristic, not these, is the default (see the crate feature note). Adding one
/// changes only which rendering is selected and the reported
/// [`TokenEstimator::tokenizer_id`]; the recovery archive is unaffected, since it
/// always records the passthrough tokenizer id `0` regardless of the selector.
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
    /// there is no fallible path to trip the crate's no-panic contract.
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
    use super::{ByteLenEstimator, HeuristicEstimator, TokenEstimator, ids};

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
        // (leading spaces merge into words); this scanner is an upper bound.
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
}
