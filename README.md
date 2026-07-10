# tokfold

An embeddable Rust engine that **reversibly** compresses LLM agent context — tool
outputs, JSON, logs — to cut token cost.

It is an engine, not an app: a deterministic, sans-io library you call at ingestion
to turn bulky context into a denser rendering the model reads, plus a recovery
archive that reconstructs the original on demand. A single `tokfold` binary wraps it
for the command line.

---

## Status

**v0.0.1 — skeleton under active development.**

- The public API is **unstable** and will change without deprecation windows.
- **Not published** to any registry. There is no `cargo add tokfold`, no
  crates.io page, no docs.rs page, and no CI badge, because none of those exist yet.
- The engine crate is `tokfold-core`; the CLI binary is `tokfold`; the MCP
  integration (`tokfold-mcp`) is an experimental stub.

This README describes what the project *is* and what it *refuses to claim*. It
contains no benchmark numbers on purpose — see [Benchmarks](#benchmarks).

---

## What "reversible" means here — precisely

Reversible means **semantic identity, not byte identity.** Decompressing an archive
reproduces a document that is *equal as a value tree* to the original, not one that
is equal byte-for-byte.

Preserved **exactly**:

- **Object key order** — never reordered.
- **Duplicate keys** — kept, and kept in their original order (never collapsed
  last-wins).
- **Array element order.**
- **Number lexemes**, byte-for-byte. `1.0` comes back as `1.0`, `1e3` as `1e3`,
  a 100-digit integer as those 100 digits. Numbers are never round-tripped through
  `f64`.

Canonicalized (deliberately **not** preserved):

- **Insignificant whitespace.** Reproducing the exact whitespace would mean
  preserving the very bytes we are paid to delete.
- **String escape style.** Strings are compared *after* unescaping, so `"é"` and
  `"é"` are the same string; decompression emits one canonical escaping.
  Lone-surrogate `\uXXXX` escapes are retained as raw lexemes so they survive a
  round trip even though they cannot be unescaped to valid UTF-8.

That is the whole reversibility contract. It is enough to feed the reconstructed
bytes to anything that consumes JSON as data; it is *not* a promise that the
formatting matches.

## Why this is not called "lossless"

The word "lossless" is avoided on purpose, and the distinction is the crux of the
project.

The model does not read the reconstruction. **The model reads the compressed
rendering.** So even a perfect byte-for-byte reconstruction of the archive would
prove nothing about whether the model understood the *rendering* it was actually
shown. Byte-reversibility is a property of the recovery path; comprehension is a
property of the read path; they are different things.

The honest claim is therefore two-part:

1. **Reversible** — the archive reconstructs the original value tree, verified by a
   header checksum on decode.
2. **Measured comprehension fidelity** — how well a model reads the rendering, to be
   established with a public corpus and a reproducible harness, not asserted.

"Lossless" would collapse those two into a single word that only covers the first.
We will not do that.

---

## Guarantees

- **Zero network calls.** The engine never opens a socket. It is sans-io: no file,
  network, or clock access anywhere in the core.
- **Zero telemetry.** Nothing is phoned home, counted, or logged off-box.
- **No ML model.** The default token estimator is a pure arithmetic scanner, not a
  learned model — no weights to download, no inference to run, no megabytes of BPE
  data at rest.
- **Deterministic output.** The same logical input produces byte-identical output,
  every time, on every machine.

### Why determinism is load-bearing, not a nicety

Providers cache prompt *prefixes*: once a run of context bytes has been seen, a
later request that repeats those exact bytes reuses the cached prefix and is billed
at a fraction of the price. If a compressor emits *different* bytes for the *same*
logical content — because a hash iterates in random order, or a timestamp leaks into
the output — the prefix no longer matches, the cache silently misses, and you pay
full price again. Nondeterministic output does not just fail to help; it actively
*costs money* by invalidating a cache you were relying on. Determinism is what makes
the savings survive a multi-turn agent loop.

---

## Compression ratio is a secondary axis

A reversible compressor keeps every byte of information recoverable. By construction
it cannot beat a *lossy* compressor on raw ratio, because a lossy compressor is
allowed to throw information away and tokfold is not. If your only metric is "how
small is the output," a lossy tool will win that column, and it will win it *by
design*. That is not a bug to be closed; it is the cost of the guarantee.

The axes tokfold actually optimizes for are:

1. **Latency** — it is embeddable and starts instantly; there is no model to load
   and no service to call.
2. **Fidelity risk** — bounded, because the transform is reversible and the recovery
   path is checksum-verified.

Ratio is reported as a statistic (`byte_ratio`, `token_ratio`), never as the
headline.

---

## When NOT to use this

Reach for a different tool when its trade-off fits your problem better:

- **You can tolerate loss and want the maximum squeeze on prose → LLMLingua-2.**
  It runs an ML model to drop low-information tokens from natural-language text.
  Better ratio on prose, at the cost of a model and genuinely lossy output.
  tokfold is neither.
- **Your bulk is context you can stash out-of-band and re-fetch on demand →
  headroom (Python).** It pairs lossy compression with out-of-band cache retrieval:
  the payload lives in a cache and is pulled back when needed, rather than being
  reconstructed inline. A different architecture for a different shape of workload.
- **The tokens you want to cut are MCP tool *schemas / definitions*, not tool
  *output* → mcp-compressor.** It shrinks the schema surface an agent is handed.
  tokfold compresses the content flowing *through* those tools, not their
  definitions.
- **You just want noisy dev-command output filtered down (test runners, build
  logs) → rtk.** It filters and trims command output for humans at the terminal.
  That is a different job from reversibly re-encoding structured context for a model.
- **Your problem is context *isolation*, not compression → context-mode.** It
  partitions and scopes what context a step or subagent can see. It does not shrink
  payloads; tokfold does not isolate them.

If two of these describe you, you may want that tool *instead of*, or *alongside*,
tokfold — they are not all mutually exclusive.

---

## Not a security boundary

tokfold is **not** a prompt-injection filter and must not be placed in a threat
model as one.

It does not inspect, sanitize, score, or neutralize adversarial content. It
preserves content faithfully — including hostile content — because faithful
preservation is the entire point. Malicious text that goes in comes back out in the
rendering, unchanged. Compressing context does not reduce injection risk, and this
project will never claim that it does.

---

## Benchmarks

**None published yet.** Numbers will ship with a versioned public corpus and a
reproducible harness.

Until that corpus and harness exist, any ratio, latency, or "cuts tokens by N%"
figure would be a number nobody could reproduce — which is exactly the marketing
dishonesty this project positions itself against. So there are zero performance
numbers in this README, and that is deliberate. When numbers arrive, they will
arrive with the corpus and the code to regenerate them.

---

## Shape of the API (current, unstable)

The public surface today, for orientation only — it will change:

```rust
use tokfold_core::{Compressor, Config};

let engine = Compressor::new(Config::default());

let artifact = engine.compress(input_bytes)?;   // Result<Artifact, CompressError>
send_to_model(&artifact.rendering);              // the denser view the model reads
store(&artifact.archive);                        // versioned recovery blob

let original = engine.decompress(&artifact.archive)?; // Result<Vec<u8>, DecompressError>
```

`compress` is **total on valid JSON**: if no encoder reduces the estimated token
count, it returns a passthrough artifact with ratio `1.0`. "Couldn't compress" is a
statistic, never an error. Invalid input (including `NaN`/`Infinity`, truncated
documents, or trailing garbage) returns a `CompressError`; the caller then forwards
the original bytes unmodified. The engine never repairs input.

### What is in v0.0.1 — and what is not

Present: the parser, the archive format, the token estimator, and the first
encoders (passthrough, whitespace minification, shape-deduplicated tabular
re-encoding). Reserved but **not implemented**: legend folding, alternative
tokenizer backends, language bindings, and any hardened MCP proxy. This is a
skeleton; treat every part of it as subject to change.

---

## Licence

Dual-licensed under either of

- MIT ([LICENSE-MIT](LICENSE-MIT))
- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.

**Minimum supported Rust version: 1.85** (edition 2024). MSRV changes are treated as
breaking while the crate is pre-1.0.
