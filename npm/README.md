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
    windows-x64/
  tests/                       the launcher's test suite; not part of any package
```

## Tests

```sh
cd npm/tests && node --test
```

`npm/tests/` covers `bin/tokfold` and `lib/resolve.js`: the platform table against
the directories in `platforms/`, the musl refusal, the missing-package path, the
launcher's exit-code and signal contract, and what each of the six packages would
actually publish. `.github/workflows/ci.yml` runs it on every push and pull
request: Node 18 and 22 on Linux, and Node 22 on Windows and macOS.

The three operating systems are not redundancy. Windows is the only host where
`lib/resolve.js` takes its `.exe` branch for real, and the only one where the
packaging test has to go through a shell to reach `npm.cmd`; macOS is the only
non-Linux host that runs the process-level suite, and signals, orphan reaping and
inherited descriptors are kernel behaviour rather than something Linux can vouch
for on macOS's behalf.

What Windows skips is narrower than it looks. Only the tests that need the
launcher to *succeed* are skipped there, because the stand-in binary is a
`#!/bin/sh` script Windows cannot exec — so the exit-code and signal contract is
verified on Linux and macOS. Every test in which the launcher is supposed to
fail runs on all three, because none of them reaches the stand-in: an
unsupported platform, a platform package that is not installed, and a binary
that cannot be started all end before there is a child process, and those are
the paths a user is most likely to hit.

Two of those deserve naming, because they are the reason the `spawn` call is
wrapped in a `try`. Node reports most start-up failures on the asynchronous
`error` event but throws the rest synchronously, so "the file is there and the
kernel will not run it" arrives by a different route than "the file is not
there". The suite covers both: a plain file where `bin/` should be, which every
platform reports the same way, and a truncated ELF. The second one is probed for
first and skipped when the probe says the host started it anyway — glibc's
`execvp` retries an `ENOEXEC` through `/bin/sh`, so on a glibc Linux a corrupt
binary begins life as a shell script instead of failing, and asserting a
launcher-level failure there would be asserting something untrue.

Two constraints on anything added there. It uses **`node:test` and
`node:assert/strict` only** — the packages in this directory ship zero
dependencies, and a `node_modules` under `npm/` would be a regression in the thing
being tested, so there is no lockfile and nothing to install. And it lives *beside*
`tokfold/` rather than inside it, so no `files` entry can reach it and no test file
can end up in the published tarball.

That second one is asserted rather than assumed. `packaging.test.js` runs
`npm pack --dry-run` in all six package directories and checks the file list against
an allow list — an allow list rather than an exact set, because a release run stages
a binary and copies two licence files in before packing, so the exact set is
different in CI from what a checkout produces. It also asserts the launcher tarball
still contains the two files it exists to ship, since an allow list on its own is
satisfied by an empty package.

Run it with no path argument, from inside the directory. `node --test <dir>` only
accepts a directory from Node 22 onwards — on the Node 18 floor the package
declares, a positional is read as a module path and the run fails outright.

The launcher is spawned as a real process against a stand-in binary
(`fake-tokfold.sh`), because exit codes, signals and inherited descriptors are not
observable from inside the module. Non-host platforms are simulated by redefining
`process.platform`, `process.arch` and `process.report` from outside the launcher —
in-process for the resolver tests, via `node --require` for the process-level ones.
Nothing in `tokfold/` has a hook, flag or export that exists for the tests.

## The launcher outlives the binary, never the other way round

`bin/tokfold` spawns the binary **asynchronously** and forwards `SIGINT`,
`SIGTERM`, `SIGHUP` and `SIGQUIT` to it. `spawnSync` would be shorter and is
wrong: it blocks the event loop for the whole life of the child, so no signal
handler can run while the child is alive — and with no handler installed, a signal
sent to the launcher alone gets its default disposition and kills the launcher on
the spot, leaving the binary running, reparented to init, still holding the
caller's fds 0/1/2.

