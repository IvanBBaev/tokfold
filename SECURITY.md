# Security Policy

tokfold reversibly compresses agent context — tool outputs, JSON, logs and
transcripts — which routinely contain secrets and personal data. Two properties
are therefore security-critical, not merely correctness concerns:

- **Semantic integrity.** A compress/decompress cycle must reconstruct a
  semantically identical document, or fail loudly. A bug that *silently* alters
  meaning is a security bug.
- **Confidentiality.** `tokfold-core` performs no I/O and must never reach the
  network. A networking dependency in core would turn full transcripts into an
  exfiltration surface.

Note the deliberate non-claim: tokfold is **not** a prompt-injection filter and is
not marketed as reducing injection risk. Reports framed as "the compressor let a
malicious payload through" are out of scope unless they also demonstrate one of the
three semantic-integrity classes required under *Reporting a Vulnerability*:
`silent-meaning-change`, `fail-open-on-corruption` or `integrity-check-bypass`. The
impact-class field there has six members and naming the open-ended one (`other`) does
not clear this bar: a payload that survives a round trip intact is the format doing
its job.

A second deliberate non-claim: the archive's SHA-256 is **unkeyed** and travels inside
the archive it covers, so it detects corruption, not tampering — anyone who can modify
an archive can recompute the digest to match (`crates/tokfold-core/src/format.rs`
module docs; `docs/threat-model.md`, *Integrity of reconstruction*). A report that a
*forged* archive decodes to whatever the forger chose is therefore out of scope: that
is the documented design, and defending against it would need a keyed construction (a
MAC), which this format does not have and does not claim.

## Threat model and privacy

- [`docs/threat-model.md`](docs/threat-model.md) — trust boundaries, assets and
  mitigations for the library, the CLI and the experimental `tokfold mcp` server,
  with the code that holds each one, plus the issues that are known and still open.
  Check it before reporting: a limitation listed there as known is already recorded.
- [`docs/privacy.md`](docs/privacy.md) — what tokfold does and does not do with your
  data: network, telemetry, the files it reads and writes, what the MCP server sends
  back to its client, environment variables and the npm launcher.

## Supported Versions

| Version | Distributed on | Supported |
|---|---|---|
| `0.0.1` | npm (`tokfold` and its five platform packages) | Yes — the only published version |
| `main` | not distributed | Assessed — fixes land there first, and ship to users in the next published version |
| anything else | — | No |

`0.0.1` is published on **npm only**. Nothing has been published to crates.io or
PyPI, and no git tag has been created for it. No build embeds a commit either:
`tokfold --version` prints `tokfold 0.0.1` from every build of every commit, so the
binary cannot tell you which bytes you have. Say which commit you built from
(`git rev-parse HEAD`); for an installed package, identify the bytes by hashing the
file rather than by asking the binary.

Only the latest published `0.y` line receives security fixes; older `0.y` lines get
nothing. At 1.0 that becomes "latest `x.y` minor only".

## Reporting a Vulnerability

**GitHub Private Vulnerability Reporting is switched off on this repository**, so the
**Security** tab has no **Report a vulnerability** button today
(`gh api repos/IvanBBaev/tokfold/private-vulnerability-reporting` answers
`{"enabled":false}`, measured 2026-10-01). It is a repository setting, not something a
reporter can work around.

Until it is on, open a public issue whose **title** is "security report, requesting a
private channel" and whose body adds nothing — no details, no reproduction, no input
— and a private channel will be opened for you. GitHub will not accept an issue
without a title, so an issue cannot consist of a body alone: put the sentence in the
title and leave the body empty. Disclosing nothing but the existence of a report is
what keeps that fallback safe. If the button *is* there when you look, the setting was
enabled after this was measured: use it instead.

Beyond that one sentence, do **not** open a public issue, discussion, or pull
request for a suspected vulnerability, and do not disclose it elsewhere until the
coordinated-disclosure window has closed.

Please include the following. The **impact class** is required — it sets the severity
a report starts from (`silent-meaning-change` is treated as the highest). Three of its
six members are semantic-integrity classes and three are not, so naming one is not a
claim that the report is about reversibility:

- **Impact class** (pick the closest):
  - `silent-meaning-change` — decompression returns bytes that are not
    semantically equal to the original, yet no error is raised. Violates the core
    reversibility contract; treated as the highest severity.
  - `fail-open-on-corruption` — a corrupted or truncated archive decodes to
    plausible-but-wrong bytes instead of returning a `DecompressError`. A *forged*
    archive does not qualify: the digest is unkeyed by design, so a modifying
    adversary can always recompute it.
  - `integrity-check-bypass` — a check that should fail does not: an archive whose
    stored digest disagrees with the bytes it covers, or one with a reserved flag bit
    set, decodes without an error. Recomputing the digest over changed bytes is not a
    bypass; the digest is unkeyed by design.
  - `availability` — tokfold panics, aborts, hangs, or exhausts memory, denying
    service to the host agent. Crafted input is the usual route but not the only one:
    the MCP server's serve loop does not read while it writes, so a burst of
    well-formed requests can deadlock it. Any component counts — the library, the
    CLI, the MCP server, the npm launcher — not only the engine. That particular
    deadlock is measured and recorded under *Known limitations and open issues* in
    [`docs/threat-model.md`](docs/threat-model.md), so a report restating it will be
    closed as known.
  - `confidentiality` — transcript content reaches somewhere it was not asked to go.
    `tokfold-core`, or a path reachable from it, performing any I/O at all is the
    sharpest case, because core claims to perform none; a leak by the CLI, the MCP
    server or the launcher qualifies too, as does any of them reaching the network.
  - `other` — describe it.
- **Affected component**: `tokfold-core`, `tokfold-cli`, `tokfold-mcp`, or the npm
  launcher (`npm/tokfold/bin/tokfold`). The launcher ships inside the published
  package and decides what file descriptors the binary receives and what its exit
  reports, so a finding can live there with the Rust untouched.
- **Affected version(s)** and, if known, the commit.
- **Description** of the flaw and its impact.
- **Reproduction**: the exact input bytes, how tokfold was invoked, and the expected
  vs. actual result. What "invoked" means depends on the component: the `Config` for a
  library report, the whole command line for the CLI (including `--profile`, which
  selects the encoders), or the tool name and its arguments for an MCP report (`text`
  and optionally `profile` for `tokfold_compress` and `tokfold_estimate`, `archive`
  for `tokfold_decompress`). A minimal reproducing input (attached or inlined)
  speeds triage enormously; a failing property-test case is ideal.

**Scope note on `tokfold-mcp`.** That crate is EXPERIMENTAL and explicitly not
hardened or audited — see its README, and the notice the CLI prints when
`tokfold mcp` starts (`cmd_mcp` in `crates/tokfold-cli/src/main.rs` writes the
crate's `EXPERIMENTAL_NOTICE` to standard error). The crate itself prints nothing, so
a host that embeds it as a library and drives its own transport shows that warning to
nobody: the label travels with the CLI, not with the code. It shipped in `0.0.1` in
that state, labelled, rather than being held back until hardening landed; hardening is
a milestone this project still owes, not a gate the release passed. Reports against it
are welcome and will be recorded, and the **impact classes** still apply to it, but
"unhardened" is the crate's declared state rather than a vulnerability: a report that
only restates the missing hardening will be closed as known.

## Coordinated Disclosure

- We aim to acknowledge a report within **3 business days** and to send an initial
  assessment within **7 days**.
- We follow a **90-day coordinated-disclosure** window: the issue is disclosed
  publicly once a fix has landed on `main` or 90 days have elapsed, whichever comes
  first, via a **GitHub Security Advisory** on this repository. Because `0.0.1` is
  published on npm, an advisory can be keyed to the npm package name and reach
  `npm audit`. A **RUSTSEC** entry cannot: that database's crate advisories are keyed
  to a crates.io package name, and nothing has been published to crates.io. If a fix
  needs more time we will say so and agree a revised date with you.
- Please keep the report private until that window closes.

## Acknowledgments

Reporters who follow this policy are credited in the **GitHub Security Advisory** and
in the `CHANGELOG.md` entry for the fix, unless you ask to remain anonymous. Entries
land under `[Unreleased]` first and move into a released section when a version
carries them, so a fix made after `0.0.1` is credited under `[Unreleased]` until the
version that carries it ships. Those two are the whole record: this project publishes
no separate release notes, and an npm package page carries none of its own. There is
**no bug-bounty program** and no monetary reward — thanks and credit only.
