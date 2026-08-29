"use strict";

// Shared fixtures for the npm launcher tests.
//
// Almost every test here needs two things the running process cannot offer: a
// machine that is not this one, and a `node_modules` tree laid out the way npm
// lays one out. This module builds both, and nothing else in the suite touches
// the filesystem or `process` directly.
//
// The code under test is always the shipped file, byte for byte. `npm/tokfold`
// is *copied* into a scratch tree rather than read from the checkout, because
// `require.resolve("tokfold-darwin-arm64/package.json")` searches upward from
// the file doing the resolving: only a copy sitting under a real
// `node_modules/` can resolve a platform package, and only that layout is the
// one users get.

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

/** Repository root, reached from `npm/tests/`. */
const REPO_ROOT = path.resolve(__dirname, "..", "..");

/** The package a user installs: `npm i -g tokfold`. */
const PACKAGE_DIR = path.join(REPO_ROOT, "npm", "tokfold");

/** One directory per per-platform package, each shipping a single binary. */
const PLATFORMS_DIR = path.join(REPO_ROOT, "npm", "platforms");

/** The stand-in child process; see the script's own header. */
const FAKE_BINARY = path.join(__dirname, "fake-tokfold.sh");

/** `node --require` target that forges `process.platform` and friends. */
const PRELOAD = path.join(__dirname, "preload-platform.js");

/** @returns {object} the parsed `npm/tokfold/package.json`. */
function launcherManifest() {
  return readJson(path.join(PACKAGE_DIR, "package.json"));
}

/** @returns {string[]} sorted directory names under `npm/platforms/`. */
function platformDirs() {
  return fs.readdirSync(PLATFORMS_DIR).sort();
}

/**
 * @param {string} dir a name returned by {@link platformDirs}
 * @returns {object} that platform package's parsed manifest
 */
function platformManifest(dir) {
  return readJson(path.join(PLATFORMS_DIR, dir, "package.json"));
}

function readJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

