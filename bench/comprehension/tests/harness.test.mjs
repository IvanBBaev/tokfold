// End-to-end tests of harness.mjs against fake `claude` and `tokfold`
// executables: no model is called and no binary needs to be built. Each test
// counts the fake CLI's invocations from its log, so the call cap is checked on
// what was spawned, not on what the harness reports.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const HARNESS = path.join(HERE, "..", "harness.mjs");
const FAKE_CLAUDE = path.join(HERE, "fake-claude.mjs");
const FAKE_TOKFOLD = path.join(HERE, "fake-tokfold.mjs");

function harness(args, { mode = "answer" } = {}) {
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "tokfold-comprehension-test-"));
  const log = path.join(out, "calls.log");
  const r = spawnSync(process.execPath, [HARNESS, "--claude", FAKE_CLAUDE, "--tokfold", FAKE_TOKFOLD, "--out", out, ...args], {
    encoding: "utf8",
    timeout: 60000,
    env: { ...process.env, FAKE_CLAUDE_MODE: mode, FAKE_CLAUDE_KEYS: path.join(out, "questions.json"), FAKE_CLAUDE_LOG: log },
  });
  const calls = fs.existsSync(log) ? fs.readFileSync(log, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l)) : [];
  const runFile = path.join(out, "run.json");
  const run = fs.existsSync(runFile) ? JSON.parse(fs.readFileSync(runFile, "utf8")) : null;
  return { ...r, out, calls, run, cleanup: () => fs.rmSync(out, { recursive: true, force: true }) };
}

test("a dry run renders and writes every prompt but calls no model", (t) => {
  const r = harness(["--count", "3", "--dry-run"]);
  t.after(r.cleanup);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.calls.length, 0);
  assert.equal(r.run.totals.calls, 0);
  assert.equal(r.run.totals.planned, 12);
  assert.equal(fs.readdirSync(path.join(r.out, "prompts")).length, 6);
  const prompt = fs.readFileSync(path.join(r.out, "prompts", "D01.tokfold.txt"), "utf8");
  assert.match(prompt, /<document>\n#tkfd1 tbl\n/);
  assert.equal(r.run.config.documents.perDocument[0].encoder, "tbl");
});

test("a plan above the hard cap is refused before any call", (t) => {
  // 51 documents x 2 models x 2 renderings = 204 calls.
  const r = harness(["--count", "51"]);
  t.after(r.cleanup);
  assert.equal(r.status, 2);
  assert.match(r.stderr, /the plan needs 204 model calls, more than the cap of 200/);
  assert.equal(r.calls.length, 0);
  assert.equal(r.run, null);
});

test("--max-calls cannot raise the hard cap", (t) => {
  const r = harness(["--count", "1", "--max-calls", "201"]);
  t.after(r.cleanup);
  assert.equal(r.status, 2);
  assert.match(r.stderr, /--max-calls 201 exceeds the hard cap of 200/);
  assert.equal(r.calls.length, 0);
});

test("a plan above --max-calls is refused before any call", (t) => {
  const r = harness(["--count", "2", "--max-calls", "7"]);
  t.after(r.cleanup);
  assert.equal(r.status, 2);
  assert.match(r.stderr, /needs 8 model calls, more than the cap of 7/);
  assert.equal(r.calls.length, 0);
});

test("a full run grades, sums cost and records the returned model ids", (t) => {
  const r = harness(["--count", "2", "--models", "m1,m2"]);
  t.after(r.cleanup);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.calls.length, 8);
  for (const argv of r.calls) {
    assert.deepEqual(argv.slice(0, 1), ["-p"]);
    assert.ok(argv.includes("--safe-mode"));
    assert.equal(argv[argv.indexOf("--tools") + 1], "");
  }
  assert.equal(r.run.totals.calls, 8);
  assert.equal(r.run.totals.stopped, null);
  assert.ok(Math.abs(r.run.totals.costUsd - 0.008) < 1e-9);
  assert.equal(r.run.config.claude.version, "0.0.0 (fake claude)");
  for (const c of r.run.summary.cells) {
    assert.equal(c.correct, c.total);
    assert.ok(c.total > 0);
    assert.equal(c.calls, 2);
    assert.equal(c.inputTokens, 32);
    assert.deepEqual(c.modelIds, [`fake-${c.model}`]);
  }
  assert.deepEqual(r.run.summary.pairs.map((p) => [p.onlyFirst, p.onlySecond]), [[0, 0], [0, 0]]);
  assert.match(fs.readFileSync(path.join(r.out, "summary.md"), "utf8"), /\| m1 \| fake-m1 \| json \|/);
});

test("retries of failing calls never exceed --max-calls", (t) => {
  // 4 planned calls, a cap of 5 and 3 retries each: the first attempts take
  // 4, one retry takes the fifth, and nothing more is spawned.
  for (const concurrency of ["1", "4"]) {
    const r = harness(["--count", "2", "--models", "m1", "--max-calls", "5", "--retries", "3", "--concurrency", concurrency], { mode: "fail" });
    t.after(r.cleanup);
    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.calls.length, 5, `concurrency ${concurrency}`);
    assert.equal(r.run.totals.calls, 5);
    assert.match(r.run.totals.stopped, /call cap of 5 reached/);
    assert.ok(r.run.records.every((x) => x.attempts >= 1 && !x.ok));
    assert.equal(r.run.summary.cells.reduce((s, c) => s + c.failedCalls, 0), 4);
  }
});

test("a reply without a JSON object counts as unparsed and wrong", (t) => {
  const r = harness(["--count", "1", "--models", "m1", "--renderings", "json"], { mode: "garbage" });
  t.after(r.cleanup);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.calls.length, 1);
  const [cell] = r.run.summary.cells;
  assert.equal(cell.unparsed, 1);
  assert.equal(cell.correct, 0);
  assert.ok(cell.total > 0);
});

test("--max-cost-usd stops issuing calls once the reported cost reaches it", (t) => {
  const r = harness(["--count", "3", "--models", "m1", "--concurrency", "1", "--max-cost-usd", "0.002"]);
  t.after(r.cleanup);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(r.calls.length, 2);
  assert.match(r.run.totals.stopped, /cost limit/);
});
