"use strict";

// Tests for what the six packages in `npm/` would actually publish.
//
// Every other suite here reads the checkout. This one asks npm, because the
// `files` allow list in a manifest is a claim about a tarball and not the
// tarball itself: npm adds `package.json`, the README and the licences whatever
// `files` says, an `.npmignore` anywhere above can subtract, a symlink is
// dropped silently, and `release.yml` stages binaries and copies the two root
// licences into these directories before packing. Only npm knows the result.
//
// The cost of being wrong is one-way. A published version can never be replaced
// -- npm refuses to reuse a version number -- so a file that reaches a tarball
// by accident is public for good, and withdrawing it costs a release.
//
// `npm/README.md` states the invariant this pins: no test file can end up in a
// published tarball. Until this file existed, nothing enforced it. The layout
// made it true -- the suite sits beside `tokfold/` rather than inside it -- and
// a human remembering to run `npm pack --dry-run` was how you would find out
// otherwise. A layout is not a check.

const assert = require("node:assert/strict");
const path = require("node:path");
const test = require("node:test");
const { spawnSync } = require("node:child_process");

const {
  PACKAGE_DIR,
  PLATFORMS_DIR,
  platformDirs,
  platformManifest,
} = require("./fixtures.js");

// `npm` on PATH is `npm.cmd` on Windows, and since the fix for CVE-2024-27980
// Node refuses to spawn a `.cmd` without a shell -- the call fails with EINVAL
// before npm is reached. Every argument passed below is a fixed literal, so a
// shell on that one platform interpolates nothing.
const THROUGH_SHELL = process.platform === "win32";

/**
 * @param {string[]} args
 * @param {string} [cwd]
 * @returns {ReturnType<typeof spawnSync>}
 */
function npm(args, cwd) {
  return spawnSync("npm", args, {
    cwd,
    encoding: "utf8",
    shell: THROUGH_SHELL,
    // Packing takes well under a second per package here, but npm on a cold CI
    // runner spends its first invocation setting up a cache, and a timeout that
    // fires there would read as a packaging failure rather than a slow machine.
    timeout: 120_000,
    maxBuffer: 8 * 1024 * 1024,
  });
}

// Nothing under `npm/` installs anything -- there is no lockfile and no
// `node_modules` -- so npm is not a dependency of this suite, and a machine
// without it (a bare `node` image, a box with the CLI stripped) is not a broken
// checkout. It is still the only thing that can answer the question, so the
// honest response is to skip rather than to weaken the assertions into something
// that passes without npm.
const probe = npm(["--version"]);
const SKIP =
  probe.error === undefined && probe.status === 0
    ? false
    : "the npm CLI is not available, and only npm can say what npm would pack";

const packLists = new Map();

/**
 * Asks npm which paths a publish from `dir` would put in the tarball.
 *
 * @param {string} dir a package directory
 * @returns {string[]} sorted tarball-relative paths, "/"-separated on any host
 */
function packedPaths(dir) {
  const cached = packLists.get(dir);
  if (cached !== undefined) {
    return cached;
  }

  const result = npm(["pack", "--dry-run", "--json"], dir);

  // Both checks carry npm's own stderr, because the alternative is parsing
  // whatever came back and reporting a JSON syntax error -- which names neither
  // the package nor the reason npm declined to pack it.
  assert.equal(
    result.error,
    undefined,
    `npm could not be started in ${dir}: ${result.error && result.error.message}`,
  );
  assert.equal(
    result.status,
    0,
    `npm pack --dry-run failed in ${dir} ` +
      `(status ${result.status}, signal ${result.signal}):\n${result.stderr}`,
  );

  const report = JSON.parse(result.stdout);
  assert.equal(
    report.length,
    1,
    `npm pack reported ${report.length} tarballs for ${dir}, expected one`,
  );

  const paths = report[0].files
    .map((file) => file.path.replace(/\\/g, "/"))
    .sort();
  packLists.set(dir, paths);
  return paths;
}

