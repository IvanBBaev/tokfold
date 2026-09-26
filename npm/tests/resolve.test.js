"use strict";

// Tests for `npm/tokfold/lib/resolve.js` -- the half of the launcher that turns
// "which machine is this" into "which file do I exec".
//
// Every failure this module can produce is a message a person reads at a
// terminal and then acts on, so the assertions here are about the messages as
// much as about the paths. A resolver that fails with the right exit code and
// the wrong explanation still sends someone to the wrong bug tracker, which is
// the exact outcome the module's own comments say it exists to prevent.
//
// Nothing here runs a binary. That is `launcher.test.js`.

const assert = require("node:assert/strict");
const path = require("node:path");
const test = require("node:test");

const {
  PACKAGE_DIR,
  createInstall,
  launcherManifest,
  platformDirs,
  platformManifest,
  resolveUnder,
} = require("./fixtures.js");

// The shipped table, read from the checkout. The behavioural tests below go
// through a scratch copy instead; this one is for asserting on the table itself.
const { PACKAGES } = require(path.join(PACKAGE_DIR, "lib", "resolve.js"));

const ALL_PACKAGES = Object.values(PACKAGES);

/** A fully installed machine: every platform package present. */
const complete = createInstall({ packages: ALL_PACKAGES });

/** A machine where npm installed the launcher and nothing else. */
const bare = createInstall();

// ---------------------------------------------------------------------------
// The mapping itself
// ---------------------------------------------------------------------------

for (const [key, pkg] of Object.entries(PACKAGES)) {
  const [platform, arch] = key.split("-");

  test(`${key} resolves to the binary inside ${pkg}`, () => {
    const { path: resolved, error } = resolveUnder(complete, {
      platform,
      arch,
      // Every Linux entry in the table is a glibc build; the musl branch has
      // its own tests further down.
      libc: "glibc",
    });

    assert.equal(error, undefined);
    const exe = platform === "win32" ? "tokfold.exe" : "tokfold";
    assert.equal(resolved, path.join(complete.packageDir(pkg), "bin", exe));
  });
}

test("only the Windows target asks for a .exe", () => {
  const suffixed = Object.keys(PACKAGES).filter((key) => {
    const [platform, arch] = key.split("-");
    const { path: resolved } = resolveUnder(complete, {
      platform,
      arch,
      libc: "glibc",
    });
    return resolved.endsWith(".exe");
  });

  assert.deepEqual(suffixed, ["win32-x64"]);
});

test("resolution is path arithmetic and does not require the binary to exist", () => {
  // The fixture installs manifests but no executables, and resolution still
  // succeeds. This is deliberate in the launcher: a package that is installed
  // but empty is a different failure from a package that is missing, and it is
  // reported by `spawnSync` later with the path in hand. Collapsing the two
  // here would cost the message that names the path.
  const { path: resolved, error } = resolveUnder(complete, {
    platform: "linux",
    arch: "x64",
    libc: "glibc",
  });

  assert.equal(error, undefined);
  assert.ok(path.isAbsolute(resolved));
});

// ---------------------------------------------------------------------------
// Drift between the table, the directories, and the manifests
// ---------------------------------------------------------------------------
//
// `release.yml` runs equivalent checks, and deliberately so: that workflow only
// runs on a release, while these run on every pull request and every push to
// `main`, so a drift introduced on a branch under review surfaces there rather
// than at the moment someone tries to ship. (A push to a branch with no pull
// request open runs nothing at all -- these tests are a review gate, not a
// commit gate.) They are also the only place the reasoning is written down.
//
// What is being guarded is npm's install filter. A manifest that claims the
// wrong CPU makes npm silently decline to install the package, and the symptom
// reaching the user is "not installed" on a platform that is fully supported --
// a failure with no error message anywhere pointing at the manifest.

test("the table and npm/platforms describe the same set of targets", () => {
  const fromTable = ALL_PACKAGES.slice().sort();
  // Each manifest's own `name`, never `tokfold-${dir}`. A directory publishes
  // under the name its manifest declares, and the two agree today only by
  // coincidence: `npm/platforms/windows-x64` publishes as `tokfold-windows-x64`
  // because npm refused `tokfold-win32-x64`, not because the directory names it.
  // Reconstructing the name would make this test compare the table against a
  // string no registry has ever seen.
  const fromDisk = platformDirs()
    .map((dir) => platformManifest(dir).name)
    .sort();

  assert.deepEqual(fromTable, fromDisk);
});

