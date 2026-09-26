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
impact classes below.

## Supported Versions

| Version | Distributed on | Supported |
|---|---|---|
| `0.0.1` | npm (`tokfold` and its five platform packages) | Yes — the only supported version |
| anything else | — | No |

`0.0.1` is published on **npm only**. Nothing has been published to crates.io or
PyPI, and no git tag has been created for it, so a build from this repository is
identified by its commit rather than by a tag.

Only the latest published `0.y` line receives security fixes; older `0.y` lines get
nothing. At 1.0 that becomes "latest `x.y` minor only". Fixes land on `main` first,
and a report is assessed against the current `main` as well as against the published
version.

## Reporting a Vulnerability

Report privately through **GitHub Private Vulnerability Reporting**: the
repository's **Security** tab → **Advisories** → **Report a vulnerability**.

**If that button is not there, private reporting has not been switched on yet.** It
is a repository setting, not something a reporter can work around. In that case open
a public issue containing *only* the sentence "security report, requesting a private
channel" — no details, no reproduction, no input — and a private channel will be
opened for you. Disclosing nothing but the existence of a report is what keeps that
fallback safe.

Otherwise, do **not** open a public issue, discussion, or pull request for a
suspected vulnerability, and do not disclose it elsewhere until the
coordinated-disclosure window has closed.

Please include the following. The **semantic-integrity impact class** is required —
it determines severity and triage order:

- **Semantic-integrity impact class** (pick the closest):
  - `silent-meaning-change` — decompression returns bytes that are not
    semantically equal to the original, yet no error is raised. Violates the core
    reversibility contract; treated as the highest severity.
  - `fail-open-on-corruption` — a corrupted or forged archive decodes to
    plausible-but-wrong bytes instead of returning a `DecompressError`.
  - `integrity-check-bypass` — the header checksum or a reserved-bit guard can be
    made to pass on content it should reject.
  - `availability` — crafted input panics, aborts, hangs, or exhausts memory in
    the engine (a denial-of-service against the host agent).
  - `confidentiality` — core, or a path reachable from it, performs I/O, reaches
    the network, or otherwise leaks transcript content.
  - `other` — describe it.
- **Affected component**: `tokfold-core`, `tokfold-cli`, or `tokfold-mcp`.
- **Affected version(s)** and, if known, the commit.
- **Description** of the flaw and its impact.
- **Reproduction**: the exact input bytes, the `Config` used, and the expected vs.
  actual result. A minimal reproducing input (attached or inlined) speeds triage
  enormously; a failing property-test case is ideal.

**Scope note on `tokfold-mcp`.** That crate is EXPERIMENTAL and explicitly not
hardened or audited — see its README and the notice it prints on startup. It shipped
in `0.0.1` in that state, labelled, rather than being held back until hardening
landed; hardening is a milestone this project still owes, not a gate the release
passed. Reports against it are welcome and will be recorded, and the impact classes
above still apply to it, but "unhardened" is the crate's declared state rather than a
vulnerability: a report that only restates the missing hardening will be closed as
known.

## Coordinated Disclosure

- We aim to acknowledge a report within **3 business days** and to send an initial
  assessment within **7 days**.
- We follow a **90-day coordinated-disclosure** window: the issue is disclosed
  publicly once a fix has landed on `main` or 90 days have elapsed, whichever comes
  first, via a **GitHub Security Advisory** on this repository. Because `0.0.1` is
  published on npm, an advisory can be keyed to the npm package name and reach
  `npm audit`. A **RUSTSEC** entry cannot: that database keys advisories to a
  crates.io package name, and nothing has been published to crates.io. If a fix
  needs more time we will say so and agree a revised date with you.
- Please keep the report private until that window closes.

## Acknowledgments

Reporters who follow this policy are credited in the **GitHub Security Advisory** and
in the `CHANGELOG.md` entry for the version that carries the fix, unless you ask to
remain anonymous. Those two are the whole record: this project publishes no separate
release notes, and an npm package page carries none of its own. There is **no
bug-bounty program** and no monetary reward — thanks and credit only.
