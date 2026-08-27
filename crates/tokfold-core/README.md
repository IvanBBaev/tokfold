# tokfold-core

The engine crate of [tokfold](https://github.com/IvanBBaev/tokfold): reversible,
structure-aware context compression for LLM agents.

It is a library, not an app — deterministic and sans-io. You call it at ingestion
to turn bulky context (tool outputs, JSON, logs) into a denser rendering the model
reads, plus a recovery archive that reconstructs the original on demand.

## Status

**v0.0.1 — skeleton under active development.** The public API is unstable and
will change without deprecation windows.

At v0.0.1 every recovery archive is a passthrough blob: a ~43-byte `TKFD` header
followed by **the original bytes verbatim**. It is not encrypted, not encoded and
not obfuscated, so an archive is exactly as sensitive as its plaintext and must be
stored with the same care.

## What "reversible" means here

Semantic identity, not byte identity. Object key order, duplicate keys, array
order and number *lexemes* are preserved exactly; insignificant whitespace and
string escape style are canonicalized. The word "lossless" is avoided on purpose:
the model reads the *rendering*, not the reconstruction, so byte-reversibility of
the recovery path would prove nothing about comprehension of the read path.

## Guarantees

- **Zero network calls.** Sans-io: no file, network, or clock access anywhere in
  this crate. A `cargo-deny` policy mechanises one half of that guarantee — no
  HTTP, socket, or DNS *dependency* may enter the workspace graph, so the
  capability cannot arrive with a crate. It reaches no further: `std::net` is
  standard library and no lint can ban it, so first-party code opening a socket is
  caught by review rather than by the build, and `std::fs` and `std::time` are
  outside that policy's view entirely. The property holds by design; only part of
  it is mechanised.
- **Zero telemetry**, **deterministic output** — the same logical input produces
  byte-identical output on every machine, which is what keeps provider prefix
  caches hitting.
- **No ML model.** The default estimator is a pure arithmetic scanner, not a
  learned model: no weights to download, no inference to run, no megabytes of BPE
  data at rest in the default build. (The opt-in `tiktoken` feature embeds exact
  GPT BPE tables — lookup tables, not weights — and is off by default.)

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
a statistic, never an error. That `1.0` is a floor rather than a measurement: a
passthrough rendering still carries the 18-byte `raw` sentinel, which costs about
10 estimated (11 real `cl100k`) tokens more than the bare input, and on that path
the `*_after` fields are *set* equal to their `*_before` counterparts instead of
being measured. Callers that must account for every token should measure
`Artifact::rendering` directly.

**No benchmark numbers are published yet** — they will ship with a versioned
public corpus and a reproducible harness, or not at all.

This crate is **not** a prompt-injection filter and must not be placed in a threat
model as one.

## Features

- `tiktoken` (off by default) — exact GPT tokenizer estimators `Cl100kEstimator`
  (`cl100k_base`) and `O200kEstimator` (`o200k_base`), for callers that want
  selection driven by the tokenizer they are actually billed on. It embeds
  megabytes of BPE tables, which is why it is opt-in; it never changes the archive
  format.
- `hf` — reserved for a Hugging Face tokenizer backend, declared so the id space
  (`estimator::ids::HUGGING_FACE`) is stable. **It does nothing in v0.0.1**:
  enabling it changes no code.

## Licence

Dual-licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache License
2.0 ([LICENSE-APACHE](LICENSE-APACHE)) at your option.

Minimum supported Rust version: 1.85 (edition 2024).
