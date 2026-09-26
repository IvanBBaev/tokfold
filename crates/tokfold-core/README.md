# tokfold-core

The engine crate of [tokfold](https://github.com/IvanBBaev/tokfold): reversible,
structure-aware context compression for LLM agents.

It is a library, not an app — deterministic and sans-io. You call it at ingestion
to turn bulky context (tool outputs, JSON, logs) into a denser rendering the model
reads, plus a recovery archive that reconstructs the original on demand.

## Status

**v0.0.1 — skeleton under active development.** The public API is unstable and
will change without deprecation windows.

This crate is **not on crates.io**: there is no `cargo add tokfold-core` and no
docs.rs page, so using it as a library means building from this repository. What
shipped at `0.0.1` is the `tokfold` command-line binary, on npm.

At v0.0.1 every recovery archive is a passthrough blob: a `TKFD` header of 43 to 46
bytes (47 for an original of 2^28 bytes or more, which needs a raised
`max_input_bytes`) followed by **the original bytes verbatim**. It is not encrypted,
not encoded and not obfuscated, so an archive is exactly as sensitive as its
plaintext and must be stored with the same care.

## What "reversible" means here

Semantic identity, not byte identity. Object key order, duplicate keys, array
order and number *lexemes* are preserved exactly; insignificant whitespace and
string escape style are canonicalized. That is the floor rather than a description
of this version: because the archive above is the original bytes verbatim,
`decompress` is byte-exact today, and a later encoder is allowed to give that up
while keeping the value tree. The word "lossless" is avoided on purpose:
the model reads the *rendering*, not the reconstruction, so byte-reversibility of
the recovery path would prove nothing about comprehension of the read path. The one
public API item that carries the word, `Fidelity::Lossless`, names the recovery path's class and
nothing more.

## Guarantees

- **Zero network calls.** Sans-io: no file, network, or clock access anywhere in
  this crate. A `cargo-deny` policy mechanises one half of that guarantee — no
  HTTP, socket, or DNS *dependency* may enter the workspace graph, so the
  capability cannot arrive with a crate. It reaches no further: `std::net` is
  standard library and no lint can ban it, so first-party code opening a socket is
  caught by review rather than by the build, and `std::fs` and `std::time` are
  outside that policy's view entirely. The property holds by design; only part of
  it is mechanised.
- **Zero telemetry**, **deterministic output** — the same input bytes under the same
  configuration produce byte-identical output from one build (bytes, not the same
  JSON value: the archive keeps the input as written), which is what keeps provider prefix
  caches hitting. Across targets it is not established: the tabular encoder counts
  shapes by an `FxHasher` value that differs between 32- and 64-bit targets, so an
  input whose key lists collide on one and not the other is rendered differently.
  Only 64-bit `aarch64` has been measured.
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

`compress` is total on valid JSON within the configured limits — 16 MiB of input
and 512 levels of nesting by default, both raised through `ConfigBuilder` (the input
limit no further than 4 GiB less one byte, the most the parser's `u32` offsets can
address), and past either one it returns a `CompressError`. If no encoder reduces the estimated token
count, it returns a passthrough artifact with ratio `1.0`. "Couldn't compress" is
a statistic, never an error. That `1.0` is a floor rather than a measurement: a
passthrough rendering still carries the 18-byte `raw` sentinel, which costs 10
estimated (11 real `cl100k`, 13 real `o200k`) tokens more than a bare input that
opens with a non-whitespace byte — leading whitespace can merge with the
sentinel's newline and move each figure by at most one token (over every prefix
of up to six spaces, tabs, CRs and LFs before `{}`: 10–11 estimated, 10–12
`cl100k`, 12–13 `o200k`) — and on that path the `*_after` fields are *set* equal to their `*_before`
counterparts instead of being measured. Callers that must account for every token
should measure `Artifact::rendering` directly.

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

Dual-licensed under either of MIT ([LICENSE-MIT](https://github.com/IvanBBaev/tokfold/blob/main/LICENSE-MIT)) or Apache
License 2.0 ([LICENSE-APACHE](https://github.com/IvanBBaev/tokfold/blob/main/LICENSE-APACHE)) at your option. Both texts
also travel inside the published tarball, next to this file.

Minimum supported Rust version: 1.85 (edition 2024).
