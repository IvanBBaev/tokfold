//! Configure the engine: profiles, a different estimator, and a minimum saving.
//!
//! Run with `cargo run -p tokfold-core --example profiles`.

use std::sync::Arc;

use tokfold_core::estimator::ids;
use tokfold_core::{ByteLenEstimator, Compressor, Config, EncoderId, Profile};

/// Uniform records: minification can shrink them, and the tabular encoder can
/// shrink them further, so the profiles give different answers.
const RECORDS: &str = r#"[
  { "id": 1, "name": "alpha",   "size": 120, "kind": "file" },
  { "id": 2, "name": "beta",    "size": 340, "kind": "file" },
  { "id": 3, "name": "gamma",   "size": 0,   "kind": "dir"  },
  { "id": 4, "name": "delta",   "size": 78,  "kind": "file" },
  { "id": 5, "name": "epsilon", "size": 512, "kind": "file" }
]"#;

fn run(label: &str, config: Config) -> Result<EncoderId, Box<dyn std::error::Error>> {
    let artifact = Compressor::new(config).compress(RECORDS.as_bytes())?;
    let stats = &artifact.stats;
    println!(
        "{label:<34} encoder {}  tokenizer {}  tokens {:>3} -> {:>3}  bar {} bps  rejected a candidate: {}",
        stats.encoder.0,
        stats.tokenizer_id,
        stats.est_tokens_before,
        stats.est_tokens_after,
        stats.min_saving_bps,
        stats.gate_rejected_candidate,
    );
    Ok(stats.encoder)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `Conservative` offers minification only.
    let conservative = run(
        "Conservative",
        Config::builder().profile(Profile::Conservative).build(),
    )?;
    assert_eq!(conservative, EncoderId::E1_MINIFY);

    // `Balanced` (the default) also offers the tabular encoder, which wins here.
    // `Aggressive` offers the same encoders as `Balanced` in this version.
    let balanced = run("Balanced (default)", Config::default())?;
    assert_eq!(balanced, EncoderId::E2_TABULAR);

    // The estimator is the cost model that picks the winner. `ByteLenEstimator`
    // counts bytes; it is a reference model, never the default. The id it reports
    // is out-of-band only: the archive header does not record it in this version.
    let byte_len = Config::builder()
        .estimator(Arc::new(ByteLenEstimator))
        .build();
    run("Balanced, ByteLenEstimator", byte_len)?;

    // A minimum saving, in basis points of the input estimate (clamped to 10 000).
    // Every estimator this crate ships rates the sentinel line above zero, so none
    // can save 100 % of the input: at 10 000 bps every candidate is refused and the
    // input passes through. (A custom estimator that rates a framed rendering at zero
    // tokens can clear that bar.)
    let unreachable = run(
        "Balanced, min_saving_bps 10000",
        Config::builder().min_saving_bps(10_000).build(),
    )?;
    assert_eq!(unreachable, EncoderId::PASSTHROUGH);

    // With the `tiktoken` feature, the exact GPT tokenizers can drive selection:
    // `cargo run -p tokfold-core --example profiles --features tiktoken`.
    #[cfg(feature = "tiktoken")]
    {
        let cl100k = tokfold_core::Cl100kEstimator::new()?;
        let encoder = run(
            "Balanced, Cl100kEstimator",
            Config::builder().estimator(Arc::new(cl100k)).build(),
        )?;
        println!(
            "  (cl100k id is {}; encoder {})",
            ids::CL100K_BASE,
            encoder.0
        );
    }

    println!(
        "heuristic id {}, byte-length id {}",
        ids::HEURISTIC,
        ids::BYTE_LEN
    );
    Ok(())
}
