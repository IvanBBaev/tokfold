//! The public compression facade: [`Compressor`], its [`Config`] builder, and the
//! [`Artifact`] a `compress` call returns.
//!
//! This module is the only entry point a caller needs. It wires the frozen pieces
//! together — UTF-8 validation, the lexeme-preserving [`tape`](crate::tape) parser,
//! the sealed [`encoder`](crate::encoder) candidate rule, and the
//! [`format`](crate::format) archive framing — behind a small, sans-io, synchronous
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
//! `decode_full` pattern in [`format`](crate::format) and lets
//! [`decompress`](Compressor::decompress) reconstruct the original byte-for-byte and
//! verify its `SHA-256` before returning anything.
//!
//! # Determinism
//!
//! The same logical input yields byte-identical `rendering` and `archive` on every
//! call and every build. Nothing here reads a clock, hashes with a per-process seed,
//! or iterates a `std` `HashMap`, so the provider's prompt cache stays warm (§10).

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
/// candidate rule and the do-no-harm guarantee (§7) are unconditional, and archive
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
}

impl ConfigBuilder {
    fn new() -> Self {
        Self {
            profile: Profile::default(),
            max_input_bytes: DEFAULT_MAX_INPUT_BYTES,
            max_depth: DEFAULT_MAX_DEPTH,
            estimator: Arc::new(HeuristicEstimator),
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
    /// [`HeuristicEstimator`]. Must be pure and deterministic (§10).
    #[must_use]
    pub fn estimator(mut self, estimator: Arc<dyn TokenEstimator>) -> Self {
        self.estimator = estimator;
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
        }
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// The frozen wire id of the encoder that produced a rendering.
///
/// Exposed in [`Stats`] so callers can attribute a result to an encoder without the
/// sealed [`Encoder`](crate::encoder) enum leaking into the public API.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderId(
    /// The wire id: `0` = passthrough, `1` = E1 minify, `2` = E2 tabular.
    pub u8,
);

/// What a compression pass achieved.
///
/// The `*_before` / `*_after` pairs describe the model-facing rendering; the ratios
/// are the headline numbers. A passthrough result reports both ratios as exactly
/// `1.0` — "couldn't compress" is a statistic, never harm.
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
    /// Estimated tokens of the rendering; equals `est_tokens_before` for passthrough.
    pub est_tokens_after: usize,
    /// Which encoder shaped the rendering.
    pub encoder: EncoderId,
    /// Id of the estimator that drove selection (`format` field 4).
    pub tokenizer_id: u16,
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
    /// Token-reduced, sentinel-framed text for model context. Canonicalized, so not
    /// byte-identical to the input.
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
        let selection = encoder::select(&parsed, text, estimator, enabled);

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
            assert!(matches!(art.stats.fidelity, Fidelity::Lossless));
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
}
