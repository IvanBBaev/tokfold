#!/usr/bin/env node
// Comprehension harness: does a model answer questions about a tool result as
// well from `tokfold compress` output as from the original JSON?
//
// Every call goes through the Claude Code CLI (`claude -p`), so it counts
// against the caller's Claude subscription or API account. A run refuses to
// start if its plan needs more than `--max-calls` calls, and `--max-calls`
// itself may not exceed HARD_CAP (200). Retries count as calls. `--dry-run`
// builds and renders everything, writes the prompts, and makes no model call.
//
// Usage: see bench/comprehension/README.md, or run with --help.

import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { HARD_CAP, SYSTEM_PROMPT, buildPrompt, capError, isCorrect, makeQuestions, parseAnswers, planCalls, renderSummary, summarize } from "./lib/core.mjs";
import { generateDocuments } from "./lib/fixtures.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, "..", "..");
const RENDERINGS = ["json", "tokfold"];

const HELP = `Usage: node bench/comprehension/harness.mjs [options]

Asks each model questions about each document, once per rendering (the
original JSON and \`tokfold compress\` output), and grades the answers against
keys computed from the parsed JSON.

  --models <a,b>        models for \`claude -p --model\` (default: haiku,sonnet)
  --renderings <a,b>    json, tokfold or both (default: json,tokfold)
  --count <n>           synthetic documents to generate (default: 40)
  --seed <n>            seed for documents and questions (default: 20261115)
  --docs <dir>          use the *.json files in <dir> instead of synthetic ones
  --profile <p>         tokfold compress --profile (default: balanced)
  --tokfold <path>      tokfold binary (default: target/release/tokfold)
  --claude <path>       claude CLI (default: claude)
  --repeats <n>         ask every question this many times (default: 1)
  --max-calls <n>       refuse a plan above this many calls (default and maximum: ${HARD_CAP})
  --max-cost-usd <x>    stop issuing calls once the reported cost reaches x
  --retries <n>         extra attempts for a failed call, within the cap (default: 1)
  --concurrency <n>     calls in flight at once (default: 4)
  --timeout-ms <n>      per-call timeout (default: 300000)
  --out <dir>           output directory (default: target/comprehension/<UTC time>)
  --dry-run             plan, render and write prompts; make no model call
  -h, --help            this text

Exit codes: 0 run (or dry run) completed; 2 usage error, refused plan, or a
missing binary.`;

function fail(msg) {
  process.stderr.write(`comprehension: ${msg}\n`);
  process.exit(2);
}

/** Spawn `bin` (a `.mjs`/`.js` file runs under this Node) and collect its output. */
function run(bin, args, { input, cwd, timeoutMs } = {}) {
  const [cmd, argv] = /\.(m?js)$/.test(bin) ? [process.execPath, [bin, ...args]] : [bin, args];
  return new Promise((resolve) => {
    const child = spawn(cmd, argv, { cwd, stdio: ["pipe", "pipe", "pipe"] });
    const out = [];
    const err = [];
    let timedOut = false;
    const timer = timeoutMs ? setTimeout(() => ((timedOut = true), child.kill("SIGKILL")), timeoutMs) : null;
    child.stdout.on("data", (d) => out.push(d));
    child.stderr.on("data", (d) => err.push(d));
    child.on("error", (e) => {
      if (timer) clearTimeout(timer);
      resolve({ code: null, stdout: "", stderr: String(e.message), timedOut });
    });
    child.on("close", (code) => {
      if (timer) clearTimeout(timer);
      resolve({ code, stdout: Buffer.concat(out).toString("utf8"), stderr: Buffer.concat(err).toString("utf8"), timedOut });
    });
    child.stdin.on("error", () => {});
    child.stdin.end(input ?? "");
  });
}

function git(args) {
  const r = spawnSync("git", args, { cwd: REPO, encoding: "utf8" });
  return r.status === 0 ? r.stdout.trim() : null;
}

function intOpt(values, name, dflt) {
  if (values[name] === undefined) return dflt;
  const n = Number(values[name]);
  if (!Number.isInteger(n)) fail(`--${name} must be an integer, got ${values[name]}`);
  return n;
}