test("every platform package declares the os and cpu its table key promises", () => {
  for (const [key, pkg] of Object.entries(PACKAGES)) {
    const [platform, arch] = key.split("-");
    const dir = platformDirs().find((d) => platformManifest(d).name === pkg);
    assert.ok(dir !== undefined, `${pkg} has no directory under npm/platforms`);

    const manifest = platformManifest(dir);
    assert.deepEqual(manifest.os, [platform], `${pkg} declares the wrong os`);
    assert.deepEqual(manifest.cpu, [arch], `${pkg} declares the wrong cpu`);
  }
});

test("no platform package declares an exports field", () => {
  // `resolveBinaryPath` resolves `<pkg>/package.json` and then does path
  // arithmetic, on the stated grounds that a manifest field cannot intercept
  // the join. That is only half true: an `exports` map that does not list
  // "./package.json" blocks the manifest lookup too, and the launcher would
  // start reporting every platform package as "not installed". Nothing else in
  // the repository would notice, so it is pinned here.
  for (const dir of platformDirs()) {
    const manifest = platformManifest(dir);
    assert.equal(
      manifest.exports,
      undefined,
      `${manifest.name} declares exports, which would hide its package.json ` +
        "from require.resolve and break resolveBinaryPath",
    );
  }
});

test("optionalDependencies pins exactly the packages the table can resolve", () => {
  const pinned = Object.keys(launcherManifest().optionalDependencies).sort();

  assert.deepEqual(pinned, ALL_PACKAGES.slice().sort());
});

test("every platform package is at the version the launcher pins for it", () => {
  // The launcher and the binary are two halves of one release, and the release
  // workflow enforces that by comparing every version site against the
  // dispatched `version` input. That check only ever runs on a publish. This one
  // catches the same drift while it is still a diff: bump `npm/tokfold` and
  // forget one `npm/platforms/*/package.json`, and the launcher ships pinning a
  // version of that platform package which does not exist on the registry --
  // npm cannot install it, and every user on that one platform gets "the package
  // holding this platform's binary is not installed" on a supported machine.
  const manifest = launcherManifest();

  for (const dir of platformDirs()) {
    const platform = platformManifest(dir);
    assert.equal(
      manifest.optionalDependencies[platform.name],
      platform.version,
      `${platform.name} is at ${platform.version} but the launcher pins ` +
        `${manifest.optionalDependencies[platform.name]}`,
    );
    assert.equal(
      platform.version,
      manifest.version,
      `${platform.name} and the launcher must ship as one release`,
    );
  }
});

test("the table is frozen", () => {
  assert.ok(Object.isFrozen(PACKAGES));
});

// ---------------------------------------------------------------------------
// Platforms with no build
// ---------------------------------------------------------------------------

test("an unsupported operating system is refused by name", () => {
  const { path: resolved, error } = resolveUnder(complete, {
    platform: "freebsd",
    arch: "x64",
    libc: "glibc",
  });

  assert.equal(resolved, undefined);
  assert.ok(error instanceof Error);
  assert.ok(
    error.message.startsWith("tokfold: no prebuilt binary for freebsd-x64."),
    error.message,
  );
});

test("an unsupported architecture is refused by name", () => {
  const { error } = resolveUnder(complete, {
    platform: "linux",
    arch: "riscv64",
    libc: "glibc",
  });

  assert.ok(
    error.message.startsWith("tokfold: no prebuilt binary for linux-riscv64."),
    error.message,
  );
});

test("the refusal lists every supported target and offers a source build", () => {
  const { error } = resolveUnder(complete, {
    platform: "freebsd",
    arch: "x64",
    libc: "glibc",
  });

  // The list is what makes the message actionable rather than merely correct:
  // it tells someone on an unsupported machine whether they are close to a
  // supported one or nowhere near it.
  assert.ok(
    error.message.includes(
      `Supported: ${Object.keys(PACKAGES).sort().join(", ")}.`,
    ),
    error.message,
  );
  assert.ok(
    error.message.includes(
      "cargo install --git https://github.com/IvanBBaev/tokfold tokfold-cli",
    ),
    error.message,
  );
});