// ---------------------------------------------------------------------------
// What each package may contain
// ---------------------------------------------------------------------------
//
// An allow list, never an exact file set, because the correct set differs
// between a checkout and a release. `npm/platforms/*/` has no `bin/` and no
// licence files until `release.yml` builds the binaries and copies the two root
// licences in, so an exact set would have to be wrong in one of the two places
// -- and the one it would be wrong in is the release, where it fails after the
// binaries are built and for a reason that is not a defect.

const LAUNCHER_FILES = [
  "package.json",
  "README.md",
  "bin/tokfold",
  "lib/resolve.js",
  "LICENSE-MIT",
  "LICENSE-APACHE",
];

const PLATFORM_FILES = ["package.json", "LICENSE-MIT", "LICENSE-APACHE"];

/** Every package `release.yml` publishes, named as the registry names it. */
const PACKAGES = [
  {
    name: "tokfold",
    dir: PACKAGE_DIR,
    allows: (entry) => LAUNCHER_FILES.includes(entry),
  },
  ...platformDirs().map((dir) => ({
    name: platformManifest(dir).name,
    dir: path.join(PLATFORMS_DIR, dir),
    // A platform package is one binary and its paperwork. The binary's name
    // differs per target (`tokfold.exe` on Windows) and the release workflow is
    // what puts it there, so `bin/` is admitted wholesale rather than by name.
    allows: (entry) => PLATFORM_FILES.includes(entry) || entry.startsWith("bin/"),
  })),
];

for (const pkg of PACKAGES) {
  test(`${pkg.name} publishes nothing its manifest did not promise`, { skip: SKIP }, () => {
    const unexpected = packedPaths(pkg.dir).filter((entry) => !pkg.allows(entry));

    assert.deepEqual(
      unexpected,
      [],
      `${pkg.name} would publish ${unexpected.join(", ")}`,
    );
  });
}

test("the launcher tarball carries both halves of the launcher", { skip: SKIP }, () => {
  // The allow list above is satisfied by an empty tarball, and a package that
  // ships nothing is the worse bug: `npm i -g tokfold` succeeds, npm links a
  // `tokfold` command onto PATH, and the command is a missing file. A renamed
  // `files` entry, a stray `.npmignore` or a `bin/` that became a symlink all
  // produce exactly that, with every other check here still green.
  const packed = packedPaths(PACKAGE_DIR);

  for (const required of ["bin/tokfold", "lib/resolve.js"]) {
    assert.ok(
      packed.includes(required),
      `the tokfold tarball is missing ${required}; it has ${packed.join(", ")}`,
    );
  }
});

/**
 * Whether a tarball path belongs to this suite rather than to the product.
 *
 * Named files and not just a directory check: the point is that no test
 * artefact ships however it arrived -- a `tests/` directory copied inside
 * `tokfold/`, a `*.test.js` dropped beside `lib/resolve.js`, or the stand-in
 * binary staged into a `bin/` by a release run that resolved the wrong path.
 *
 * @param {string} entry a tarball path
 * @returns {boolean}
 */
function isTestArtefact(entry) {
  const segments = entry.split("/");
  const base = segments[segments.length - 1];

  return (
    segments.slice(0, -1).includes("tests") ||
    base.endsWith(".test.js") ||
    ["fixtures.js", "preload-platform.js", "fake-tokfold.sh"].includes(base)
  );
}

test("no package carries a file from this suite into a tarball", { skip: SKIP }, () => {
  // Stated on its own rather than left to the allow lists, because this is the
  // guarantee `npm/README.md` makes and the allow lists are not it. A platform
  // package admits anything under `bin/`, so the stand-in shell script would
  // pass the check above; and widening an allow list is precisely how this
  // invariant would otherwise be lost without anyone deciding to lose it.
  const shipped = [];

  for (const pkg of PACKAGES) {
    for (const entry of packedPaths(pkg.dir)) {
      if (isTestArtefact(entry)) {
        shipped.push(`${pkg.name}:${entry}`);
      }
    }
  }

  assert.deepEqual(
    shipped,
    [],
    `test files would be published: ${shipped.join(", ")}`,
  );
});
