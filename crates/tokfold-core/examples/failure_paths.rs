//! What the engine does when it cannot help: passthrough is a statistic, invalid
//! input is a `CompressError`, and a damaged archive fails closed.
//!
//! Run with `cargo run -p tokfold-core --example failure_paths`.

use tokfold_core::{CompressError, Compressor, Config, DecompressError, EncoderId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Compressor::new(Config::default());

    // 1. Nothing to gain. An already-minified document cannot be made cheaper, so
    //    the engine returns a passthrough artifact. That is a statistic, not an error.
    let minified = br#"{"ok":true,"id":42}"#;
    let artifact = engine.compress(minified)?;
    assert_eq!(artifact.stats.encoder, EncoderId::PASSTHROUGH);
    assert!((artifact.stats.token_ratio() - 1.0).abs() < f64::EPSILON);
    // The ratio is a floor, not a measurement: the passthrough rendering is the input
    // behind an 18-byte `raw` sentinel line, so it is longer than the input. Callers
    // that account for every token should measure the rendering itself.
    assert_eq!(artifact.rendering.len(), minified.len() + 18);
    println!(
        "passthrough: ratio {:.1}, rendering:",
        artifact.stats.token_ratio()
    );
    println!("{}", artifact.rendering);

    // 2. Not JSON. The engine never repairs input; the caller forwards the original.
    match engine.compress(b"{\"truncated\": [1, 2") {
        Err(CompressError::InvalidJson { byte_offset, msg }) => {
            println!("invalid JSON at byte {byte_offset}: {msg}");
        }
        other => return Err(format!("expected InvalidJson, got {other:?}").into()),
    }

    // 3. A damaged archive. Any integrity mismatch is an error, never partially
    //    recovered bytes. The SHA-256 in the header detects corruption, not
    //    tampering: it is unkeyed and travels with the payload.
    let mut archive = engine.compress(br#"{ "a": [1, 2, 3] }"#)?.archive;
    let last = archive
        .last_mut()
        .ok_or("compress returned an empty archive")?;
    *last ^= 0x01;
    match engine.decompress(&archive) {
        Err(DecompressError::ChecksumMismatch) => println!("damaged archive: checksum mismatch"),
        other => return Err(format!("expected ChecksumMismatch, got {other:?}").into()),
    }

    // 4. Not an archive at all.
    match engine.decompress(b"not an archive") {
        Err(DecompressError::BadMagic) => println!("foreign bytes: bad magic"),
        other => return Err(format!("expected BadMagic, got {other:?}").into()),
    }

    Ok(())
}
