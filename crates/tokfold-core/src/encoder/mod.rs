//! The sealed encoder set and the candidate-based selection that chooses one.
//!
//! # Why the set is sealed
//!
//! `Encoder` is a `pub(crate)` enum, never a public trait. A third-party encoder
//! could emit a rendering it cannot reversibly reconstruct, silently breaking the
//! archive contract (§2.3), and its trait would freeze this module's internal
//! interfaces into public API. The **only** public extension point in the crate is
//! [`TokenEstimator`]: a caller may supply a cost
//! model, not a codec. An encoder reaches callers solely as an id inside `Stats`
//! (§7).
//!
//! # The candidate rule (§7, §8.2)
//!
//! Selection is a comparison, not a guess. For every enabled encoder `select`
//! renders a candidate and keeps it only when the estimator rates it *strictly*
//! below the original; the lowest estimate wins, ties broken by the lower encoder
//! id for determinism (§10). If nothing wins the result is `Encoder::Passthrough`
//! at ratio 1.0 — "couldn't compress" is a statistic, never an error.
//!
//! This byte-blind rule is mandatory, not cosmetic: minification is **not**
//! uniformly a token win (cl100k/o200k tokenize a multi-space indentation run and a
//! post-key `": "` as single tokens), so a byte-count objective would select
//! encoders that add tokens. The two `compress` guarantees follow directly:
//!
//! * **Total on valid JSON** — no winner yields a passthrough artifact, never an error.
//! * **Do-no-harm** — the chosen rendering's estimated tokens never exceed the
//!   input's, or the result is passthrough.

use crate::estimator::TokenEstimator;
use crate::tape::Tape;

mod e1_minify;
mod e2_tabular;

/// The sealed set of encoders. See the module docs for why this is an enum and not
/// a public trait. Ids are frozen (§3) and must never be renumbered; they identify the
/// winning encoder out-of-band via `Stats.encoder` and the rendering's sentinel tag,
/// not on the wire — v0.0.1 always records `encoder_id = 0` (passthrough) in the
/// archive header.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Encoder {
    /// No transformation; the input is emitted verbatim behind a `raw` sentinel. id 0.
    Passthrough,
    /// Token-aware JSON minification (§7). id 1.
    E1Minify,
    /// Shape-deduplicated tabular re-encoding (§7). id 2.
    E2Tabular,
}

impl Encoder {
    /// The frozen id (§3) for this encoder. In v0.0.1 the compressor writes only
    /// `encoder_id = 0` (passthrough) to the archive header; ids 1 and 2 identify the
    /// rendering out-of-band via `Stats.encoder` and the sentinel tag, never on the wire.
    pub(crate) const fn id(self) -> u8 {
        match self {
            Self::Passthrough => 0,
            Self::E1Minify => 1,
            Self::E2Tabular => 2,
        }
    }

    /// The sentinel tag opening this encoder's rendering: `raw`, `min` or `tbl` (§7).
    const fn tag(self) -> &'static str {
        match self {
            Self::Passthrough => "raw",
            Self::E1Minify => "min",
            Self::E2Tabular => "tbl",
        }
    }
}

/// Fixed opening of every rendering's sentinel line, before the encoder tag.
const SENTINEL_OPEN: &str = "\u{27E6}tkfd:v1:";

/// Closing bracket of the sentinel line.
const SENTINEL_CLOSE: char = '\u{27E7}';

/// Upper bound on the sentinel line's byte length, used only as a capacity hint.
const SENTINEL_MAX_BYTES: usize = 24;

/// Write the sentinel line `⟦tkfd:v1:<tag>⟧` plus a trailing newline into `out`.
fn write_sentinel(enc: Encoder, out: &mut String) {
    out.push_str(SENTINEL_OPEN);
    out.push_str(enc.tag());
    out.push(SENTINEL_CLOSE);
    out.push('\n');
}

/// Frame `body` as a full rendering: the sentinel line followed by the body.
fn framed(enc: Encoder, body: &str) -> String {
    let mut out = String::with_capacity(body.len().saturating_add(SENTINEL_MAX_BYTES));
    write_sentinel(enc, &mut out);
    out.push_str(body);
    out
}

/// Render `input` under `enc`, or `None` when the encoder does not apply.
///
/// The returned string is the full rendering the model reads: the sentinel line
/// followed by the encoder body. [`Encoder::Passthrough`] always applies and
/// returns the input verbatim behind a `raw` sentinel; the other encoders return
/// `None` only when the input does not fit their shape — E1 when a span cannot be
/// resolved, E2 when no array qualifies.
///
/// Rendering is not deciding: no encoder here measures its candidate against the
/// input, so a body returned by this function may well cost more than it saves.
/// [`select`] alone applies the token gate that keeps such a candidate from
/// winning.
pub(crate) fn render(enc: Encoder, tape: &Tape, input: &str) -> Option<String> {
    match enc {
        Encoder::Passthrough => Some(framed(enc, input)),
        Encoder::E1Minify => e1_minify::render(tape, input).map(|body| framed(enc, &body)),
        Encoder::E2Tabular => e2_tabular::render(tape, input).map(|body| framed(enc, &body)),
    }
}

