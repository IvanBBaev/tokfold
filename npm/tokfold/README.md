# tokfold

Reversible, structure-aware context compression for LLM agents — the `tokfold`
command-line tool.

This package ships a prebuilt native binary. It is the command-line front end to
an embeddable Rust engine that compresses agent context (tool output, JSON, logs)
into a denser rendering a model reads, plus a recovery archive that reconstructs
the original on demand.

```
npm install -g tokfold
tokfold compress --input big.json --archive big.tkfd
tokfold expand   --input big.tkfd
tokfold stats    --input big.json
```

## Status — read this before using it

**v0.0.1. A skeleton under active development.** The interface is unstable and
will change without deprecation windows. There are no published benchmarks, and
the README in the repository explains at length why there are none yet.

The `tokfold mcp` subcommand starts an **experimental, unhardened, unaudited** MCP
stdio server. It sees everything passed through it. Do not put production secrets
through it.

## What "reversible" means here

Semantic identity, not byte identity. Decompressing an archive reproduces a
document equal to the original *as a value tree*. Object key order, duplicate keys,
array order and number lexemes are preserved byte-for-byte; insignificant
whitespace and string escape style are canonicalised, because reproducing the
exact whitespace would mean preserving the very bytes we are paid to delete.

The word "lossless" is avoided on purpose. The model does not read the
reconstruction — it reads the compressed rendering — so byte-reversibility of the
recovery path proves nothing about comprehension of the read path. Those are two
different claims and only the first one is proven today.

## Not a security boundary

tokfold is **not** a prompt-injection filter and must not be placed in a threat
model as one. It does not inspect, sanitize, score or neutralize adversarial
content; it preserves content faithfully, hostile content included, because
faithful preservation is the entire point.

**A recovery archive is not a protective wrapper.** At v0.0.1 every archive is a
passthrough blob: a ~43-byte header followed by the original bytes verbatim — not
encrypted, not encoded, not obfuscated. An archive is exactly as sensitive as its
plaintext.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | success — including a downstream that closed the pipe early |
| `2` | bad input: usage, I/O, or an input the compressor rejects |
| `3` | a corrupt or unrecoverable archive on `expand` |
| `1` | **the launcher failed and tokfold never ran** — unsupported platform, or the binary's package is not installed |

`1` is not a tokfold exit code. It is emitted only by this npm wrapper, so a
script can tell an installation problem apart from a data problem.

## Supported platforms

macOS arm64 and x64, Linux x64 and arm64 (**glibc**), Windows x64. The binary for
your machine arrives as an optional dependency; the other four are never
downloaded.

musl systems (Alpine and `-alpine` images) are **not** supported — the wrapper
detects musl and says so rather than failing with a confusing loader error. Use a
glibc image, or build from source:

```
cargo install --git https://github.com/IvanBBaev/tokfold tokfold-cli
```

## Licence

MIT OR Apache-2.0, at your option.

Source, full documentation and the honest-benchmarks discussion:
<https://github.com/IvanBBaev/tokfold>
