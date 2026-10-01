# tokfold fuzz targets

[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) targets for
`tokfold-core` and `tokfold-mcp`.

This directory is its own Cargo workspace with its own `Cargo.lock`. It is **not** a
member of the root workspace, so `cargo build`, `cargo test`, `cargo package
--workspace` and `cargo deny check` at the repository root never compile it, and
nothing outside this directory needs a nightly toolchain.

## Prerequisites

- A nightly toolchain. cargo-fuzz builds with `-Zsanitizer=address` by default.
  CI pins `nightly-2026-09-28`; any recent nightly should work locally.
- cargo-fuzz. CI installs 0.13.2:

  ```sh
  rustup toolchain install nightly-2026-09-28 --profile minimal
  cargo install cargo-fuzz --version 0.13.2 --locked
  ```

## Running

Run every command from this directory (`fuzz/`):

```sh
cargo +nightly fuzz list                    # the five targets
cargo +nightly fuzz build                   # build them all
cargo +nightly fuzz run roundtrip corpus/roundtrip seeds/roundtrip -- \
    -max_total_time=300 -dict=json.dict
```

libFuzzer adds new inputs only to the **first** corpus directory, which here is
`corpus/<target>`. That directory is git-ignored. `seeds/<target>` is read and never
written, so the committed seeds stay small and reviewed.

cargo-fuzz 0.13.2 has no `--locked` option on `build`, `run` or `list`. To make sure
`Cargo.lock` is current, run `cargo +nightly fetch --locked` first. CI does exactly
that.

CI's `fuzz` job in `.github/workflows/ci.yml` runs every target from an empty
`corpus/<target>` and the committed `seeds/<target>`, for 60 seconds a target on a
pull request or a push to `main`, and for 600 seconds a target on the weekly
schedule and on a manual run. No run keeps the corpus it grew.

## Targets

Every target must never panic. What each one asserts on top of that:

| Target | Input | Asserts |
|---|---|---|
| `decompress` | arbitrary bytes, read twice: as an archive, and as an edit to an archive `compress` wrote | An accepted `Header::decode` consumes no more than it was given and re-encodes to exactly the bytes it consumed. A successful `Compressor::decompress` implies the header decodes, the output is the payload verbatim, and its length and SHA-256 are the ones the header records. The encoder id, tokenizer id and flags are checked against the fixed values `0`, `0` and the default -- not against the header -- because those are the only metadata v0.0.1 writes. On the edited archive the property has a second side: `decompress` accepts the bytes exactly when the edit changed none of them, and then the output is the document itself. See "What the `decompress` target reaches". |
| `roundtrip` | selector byte + document | On `Ok`, the Option B contract holds: the archive decompresses to the input byte for byte. The oracle is *not* asked about those bytes -- byte equality already settles the semantic half, so `oracle_eq(text, text)` would hold for a decoder that returned its own argument, and the target says so where the call was removed. The rendering starts with the sentinel of its encoder, and the body is then checked per encoder id -- but for only two of the three ids this build writes: `raw` (`0`) must be the input and `min` (`1`) must be `json_semantically_eq` to it, while `tbl` (`2`) falls to the `_ => {}` arm, so this target asserts nothing about a table body beyond its sentinel and its stats. The stats agree with `HeuristicEstimator` and the saving margin. A second compression is byte-identical. On `Err`, the variant must match the input: `NotUtf8` at `valid_up_to`, `InvalidJson` only where the oracle also rejects, `DepthExceeded` naming the lowered limit. Any other error fails the target. |
| `tape_parse` | selector byte + UTF-8 document | The tape is well formed: spans are in bounds on char boundaries, literals match their lexeme, strings and keys are quoted, depths are exact, and member and element counts match. An unbounded parse succeeds exactly when the oracle accepts. A depth limit either leaves the tape identical, gives `DepthExceeded` with that limit, or returns the same error as the unbounded parse. |
| `mcp_handle_line` | selector byte + session | Each `\n`-separated line goes to one `Server` in order. Every reply is a single line that parses with both `tokfold_mcp::json::parse` and the oracle. It fits the frame limit whenever the limit is 95 bytes or more. Below 95, a reply may reach 93 bytes plus the number of decimal digits in the limit, which is the length of the last-resort refusal. |
| `mcp_json` | UTF-8 text | Anything `json::parse` accepts, the oracle accepts too. The check runs in one direction only, because this parser deliberately rejects lone surrogates and duplicate keys. The writer reaches a fixed point: writing, re-parsing and writing again gives the same text, and the oracle accepts that text. |

