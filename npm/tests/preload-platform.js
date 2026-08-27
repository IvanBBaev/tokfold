"use strict";

// Injects a pretend machine into a Node process, for the launcher tests.
//
// `bin/tokfold` is a program, not a module: it decides everything at load time
// and then calls `process.exit`. There is no seam to stub, and adding one would
// mean shipping a hook that exists only for the tests -- a weaker launcher for a
// stronger-looking suite. So the pretend machine is installed from outside the
// program instead, with `node --require`, which runs before the launcher's first
// line and leaves the launcher itself untouched.
//
// What it overrides is exactly what `lib/resolve.js` reads: `process.platform`,
// `process.arch`, and `process.report`, which is where the musl check looks.
// All three are configurable own properties of `process`, so redefining them is
// ordinary JavaScript rather than a trick.
//
//   TOKFOLD_TEST_PLATFORM  value for process.platform
//   TOKFOLD_TEST_ARCH      value for process.arch
//   TOKFOLD_TEST_LIBC      glibc | musl | absent   (see below)
//
// With none of these set this file does nothing at all, so a test that wants
// the real machine simply does not pass `--require`.
//
// TOKFOLD_TEST_LIBC has to be explicit whenever the platform is forced to
// `linux`, and the reason is a trap worth stating: `process.report` on macOS and
// Windows reports no `glibcVersionRuntime` either, because there is no glibc
// there. Forcing `process.platform` to `linux` on a macOS host and leaving the
// real report in place therefore produces a machine that looks like Alpine, and
// every "linux" test would silently be testing the musl branch.

const platform = process.env.TOKFOLD_TEST_PLATFORM;
const arch = process.env.TOKFOLD_TEST_ARCH;
const libc = process.env.TOKFOLD_TEST_LIBC;

if (platform !== undefined) {
  Object.defineProperty(process, "platform", {
    value: platform,
    configurable: true,
    enumerable: true,
    writable: false,
  });
}

if (arch !== undefined) {
  Object.defineProperty(process, "arch", {
    value: arch,
    configurable: true,
    enumerable: true,
    writable: false,
  });
}

if (libc !== undefined) {
  // `absent` models an embedded runtime with no diagnostic report at all, where
  // `process.report.getReport()` throws a TypeError rather than returning
  // anything. The launcher treats that as "assume glibc"; see `isMusl()`.
  const report =
    libc === "absent"
      ? undefined
      : {
          getReport: () => ({
            header:
              libc === "musl"
                ? {}
                : { glibcVersionRuntime: "2.36" },
          }),
        };

  Object.defineProperty(process, "report", {
    value: report,
    configurable: true,
    enumerable: true,
    writable: false,
  });
}