// ---------------------------------------------------------------------------
// musl
// ---------------------------------------------------------------------------
//
// `process.platform` is "linux" on Alpine as well as on Debian, so without a
// libc check an Alpine user resolves a glibc binary and gets a dynamic-loader
// error naming neither musl nor tokfold. Both branches are exercised by
// injecting the condition the launcher reads -- its own diagnostic report --
// rather than by asking the launcher to accept an override.

test("a musl runtime is refused with an Alpine-specific message", () => {
  const { path: resolved, error } = resolveUnder(complete, {
    platform: "linux",
    arch: "x64",
    libc: "musl",
  });

  assert.equal(resolved, undefined);
  assert.ok(error.message.includes("musl libc"), error.message);
  assert.ok(error.message.includes("Alpine"), error.message);
  // Naming the fix is the point of the message; "-alpine" images are how most
  // people arrive here without knowing they chose a libc at all.
  assert.ok(error.message.includes("node:22-slim"), error.message);
  assert.ok(
    error.message.includes(
      "cargo install --git https://github.com/IvanBBaev/tokfold tokfold-cli",
    ),
    error.message,
  );
});

test("a glibc runtime resolves the -gnu package", () => {
  const { path: resolved, error } = resolveUnder(complete, {
    platform: "linux",
    arch: "x64",
    libc: "glibc",
  });

  assert.equal(error, undefined);
  assert.ok(resolved.includes("tokfold-linux-x64-gnu"), resolved);
});

test("an unsupported architecture on musl is told about both", () => {
  // Two independent reasons the install cannot work, and naming either one alone
  // costs a round trip: the libc message sends someone to swap their base image,
  // where the architecture is still unsupported, and the architecture message alone
  // lets them go add a glibc target they could not have run. Building from source is
  // the one remedy that answers both, so it is the one the message ends on.
  const { error } = resolveUnder(complete, {
    platform: "linux",
    arch: "riscv64",
    libc: "musl",
  });

  assert.ok(error.message.includes("no prebuilt binary for linux-riscv64"), error.message);
  assert.ok(error.message.includes("musl libc"), error.message);
  assert.ok(
    error.message.includes(
      "cargo install --git https://github.com/IvanBBaev/tokfold tokfold-cli",
    ),
    error.message,
  );
});

test("an unsupported architecture on glibc is not told about musl", () => {
  // The libc sentence is conditional, not boilerplate: on a glibc host it would be
  // a false explanation for a failure that has nothing to do with libc.
  const { error } = resolveUnder(complete, {
    platform: "linux",
    arch: "riscv64",
    libc: "glibc",
  });

  assert.ok(error.message.includes("no prebuilt binary for linux-riscv64"), error.message);
  assert.ok(!error.message.includes("musl"), error.message);
});

test("a diagnostic report with no header is treated as musl", () => {
  const { error } = resolveUnder(complete, {
    platform: "linux",
    arch: "x64",
    libc: "no-header",
  });

  assert.ok(error.message.includes("musl libc"), error.message);
});

test("a runtime with no diagnostic report assumes glibc rather than failing", () => {
  // Stated policy in `isMusl`: a failure to detect must not become a failure to
  // run. Assuming glibc is what the launcher did before the check existed, so
  // an undetectable runtime is no worse off than it used to be.
  const { path: resolved, error } = resolveUnder(complete, {
    platform: "linux",
    arch: "arm64",
    libc: "absent",
  });

  assert.equal(error, undefined);
  assert.ok(resolved.includes("tokfold-linux-arm64-gnu"), resolved);
});

test("a diagnostic report that throws assumes glibc rather than failing", () => {
  const { path: resolved, error } = resolveUnder(complete, {
    platform: "linux",
    arch: "x64",
    libc: "throws",
  });

  assert.equal(error, undefined);
  assert.ok(resolved.includes("tokfold-linux-x64-gnu"), resolved);
});