### Selector bytes

The first input byte of four targets picks a configuration. For three of them the
rest of the input is the document or session; `decompress` reads an offset from the
next two bytes and takes everything after that as bytes to write.

- `roundtrip`
  - Bits 0-1 pick the profile: `0` conservative, `1` balanced, `2` and `3`
    aggressive.
  - Bit 2 overrides `min_saving_bps` with `700 * bits 3-6`, so `0` to `10 500`.
    `compress` clamps a margin above `10 000` down to it, which bits 3-6 reach only
    at `15`.
  - Bit 7 lowers `max_depth` to 4.
- `tape_parse` -- an unbounded parse runs whatever the selector says. The selector
  decides only whether a bounded parse runs beside it, to be compared against it:
  - bit 7 set: no bounded parse runs at all;
  - bit 7 clear: a bounded parse runs at depth limit `selector & 0x1F`, so bits 0-4,
    `0` to `31`. Bits 5 and 6 are unused.
- `mcp_handle_line` sets the frame limit:
  - `0`-`63` give `MAX_MESSAGE_BYTES` (32 MiB);
  - `64`-`127` give `95 + 16 * (byte - 64)`;
  - `128`-`255` give `byte - 128`.
- `decompress` -- the first three bytes drive the second half, which edits an archive
  `compress` really wrote. An input shorter than three bytes drives the first half
  only, and the first half reads the whole input either way.
  - Bits 6-7 pick the operation: `0` and `1` overwrite, `2` truncate, `3` insert.
  - Bits 0-5 pick the document, modulo the number of documents the target holds, so
    every value of them selects one.
  - Bytes 1-2 are a big-endian offset into the archive, taken modulo its length, so
    every offset is in range.
  - Byte 3 onwards is what gets written or inserted. Only the overwrite arm truncates
    it, with `let width = replacement.len().min(patched.len() - offset)`, because an
    overwrite has to stay inside the archive. The insert arm splices all of it in and
    the archive grows, so bytes past the end of the archive still reach `decompress`.
  - Two of the four operations can leave the archive exactly as `compress` wrote it:
    an overwrite whose replacement is empty (`patch-noop.bin`, measured accepted, the
    273-byte document back) and an insert whose replacement is empty, which splices
    nothing. The truncate arm never can, because the offset is taken modulo the
    archive length and so always drops at least one byte. The target compares bytes
    and not operations, so a replacement that happens to equal what it overwrites
    counts as unchanged too.

### The oracle

"The oracle" is `crates/tokfold-core/tests/oracle.rs`, the independent JSON parser
and semantic-equality check the core test suite uses. `src/lib.rs` includes it by
path. The oracle is recursive with no depth limit, so the targets consult it only
for inputs of at most 256 levels of bracket nesting and 64 KiB. Past that limit the
oracle's half of a check is skipped, and the code under test still runs.

## Seeds and dictionary

`seeds/` holds two kinds of input.

76 hand-built inputs, spread over the five `seeds/<target>/` directories. Seventy of
them come from fourteen originals -- seven JSON documents (`object`, `table` a
table-shaped array, `pretty` pretty-printed, `scalar`, `number`, `nested`, `empty` an
empty object) and seven MCP sessions (handshake, discover, compress, estimate,
decompress, batch and a malformed line) -- each copied once per selector byte the target
is worth driving under. That is what turns fourteen originals into seventy files; each
table's `Selector byte` column was read off the files, and what each one selects follows
from "Selector bytes". The other six carry no document at all: they are the
`patch-*.bin` files under `seeds/decompress/`, three and four bytes each, and each one
names a branch of `decompress` by derivation rather than a document:

| Directory | Files | Originals x selectors | Selector byte, and what it selects |
|---|---:|---|---|
| `seeds/decompress/` | 13 | 7 documents x 1, plus 6 branch inputs | The seven `.tkfd` files choose nothing: each is a valid archive of one document, not the document, and the second half reads its selector off the archive's own leading bytes -- which, since every archive opens with `TKFD`, is the *same* selector for all seven. The six `patch-*.bin` files are three or four bytes: a selector, a two-byte offset, and in four of the six one replacement byte. All thirteen select document index 2, which is the `decompress` target's own third document and not the `table` original in this directory. Derivations and the branch each one reaches: "What the thirteen `decompress` seeds reach" |
| `seeds/mcp_json/` | 7 | 7 documents x 1 | none: the target reads the whole input as text |
| `seeds/tape_parse/` | 14 | 7 documents x 2 | `0x80` (`<doc>.json`): the unbounded parse alone, no bounded one. `0x02` (`<doc>-d2.json`): a bounded parse at depth limit 2 beside it |
| `seeds/roundtrip/` | 28 | 7 documents x 4 | `0x00` (`<doc>-00.json`): conservative, no override. `0x01` (`-01`): balanced. `0x02` (`-02`): aggressive. `0x84` (`-84`): conservative with `min_saving_bps` overridden to 0 -- the do-no-harm gate off -- and `max_depth` lowered to 4 |
| `seeds/mcp_handle_line/` | 14 | 7 sessions x 2 | `0x00` (`<session>.txt`): the 32 MiB frame limit. `0x50` (`<session>-small.txt`): a 351-byte limit, from `95 + 16 * (0x50 - 64)` |

