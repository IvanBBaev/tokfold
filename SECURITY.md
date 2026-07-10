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

Only the **latest published minor** receives security fixes. While the project is
pre-1.0, that means the most recent `0.y` line; older `0.y` lines get nothing.

| Version | Supported |
|---|---|
| latest `0.y` minor | yes |
| any older `0.y` | no |

Once the project reaches 1.0, this table switches to "latest `x.y` minor only".

## Reporting a Vulnerability

Report privately through **GitHub Private Vulnerability Reporting**:
the repository's **Security** tab → **Advisories** → **Report a vulnerability**.

Do **not** open a public issue, discussion, or pull request for a suspected
vulnerability, and do not disclose it elsewhere until the coordinated-disclosure
window has closed.

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

## Coordinated Disclosure

- We aim to acknowledge a report within **3 business days** and to send an initial
  assessment within **7 days**.
- We follow a **90-day coordinated-disclosure** window: the issue is disclosed
  publicly (with an advisory and, where relevant, a RUSTSEC entry) once a fix is
  released or 90 days have elapsed, whichever comes first. If a fix needs more
  time we will say so and agree a revised date with you.
- Please keep the report private until that window closes.

## Acknowledgments

Reporters who follow this policy are credited in the published advisory and release
notes, unless you ask to remain anonymous. There is **no bug-bounty program** and
no monetary reward — thanks and credit only.