That is what `timeout`, a cancelled CI job, systemd with `KillMode=process`, and
any parent calling `child.kill()` on a pid all do. For `tokfold mcp` it means an
orphaned protocol server holding a client's pipes open after the client believes
it killed the session. `Ctrl-C` never showed it, which is why it went unnoticed:
a terminal interrupt goes to the whole foreground process group, so the binary was
signalled directly and died on its own.

`SIGKILL` is the exception no launcher can cover. Everything else is tested —
`launcher.test.js` signals the launcher's pid alone and asserts the binary is gone
afterwards, and those tests fail against the previous `spawnSync` version.

Two smaller things in the same file, for the same reason of being frozen once
published. Its own error messages go through `fs.writeSync(2, …)` rather than
`process.stderr.write`, which is documented as *asynchronous* when stderr is a TTY
on Windows and would let `process.exit` discard the one explanation the user gets.
And `lib/resolve.js` resolves `<pkg>/package.json` and joins to `bin/tokfold`
rather than resolving the binary's subpath directly, so a future `exports` field in
a platform package cannot intercept the lookup.

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

Four of the five are executed before they are published: the three native legs run
a compress/expand round trip on the runner that built them, and the aarch64 Linux
binary runs the same round trip under `qemu-user-static` against the cross sysroot
the linker step already installs. Only `darwin-x64` ships unexecuted — the macOS
runners are arm64 and Rosetta is not guaranteed to be present. Emulation is
invoked explicitly rather than through `binfmt_misc`, so the step does not depend
on whether the runner image has the handlers registered.

That is the workflow as it stands, and it is not retroactive. The four platform
packages that went out in the first release run were built before the aarch64
smoke test existed, when the step was gated on `if: matrix.native` — so the published
`tokfold-linux-arm64-gnu` binary has never been executed by anyone, anywhere. Later
release runs rebuild and smoke-test that target, which verifies the *source* at
that commit, but the skip means they do not republish it: the bytes a user
downloads stay the ones from the first run. Replacing them would cost a version
number. It is written down here rather than quietly fixed, because "verified" and
"verified for these exact bytes" are different claims and only the second one is
worth much. `darwin-x64` at `0.0.1` was likewise unexecuted in CI, though that one
has since been run by hand under Rosetta 2 and round-trips correctly.

The Windows binary is linked against the **static** C runtime. rustc does not do
that by default on `x86_64-pc-windows-msvc` — `rustc --print cfg` for that target
lists no `crt-static` feature — so a default build produces a `tokfold.exe` that
needs `VCRUNTIME140.dll`, which ships with the Visual C++ Redistributable and not
with Windows. A user installing a CLI from npm has not been told a C++ toolchain
is involved, and the failure they would get names a DLL rather than tokfold. The
flag is scoped to that one target as a `[target.<triple>]` entry, so it does not
reach build scripts or proc macros, which are host artefacts a bare `RUSTFLAGS`
would break.

Release builds also run **without a cargo cache**, unlike `ci.yml`. `--locked` pins
which dependency versions resolve; it says nothing about where a compiled `.rlib`
came from, so a restored cache entry would let the provenance attestation read
"built from commit X" while covering a binary partly made of code that commit
cannot account for.

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

The publish step **skips any package whose exact version is already on the registry
and was published by the account this run is authenticated as**, so a run that
failed halfway can simply be run again. This is not theoretical tidiness: the first
real release of `0.0.1` published four platform packages and was then refused on
the fifth, and npm never lets a version be reused, so without the skip the only way
out of a partial release would have been to burn a version number.

The publisher half of that condition is load-bearing, not paranoia. "Already there"
and "already ours" are different questions, and only the second one is safe to skip
on: these names are public, some of them are still unclaimed, and the design
publishes the most valuable one — `tokfold` itself — last. A bare existence check
would turn a squatter's upload into a workflow that reports success having uploaded
nothing, while `npm i -g tokfold` runs their code. So the step compares
`_npmUser.name` against `npm whoami` and stops the entire release on any other
answer, including a registry that will not say. A lookup that fails for any other
reason falls through to publishing, where npm itself rejects a duplicate version —
the cost of trying is an error, the cost of wrongly skipping is a release that
silently did not happen.

