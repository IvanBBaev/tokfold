"use strict";

// Tests for `npm/tokfold/bin/tokfold` -- the process the `tokfold` command
// actually is once npm has linked it onto PATH.
//
// These run the launcher as a real child process, because everything it
// promises is a property of a process rather than of a function: the exit code
// a shell sees, the file descriptors the binary is handed, the signal a
// `Ctrl-C` turns into. None of that is observable from inside the module.
//
// The binary at the far end is `fake-tokfold.sh`, not a compiled tokfold. The
// launcher's contract says nothing about what the child does, only that it is
// reproduced faithfully, so a child that can be told to exit 3 on demand tests
// the contract better than one that has to be cross-compiled first -- and it
// keeps this suite runnable with no Rust toolchain at all. `release.yml` already
// runs the real binary through the launcher before publishing.

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const { spawnSync } = require("node:child_process");

const { PACKAGE_DIR, PRELOAD, createInstall } = require("./fixtures.js");

const { PACKAGES } = require(path.join(PACKAGE_DIR, "lib", "resolve.js"));

const HOST_PACKAGE = PACKAGES[`${process.platform}-${process.arch}`];

// The stand-in binary is a `#!/bin/sh` script, which Windows cannot exec, and a
// host with no entry in the table has no package to install the stand-in into.
// Both are honest reasons to skip rather than to weaken an assertion; the
// resolver suite covers every platform on every host.
const SKIP =
  process.platform === "win32"
    ? "the stand-in binary is a shell script, which Windows cannot exec"
    : HOST_PACKAGE === undefined
      ? `no platform package for ${process.platform}-${process.arch}`
      : false;

/** npm's layout on a machine where everything installed correctly. */
const installed = SKIP
  ? null
  : createInstall({ packages: [HOST_PACKAGE], withBinary: [HOST_PACKAGE] });

/** The package is present but carries no executable. */
const empty = SKIP ? null : createInstall({ packages: [HOST_PACKAGE] });

/** Only the launcher installed -- optional dependencies were skipped. */
const bare = createInstall();

/**
 * Runs a launcher through Node, the way npm's generated shim does.
 *
 * @param {{launcher: string}} install
 * @param {string[]} args
 * @param {{env?: object, input?: string, forgePlatform?: boolean}} [options]
 */
function run(install, args, options = {}) {
  const nodeArgs = options.forgePlatform ? ["--require", PRELOAD] : [];

  return spawnSync(process.execPath, [...nodeArgs, install.launcher, ...args], {
    encoding: "utf8",
    // The launcher hands the child its own descriptors, so everything the
    // child writes lands in this pipe. Large enough that a truncation is the
    // launcher's doing and not this harness's.
    maxBuffer: 16 * 1024 * 1024,
    env: { ...process.env, ...(options.env ?? {}) },
    input: options.input,
  });
}

// ---------------------------------------------------------------------------
// Exit codes
// ---------------------------------------------------------------------------
//
// The documented contract: 0 success, 2 bad input, 3 corrupt archive, and 1
// reserved for the launcher itself failing so tokfold never ran. Scripts branch
// on those, so the launcher must not invent, remap or swallow one.

for (const code of [0, 2, 3, 42, 255]) {
  test(`a child exit code of ${code} passes through unchanged`, { skip: SKIP }, () => {
    const result = run(installed, ["compress"], {
      env: { TOKFOLD_FAKE_EXIT: String(code) },
    });

    assert.equal(result.status, code);
    assert.equal(result.signal, null);
  });
}

test("a child exit code of 1 is passed through, not replaced", { skip: SKIP }, () => {
  // 1 is the launcher's own failure code, so this is the one value where
  // "pass it through" and "report my own failure" collide. The launcher must
  // still not intercept it: a child that exits 1 has run, and rewriting that
  // into anything else would be inventing a code.
  const result = run(installed, ["compress"], {
    env: { TOKFOLD_FAKE_EXIT: "1" },
  });

  assert.equal(result.status, 1);
  // And the launcher stayed silent -- the 1 came from the child, so there is no
  // launcher-level explanation to print.
  assert.equal(result.stderr, "");
});

