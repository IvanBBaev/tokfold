//! `compress` then `decompress`, on arbitrary bytes and every profile.
//!
//! The first input byte picks the configuration; the rest is the document.
//!
//! * bits 0-1: profile (`0` conservative, `1` balanced, `2` and `3` aggressive);
//! * bit 2: when set, `min_saving_bps` is overridden with `700 * bits 3-6`
//!   (0 to 10 500, which `compress` clamps to 10 000);
//! * bit 7: when set, `max_depth` is lowered to 4 so the depth refusal is reached.
//!
//! Must never panic. On success the Option B contract must hold — the archive
//! decompresses to the input byte for byte, and the rendering the encoder vouches for
//! is `json_semantically_eq` to it — and so must the do-no-harm gate and determinism.
//! On failure the error must be the right one: the engine is total on valid JSON
//! within its limits.

#![no_main]

use libfuzzer_sys::fuzz_target;
use tokfold_core::{
    CompressError, Compressor, Config, HeuristicEstimator, Profile, TokenEstimator,
};
use tokfold_fuzz::{oracle_eq, oracle_valid};

const LOW_DEPTH: usize = 4;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, input)) = data.split_first() else {
        return;
    };
    let profile = match selector & 0b11 {
        0 => Profile::Conservative,
        1 => Profile::Balanced,
        _ => Profile::Aggressive,
    };
    let mut builder = Config::builder().profile(profile);
    if selector & 0b100 != 0 {
        builder = builder.min_saving_bps(u32::from((selector >> 3) & 0x0F) * 700);
    }
    let low_depth = selector & 0x80 != 0;
    if low_depth {
        builder = builder.max_depth(LOW_DEPTH);
    }
    let compressor = Compressor::new(builder.build());

    match compressor.compress(input) {
        Ok(artifact) => {
            let text = std::str::from_utf8(input).expect("compress accepted non-UTF-8");

            // Reversibility: byte-exact, and that is the whole of it. Byte equality
            // already settles the semantic half of the contract, so asking the oracle
            // about the restored bytes here would be `oracle_eq(text, text)` — true for
            // any implementation, including one that returns its own argument. The
            // oracle earns its place below, on the minified body, which is not the
            // input. If byte-exactness is ever relaxed, the oracle call belongs back
            // here and this comment is the reason it was removed.
            let restored = compressor
                .decompress(&artifact.archive)
                .expect("an archive compress just wrote does not decompress");
            assert_eq!(restored, input, "the archive does not restore the input");

            // The rendering: one sentinel line, then a body the encoder vouches for.
            let (frame, body) = artifact
                .rendering
                .split_once('\n')
                .expect("the rendering has no sentinel line");
            let stats = &artifact.stats;
            let tag = match stats.encoder.0 {
                0 => "raw",
                1 => "min",
                2 => "tbl",
                other => panic!("unknown encoder id {other}"),
            };
            assert_eq!(frame, format!("\u{27E6}tkfd:v1:{tag}\u{27E7}"));
            match stats.encoder.0 {
                0 => assert_eq!(body, text, "the raw body is not the input"),
                1 => {
                    if let Some(eq) = oracle_eq(text, body) {
                        assert!(eq, "the minified body is not semantically equal");
                    }
                }
                _ => {}
            }

            // The do-no-harm gate, recomputed from the default estimator.
            let estimator = HeuristicEstimator;
            assert_eq!(stats.bytes_before, input.len());
            assert_eq!(stats.est_tokens_before, estimator.estimate(text));
            if stats.encoder.0 == 0 {
                assert_eq!(stats.bytes_after, stats.bytes_before);
                assert_eq!(stats.est_tokens_after, stats.est_tokens_before);
            } else {
                assert_eq!(stats.bytes_after, artifact.rendering.len());
                assert_eq!(
                    stats.est_tokens_after,
                    estimator.estimate(&artifact.rendering)
                );
                assert!(stats.est_tokens_after < stats.est_tokens_before);
                let saved = (stats.est_tokens_before - stats.est_tokens_after) as u128;
                assert!(
                    saved * 10_000
                        >= stats.est_tokens_before as u128 * u128::from(stats.min_saving_bps),
                    "an accepted rendering does not clear its own margin"
                );
            }

            // Determinism: the same bytes under the same config, byte-identical output.
            let again = compressor.compress(input).expect("second compress failed");
            assert_eq!(again.rendering, artifact.rendering);
            assert_eq!(again.archive, artifact.archive);
        }
        Err(CompressError::NotUtf8 { byte_offset }) => {
            let err = std::str::from_utf8(input).expect_err("NotUtf8 on valid UTF-8");
            assert_eq!(byte_offset, err.valid_up_to());
        }
        Err(CompressError::InvalidJson { .. }) => {
            let text = std::str::from_utf8(input).expect("InvalidJson on non-UTF-8");
            if let Some(valid) = oracle_valid(text) {
                assert!(!valid, "compress refused a document the oracle accepts");
            }
        }
        Err(CompressError::DepthExceeded { limit, .. }) => {
            // The default limit (512) is beyond what the oracle may be asked about,
            // so only the lowered one is checked against the input's real nesting.
            if low_depth {
                assert_eq!(limit, LOW_DEPTH);
            }
        }
        Err(CompressError::InputTooLarge { .. }) => {
            panic!("InputTooLarge below the 16 MiB default on a fuzz-sized input");
        }
        Err(other) => panic!("unexpected error variant: {other:?}"),
    }
});
