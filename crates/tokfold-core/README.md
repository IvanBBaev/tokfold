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
string escape style are canonicalized. That is the floor rather than a
description of this version: because the v0.0.1 passthrough archive described
under **Status** is the original bytes verbatim, `decompress` is byte-exact
today, and a later encoder is allowed to give that up while keeping the value
tree. The word "lossless" is avoided on purpose: the model reads the
*rendering*, not the reconstruction, so byte-reversibility of the recovery path
would prove nothing about comprehension of the read path. The one public API
item that carries the word, `Fidelity::Lossless`, names the recovery path's
class and nothing more.

## Guarantees

- **Zero network calls.** Sans-io: no file, network, or clock access anywhere in
  this crate. A `cargo-deny` denylist mechanises part of one half of that
  guarantee — the HTTP, socket and DNS crates it names may not enter the workspace
  graph. A crate it does not name is not stopped, and `libc` and `nix` are
  deliberately not banned although they can open sockets (`sha2` pulls `libc` in
  through `cpufeatures` on `aarch64` Linux, Android and Apple targets and on
  `loongarch64` Linux), so the capability can still arrive with a crate. It reaches no further: `std::net` is
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
  Every pull request and every push to `main` measures one pinned colliding rendering
  on `x86_64` Linux and `aarch64` macOS (and, reporting only, `x86_64` Windows);
  32-bit targets, `sparc64` and `wasm64` are measured nowhere.
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

Three runnable programs in
[`examples/`](https://github.com/IvanBBaev/tokfold/tree/main/crates/tokfold-core/examples)
cover the round trip, the configuration knobs and the failure paths — `cargo run -p
tokfold-core --example roundtrip` (then `profiles`, `failure_paths`) from a checkout
of the repository. They are not part of the packaged crate.

`compress` is total on valid JSON within the configured limits — 16 MiB of input
and 512 levels of nesting by default, both raised through `ConfigBuilder` (the input
limit no further than 4 GiB less one byte, the most the parser's `u32` offsets can
address), and past either one it returns a `CompressError`. If no encoder reduces the estimated token
count, it returns a passthrough artifact with ratio `1.0`. "Couldn't compress" is
a statistic, never an error. A passthrough rendering is the input itself, byte for
byte, with no sentinel line, so on that path the `*_after` fields equal their
`*_before` counterparts because the rendering *is* the input. Every other rendering
opens with a one-line `#tkfd1 <tag>` sentinel (`min`, `tbl` or `jsonl`), and the
do-no-harm gate prices that line in: an encoder wins only when the estimate of its
whole rendering, sentinel included, is lower than the input's. (`0.0.1` renderings
open with a `⟦tkfd:v1:<tag>⟧` line instead, a passthrough one included; archives
did not change, and every `0.0.1` archive still decompresses.)

JSON Lines is the one input that is not a single JSON document and is still
accepted (after `0.0.1`, which rejected it): two or more JSON values, one per line,
blank and CRLF lines allowed, each line under the nesting limit on its own. Under
the default `Balanced` profile and `Aggressive` the records are rendered as the
tabular encoder renders the array of them, behind a `#tkfd1 jsonl` sentinel,
when that saves estimated tokens; `Conservative` passes them through. Either way
the archive holds the original bytes, so `decompress` returns the lines exactly.

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
License 2.0 ([LICENSE-APACHE](https://github.com/IvanBBaev/tokfold/blob/main/LICENSE-APACHE)) at your option. The two files
are symlinks here to the workspace root, and `cargo package` resolves them: the
tarball it builds carries their texts as regular files beside this README, byte-identical
to the root copies (`cargo package --list -p tokfold-core`, then `tar tvzf` the
`.crate`). Nothing has published that tarball: crates.io is still on hold.

Minimum supported Rust version: 1.85 (edition 2024).