// ---------------------------------------------------------------------------
// Arguments and descriptors
// ---------------------------------------------------------------------------

test("arguments reach the binary verbatim, with no shell in between", { skip: SKIP }, () => {
  const args = [
    "compress",
    "--input",
    "a file with spaces.json",
    "; echo pwned",
    "$(id)",
    "`id`",
    "--flag=quote'and\"quote",
    "--",
    "-",
  ];

  const result = run(installed, args);

  assert.equal(result.status, 0);
  assert.deepEqual(
    result.stdout.split("\n").filter(Boolean),
    args.map((arg) => `arg:[${arg}]`),
  );
});

test("no arguments at all is a valid invocation", { skip: SKIP }, () => {
  const result = run(installed, []);

  assert.equal(result.status, 0);
  assert.equal(result.stdout, "");
});

test("stdin reaches the binary", { skip: SKIP }, () => {
  // `stdio: "inherit"` is load-bearing for the streaming subcommands and for
  // `mcp`, which is a line-framed protocol over stdin/stdout. If the launcher
  // ever grew a relay in the middle this is the first thing that would break.
  const payload = '{"k":[{"a":1},{"a":2}]}\n';
  const result = run(installed, ["expand"], {
    env: { TOKFOLD_FAKE_MODE: "stdin" },
    input: payload,
  });

  assert.equal(result.status, 0);
  assert.equal(result.stdout, payload);
});

test("a large stdout is not truncated or buffered away", { skip: SKIP }, () => {
  const bulk = path.join(installed.root, "bulk.txt");
  const payload = `${"tokfold".repeat(8)}\n`.repeat(4096);
  fs.writeFileSync(bulk, payload);

  const result = run(installed, ["expand"], {
    env: { TOKFOLD_FAKE_MODE: "file", TOKFOLD_FAKE_FILE: bulk },
  });

  assert.equal(result.status, 0);
  assert.equal(result.stdout.length, payload.length);
});

test("the binary's stderr reaches the caller's stderr", { skip: SKIP }, () => {
  // No arguments, so the stand-in writes nothing to stdout: this also pins that
  // the two streams stay separated across the launcher rather than being merged
  // into one, which would corrupt every `tokfold compress > out` redirection.
  const result = run(installed, [], {
    env: {
      TOKFOLD_FAKE_STDERR: "tokfold: input is not JSON",
      TOKFOLD_FAKE_EXIT: "2",
    },
  });

  assert.equal(result.status, 2);
  assert.equal(result.stderr, "tokfold: input is not JSON\n");
  assert.equal(result.stdout, "");
});

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

for (const signal of ["TERM", "INT"]) {
  test(`a child killed by SIG${signal} kills the launcher the same way`, { skip: SKIP }, () => {
    // A shell reports 130 for an interrupted command because the process *died
    // by* SIGINT, not because it exited with 130. Turning the signal into a
    // number here would erase that distinction, so the launcher re-raises it on
    // itself and dies with the child's own cause of death.
    const result = run(installed, ["compress"], {
      env: { TOKFOLD_FAKE_SIGNAL: signal },
    });

    assert.equal(result.signal, `SIG${signal}`);
    assert.equal(result.status, null);
  });
}

test("a signal the runtime ignores falls back to exit 1", { skip: SKIP }, () => {
  // Node ignores SIGPIPE process-wide, so re-raising it cannot kill the
  // launcher and control reaches the `process.exit` after `process.kill`.
  //
  // This pins a documented corner rather than asserting it is ideal: the child
  // did run, yet the caller sees 1, which the launcher's own header reserves for
  // "tokfold never ran". It is unreachable with the real binary -- Rust ignores
  // SIGPIPE too, and tokfold-cli treats a closed pipe as a clean exit -- and the
  // alternative, inventing a number for the signal, is the thing the exit-code
  // contract exists to prevent. If that ever changes, this test is where the
  // decision is recorded.
  const result = run(installed, ["compress"], {
    env: { TOKFOLD_FAKE_SIGNAL: "PIPE" },
  });

  assert.equal(result.status, 1);
  assert.equal(result.signal, null);
});