// Scratch trees are removed when the test process ends rather than in an
// `after` hook: a failing assertion must be able to leave the tree behind long
// enough to be inspected, and `exit` fires on every path out including a
// throwing test file.
const scratchRoots = [];
process.on("exit", () => {
  for (const root of scratchRoots) {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

/**
 * Builds the `node_modules` layout npm produces, with a chosen subset of the
 * platform packages present.
 *
 * Which subset is the whole point. npm installs exactly one of the five, so
 * "the package for this machine is missing" is not an exotic state to simulate:
 * it is what every user on a platform whose package failed to publish sees, and
 * it is the current state of `tokfold-win32-x64`.
 *
 * @param {object} [options]
 * @param {string[]} [options.packages] platform package names to install
 * @param {string[]} [options.withBinary] which of those also get an executable
 * @param {string[]} [options.unrunnable] which get a `bin/tokfold` that exists
 *   and is executable but is not a program -- a truncated download, a partly
 *   written package, a binary built for another architecture
 * @param {string[]} [options.binIsFile] which get a plain file where `bin/`
 *   should be, so the path the launcher builds cannot resolve. Contrived on its
 *   own; kept because it is the one unstartable state every platform reports
 *   the same way, and `unrunnable` is not (see the launcher suite)
 * @returns {{root: string, launcher: string, resolveModule: string,
 *            packageDir: (name: string) => string,
 *            binaryPath: (name: string) => string}}
 */
function createInstall({
  packages = [],
  withBinary = [],
  unrunnable = [],
  binIsFile = [],
} = {}) {
  // `realpathSync` matters on macOS, where `os.tmpdir()` is `/var/folders/...`
  // but `require.resolve` returns the `/private/var/...` it really lives at.
  // Without it every expected path in the suite would differ from the resolved
  // one by a symlink nobody put there.
  const root = fs.realpathSync(
    fs.mkdtempSync(path.join(os.tmpdir(), "tokfold-launcher-test-")),
  );
  scratchRoots.push(root);

  const nodeModules = path.join(root, "node_modules");
  fs.mkdirSync(nodeModules, { recursive: true });
  fs.cpSync(PACKAGE_DIR, path.join(nodeModules, "tokfold"), { recursive: true });

  const byName = new Map(
    platformDirs().map((dir) => [platformManifest(dir).name, dir]),
  );

  // The real packages carry `tokfold.exe` on Windows; the launcher picks the
  // name from the *running* platform, so the fixture follows the host.
  const exe = process.platform === "win32" ? "tokfold.exe" : "tokfold";

  for (const name of packages) {
    const dir = byName.get(name);
    if (dir === undefined) {
      throw new Error(`no platform package named ${name} under npm/platforms`);
    }
    const dest = path.join(nodeModules, name);
    fs.mkdirSync(dest, { recursive: true });
    fs.cpSync(
      path.join(PLATFORMS_DIR, dir, "package.json"),
      path.join(dest, "package.json"),
    );

    if (binIsFile.includes(name)) {
      fs.writeFileSync(path.join(dest, "bin"), "not a directory\n");
      continue;
    }

    fs.mkdirSync(path.join(dest, "bin"), { recursive: true });
    const target = path.join(dest, "bin", exe);

    if (withBinary.includes(name)) {
      fs.copyFileSync(FAKE_BINARY, target);
      fs.chmodSync(target, 0o755);
    } else if (unrunnable.includes(name)) {
      // Shaped like a truncated ELF rather than filled with noise, because that
      // is what the state actually looks like in the field: an interrupted
      // download or an unpacked-but-incomplete package leaves a real header and
      // nothing behind it. Executable, so the failure is "cannot run this", not
      // "cannot read this".
      fs.writeFileSync(target, Buffer.from("\x7fELF\x02\x01\x01\0truncated"));
      fs.chmodSync(target, 0o755);
    }
  }

  return {
    root,
    launcher: path.join(nodeModules, "tokfold", "bin", "tokfold"),
    resolveModule: path.join(nodeModules, "tokfold", "lib", "resolve.js"),
    packageDir: (name) => path.join(nodeModules, name),
    binaryPath: (name) => path.join(nodeModules, name, "bin", exe),
  };
}

/**
 * Replaces the three `process` properties `lib/resolve.js` reads, and returns
 * the function that puts them back.
 *
 * All three are configurable own properties, so this is a redefinition rather
 * than a monkey-patch of anything the launcher owns; the launcher keeps reading
 * `process.platform` exactly as it does in production.
 *
 * `libc` is never defaulted from the host. See `preload-platform.js` for why
 * that would quietly turn every "linux" test on a macOS or Windows developer
 * machine into a musl test.
 *
 * @param {object} runtime
 * @param {string} runtime.platform
 * @param {string} runtime.arch
 * @param {"glibc"|"musl"|"no-header"|"absent"|"throws"} runtime.libc
 * @returns {() => void} restores the real values
 */
function stubRuntime({ platform, arch, libc }) {
  const saved = ["platform", "arch", "report"].map((key) => [
    key,
    Object.getOwnPropertyDescriptor(process, key),
  ]);

  const define = (key, value) =>
    Object.defineProperty(process, key, {
      value,
      configurable: true,
      enumerable: true,
      writable: false,
    });

  define("platform", platform);
  define("arch", arch);
  define("report", reportFor(libc));

  return () => {
    for (const [key, descriptor] of saved) {
      Object.defineProperty(process, key, descriptor);
    }
  };
}

function reportFor(libc) {
  switch (libc) {
    case "glibc":
      return { getReport: () => ({ header: { glibcVersionRuntime: "2.36" } }) };
    case "musl":
      // A musl build reports a header with no glibc version in it.
      return { getReport: () => ({ header: {} }) };
    case "no-header":
      // Older and cut-down runtimes emit a report with no header at all.
      return { getReport: () => ({}) };
    case "absent":
      // No diagnostic report API: `process.report.getReport` throws a
      // TypeError on a `undefined` receiver, which the launcher catches.
      return undefined;
    case "throws":
      return {
        getReport: () => {
          throw new Error("report generation is disabled in this runtime");
        },
      };
    default:
      throw new Error(`unknown libc fixture: ${libc}`);
  }
}

/**
 * Loads a scratch copy of `lib/resolve.js` under a forged machine.
 *
 * The module reads `process.platform` inside `resolveBinaryPath()` rather than
 * at load time, so the stub only has to be in place for the call. It is put
 * back before the assertion runs, which keeps a failing test from leaving the
 * test runner on a machine that does not exist.
 *
 * @param {ReturnType<typeof createInstall>} install
 * @param {Parameters<typeof stubRuntime>[0]} runtime
 * @returns {{path?: string, error?: Error}}
 */
function resolveUnder(install, runtime) {
  const { resolveBinaryPath } = require(install.resolveModule);
  const restore = stubRuntime(runtime);
  try {
    return { path: resolveBinaryPath() };
  } catch (error) {
    return { error };
  } finally {
    restore();
  }
}

module.exports = {
  FAKE_BINARY,
  PACKAGE_DIR,
  PLATFORMS_DIR,
  PRELOAD,
  REPO_ROOT,
  createInstall,
  launcherManifest,
  platformDirs,
  platformManifest,
  resolveUnder,
  stubRuntime,
};
