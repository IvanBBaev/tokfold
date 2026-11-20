//! Configure the engine: profiles, a different estimator, and a minimum saving.
//!
//! Run with `cargo run -p tokfold-core --example profiles`.

use std::sync::Arc;

use tokfold_core::estimator::ids;
use tokfold_core::{ByteLenEstimator, Compressor, Config, EncoderId, Profile};

/// Uniform records: minification can shrink them, and the table encoders can
/// shrink them further, so the profiles give different answers.
const RECORDS: &str = r#"[
  { "id": 1, "name": "alpha",   "size": 120, "kind": "file" },
  { "id": 2, "name": "beta",    "size": 340, "kind": "file" },
  { "id": 3, "name": "gamma",   "size": 0,   "kind": "dir"  },
  { "id": 4, "name": "delta",   "size": 78,  "kind": "file" },
  { "id": 5, "name": "epsilon", "size": 512, "kind": "file" }
]"#;

/// Records that repeat two long values: the value dictionary of `Aggressive` can
/// state each once, in a legend, and refer to it by index everywhere else.
const REPEATS: &str = r#"[
  { "id": 1, "caller": "8a5055c9c61122780043563ef53438e3", "group": "5137153cc611227c000bbd1bd8cd2005" },
  { "id": 2, "caller": "5137153cc611227c000bbd1bd8cd2005", "group": "8a5055c9c61122780043563ef53438e3" },
  { "id": 3, "caller": "8a5055c9c61122780043563ef53438e3", "group": "5137153cc611227c000bbd1bd8cd2005" },
  { "id": 4, "caller": "5137153cc611227c000bbd1bd8cd2005", "group": "8a5055c9c61122780043563ef53438e3" },
  { "id": 5, "caller": "8a5055c9c61122780043563ef53438e3", "group": "5137153cc611227c000bbd1bd8cd2005" },
  { "id": 6, "caller": "5137153cc611227c000bbd1bd8cd2005", "group": "8a5055c9c61122780043563ef53438e3" }
]"#;

fn run(label: &str, input: &str, config: Config) -> Result<EncoderId, Box<dyn std::error::Error>> {
    let artifact = Compressor::new(config).compress(input.as_bytes())?;
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
        RECORDS,
        Config::builder().profile(Profile::Conservative).build(),
    )?;
    assert_eq!(conservative, EncoderId::E1_MINIFY);

    // `Balanced` (the default) also offers the two table encoders, tabular and grid.
    // Both state the keys once, in a header; the grid wins here because it writes
    // the keys unquoted and each row without the brackets the tabular encoder uses.
    let balanced = run("Balanced (default)", RECORDS, Config::default())?;
    assert_eq!(balanced, EncoderId::E5_GRID);

    // `Aggressive` adds the value dictionary (refs). On these records no long value
    // repeats, so it declines and the grid still wins; on records that repeat two
    // 32-character ids it wins, where `Balanced` stays with the grid.
    let aggressive = || Config::builder().profile(Profile::Aggressive).build();
    assert_eq!(
        run("Aggressive", RECORDS, aggressive())?,
        EncoderId::E5_GRID
    );
    assert_eq!(
        run("Balanced, repeated ids", REPEATS, Config::default())?,
        EncoderId::E5_GRID
    );
    assert_eq!(
        run("Aggressive, repeated ids", REPEATS, aggressive())?,
        EncoderId::E3_REFS
    );

    // The estimator is the cost model that picks the winner. `ByteLenEstimator`
    // counts bytes; it is a reference model, never the default. The id it reports
    // is out-of-band only: the archive header does not record it in this version.
    let byte_len = Config::builder()
        .estimator(Arc::new(ByteLenEstimator))
        .build();
    run("Balanced, ByteLenEstimator", RECORDS, byte_len)?;

    // A minimum saving, in basis points of the input estimate (clamped to 10 000).
    // Every estimator this crate ships rates the sentinel line above zero, so no
    // encoder can save 100 % of the input: at 10 000 bps every candidate is refused
    // and the input passes through. (A custom estimator that rates a rendering with a
    // sentinel at zero tokens can clear that bar.)
    let unreachable = run(
        "Balanced, min_saving_bps 10000",
        RECORDS,
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
            RECORDS,
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