1 264 files named `jsonts-*`, 17 040 bytes in total: 316 of the 318 cases in the
[JSONTestSuite](https://github.com/nst/JSONTestSuite) `test_parsing/` directory
(accept, reject and implementation-defined; the two the import leaves out are named
under **Not imported** below, and `import-jsontestsuite.sh` skips those two by name
and copies everything else), imported into each of the four targets that parse JSON:

| Directory | Files | Bytes | Selector byte |
|---|---:|---:|---|
| `seeds/mcp_json/` | 316 | 4 023 | none |
| `seeds/tape_parse/` | 316 | 4 339 | `0x1F`: depth limit 31, so the bounded and unbounded parse both run |
| `seeds/roundtrip/` | 316 | 4 339 | `0x00`: conservative profile, no overrides |
| `seeds/mcp_handle_line/` | 316 | 4 339 | `0x00`: 32 MiB frame limit, each line of the case is one message |

`decompress` gets none, because it reads `.tkfd` archives and not JSON.

- **Source:** https://github.com/nst/JSONTestSuite, commit
  `1ef36fa01286573e846ac449e8683f8833c5b26a` (`master`, 2024-11-22).
- **License:** MIT, read from that commit's `LICENSE` file. MIT requires the
  copyright and permission notice to travel with copies, so
  `seeds/JSONTestSuite.NOTICE` carries the source, the commit, what was changed and
  the full license text. It sits in `seeds/` and not in any `seeds/<target>/`, so
  no fuzz command in this README or in CI reads it as an input.
- **Regenerating:** `./import-jsontestsuite.sh /path/to/JSONTestSuite` from this
  directory, with that clone at the commit named under **Source**. It replaces every
  `jsonts-*` file.
- **Not imported:** `n_structure_100000_opening_arrays.json` (100 000 bytes) and
  `n_structure_open_array_object.json` (250 001 bytes). Without `-max_len`, libFuzzer
  generates inputs up to `max(4 096, largest seed)`: 4 096 is a floor and not a size
  read off the corpus, and only a seed above it raises the ceiling. Measured on
  `mcp_json` with `-runs=0`: 4 096 bytes with the imported seeds, whose largest is the
  1 000-byte `jsonts-i_structure_500_nested_arrays.json`, and 250 001 with those two
  added, for the same edge coverage (1 056) -- so a corpus of 1 000-byte files does
  not explain a 4 096-byte ceiling, and the two files do explain a 250 001-byte one.
  Deep nesting is probed separately, see "Measured runs".

Seeds for selector targets carry a leading selector byte, with one exception: the
seven `.tkfd` archives under `seeds/decompress/` begin with the `T` of the `TKFD`
magic, which the second half reads as a selector nobody chose (`0x54` is an
overwrite of the `table` document).

`json.dict` is a libFuzzer dictionary with the following tokens:

- JSON tokens and escapes, including surrogate pairs;
- the `⟦tkfd:v1:…⟧` sentinel and the `TKFD` magic;
- JSON-RPC and MCP keys and method names.

## Reproducing a crash

libFuzzer writes a failing input to `artifacts/<target>/crash-<hash>`. To re-run it:

```sh
cargo +nightly fuzz run <target> artifacts/<target>/crash-<hash>
cargo +nightly fuzz tmin <target> artifacts/<target>/crash-<hash>   # minimize it
```

A fixed crash should also get a regression test in the owning crate's test suite,
so that stable `cargo test` keeps checking it without a fuzzer.

## Measured runs

These runs were measured on 2026-09-28 on macOS arm64 (10 cores) with
`nightly-2026-09-28` and cargo-fuzz 0.13.2, in the default cargo-fuzz build: release,
debug assertions, overflow checks, AddressSanitizer. All five targets ran in parallel
with this command:

```
cargo +nightly fuzz run <target> corpus/<target> seeds/<target> -- \
    -max_total_time=300 -dict=json.dict -rss_limit_mb=4096
```

The command is not reproducible as written, because `seeds/<target>` has grown since:
the 316 `jsonts-*` files were added to the four JSON targets on 2026-09-30, and the six
`patch-*.bin` files to `seeds/decompress` later the same day. What this campaign read
was 28 seeds for `roundtrip`, 14 each for `tape_parse` and `mcp_handle_line`, 7 for
`mcp_json`, and for `decompress` the 7 `.tkfd` archives.

Every run exited 0. None of them found a crash:

| Target | Executions in 300 s | exec/s | Edge coverage | Features |
|---|---:|---:|---:|---:|
| `decompress` (first half only) | 3 334 136 | 11 076 | 246 | 417 |
| `roundtrip` | 959 870 | 3 188 | 2 152 | 7 398 |
| `tape_parse` | 1 617 390 | 5 373 | 1 222 | 5 002 |
| `mcp_handle_line` | 93 131 | 309 | 2 709 | 8 140 |
| `mcp_json` | 1 924 008 | 6 392 | 1 228 | 5 375 |

The `decompress` rows in both campaign tables were measured before the target grew its
second half. "What the `decompress` target reaches" has the measurement for the target
as it stands.

These numbers describe one run on one machine. They are not a coverage claim, and
their exec/s column may not be set against another campaign's -- see "The exec/s
columns are not comparable across campaigns".

Nesting was also probed directly. One million `[` bytes were fed to every target as
a single input: raw, and for `mcp_handle_line` also as the text of an MCP `compress`
call. No target overflowed its stack or panicked, measured in the same build.

### The exec/s columns are not comparable across campaigns

Every exec/s in this file is the figure libFuzzer printed on that run's own `DONE`
line. Read down one campaign's column and the rows are comparable. Read *across*
campaigns and they are not, and the reason is measured rather than assumed.

On 2026-09-30 the 2026-09-28 command was re-run as written -- five targets at once,
each over a copy of its accumulated `corpus/<target>`, no `-timeout`, 300 s, the same
nightly and the same build -- and every target came back faster, by a factor between
5.7 and 17.2:

| Target | 2026-09-28 | Re-run 2026-09-30 | Factor |
|---|---:|---:|---:|
| `decompress` | 11 076 | 190 598 | 17.2 |
| `roundtrip` | 3 188 | 18 203 | 5.7 |
| `tape_parse` | 5 373 | 40 123 | 7.5 |
| `mcp_handle_line` | 309 | 4 220 | 13.7 |
| `mcp_json` | 6 392 | 82 519 | 12.9 |

Four candidate causes were measured on `decompress`, each against arm A, a
31-second sequential run from an empty corpus directory that reached 252 728 exec/s.
Starting from a copy of the 58-entry `corpus/decompress` costs 6 per cent (arm B,
237 183). Running all five targets at once on ten cores costs 23 per cent (arm C,
193 839). Stretching one sequential run from 31 seconds to 301 costs nothing at all
(arm D, 254 185, at the same 967 edges). And the target's own second half *raised*
throughput rather than lowering it: 218 512 before it against 247 766 after, 31
seconds each over the same 7 `.tkfd` archives. Compounded, the two that cost anything
predict a factor of 1.39; the re-run measures 1.33 against arm A. Against 2026-09-28
the residual is 17.2.

The arms, with `$T` any scratch directory -- never the repository's own `corpus/`,
which a run would write to. Arms A to D read `seeds/decompress` as it stood on
2026-09-30, all 13 files:

```
# A  one run, empty corpus dir, alone on the machine            252 728 exec/s
cargo +nightly fuzz run decompress "$T/a" seeds/decompress -- \
    -max_total_time=30 -timeout=10 -dict=json.dict -rss_limit_mb=4096
# B  as A, but "$T/b" is a copy of corpus/decompress            237 183
# C  as A, but all five targets launched together               193 839
# D  as A with -max_total_time=300                              254 185
# E  the 2026-09-28 command, all five together, each over a
#    copy of its corpus/<target>, no -timeout, 300 s            190 598
```

**No cause has been established for the rest of the factor.** What is established is
that it is not a property of any one target's code, because it is there for all five.
Two runs of one unchanged configuration, each alone on the machine, also came back
11 per cent apart -- 224 479 and 252 728 -- so a single-digit difference between any
two rows in this file carries no information either. Edge coverage and features are
unaffected: the re-run reached the same 967 edges for `decompress`, and those columns
stay comparable across campaigns.

### With the JSONTestSuite seeds

Measured on 2026-09-30, same machine, toolchain and build as the 2026-09-28
campaign. All five targets ran in parallel, each from an empty corpus directory and
`seeds/<target>` as it stood then: the four JSON targets with their 316 `jsonts-*`
files, and `decompress` with its 7 `.tkfd` archives and none of the six `patch-*.bin`,
which did not exist yet:

```
cargo +nightly fuzz run <target> <empty dir> seeds/<target> -- \
    -max_total_time=130 -timeout=10 -dict=json.dict -rss_limit_mb=4096
```

Each run reached `Done` and wrote no artifact. None found a crash:

| Target | Executions in 130 s | exec/s | Edge coverage | Features |
|---|---:|---:|---:|---:|
| `decompress` (first half only) | 7 096 896 | 54 174 | 247 | 419 |
| `roundtrip` | 1 165 086 | 8 893 | 2 186 | 8 267 |
| `tape_parse` | 2 232 646 | 17 043 | 1 236 | 5 207 |
| `mcp_handle_line` | 137 891 | 1 052 | 2 757 | 9 126 |
| `mcp_json` | 2 943 154 | 22 466 | 1 276 | 5 870 |

Edge coverage of the seeds alone, before any mutation (`-runs=0`, empty corpus
directory), without and with the `jsonts-*` files. `decompress` has no row because it
gets none of them; its seeds-alone figures are in "What the `decompress` target
reaches":

| Target | Without | With |
|---|---:|---:|
| `roundtrip` | 1 357 | 1 858 |
| `tape_parse` | 739 | 1 102 |
| `mcp_handle_line` | 1 896 | 2 131 |
| `mcp_json` | 687 | 1 056 |

These numbers describe one run on one machine. They are not a coverage claim.

### What the `decompress` target reaches

`decompress` is the one target whose first half arbitrary bytes cannot drive, and the
campaign tables under "Measured runs" are the evidence: its edge coverage is the lowest
of the five, about a fifth of the next lowest, while its exec/s is the highest. The
reason is in `Compressor::decompress` in `crates/tokfold-core/src/compressor.rs`, which
returns `Ok` only after `format::verify_checksum` matches the header's 32-byte field
against the SHA-256 of the payload -- and that is the *last* thing it checks. A nonzero
encoder id, a nonzero tokenizer id, non-default flags, and a payload length disagreeing
with `original_len` are each refused before it. So every assertion the target makes
after `Ok` restates a check `decompress` itself performed on the way to that `Ok`: they
guard a refactor that drops one, not this build. Reaching `Ok` at all asks the fuzzer
for a correct SHA-256 of its own payload.

That is what the second half is for. It reads the same bytes as an edit to an archive
`compress` really wrote, and what that buys is `Ok`: an accepted archive, which
arbitrary bytes reach only by handing over a correct SHA-256 of their own payload. It
does not buy a rejection arbitrary bytes cannot reach. They reach every rejection,
`ChecksumMismatch` among them -- measured from a hand-built header over a zeroed
digest and from all 255 single-byte rewrites of a seed's last payload byte, as the
target's module doc records.

Against a known original the fail-closed property is checkable in one direction: an
edit that changed no byte must be accepted, so a rejection proves the bytes differ
from the ones `compress` wrote. The converse does not hold, and the target no longer
asserts it. Every byte of *one* archive is load-bearing -- the length varint reader in
`crates/tokfold-core/src/format.rs` rejects an overlong encoding, pinned by
`overlong_leb128_rejected`, so `original_len` has one spelling per value; the three
metadata fields are compared against fixed values; and the payload is covered by the
digest -- but that bounds corruption of *that* archive, not substitution of a
different one. The digest travels inside the bytes being edited, so a valid archive of
another document is accepted with no second preimage at all: `00 00 00` followed by
the 45-byte archive of `[]` overwrites the 45-byte archive of `{}` whole and
decompresses to `[]`, a 48-byte input away from a crash report about a crate bug that
does not exist (measured 2026-09-30; pinned by
`a_valid_archive_of_another_document_is_accepted` in the core crate's
`tests/hostile.rs`, which also pins that no single-byte edit of that archive is
accepted at all -- 0 of 11 475). So the target demands `patched == *archive` only of
an `Ok` that hands back *this* document, and treats any other document as a
legitimate answer. What a second preimage would buy is a different payload under the
*same* digest; the single-bit case of that is pinned in the crate's own suite, by
`decompress_rejects_every_single_bit_flip`.

### What the thirteen `decompress` seeds reach

The second half picks its document from `data[0] & 0x3F`, modulo the number of
documents the target holds -- and it holds **three**, built in `documents()`: `{}` at
2 bytes, a flat object at 45, and an 8-row table of `{"id":N,"name":"row-N","ok":true}`
at 273. Their archives are 45, 88 and 317 bytes (measured 2026-09-30); 45 is both the
first document's archive and the second document's length, which is a coincidence and
not a relation. Only the table's length crosses the one-byte `LEB128` boundary at 128,
which is what the two asserts in `documents()` pin: `original_len` is written in one
byte for some of them and in two for others.

Those three are the target's own. They are not the seven originals the `seeds/`
directories are built from, and the name `table` denotes a different document in each
set: the corpus `table` is four rows and 122 bytes -- `seeds/mcp_json/table.json` is
the document alone at 122, and the 123-byte copies in the other directories are that
document behind one selector byte -- so its archive would carry a one-byte
`original_len` where the target's carries two, and every offset in the `patch-*.bin`
table would land on a different field.

Every seed in `seeds/decompress/` selects index 2, the 8-row table: `TKFD` opens all
seven archives, and `0x54 & 0x3F` is 20, which is 2 modulo 3, while the patch
selectors `0x02`, `0x82` and `0xC2` all mask to 2 directly. So no seed reaches
document 0 or 1; only a mutation of byte 0 does. The seven `.tkfd` seeds also share
their operation and offset -- `0x54 >> 6` is 1, an overwrite, and `KF` is `0x4B46`,
which is 250 modulo 317 -- so they differ only in the replacement, and the three
longest are truncated to the 67 bytes left.

The six `patch-*.bin` files are the derived branches. Measured 2026-09-30 by replaying
each file's own rules onto that 317-byte archive and handing the result to
`tokfold expand` (release build of `0f759f6` with `crates/tokfold-{cli,core}` clean,
macOS arm64) -- the header layout each offset is read against is
`crates/tokfold-core/src/format.rs`: magic `0..4`, `version` `4`, `encoder_id` `5`,
`tokenizer_id` `6..8`, `flags` `8..10`, `original_len` from `10` as a 1-to-10-byte
`LEB128`, then the 32-byte digest, then the payload.

| Seed | Bytes | Op | Offset | Replacement | Field it lands on | What `decompress` does |
|---|---|---|---:|---|---|---|
| `patch-noop.bin` | `02 00 64` | overwrite | 100 | none | payload, zero wide | accepts; the 273-byte document back |
| `patch-meta.bin` | `02 00 05 01` | overwrite | 5 | `01` | `encoder_id` | `Corrupt` at byte 5 |
| `patch-len.bin` | `02 00 0b 00` | overwrite | 11 | `00` | second byte of `original_len` | `Corrupt` at byte 11 -- `91 00` is an overlong spelling of 17 |
| `patch-payload.bin` | `02 00 64 ff` | overwrite | 100 | `ff` | payload | `ChecksumMismatch` |
| `patch-insert.bin` | `c2 00 64 41` | insert | 100 | `41` | payload, archive grows to 318 | `Corrupt` at byte 10 -- the payload no longer matches `original_len` |
| `patch-short.bin` | `82 00 14` | truncate | 20 | none | cuts inside the digest | `Corrupt` at byte 12 -- the digest starts there and 8 of its 32 bytes survive |

Each name is therefore earned against this document and would not be against another:
`patch-len` names the length varint only because index 2's length needs two bytes, and
`patch-short` reports byte 12 only because the varint ends at 11.

Measured on 2026-09-30, same machine, toolchain and build as the runs under
"Measured runs", each from
a fresh corpus directory with the same flags, 30 seconds each:

| Target as of | Seed corpus | Seeds alone | After 30 s | Executions |
|---|---|---:|---:|---:|
| the first half only | the 7 `.tkfd` archives | 153 / 203 | 247 / 418 | 6 773 894 |
| both halves | the 7 `.tkfd` archives | 860 / 954 | 967 / 1 301 | 7 680 761 |
| both halves | all 13 seeds | 891 / 1 074 | 967 / 1 295 | 6 958 875 |
| both halves | the 6 `patch-*.bin` alone | 883 / 986 | not run | -- |

Each cell is edge coverage / features. No run found a crash or wrote an artifact.

Two things to read out of that. The second half is where the coverage is: the target
reaches nearly four times the edges it did, 247 to 967, and exec/s did not drop. The
comparison that shows that is the two rows for the 7 `.tkfd` archives, which differ in
nothing but the target: 218 512 against 247 766, so it rose, because the documents and
their archives are built once in a `OnceLock` and not per execution. Every exec/s in
this section is the figure libFuzzer printed on that run's own `DONE` line, which is
its executions over the 31 seconds it reported for a `-max_total_time` of 30. The
`all 13 seeds` row is not the one to set against the first-half row: it moves the seed
set as well as the target, and the 9 per cent between it and the `both halves` row for
those same 7 archives, 247 766 against 224 479, is not evidence about the seed set.
Two runs of the `all 13 seeds` configuration, each alone on the machine, came back
11 per cent apart: 224 479 and 252 728. And the six `patch-*.bin` seeds do not raise the
ceiling -- both seed sets land on the same 967 within 30 seconds. They move the
starting point, 860 to 891, and 883 of that is reached by 22 bytes of selector with no
document in them at all. What they buy is determinism: each one names a branch by
derivation rather than leaving the fuzzer to find it, which is what matters to a
60-second run in CI. `patch-noop.bin` writes nothing and must be accepted,
`patch-payload.bin` breaks the digest, `patch-meta.bin` sets the encoder id to `1`,
`patch-len.bin` makes the length varint overlong, `patch-short.bin` cuts the input
inside the digest, and `patch-insert.bin` makes the payload one byte longer than
`original_len`.

These numbers describe one run on one machine. They are not a coverage claim.
