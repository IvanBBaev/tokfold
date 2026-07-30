# tokfold-core

The engine crate of [tokfold](https://github.com/IvanBBaev/tokfold): reversible,
structure-aware context compression for LLM agents.

It is a library, not an app — deterministic and sans-io. You call it at ingestion
to turn bulky context (tool outputs, JSON, logs) into a denser rendering the model
reads, plus a recovery archive that reconstructs the original on demand.

## Status

**v0.0.1 — skeleton under active development.** The public API is unstable and
will change without deprecation windows.

## What "reversible" means here

Semantic identity, not byte identity. Object key order, duplicate keys, array
order and number *lexemes* are preserved exactly; insignificant whitespace and
string escape style are canonicalized. The word "lossless" is avoided on purpose:
the model reads the *rendering*, not the reconstruction, so byte-reversibility of
the recovery path would prove nothing about comprehension of the read path.

## Guarantees

- **Zero network calls.** Sans-io: no file, network, or clock access anywhere in
  this crate. Enforced in CI by a `cargo-deny` ban on networking and socket crates.
- **Zero telemetry**, **no ML model**, **deterministic output** — the same logical
  input produces byte-identical output on every machine, which is what keeps
  provider prefix caches hitting.

## Usage

```rust
use tokfold_core::{Compressor, Config};

let engine = Compressor::new(Config::default());

let artifact = engine.compress(input_bytes)?;         // Result<Artifact, CompressError>
send_to_model(&artifact.rendering);                    // the denser view the model reads
store(&artifact.archive);                              // versioned recovery blob

let original = engine.decompress(&artifact.archive)?;  // Result<Vec<u8>, DecompressError>
```

`compress` is total on valid JSON: if no encoder reduces the estimated token
count, it returns a passthrough artifact with ratio `1.0`. "Couldn't compress" is
a statistic, never an error.

**No benchmark numbers are published yet** — they will ship with a versioned
public corpus and a reproducible harness, or not at all.

This crate is **not** a prompt-injection filter and must not be placed in a threat
model as one.

## Licence

Dual-licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache License
2.0 ([LICENSE-APACHE](LICENSE-APACHE)) at your option.

Minimum supported Rust version: 1.85 (edition 2024).