test("the musl check never fires off Linux", () => {
  // macOS and Windows report no `glibcVersionRuntime` either, because they have
  // no glibc. Only the early `process.platform !== "linux"` return keeps a Mac
  // from being told it is running Alpine -- and keeps this suite honest when it
  // runs on one.
  for (const [platform, arch] of [
    ["darwin", "arm64"],
    ["darwin", "x64"],
    ["win32", "x64"],
  ]) {
    const { path: resolved, error } = resolveUnder(complete, {
      platform,
      arch,
      libc: "musl",
    });

    assert.equal(error, undefined, `${platform}-${arch} was refused as musl`);
    assert.ok(resolved.includes(PACKAGES[`${platform}-${arch}`]), resolved);
  }
});

// ---------------------------------------------------------------------------
// The platform package is not installed
// ---------------------------------------------------------------------------
//
// Not a hypothetical. `npm ci --omit=optional` produces exactly this, and it is
// a common default in CI images; so does any install that ran while one of the
// five platform packages was missing from the registry, which is the state this
// project spent a release in.

for (const [key, pkg] of Object.entries(PACKAGES)) {
  const [platform, arch] = key.split("-");

  test(`a missing ${pkg} is reported by name`, () => {
    const { path: resolved, error } = resolveUnder(bare, {
      platform,
      arch,
      libc: "glibc",
    });

    assert.equal(resolved, undefined);
    assert.ok(
      error.message.startsWith(
        `tokfold: the package holding this platform's binary (${pkg}) is not installed.`,
      ),
      error.message,
    );
  });
}

test("the missing-package error points at the launcher, pinned, not the package", () => {
  // `npm install tokfold-linux-x64-gnu` is wrong twice over. It fetches whatever
  // is newest, and the launcher and the binary are two halves of one release; and
  // it drops the platform package into the current directory, which is not where
  // a launcher installed with `-g` looks for it. Following that advice therefore
  // reports success and changes nothing, which is worse than no advice at all.
  // Reinstalling the launcher is what actually re-runs npm's install filter.
  const manifest = launcherManifest();

  for (const [key, pkg] of Object.entries(PACKAGES)) {
    const [platform, arch] = key.split("-");
    const { error } = resolveUnder(bare, { platform, arch, libc: "glibc" });

    assert.ok(
      error.message.includes(`npm install -g tokfold@${manifest.version}`),
      error.message,
    );
    assert.ok(
      error.message.includes(`Installing ${pkg} by itself is not the fix`),
      error.message,
    );
    assert.equal(
      manifest.optionalDependencies[pkg],
      manifest.version,
      "the advice pins the launcher's version, which is only the right advice " +
        "while the launcher pins its platform packages to that same version",
    );
  }
});

test("the missing-package error explains how people get here", () => {
  const { error } = resolveUnder(bare, {
    platform: "linux",
    arch: "x64",
    libc: "glibc",
  });

  assert.ok(error.message.includes("--omit=optional"), error.message);
  assert.ok(error.message.includes("lockfile"), error.message);
});

test("the missing-package error keeps the resolution failure as its cause", () => {
  // The rewritten message is for a person; the cause is for whoever ends up
  // debugging why `require.resolve` failed on a machine where the directory
  // appears to be present.
  const { error } = resolveUnder(bare, {
    platform: "darwin",
    arch: "arm64",
    libc: "glibc",
  });

  assert.ok(error.cause instanceof Error, "no cause attached");
  assert.equal(error.cause.code, "MODULE_NOT_FOUND");
});

// ---------------------------------------------------------------------------
// The suite must not ship
// ---------------------------------------------------------------------------

test("the test suite lives outside the published package", () => {
  // `npm/tokfold/package.json` carries a `files` allow list, so a directory
  // inside the package would already be excluded -- but only until someone adds
  // a `test/` entry or replaces the list. Living outside the package directory
  // entirely is the property that cannot be undone by editing a manifest.
  const relative = path.relative(PACKAGE_DIR, __dirname);

  assert.ok(
    relative.startsWith(".."),
    `tests are inside the published package at ${relative}`,
  );
});