Publishing also runs with `--ignore-scripts`, and a verify-job step fails the run if
any of the six manifests grows a `scripts` block. Either half alone would do today;
both exist so that editing one away does not silently ship code that runs on every
user's machine at install time.

### Why the Windows package is `windows-x64` and not `win32-x64`

Node calls the platform `win32`, the table in `lib/resolve.js` keys on `win32-x64`,
and the obvious package name to match it is `tokfold-win32-x64`. That name is not
what is published, and the reason is worth writing down because nothing in the
code explains it.

What is known, rather than assumed. The first release run published four platform
packages and was refused on the fifth, `tokfold-win32-x64`, with
`403 Forbidden - PUT https://registry.npmjs.org/tokfold-win32-x64 - Package name
triggered spam detection`. A second run 1h38m later was refused on the same name
with the same message — and that run was not a burst: the ownership-checked skip
meant it attempted exactly one publish, and that one publish was still rejected. A
third run two days later, from the same token, was refused again. So this is not a
rate limit and not a property of publishing six names at once; it is a property of
that one name. The message says so.

The name was free the whole time — `tokfold-win32-x64` still 404s — so this is the
registry declining to create it, not a collision. Unscoped `<tool>-win32-x64` is
the exact shape a wave of dependency-confusion squats took, and the classifier
appears to have learnt it.

This is documented precedent, not a guess. `git-cliff` hit the identical refusal
on 2023-01-09 and wrote it up — same status, same URL shape, same sentence — and
renamed the same day (`git-cliff` commit `ce1d468`, "rename the NPM binary package
for Windows"). npm support's reply to that report said the block was deliberate
("we've initiated some blocks related to package names… as support, we're able to
move beyond the block") and unblocked the name by creating a placeholder and
transferring write access to the maintainer. That is why `git-cliff-win32-x64` is
a `0.0.1-security` stub held by an npm staff account and `git-cliff-windows-x64`
is the live one: the stub is a fossil of the block, not the remains of a
withdrawn package. esbuild and turbo ship `-windows-` names too.

`win32` is not categorically banned — recent unscoped `*-win32-x64` packages do
exist — but it is the token this name was rejected on, and `windows-x64` is the
ecosystem's own answer to the same problem. Two other reflexes are worth ruling
out in writing, because both look obvious and neither works: retrying the same
name does not clear the flag (no publicly documented case of it doing so, and
this project's own three attempts across two days are three more), and appending
`-msvc` does not either — several `*-win32-x64-msvc` names were refused with the
same message, so the signal is not carried by the trailing token. Scoping would
sidestep it entirely, since npm's package-name guidelines apply the similarity
and authorship rules only to unscoped names; that is a larger decision than this
release needed, and the names are frozen now that the launcher has shipped.

So the package is `tokfold-windows-x64`. The table key stays `win32-x64`, because
that is what `process.platform` reports and the key is not negotiable; only the
registry name changed. `lib/resolve.js` was built to allow exactly this — its keys
are Node's names and its values are npm's, and they already disagree for Linux.

This was free to do only because the launcher had not been published. The set of
package *names* stops being free the moment `tokfold` itself goes out with those
five names frozen into its `optionalDependencies`.

Publishing the launcher **last** is what keeps this from being a user-visible
failure, and the ordering matters more than it first looks. Under npm and pnpm, a
launcher whose `optionalDependencies` name a package that does not exist installs
happily and then fails at run time on exactly the platform whose package is
missing — bad, but confined to that platform.

**Yarn does not do that.** A 404 on an optional dependency is fatal during
resolution, before `os`/`cpu` filtering has a chance to rule the package out, so a
single missing platform package breaks every yarn install on *every* platform. This
was measured against yarn 4.9.2 and 1.22.22 with a byte-identical control tarball;
npm and pnpm skipped the same package cleanly. Publishing the launcher last is
therefore not tidiness. It is the difference between a partial release being
invisible and a partial release being broken for everyone.

Nothing observed either failure: the launcher has never been published in the
incomplete state.