function loadDocuments(values, seed) {
  if (values.docs) {
    const dir = path.resolve(values.docs);
    let names;
    try {
      names = fs.readdirSync(dir).filter((f) => f.endsWith(".json")).sort();
    } catch (e) {
      fail(`cannot read --docs ${dir}: ${e.message}`);
    }
    return {
      source: `local directory ${dir} (not committed)`,
      docs: names.map((f, i) => ({ id: `D${String(i + 1).padStart(2, "0")}`, tool: f, text: fs.readFileSync(path.join(dir, f), "utf8") })),
    };
  }
  const count = intOpt(values, "count", 40);
  if (count < 1) fail("--count must be at least 1");
  return { source: `synthetic, bench/comprehension/lib/fixtures.mjs seed ${seed}`, docs: generateDocuments(seed, count) };
}

async function main() {
  let values;
  try {
    ({ values } = parseArgs({
      options: {
        models: { type: "string" },
        renderings: { type: "string" },
        count: { type: "string" },
        seed: { type: "string" },
        docs: { type: "string" },
        profile: { type: "string" },
        tokfold: { type: "string" },
        claude: { type: "string" },
        repeats: { type: "string" },
        "max-calls": { type: "string" },
        "max-cost-usd": { type: "string" },
        retries: { type: "string" },
        concurrency: { type: "string" },
        "timeout-ms": { type: "string" },
        out: { type: "string" },
        "dry-run": { type: "boolean" },
        help: { type: "boolean", short: "h" },
      },
    }));
  } catch (e) {
    fail(e.message);
  }
  if (values.help) {
    process.stdout.write(HELP + "\n");
    return;
  }

  const models = (values.models ?? "haiku,sonnet").split(",").map((s) => s.trim()).filter(Boolean);
  const renderings = (values.renderings ?? RENDERINGS.join(",")).split(",").map((s) => s.trim()).filter(Boolean);
  for (const r of renderings) if (!RENDERINGS.includes(r)) fail(`unknown rendering ${r}; expected ${RENDERINGS.join(" or ")}`);
  if (models.length === 0 || renderings.length === 0) fail("at least one model and one rendering are needed");
  const seed = intOpt(values, "seed", 20261115);
  const repeats = intOpt(values, "repeats", 1);
  const maxCalls = intOpt(values, "max-calls", HARD_CAP);
  const retries = intOpt(values, "retries", 1);
  const concurrency = intOpt(values, "concurrency", 4);
  const timeoutMs = intOpt(values, "timeout-ms", 300000);
  const maxCost = values["max-cost-usd"] === undefined ? null : Number(values["max-cost-usd"]);
  if (maxCost !== null && !(maxCost > 0)) fail(`--max-cost-usd must be a positive number, got ${values["max-cost-usd"]}`);
  if (repeats < 1 || retries < 0 || concurrency < 1 || timeoutMs < 1) fail("--repeats, --concurrency and --timeout-ms must be positive and --retries non-negative");
  const profile = values.profile ?? "balanced";
  const tokfold = path.resolve(values.tokfold ?? path.join(REPO, "target", "release", "tokfold"));
  const claude = values.claude ?? "claude";
  const dryRun = Boolean(values["dry-run"]);

  // Documents and questions.
  const { source, docs: raw } = loadDocuments(values, seed);
  const docs = [];
  const skipped = [];
  raw.forEach((d, i) => {
    let parsed;
    try {
      parsed = JSON.parse(d.text);
    } catch {
      skipped.push({ id: d.id, tool: d.tool, reason: "not JSON" });
      return;
    }
    const questions = makeQuestions(d.id, parsed, seed + i + 1);
    if (questions.length === 0) skipped.push({ id: d.id, tool: d.tool, reason: "no list of six or more objects with a unique identifier" });
    else docs.push({ ...d, questions });
  });
  if (docs.length === 0) fail("no usable document");

  // The plan, refused before anything is rendered or called if it is over the cap.
  const plan = planCalls(docs.map((d) => d.id), models, renderings, repeats);
  const capMsg = capError(plan.length, maxCalls);
  if (capMsg) fail(capMsg);

  if (!fs.existsSync(tokfold)) fail(`tokfold binary not found at ${tokfold}; build it with \`cargo build --release -p tokfold-cli\` or pass --tokfold`);
  const startedAt = new Date().toISOString();
  const out = path.resolve(values.out ?? path.join(REPO, "target", "comprehension", startedAt.replace(/[:.]/g, "-")));
  fs.mkdirSync(path.join(out, "inputs"), { recursive: true });
  fs.mkdirSync(path.join(out, "prompts"), { recursive: true });

  // Renderings.
  for (const d of docs) {
    const src = path.join(out, "inputs", `${d.id}.json`);
    fs.writeFileSync(src, d.text);
    const r = await run(tokfold, ["compress", "--profile", profile, "-i", src], { timeoutMs: 60000 });
    if (r.code !== 0) fail(`tokfold compress failed on ${d.id} (exit ${r.code}): ${r.stderr.trim()}`);
    fs.writeFileSync(path.join(out, "inputs", `${d.id}.tokfold.txt`), r.stdout);
    // `#tkfd1 <tag>` opens a framed rendering; `0.0.1` wrote `⟦tkfd:v1:<tag>⟧` (with
    // `raw` on passthrough). An unframed rendering is a passthrough: the input itself.
    const head = /^(?:#tkfd1 (\S+)|⟦tkfd:v1:([^⟧]*)⟧)\n/.exec(r.stdout);
    d.rendered = { json: d.text, tokfold: r.stdout };
    d.encoder = head ? (head[1] ?? head[2]) : "passthrough";
    d.bytes = { json: Buffer.byteLength(d.text), tokfold: Buffer.byteLength(r.stdout) };
    for (const rendering of renderings) fs.writeFileSync(path.join(out, "prompts", `${d.id}.${rendering}.txt`), buildPrompt(d.rendered[rendering], d.questions));
  }
  fs.writeFileSync(
    path.join(out, "questions.json"),
    JSON.stringify(docs.flatMap((d) => d.questions.map((q) => ({ doc: d.id, ...q }))), null, 1) + "\n",
  );

  const claudeVersion = dryRun ? "(not queried: dry run)" : (await run(claude, ["--version"], { timeoutMs: 30000 })).stdout.trim() || "(unknown)";
  const config = {
    startedAt,
    dryRun,
    documents: {
      source,
      count: docs.length,
      skipped,
      perDocument: docs.map((d) => ({ id: d.id, tool: d.tool, questions: d.questions.length, encoder: d.encoder, bytes: d.bytes })),
    },
    questions: docs.reduce((s, d) => s + d.questions.length, 0),
    profile,
    tokfold: { path: tokfold, sha256: createHash("sha256").update(fs.readFileSync(tokfold)).digest("hex") },
    repo: { head: git(["rev-parse", "HEAD"]) ?? "(unknown)", dirty: (git(["status", "--porcelain"]) ?? "") !== "" },
    claude: {
      path: claude,
      version: claudeVersion,
      args: ["-p", "--model", "<model>", "--output-format", "json", "--tools", "", "--safe-mode", "--system-prompt", SYSTEM_PROMPT, "--no-session-persistence", "--strict-mcp-config"],
    },
    models,
    renderings,
    seed,
    repeats,
    maxCalls,
    maxCostUsd: maxCost,
    retries,
  };

  const bytes = (r) => docs.reduce((s, d) => s + d.bytes[r], 0);
  process.stderr.write(
    `comprehension: ${docs.length} documents (${skipped.length} skipped), ${config.questions} questions, ` +
      `${plan.length} planned calls (cap ${maxCalls}); bytes ${renderings.map((r) => `${r} ${bytes(r)}`).join(", ")}; ` +
      `encoders ${JSON.stringify(docs.reduce((m, d) => ((m[d.encoder] = (m[d.encoder] ?? 0) + 1), m), {}))}\n`,
  );

  if (dryRun) {
    const runRecord = { config, totals: { planned: plan.length, calls: 0, costUsd: 0 } };
    fs.writeFileSync(path.join(out, "run.json"), JSON.stringify(runRecord, null, 1) + "\n");
    process.stdout.write(`dry run: ${plan.length} model calls planned, none made; prompts in ${path.join(out, "prompts")}\n`);
    return;
  }

  // The calls. `made` is incremented before every spawn and never passes maxCalls.
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), "tokfold-comprehension-"));
  const byId = new Map(docs.map((d) => [d.id, d]));
  let made = 0;
  let cost = 0;
  let stopped = null;
  const records = [];
  // Planned calls not yet started. A retry may only use headroom these do not
  // need, so every planned call gets its first attempt before any retry.
  const queue = plan.slice();

  async function attempt(call) {
    const d = byId.get(call.doc);
    const args = config.claude.args.map((a) => (a === "<model>" ? call.model : a));
    const r = await run(claude, args, { input: buildPrompt(d.rendered[call.rendering], d.questions), cwd: scratch, timeoutMs });
    let reply = null;
    try {
      reply = JSON.parse(r.stdout);
    } catch {
      // not a JSON reply: a failed call
    }
    const ok = r.code === 0 && reply !== null && reply.is_error !== true && typeof reply.result === "string";
    return { r, reply, ok };
  }

  async function one(call) {
    const d = byId.get(call.doc);
    const rec = { ...call, attempts: 0, ok: false, parsed: false, costUsd: 0, inputTokens: 0, outputTokens: 0, modelIds: [], errors: [] };
    while (rec.attempts <= retries) {
      if (made >= maxCalls || (rec.attempts > 0 && made + queue.length >= maxCalls)) {
        stopped ??= `call cap of ${maxCalls} reached`;
        break;
      }
      if (maxCost !== null && cost >= maxCost) {
        stopped ??= `cost limit of $${maxCost} reached`;
        break;
      }
      made++;
      rec.attempts++;
      const { r, reply, ok } = await attempt(call);
      if (reply) {
        const c = Number(reply.total_cost_usd) || 0;
        rec.costUsd += c;
        cost += c;
        const u = reply.usage ?? {};
        rec.inputTokens += (u.input_tokens ?? 0) + (u.cache_creation_input_tokens ?? 0) + (u.cache_read_input_tokens ?? 0);
        rec.outputTokens += u.output_tokens ?? 0;
        rec.modelIds = [...new Set([...rec.modelIds, ...Object.keys(reply.modelUsage ?? {})])].sort();
        rec.usage = reply.usage;
        rec.modelUsage = reply.modelUsage;
      }
      if (ok) {
        rec.ok = true;
        rec.result = reply.result;
        break;
      }
      rec.errors.push({ code: r.code, timedOut: r.timedOut, stderr: r.stderr.slice(0, 500), result: reply?.result ?? null, subtype: reply?.subtype ?? null });
    }
    const answers = rec.ok ? parseAnswers(rec.result) : null;
    rec.parsed = answers !== null;
    rec.grades = d.questions.map((q) => {
      const given = answers ? answers[q.id] : undefined;
      return { qid: q.id, kind: q.kind, answer: q.answer, given: given === undefined ? null : given, correct: answers ? isCorrect(given, q.answer) : false };
    });
    records.push(rec);
    process.stderr.write(
      `  ${call.doc} ${call.model} ${call.rendering}#${call.repeat}: ${rec.grades.filter((g) => g.correct).length}/${rec.grades.length}` +
        `${rec.ok ? "" : " (call failed)"} [${made} calls, $${cost.toFixed(4)}]\n`,
    );
  }

  await Promise.all(
    Array.from({ length: Math.min(concurrency, queue.length) }, async () => {
      while (queue.length > 0) await one(queue.shift());
    }),
  );
  fs.rmSync(scratch, { recursive: true, force: true });

  const order = new Map(plan.map((c, i) => [`${c.doc}|${c.model}|${c.rendering}|${c.repeat}`, i]));
  records.sort((a, b) => order.get(`${a.doc}|${a.model}|${a.rendering}|${a.repeat}`) - order.get(`${b.doc}|${b.model}|${b.rendering}|${b.repeat}`));
  const summary = summarize(records, models, renderings);
  const runRecord = { config, totals: { planned: plan.length, calls: made, costUsd: cost, stopped }, summary, records };
  fs.writeFileSync(path.join(out, "run.json"), JSON.stringify(runRecord, null, 1) + "\n");
  const md = renderSummary(runRecord) + (stopped ? `\nStopped early: ${stopped}.\n` : "");
  fs.writeFileSync(path.join(out, "summary.md"), md);
  process.stdout.write(md + `\nWritten to ${out}\n`);
}

main().catch((e) => {
  process.stderr.write(`comprehension: ${e.stack ?? e}\n`);
  process.exit(1);
});