// ---------------------------------------------------------------------------
// The launcher's own failures. All of them, and only these, exit 1.
// ---------------------------------------------------------------------------

test("an unsupported platform exits 1 and explains itself on stderr", { skip: SKIP }, () => {
  const result = run(installed, ["--version"], {
    forgePlatform: true,
    env: {
      TOKFOLD_TEST_PLATFORM: "freebsd",
      TOKFOLD_TEST_ARCH: "x64",
      TOKFOLD_TEST_LIBC: "glibc",
    },
  });

  assert.equal(result.status, 1);
  assert.match(result.stderr, /^tokfold: no prebuilt binary for freebsd-x64\./);
  // Nothing on stdout: a script capturing stdout must not silently collect an
  // error message where it expected a rendering.
  assert.equal(result.stdout, "");
});

test("an unsupported architecture exits 1", { skip: SKIP }, () => {
  const result = run(installed, ["--version"], {
    forgePlatform: true,
    env: {
      TOKFOLD_TEST_PLATFORM: "linux",
      TOKFOLD_TEST_ARCH: "riscv64",
      TOKFOLD_TEST_LIBC: "glibc",
    },
  });

  assert.equal(result.status, 1);
  assert.match(result.stderr, /^tokfold: no prebuilt binary for linux-riscv64\./);
  assert.equal(result.stdout, "");
});

test("a musl runtime exits 1 rather than loading a glibc binary", { skip: SKIP }, () => {
  const result = run(installed, ["--version"], {
    forgePlatform: true,
    env: {
      TOKFOLD_TEST_PLATFORM: "linux",
      TOKFOLD_TEST_ARCH: "x64",
      TOKFOLD_TEST_LIBC: "musl",
    },
  });

  assert.equal(result.status, 1);
  assert.match(result.stderr, /musl libc/);
  assert.match(result.stderr, /Alpine/);
  assert.equal(result.stdout, "");
});

test("a missing platform package exits 1 and names the package", { skip: SKIP }, () => {
  // The live case: `tokfold-win32-x64` is not on the registry, so every Windows
  // user reaches this path. It is also what `npm ci --omit=optional` produces
  // on any platform.
  const result = run(bare, ["--version"]);

  assert.equal(result.status, 1);
  assert.match(result.stderr, new RegExp(`\\(${HOST_PACKAGE}\\) is not installed`));
  assert.match(result.stderr, /npm install /);
  assert.equal(result.stdout, "");
});

test("an installed package with no binary exits 1 and names the path", { skip: SKIP }, () => {
  // Distinct from the case above, and the distinction is the whole point of
  // resolving before spawning: here the package is present, so the advice is not
  // "install it" but "this file is missing", with the path to check.
  const result = run(empty, ["--version"]);

  assert.equal(result.status, 1);
  assert.match(result.stderr, /^tokfold: failed to run the binary at /);
  assert.ok(result.stderr.includes(empty.packageDir(HOST_PACKAGE)), result.stderr);
  assert.equal(result.stdout, "");
});

test("every launcher-level failure uses exit code 1 and only 1", { skip: SKIP }, () => {
  // Stated as one assertion because it is one guarantee: a 1 from this command
  // means tokfold never ran, so no launcher failure may borrow 2 or 3, and no
  // launcher failure may exit 0 either.
  const failures = [
    run(installed, [], {
      forgePlatform: true,
      env: {
        TOKFOLD_TEST_PLATFORM: "freebsd",
        TOKFOLD_TEST_ARCH: "x64",
        TOKFOLD_TEST_LIBC: "glibc",
      },
    }),
    run(installed, [], {
      forgePlatform: true,
      env: {
        TOKFOLD_TEST_PLATFORM: "linux",
        TOKFOLD_TEST_ARCH: "x64",
        TOKFOLD_TEST_LIBC: "musl",
      },
    }),
    run(bare, []),
    run(empty, []),
  ];

  assert.deepEqual(
    failures.map((result) => result.status),
    [1, 1, 1, 1],
  );
  for (const result of failures) {
    assert.equal(result.signal, null);
    assert.notEqual(result.stderr, "");
  }
});