/// The outcome of [`select`]: which encoder won, the rendering it produced, and the
/// token estimates that decided it.
pub(crate) struct Selection {
    /// The winning encoder ([`Encoder::Passthrough`] when nothing beat the input).
    pub encoder: Encoder,
    /// The full rendering, sentinel line included, ready for model context.
    pub rendering: String,
    /// Estimated tokens of the original input.
    pub est_tokens_before: usize,
    /// Estimated tokens of `rendering`; equals `est_tokens_before` for passthrough,
    /// so `token_ratio` is exactly 1.0 there. The `raw` sentinel is framing
    /// overhead, not counted as compression harm.
    pub est_tokens_after: usize,
}

/// Apply the candidate rule (§7, §8.2) and return the winning [`Selection`].
///
/// `enabled` lists the encoders to try; [`Encoder::Passthrough`] is always the
/// fallback and is skipped if present. Determinism is load-bearing (§10): the same
/// inputs always yield the same selection, so the tie-break to the lower encoder id
/// is part of the contract, not an implementation detail.
///
/// `min_saving_bps` raises the bar from "any strict token win" to "a win of at least
/// this fraction of the input estimate", so a caller can refuse decisions that sit
/// inside the estimator's known calibration error. At `0` the rule is byte-for-byte
/// the v0.0.1 rule.
pub(crate) fn select(
    tape: &Tape,
    input: &str,
    estimator: &dyn TokenEstimator,
    enabled: &[Encoder],
    min_saving_bps: u32,
) -> Selection {
    let est_before = estimator.estimate(input);
    let mut best: Option<(usize, Encoder, String)> = None;

    for &enc in enabled {
        if matches!(enc, Encoder::Passthrough) {
            // Passthrough is the fallback, never a candidate that must beat the input.
            continue;
        }
        let Some(candidate) = render(enc, tape, input) else {
            continue; // encoder does not apply to this input
        };
        let est = estimator.estimate(&candidate);
        if !clears_margin(est, est_before, min_saving_bps) {
            continue; // do-no-harm: keep only a win that clears the margin
        }
        let wins = match &best {
            None => true,
            Some((best_est, best_enc, _)) => prefers(est, enc.id(), *best_est, best_enc.id()),
        };
        if wins {
            best = Some((est, enc, candidate));
        }
    }

    match best {
        Some((est_after, encoder, rendering)) => Selection {
            encoder,
            rendering,
            est_tokens_before: est_before,
            est_tokens_after: est_after,
        },
        None => Selection {
            encoder: Encoder::Passthrough,
            rendering: framed(Encoder::Passthrough, input),
            est_tokens_before: est_before,
            est_tokens_after: est_before,
        },
    }
}

/// The do-no-harm gate: whether a candidate's estimated saving is large enough to
/// keep it.
///
/// Two conditions, deliberately separate. The strict `<` is the v0.0.1 rule and is
/// kept unconditionally, so a zero margin can never silently relax the gate to `<=`.
/// The second condition then demands that the saving be at least `bps` basis points
/// of the input estimate.
///
/// The comparison is cross-multiplied rather than divided so it stays exact integer
/// arithmetic — a float ratio would reintroduce rounding into a decision that must be
/// bit-deterministic (§10). `u64` keeps the product safe for the 16 MiB input ceiling
/// on a 32-bit target: the largest factor is `est_before * 10_000`, and `est_before`
/// cannot exceed the input's character count.
const fn clears_margin(est_after: usize, est_before: usize, bps: u32) -> bool {
    if est_after >= est_before {
        return false;
    }
    let saved = (est_before - est_after) as u64;
    saved.saturating_mul(BPS_SCALE) >= (est_before as u64).saturating_mul(bps as u64)
}

/// Basis-point scale: 10 000 bps = 100%.
const BPS_SCALE: u64 = 10_000;

