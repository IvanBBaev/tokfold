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
//! The chosen estimator's [`TokenEstimator::tokenizer_id`] is reported out of band in
//! [`Stats`](crate::Stats) so a caller can attribute a result to the cost model that
//! selected it. It is **not** written into the archive: v0.0.1 always records
//! `tokenizer_id = 0` in the header and `decompress` fail-closes on anything else,
//! because the archive is a passthrough blob whose recovery does not consult a cost
//! model.
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
/// nondeterministic selection and defeats prompt caching (§10). The trait is the
/// only public extension point in the crate; a caller may supply its own exact
/// tokenizer, but a stateful or seeded one violates the contract.
pub trait TokenEstimator: Send + Sync {
    /// Estimate the token count of `text`. Pure: equal input yields equal output.
    fn estimate(&self, text: &str) -> usize;

    /// Stable id identifying this cost model, reported in [`Stats`](crate::Stats).
    ///
    /// Ids are frozen in [`ids`]; a given estimator must always return the same one.
    /// The id is *not* written into the archive header — v0.0.1 always stores `0`
    /// there (see the module docs).
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

/// Frozen tokenizer ids for the header's `tokenizer_id` field (§3).
///
/// Ids are part of the wire format: once shipped, a value's meaning never changes.
/// Values `2..=4` are reserved names only — no implementation ships in v0.0.1.
pub mod ids {
    /// [`super::HeuristicEstimator`] — the default cost model.
    pub const HEURISTIC: u16 = 0;

    /// [`super::ByteLenEstimator`] — reference only, never the default.
    pub const BYTE_LEN: u16 = 1;

    /// Reserved: the `cl100k_base` tokenizer (feature `tiktoken`). Not implemented in v0.0.1.
    pub const CL100K_BASE: u16 = 2;

    /// Reserved: the `o200k_base` tokenizer (feature `tiktoken`). Not implemented in v0.0.1.
    pub const O200K_BASE: u16 = 3;

    /// Reserved: a Hugging Face tokenizer (feature `hf`). Not implemented in v0.0.1.
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
/// What the engine relies on is narrower and does hold: selection compares two
/// estimates produced by this same model, so most of that bias cancels. The residual
/// one-sided error on the reported *saving* is measured and stated as
/// [`HeuristicEstimator::MEASURED_OVER_CLAIM_BPS`]. Do not use `estimate` anywhere an
/// absolute token count matters — bill, budget or context-window arithmetic — use an
/// exact tokenizer for that.
///
/// **Purity.** `estimate` reads only its argument and uses only integer arithmetic,
/// so it is deterministic across processes — a precondition for prompt-cache-safe
/// output (§10).
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
    /// decisions most at risk of flipping sign, and because E1's claimed saving on
    /// canonical 4-space-indented JSON asymptotes near 11.2% — a margin much above
    /// this would retire that encoder on the commonest real shape.
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
    fn shipped_estimators_declare_no_margin() {
        // Both v0.0.1 estimators leave the candidate rule at "any strict token win",
        // so selection is unchanged from the release that predates the margin.
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
