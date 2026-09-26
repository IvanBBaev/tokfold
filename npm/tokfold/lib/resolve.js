"use strict";

// Finds the prebuilt `tokfold` binary for the machine this is running on.
//
// The npm package a user installs carries no binary of its own. It declares
// `optionalDependencies` on five per-platform packages, each holding exactly one
// executable, and npm installs only the one whose `os`/`cpu` match. This module
// turns the running platform into the name of that package and then into a path.
//
// Every failure path here ends in a thrown Error whose message is meant to be
// read by a person at a terminal, because that is the only place it is ever
// shown. "Cannot find module" from a bare `require` would be true and useless.

const path = require("node:path");

/**
 * Node's `${process.platform}-${process.arch}` mapped to the package carrying the
 * matching binary.
 *
 * The keys are Node's names and the values are npm package names; the two do not
 * have to agree, and for Linux and Windows they deliberately do not. Node reports
 * `linux` for both glibc and musl systems, so the package names carry an explicit
 * `-gnu` suffix to leave room for `-musl` packages later without renaming what is
 * already published. `isMusl()` below is what actually keeps a musl machine from
 * resolving a glibc binary.
 *
 * Windows diverges for a duller reason: npm's registry refused `tokfold-win32-x64`
 * at publish time with "Package name triggered spam detection" -- three times
 * over two days, two of those refusals 1h38m apart on the first day, while the
 * four sibling names published from the same token in the same run went through.
 * Unscoped `<tool>-win32-x64` is the shape a wave of dependency-confusion squats
 * took, and the classifier appears to have learnt it.
 *
 * `git-cliff` is the one real precedent for renaming rather than scoping: it hit
 * the identical "spam detection" refusal in 2023 and renamed the same day, and
 * its own `git-cliff-win32-x64` is now a tombstoned `0.0.1-security` held by npm
 * staff. Two names usually cited alongside it are not precedents for this fix at
 * all: esbuild publishes the *scoped* `@esbuild/win32-x64` and turbo the *scoped*
 * `@turbo/windows-64`, and a scoped name never meets this classifier. Stated
 * honestly: both of those projects solved the problem by scoping, and scoping is
 * the only measure observed to sidestep the classifier outright. Renaming the
 * unscoped name is the narrower fix, and the one taken here.
 *
 * The key stays `win32-x64` because that is what Node reports; only the name on
 * the registry changed.
 *
 * Adding a target means: a new entry here, a new directory under
 * `npm/platforms/`, a new entry in this package's `optionalDependencies`, and a
 * new leg in the release workflow's build matrix. All four, or the result is a
 * package that resolves to nothing.
 */
const PACKAGES = Object.freeze({
  "darwin-arm64": "tokfold-darwin-arm64",
  "darwin-x64": "tokfold-darwin-x64",
  "linux-arm64": "tokfold-linux-arm64-gnu",
  "linux-x64": "tokfold-linux-x64-gnu",
  "win32-x64": "tokfold-windows-x64",
});

/**
 * True when the running Node links musl libc rather than glibc — Alpine, and the
 * `-alpine` container images built on it.
 *
 * This matters because `process.platform` is `linux` on both, so without this
 * check an Alpine user resolves a glibc binary and gets `Error loading shared
 * library libgcc_s.so.1` or a bare "not found" from the kernel — an error that
 * names neither musl nor tokfold and sends people to the wrong bug tracker.
 *
 * The detection reads Node's own diagnostic report: `glibcVersionRuntime` is
 * present in the header only when the runtime is linked against glibc. This is
 * the same technique napi-rs and lightningcss use. It is wrapped in a try/catch
 * because `process.report` can be missing or restricted in embedded runtimes, and
 * a failure to detect must not become a failure to run: assuming glibc is the
 * behaviour we had before this check existed, so an undetectable environment is
 * no worse off than it used to be.
 *
 * @returns {boolean}
 */
function isMusl() {
  if (process.platform !== "linux") {
    return false;
  }
  try {
    const report = process.report.getReport();
    return !report.header || report.header.glibcVersionRuntime === undefined;
  } catch {
    return false;
  }
}

/**
 * Absolute path to the `tokfold` executable for this machine.
 *
 * @returns {string}
 * @throws {Error} if the platform has no build, or the package holding it is not
 *   installed. The message says which of the two happened and what to do.
 */
function resolveBinaryPath() {
  const key = `${process.platform}-${process.arch}`;
  const pkg = PACKAGES[key];
  const musl = isMusl();

  // A musl host whose architecture has no build at all fails for two independent
  // reasons, and reporting either one alone costs the reader a round trip: the libc
  // message sends them to swap their base image, after which the arch is still
  // unsupported, and the arch message alone lets them go build a glibc target they
  // could not have run. So this branch names both and the remedy that answers both,
  // and an architecture that does have a build still falls through to the libc check
  // below with its own message unchanged.
  if (pkg === undefined) {
    throw new Error(
      `tokfold: no prebuilt binary for ${key}.\n` +
        `Supported: ${Object.keys(PACKAGES).sort().join(", ")}.\n` +
        (musl
          ? "This build of Node also links musl libc (Alpine or an -alpine image), and " +
            "tokfold ships glibc binaries only, so a supported architecture would still " +
            "need a glibc image.\n"
          : "") +
        "Build from source instead: cargo install --git https://github.com/IvanBBaev/tokfold tokfold-cli",
    );
  }

  if (musl) {
    throw new Error(
      "tokfold: this build of Node links musl libc (Alpine or an -alpine image), " +
        "and tokfold currently ships glibc binaries only.\n" +
        "Use a glibc-based image (for example node:22-slim instead of node:22-alpine), " +
        "or build from source: cargo install --git https://github.com/IvanBBaev/tokfold tokfold-cli",
    );
  }

  // Resolve the package's manifest and walk to the binary beside it, rather than
  // resolving the binary's subpath directly. `require.resolve` applies a
  // package's `exports` field to subpaths, so a future `exports` on a platform
  // package would silently break a direct subpath lookup; `package.json` stays
  // resolvable in practice and the join is plain path arithmetic that no manifest
  // field can intercept.
  let manifest;
  try {
    manifest = require.resolve(`${pkg}/package.json`);
  } catch (cause) {
    throw new Error(
      `tokfold: the package holding this platform's binary (${pkg}) is not installed.\n` +
        "This usually means the install ran with optional dependencies disabled " +
        "(npm ci --omit=optional, or NPM_CONFIG_OPTIONAL=false), or a lockfile built " +
        "on a different platform was reused without re-resolving.\n" +
        "Fix it by reinstalling the launcher with optional dependencies enabled: " +
        `npm install -g tokfold@${require("../package.json").version}, or re-run ` +
        "your project's install without --omit=optional.\n" +
        `Installing ${pkg} by itself is not the fix: the launcher finds it only ` +
        "beside its own install, not in the current directory, and the newest " +
        "version of it need not be the launcher's.",
      { cause },
    );
  }

  const exe = process.platform === "win32" ? "tokfold.exe" : "tokfold";
  return path.join(path.dirname(manifest), "bin", exe);
}

module.exports = { PACKAGES, isMusl, resolveBinaryPath };
