//! Compress a tool result, hand the rendering to the model, keep the archive, and
//! recover the original from it.
//!
//! Run with `cargo run -p tokfold-core --example roundtrip`.

use tokfold_core::{Compressor, Config, EncoderId};

/// A tool result of the shape the tabular encoder is built for: an array of objects
/// that share one key list.
const TOOL_RESULT: &str = r#"{
  "result": [
    { "number": "INC0010001", "state": "New",         "priority": "2 - High",     "assigned_to": "beth.anglin" },
    { "number": "INC0010002", "state": "In Progress", "priority": "3 - Moderate", "assigned_to": "david.loo" },
    { "number": "INC0010003", "state": "On Hold",     "priority": "4 - Low",      "assigned_to": "beth.anglin" },
    { "number": "INC0010004", "state": "Resolved",    "priority": "2 - High",     "assigned_to": "fred.luddy" },
    { "number": "INC0010005", "state": "New",         "priority": "1 - Critical", "assigned_to": "david.loo" }
  ]
}"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `Config::default()` is the `Balanced` profile with the heuristic estimator.
    // A `Compressor` holds no state between calls and is `Send + Sync`, so one
    // instance can serve every thread.
    let engine = Compressor::new(Config::default());

    let artifact = engine.compress(TOOL_RESULT.as_bytes())?;
    let stats = &artifact.stats;

    // The rendering is what goes into the model's context. When an encoder wins, its
    // first line is the `#tkfd1 <tag>` sentinel naming the encoder; a consumer that
    // splits renderings apart must cut at the first newline, never search for the
    // sentinel. A passthrough rendering has no sentinel: it is the input itself.
    println!("--- rendering (what the model reads) ---");
    println!("{}", artifact.rendering);
    println!("--- stats ---");
    println!("encoder id:        {}", stats.encoder.0);
    println!(
        "bytes:             {} -> {} (ratio {:.3})",
        stats.bytes_before,
        stats.bytes_after,
        stats.byte_ratio()
    );
    println!(
        "estimated tokens:  {} -> {} (ratio {:.3})",
        stats.est_tokens_before,
        stats.est_tokens_after,
        stats.token_ratio()
    );
    println!("archive bytes:     {}", artifact.archive.len());

    // The input above is chosen so that an encoder wins; a different input may
    // legitimately come back as passthrough (see the `failure_paths` example).
    assert_ne!(stats.encoder, EncoderId::PASSTHROUGH);
    assert!(stats.est_tokens_after < stats.est_tokens_before);

    // The archive is the recovery path. In this version it holds the original bytes
    // verbatim behind a short header — it is as sensitive as the input itself, not
    // encrypted or obfuscated — so `decompress` is byte-exact today.
    let original = engine.decompress(&artifact.archive)?;
    assert_eq!(original, TOOL_RESULT.as_bytes());
    println!(
        "decompress:        {} bytes, identical to the input",
        original.len()
    );

    Ok(())
}