/// Whether a candidate should replace the current best: strictly fewer estimated
/// tokens, or an equal estimate broken by the lower encoder id. The id tie-break
/// keeps selection deterministic across runs (§10), which is what preserves the
/// provider's prompt cache.
const fn prefers(new_est: usize, new_id: u8, best_est: usize, best_id: u8) -> bool {
    new_est < best_est || (new_est == best_est && new_id < best_id)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{Encoder, clears_margin, prefers, render, select};
    use crate::estimator::{ByteLenEstimator, HeuristicEstimator};
    use crate::tape;

    /// The v0.0.1 candidate rule: any strict token win is kept.
    const NO_MARGIN: u32 = 0;

    fn tape_of(input: &str) -> tape::Tape {
        tape::parse(input, 512).unwrap()
    }

    /// A pretty-printed object with `n` numeric members, one per 4-space-indented
    /// line. Each indentation run is a whitespace token the heuristic charges, so a
    /// large `n` is what lets minification clear the sentinel's fixed overhead.
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

    #[test]
    fn encoder_ids_are_frozen() {
        assert_eq!(Encoder::Passthrough.id(), 0);
        assert_eq!(Encoder::E1Minify.id(), 1);
        assert_eq!(Encoder::E2Tabular.id(), 2);
    }

    #[test]
    fn passthrough_rendering_carries_the_raw_sentinel() {
        let input = "{\"a\":1}";
        let t = tape_of(input);
        let r = render(Encoder::Passthrough, &t, input).unwrap();
        assert_eq!(r, format!("\u{27E6}tkfd:v1:raw\u{27E7}\n{input}"));
    }

    #[test]
    fn minify_rendering_carries_the_min_sentinel() {
        let input = "{ \"a\" : 1 }";
        let t = tape_of(input);
        let r = render(Encoder::E1Minify, &t, input).unwrap();
        assert!(r.starts_with("\u{27E6}tkfd:v1:min\u{27E7}\n"), "got {r:?}");
        assert!(r.ends_with("{\"a\":1}"), "got {r:?}");
    }

    #[test]
    fn select_passthrough_when_nothing_wins() {
        // Already minimal: E1 cannot beat it, and the sentinel only adds tokens.
        let input = "{\"a\":1}";
        let t = tape_of(input);
        let sel = select(
            &t,
            input,
            &ByteLenEstimator,
            &[Encoder::E1Minify, Encoder::E2Tabular],
            NO_MARGIN,
        );
        assert_eq!(sel.encoder, Encoder::Passthrough);
        assert_eq!(sel.est_tokens_after, sel.est_tokens_before);
        assert!(sel.rendering.starts_with("\u{27E6}tkfd:v1:raw\u{27E7}\n"));
    }

    #[test]
    fn select_do_no_harm_holds() {
        // Whatever wins, the reported after-count never exceeds the before-count, and
        // an accepted rendering satisfies the gate that admitted it — at every margin,
        // not just at zero.
        let inputs = [
            "{\"a\":1}",
            "{ \"a\" : 1 , \"b\" : [ 2 , 3 ] }",
            "[\n  1,\n  2,\n  3\n]",
            "\"plain string\"",
        ];
        for input in inputs {
            let t = tape_of(input);
            for bps in MARGIN_LADDER {
                let sel = select(&t, input, &HeuristicEstimator, &[Encoder::E1Minify], bps);
                assert!(
                    sel.est_tokens_after <= sel.est_tokens_before,
                    "harm on {input:?} at {bps} bps"
                );
                if sel.encoder != Encoder::Passthrough {
                    assert!(
                        clears_margin(sel.est_tokens_after, sel.est_tokens_before, bps),
                        "accepted a rendering that does not clear its own gate: \
                         {input:?} at {bps} bps"
                    );
                }
            }
        }
    }

    /// Margins swept by the gate tests: zero (the v0.0.1 rule), the measured heuristic
    /// over-claim, values either side of it, and the 100% extreme.
    const MARGIN_LADDER: [u32; 7] = [0, 100, 300, 600, 1200, 5000, 10_000];

    #[test]
    fn clears_margin_is_exact_at_the_boundary() {
        // 100 -> 94 saves exactly 600 bps of the input: accepted at 600, refused at 601.
        assert!(clears_margin(94, 100, 600));
        assert!(!clears_margin(94, 100, 601));
        // A zero margin is the strict v0.0.1 rule and never relaxes to `<=`.
        assert!(clears_margin(99, 100, NO_MARGIN));
        assert!(!clears_margin(100, 100, NO_MARGIN));
        assert!(!clears_margin(101, 100, NO_MARGIN));
        // A degenerate input is not a win at any margin.
        assert!(!clears_margin(0, 0, NO_MARGIN));
        // A 100% margin demands a rendering that costs nothing at all.
        assert!(clears_margin(0, 100, 10_000));
        assert!(!clears_margin(1, 100, 10_000));
    }

    #[test]
    fn a_margin_refuses_a_win_the_v0_0_1_rule_would_keep() {
        // The mechanism has to actually bite, or every other margin test passes
        // vacuously. Take a real E1 win and ask for one basis point more than it
        // claims.
        let input = pretty_object(20);
        let t = tape_of(&input);
        let kept = select(
            &t,
            &input,
            &HeuristicEstimator,
            &[Encoder::E1Minify],
            NO_MARGIN,
        );
        assert_eq!(kept.encoder, Encoder::E1Minify, "expected a win to refuse");

        let saved = kept.est_tokens_before - kept.est_tokens_after;
        let claimed_bps = u32::try_from(saved * 10_000 / kept.est_tokens_before).unwrap();
        let refused = select(
            &t,
            &input,
            &HeuristicEstimator,
            &[Encoder::E1Minify],
            claimed_bps + 1,
        );
        assert_eq!(refused.encoder, Encoder::Passthrough);
        assert_eq!(refused.est_tokens_after, refused.est_tokens_before);
        assert!(
            refused
                .rendering
                .starts_with("\u{27E6}tkfd:v1:raw\u{27E7}\n")
        );
    }

    #[test]
    fn raising_the_margin_only_ever_removes_winners() {
        // Monotonicity: a higher bar can turn a winner into passthrough, never the
        // reverse. Without this, a margin could silently *enable* an encoder on some
        // input and the knob would not be the conservatism dial it is documented as.
        let inputs = [
            "{\"a\":1}".to_string(),
            pretty_object(2),
            pretty_object(20),
            pretty_object(60),
            "[\n  1,\n  2,\n  3\n]".to_string(),
        ];
        for input in inputs {
            let t = tape_of(&input);
            let mut still_winning = true;
            for bps in MARGIN_LADDER {
                let sel = select(
                    &t,
                    &input,
                    &HeuristicEstimator,
                    &[Encoder::E1Minify, Encoder::E2Tabular],
                    bps,
                );
                let won = sel.encoder != Encoder::Passthrough;
                assert!(
                    still_winning || !won,
                    "raising the margin to {bps} bps re-enabled an encoder on {input:?}"
                );
                still_winning = won;
            }
        }
    }

    #[test]
    fn select_minify_wins_on_pretty_printed_input() {
        // The input must be large enough that stripped indentation saves more tokens
        // than the sentinel line costs: the candidate is the *framed* rendering, so
        // E1 only wins when the whitespace savings clear that fixed overhead. A tiny
        // object correctly stays Passthrough (see `select_small_input_stays_passthrough`).
        let input = pretty_object(30);
        let t = tape_of(&input);
        let sel = select(
            &t,
            &input,
            &HeuristicEstimator,
            &[Encoder::E1Minify, Encoder::E2Tabular],
            NO_MARGIN,
        );
        assert_eq!(sel.encoder, Encoder::E1Minify);
        assert!(sel.est_tokens_after < sel.est_tokens_before);
        assert!(sel.rendering.starts_with("\u{27E6}tkfd:v1:min\u{27E7}\n"));
    }

    #[test]
    fn select_small_input_stays_passthrough() {
        // A tiny pretty-printed object saves only a few indentation tokens — less
        // than the sentinel costs — so do-no-harm keeps it as Passthrough rather than
        // shipping a framed rendering that is a net token loss.
        let input = "{\n    \"alpha\": 1,\n    \"beta\": 2\n}";
        let t = tape_of(input);
        let sel = select(
            &t,
            input,
            &HeuristicEstimator,
            &[Encoder::E1Minify],
            NO_MARGIN,
        );
        assert_eq!(sel.encoder, Encoder::Passthrough);
        assert_eq!(sel.est_tokens_after, sel.est_tokens_before);
    }

    #[test]
    fn select_is_deterministic() {
        let input = "{\n  \"x\": [1, 2, 3],\n  \"y\": \"value\"\n}";
        let t = tape_of(input);
        let a = select(
            &t,
            input,
            &HeuristicEstimator,
            &[Encoder::E1Minify],
            NO_MARGIN,
        );
        let b = select(
            &t,
            input,
            &HeuristicEstimator,
            &[Encoder::E1Minify],
            NO_MARGIN,
        );
        assert_eq!(a.encoder, b.encoder);
        assert_eq!(a.rendering, b.rendering);
        assert_eq!(a.est_tokens_after, b.est_tokens_after);
    }

    #[test]
    fn tie_breaks_to_the_lower_encoder_id() {
        // Equal estimates resolve to the lower id; a strictly smaller estimate wins
        // regardless of id; an equal (estimate, id) never displaces the incumbent.
        assert!(prefers(5, 1, 5, 2));
        assert!(!prefers(5, 2, 5, 1));
        assert!(prefers(4, 2, 5, 1));
        assert!(!prefers(6, 1, 5, 2));
        assert!(!prefers(5, 1, 5, 1));
    }
}
