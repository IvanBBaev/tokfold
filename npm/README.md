# npm distribution

This directory is the npm side of `tokfold`. It ships the **CLI binary**, not the
engine: `npm i -g tokfold` gives you the `tokfold` command, exactly the binary
`cargo build -p tokfold-cli` produces. There are no JavaScript bindings to the
engine here and none are planned in this directory — that would be a native addon,
a different product with its own public surface to support.

Nothing in this directory is compiled or published by `cargo`. It is invisible to
the Rust build: no crate lists it, and every crate manifest carries an explicit
`include` allow list, so `cargo package` cannot pick it up by accident.

## Layout

```
npm/
  tokfold/                     the package a user installs
    package.json               declares optionalDependencies on all five below
    bin/tokfold                the launcher (JS); npm links this onto PATH
    lib/resolve.js             platform -> package mapping, and the error messages
  platforms/
    darwin-arm64/              one package per target; each holds one binary
    darwin-x64/
    linux-x64-gnu/
    linux-arm64-gnu/
    win32-x64/
```

## Why five extra packages instead of one

A single package would have to carry every platform's binary, so every user would
download all five to run one. The split is the standard answer (esbuild, biome,
swc use it): each platform package declares `os` and `cpu`, the main package lists
all five under `optionalDependencies`, and npm installs **only** the one that
matches the machine. The other four are skipped before download.

`optionalDependencies` rather than `dependencies` is what makes an unsupported
platform a clean failure: the install still succeeds, and the launcher explains
what happened when you run it. Under `dependencies`, `npm install` itself would
fail on any platform not in the list.

## Why the versions are pinned exactly

The main package depends on `"tokfold-darwin-arm64": "0.0.1"` — no caret, no
tilde. The launcher and the binary are two halves of one release: a semver range
would let npm pair a new launcher with an old binary from a cache, and the failure
would look like a tokfold bug rather than a resolution artefact. Every package in
this directory carries the same version, and `.github/workflows/release.yml`
refuses to publish if any of them disagrees with `[workspace.package].version` in
the root `Cargo.toml`.

## The binaries are not in this repository

Each `platforms/*/` directory has no `bin/` until a release run builds one. The
release workflow cross-compiles the five targets, drops each binary into the
matching directory, and publishes. A checkout of this repository therefore cannot
publish a working package by hand, which is deliberate — the workflow verifies the
binary exists and refuses to publish an empty package.

## The licences are copied in at publish time, not linked

No `LICENSE-MIT` or `LICENSE-APACHE` sits in these directories, and none may be
added as a symlink. The crates reach the repository-root licences through
symlinks, which is fine there because cargo follows a symlink when it builds a
`.crate`.

**npm does not.** `npm pack` drops symlinks silently — no warning, no error, just
a tarball missing files that `files` says are in it. Linking here would have
published six packages under `MIT OR Apache-2.0` carrying neither licence text,
which both licences require to accompany a distribution.

The release workflow copies the two root files into all six directories before
packing, so there is still exactly one canonical copy in the repository and
nothing to drift. A verify step fails the run if a symlink ever reappears under
`npm/`.

## Publishing

See `.github/workflows/release.yml`. It is `workflow_dispatch`-only and defaults
to a dry run; publishing takes an explicit `dry_run: false` plus a typed
confirmation. It is never triggered by a push, a tag, or a merge.

The publish step **skips any package whose exact version is already on the
registry**, so a run that failed halfway can simply be run again. This is not
theoretical tidiness: the first real release of `0.0.1` published four platform
packages and was then refused on the fifth, and npm never lets a version be
reused, so without the skip the only way out of a partial release would have been
to burn a version number.

### The npm spam heuristic will refuse a burst of new names

Six packages published back to back from an account that has never published
before is a shape npm's anti-spam heuristic rejects, with
`403 ... Package name triggered spam detection` on the name it stops at — even
though the name is free and the same `<pkg>-<os>-<arch>` convention is used by
esbuild, swc and others. It is a property of the account and the burst, not of
the name. Re-running the workflow is the fix; the skip above makes that free, and
the heuristic's window is measured in hours, not minutes.

Publishing the launcher **last** is what keeps this from being a user-visible
failure: a launcher whose `optionalDependencies` name a package that does not
exist installs happily and then fails at run time on exactly the platform whose
package is missing. Nothing observed this — the launcher was never published in
the incomplete state.
