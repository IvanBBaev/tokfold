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
//   TOKFOLD_TEST_LIBC      musl | absent | anything else, which means glibc
//   TOKFOLD_TEST_NO_SIGNAL_TABLE  empty out os.constants.signals (see below)
//   TOKFOLD_TEST_KILL_EPERM       make ChildProcess#kill fail with EPERM (see below)
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

if (process.env.TOKFOLD_TEST_NO_SIGNAL_TABLE !== undefined) {
  // Models a runtime whose signal table does not name the signal the child died
  // of. That is not hypothetical -- Windows carries a far smaller table than
  // POSIX -- but it cannot be produced on a POSIX host by choosing an exotic
  // signal: Node names the child's signal before `child.on("exit")` sees it,
  // and a signal it cannot name is reported as `exit(0, null)`, so it never
  // reaches the launcher's lookup at all. Emptying the table is the only way to reach the
  // launcher's unmapped branch from a test, and it is a redefinition of an
  // ordinary writable property of the `os` module rather than a patch to
  // anything the launcher owns.
  //
  // `process.kill` is unaffected: it resolves signal names through V8's own
  // internal constants, not through `os.constants`, so the child still dies of
  // a real signal and only the launcher's lookup of its number misses.
  const os = require("node:os");

  Object.defineProperty(os.constants, "signals", {
    value: Object.freeze({}),
    configurable: true,
    enumerable: true,
    writable: false,
  });
}

if (process.env.TOKFOLD_TEST_KILL_EPERM !== undefined) {
  // Models a relayed signal the kernel refuses -- a binary that is running but
  // may not be signalled by this process. Node does not throw that EPERM from
  // `child.kill`; it emits it as an `error` event on the child, synchronously,
  // which is the reply this reproduces, and the only reply the launcher has to
  // survive. Nothing is sent to the child, so it keeps running exactly as it
  // would after a real refusal. Each refusal is announced on stderr, so a test
  // can tell a relay that was refused from one that was never attempted.
  const { ChildProcess } = require("node:child_process");
  const fs = require("node:fs");
  const os = require("node:os");

  ChildProcess.prototype.kill = function kill(signal) {
    fs.writeSync(2, `preload: refused kill ${signal}\n`);
    const error = new Error("kill EPERM");
    error.code = "EPERM";
    error.errno = -os.constants.errno.EPERM;
    error.syscall = "kill";
    this.emit("error", error);
    return false;
  };
}
