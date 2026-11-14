// Pure parts of the comprehension harness: questions with a programmatic key,
// the prompt, answer parsing, grading, the call plan and its cap, and the
// summary. Nothing here touches the file system or spawns a process.

import { rng } from "./fixtures.mjs";

/** No run may make more model calls than this, whatever `--max-calls` says. */
export const HARD_CAP = 200;

export const KINDS = ["count", "lookup", "count_by_value", "absent_field", "argmax", "count_conjunction"];

const isScalar = (v) => (typeof v === "string" && v !== "") || typeof v === "number" || typeof v === "boolean";
const show = (v) => (typeof v === "string" ? v : JSON.stringify(v));
const has = (o, k) => Object.prototype.hasOwnProperty.call(o, k);

/**
 * The first array of at least six objects, depth-first in key order: the list
 * every question is about. Returns `{ rows, name }` or `null`.
 */
export function findList(v, name = "") {
  if (Array.isArray(v)) {
    if (v.length >= 6 && v.every((x) => x !== null && typeof x === "object" && !Array.isArray(x))) return { rows: v, name };
    return null;
  }
  if (v !== null && typeof v === "object") {
    for (const [k, x] of Object.entries(v)) {
      const found = findList(x, k);
      if (found) return found;
    }
  }
  return null;
}

/**
 * Up to six questions about one document, each with the answer computed from
 * the parsed JSON. `id` prefixes the question ids; `seed` makes the choice of
 * fields and records reproducible. Returns `[]` when the document has no list
 * of six or more objects with a unique identifier column.
 */
export function makeQuestions(id, doc, seed) {
  const found = findList(doc);
  if (!found) return [];
  const { rows, name } = found;
  const r = rng(seed);
  const n = rows.length;
  const distinct = (k) => new Set(rows.map((x) => JSON.stringify(x[k]))).size;
  const keys = Object.keys(rows[0]).filter((k) => rows.every((x) => has(x, k) && isScalar(x[k])));
  const uniq = keys.filter(
    (k) => distinct(k) === n && rows.every((x) => typeof x[k] !== "boolean" && String(x[k]).length <= 40 && !String(x[k]).includes(",")),
  );
  if (uniq.length === 0 || keys.length < 2) return [];
  const key = uniq[0];
  const low = keys.filter((k) => k !== key && distinct(k) >= 2 && distinct(k) <= Math.max(2, Math.floor(n / 2)));
  const all = [...new Set(rows.flatMap((x) => Object.keys(x)))].sort();
  const optional = all.filter((k) => {
    const c = rows.filter((x) => has(x, k)).length;
    return c >= 1 && c <= 4 && k !== key;
  });
  const numeric = keys.filter((k) => {
    if (k === key || !rows.every((x) => typeof x[k] === "number")) return false;
    const max = Math.max(...rows.map((x) => x[k]));
    return rows.filter((x) => x[k] === max).length === 1;
  });
  const list = `the "${name}" list`;
  const qs = [];
  const add = (kind, text, answer) => qs.push({ id: `${id}Q${qs.length + 1}`, kind, text, answer: String(answer) });

  add("count", `How many records does ${list} contain?`, n);

  const row = rows[r.int(0, n - 1)];
  const field = r.pick(keys.filter((k) => k !== key));
  add("lookup", `In ${list}, in the record whose "${key}" is ${JSON.stringify(row[key])}, what is the value of "${field}"?`, show(row[field]));

  if (low.length > 0) {
    const k = r.pick(low);
    const values = [...new Set(rows.map((x) => JSON.stringify(x[k])))].sort();
    const v = values[r.int(0, values.length - 1)];
    add("count_by_value", `In ${list}, how many records have "${k}" equal to ${v}?`, rows.filter((x) => JSON.stringify(x[k]) === v).length);
  }

  if (optional.length > 0) {
    const o = r.pick(optional);
    const values = rows.filter((x) => has(x, o)).map((x) => String(x[key])).sort();
    add("absent_field", `In ${list}, list the "${key}" values of the records that have a "${o}" field at all (comma-separated).`, values.join(","));
  }

  if (numeric.length > 0) {
    const k = r.pick(numeric);
    const max = Math.max(...rows.map((x) => x[k]));
    const top = rows.find((x) => x[k] === max);
    add("argmax", `In ${list}, which record has the largest "${k}"? Answer with its "${key}" value.`, top[key]);
  }

  if (low.length >= 2) {
    const [a, b] = r.shuffle(low).slice(0, 2);
    const pivot = rows[r.int(0, n - 1)];
    const count = rows.filter((x) => JSON.stringify(x[a]) === JSON.stringify(pivot[a]) && JSON.stringify(x[b]) === JSON.stringify(pivot[b])).length;
    add(
      "count_conjunction",
      `In ${list}, how many records have "${a}" equal to ${JSON.stringify(pivot[a])} and "${b}" equal to ${JSON.stringify(pivot[b])}?`,
      count,
    );
  }
  return qs;
}

export const SYSTEM_PROMPT =
  "You answer questions about one tool-result document. Use only the document. Reply with a single JSON object and nothing else.";

/** The user turn for one document rendering and its questions. */
export function buildPrompt(rendering, questions) {
  const lines = questions.map((q) => `${q.id}: ${q.text}`).join("\n");
  return (
    "The document below is the output of a tool call.\n\n<document>\n" +
    rendering.replace(/\n$/, "") +
    "\n</document>\n\n" +
    "Answer each question using only the document. Reply with one JSON object that maps every question id to its answer as a string, and nothing else. " +
    "When a question asks for several values, give them comma-separated.\n\n" +
    lines +
    "\n"
  );
}

/** The JSON object in a model reply: bare, fenced, or the outermost braces. `null` if none parses. */
export function parseAnswers(text) {
  const attempts = [];
  const t = String(text ?? "").trim();
  attempts.push(t);
  const fence = /```(?:json)?\s*([\s\S]*?)```/.exec(t);
  if (fence) attempts.push(fence[1].trim());
  const a = t.indexOf("{");
  const b = t.lastIndexOf("}");
  if (a !== -1 && b > a) attempts.push(t.slice(a, b + 1));
  for (const s of attempts) {
    try {
      const v = JSON.parse(s);
      if (v !== null && typeof v === "object" && !Array.isArray(v)) return v;
    } catch {
      // try the next form
    }
  }
  return null;
}

/**
 * Canonical form of an answer for comparison: split on commas, trim, drop
 * surrounding quotes and backticks, lower-case, write numbers canonically,
 * drop empty items, sort.
 */
export function normalize(v) {
  if (v === undefined || v === null) return "";
  const s = Array.isArray(v) ? v.map(String).join(",") : String(v);
  return s
    .split(",")
    .map((x) => x.trim().replace(/^["'`]+|["'`]+$/g, "").trim().toLowerCase())
    .map((x) => (/^-?\d+(\.\d+)?$/.test(x) ? String(Number(x)) : x))
    .filter((x) => x !== "")
    .sort()
    .join(",");
}

export const isCorrect = (given, answer) => normalize(given) === normalize(answer);

/**
 * The calls a run will make: one per document x model x rendering x repeat,
 * document-major so a run cut short still covers every cell about equally.
 */
export function planCalls(docIds, models, renderings, repeats = 1) {
  const calls = [];
  for (let rep = 1; rep <= repeats; rep++)
    for (const doc of docIds) for (const model of models) for (const rendering of renderings) calls.push({ doc, model, rendering, repeat: rep });
  return calls;
}

/** `null` if `planned` calls fit under `maxCalls` and the hard cap; otherwise the reason. */
export function capError(planned, maxCalls) {
  if (!Number.isInteger(maxCalls) || maxCalls < 1) return `--max-calls must be a positive integer, got ${maxCalls}`;
  if (maxCalls > HARD_CAP) return `--max-calls ${maxCalls} exceeds the hard cap of ${HARD_CAP} model calls per run`;
  if (planned > maxCalls) return `the plan needs ${planned} model calls, more than the cap of ${maxCalls}; use fewer documents, models, renderings or repeats`;
  return null;
}

/** Exact two-sided sign test on `b` and `c` discordant pairs. */
export function signTest(b, c) {
  const n = b + c;
  if (n === 0) return 1;
  let tail = 0;
  let coef = 1; // C(n, 0)
  for (let i = 0; i <= Math.min(b, c); i++) {
    tail += coef;
    coef = (coef * (n - i)) / (i + 1);
  }
  return Math.min(1, (2 * tail) / 2 ** n);
}

const pct = (a, b) => (b === 0 ? "-" : `${((100 * a) / b).toFixed(1)} %`);

/**
 * Per-cell (model x rendering) accuracy, per kind, calls, cost and input
 * tokens, plus the paired comparison of each model's renderings, from the
 * graded call records.
 */
export function summarize(records, models, renderings) {
  const cells = [];
  for (const model of models)
    for (const rendering of renderings) {
      const rs = records.filter((x) => x.model === model && x.rendering === rendering);
      const graded = rs.flatMap((x) => x.grades);
      const byKind = {};
      for (const k of KINDS) {
        const g = graded.filter((x) => x.kind === k);
        if (g.length > 0) byKind[k] = { correct: g.filter((x) => x.correct).length, total: g.length };
      }
      const ids = [...new Set(rs.flatMap((x) => x.modelIds))].sort();
      cells.push({
        model,
        modelIds: ids,
        rendering,
        correct: graded.filter((x) => x.correct).length,
        total: graded.length,
        byKind,
        calls: rs.reduce((s, x) => s + x.attempts, 0),
        failedCalls: rs.filter((x) => !x.ok).length,
        unparsed: rs.filter((x) => x.ok && !x.parsed).length,
        costUsd: rs.reduce((s, x) => s + x.costUsd, 0),
        inputTokens: rs.reduce((s, x) => s + x.inputTokens, 0),
        outputTokens: rs.reduce((s, x) => s + x.outputTokens, 0),
      });
    }
  const pairs = [];
  if (renderings.length === 2)
    for (const model of models) {
      const [r0, r1] = renderings;
      const key = (x) => `${x.repeat}:${x.qid}`;
      const of = (rendering) =>
        new Map(records.filter((x) => x.model === model && x.rendering === rendering).flatMap((x) => x.grades.map((g) => [key({ ...g, repeat: x.repeat }), g.correct])));
      const a = of(r0);
      const b = of(r1);
      let onlyFirst = 0;
      let onlySecond = 0;
      let both = 0;
      let neither = 0;
      for (const [k, v] of a) {
        if (!b.has(k)) continue;
        const w = b.get(k);
        if (v && w) both++;
        else if (v) onlyFirst++;
        else if (w) onlySecond++;
        else neither++;
      }
      pairs.push({ model, first: r0, second: r1, both, onlyFirst, onlySecond, neither, signTestP: signTest(onlyFirst, onlySecond) });
    }
  return { cells, pairs };
}

/** The Markdown summary a run prints and writes beside `run.json`. */
export function renderSummary(run) {
  const { config, summary, totals } = run;
  const out = [];
  out.push(`# Comprehension run ${config.startedAt}`, "");
  out.push("## Configuration", "");
  out.push(`- documents: ${config.documents.source} (${config.documents.count} documents, ${config.questions} questions)`);
  out.push(`- tokfold binary: ${config.tokfold.path} (sha256 ${config.tokfold.sha256}), \`compress --profile ${config.profile}\``);
  out.push(`- repository HEAD: ${config.repo.head}${config.repo.dirty ? " (working tree dirty)" : ""}`);
  out.push(`- claude CLI: ${config.claude.version}; models requested: ${config.models.join(", ")}; repeats: ${config.repeats}`);
  out.push(`- legend explaining the tokfold notation: none`);
  out.push(`- calls: ${totals.calls} of ${totals.planned} planned (cap ${config.maxCalls}); cost reported by the CLI: $${totals.costUsd.toFixed(4)}`, "");
  out.push("## Accuracy per cell", "");
  out.push(`| model (requested) | model id (returned) | rendering | correct | accuracy | ${KINDS.join(" | ")} | calls | failed | unparsed | input tokens | cost USD |`);
  out.push(`|---|---|---|---|---|${KINDS.map(() => "---|").join("")}---|---|---|---|---|`);
  for (const c of summary.cells) {
    const kinds = KINDS.map((k) => (c.byKind[k] ? `${c.byKind[k].correct}/${c.byKind[k].total}` : "-")).join(" | ");
    out.push(
      `| ${c.model} | ${c.modelIds.join(", ") || "-"} | ${c.rendering} | ${c.correct}/${c.total} | ${pct(c.correct, c.total)} | ${kinds} | ${c.calls} | ${c.failedCalls} | ${c.unparsed} | ${c.inputTokens} | ${c.costUsd.toFixed(4)} |`,
    );
  }
  if (summary.pairs.length > 0) {
    out.push("", "## Paired comparison (same question, same model)", "");
    out.push("| model | both right | only first | only second | both wrong | sign test p (two-sided) |");
    out.push("|---|---|---|---|---|---|");
    for (const p of summary.pairs)
      out.push(`| ${p.model}: ${p.first} vs ${p.second} | ${p.both} | ${p.onlyFirst} | ${p.onlySecond} | ${p.neither} | ${p.signTestP.toFixed(3)} |`);
  }
  out.push("", "Input tokens are the CLI's `usage` (input + cache creation + cache read), in each model's own tokenizer, system prompt and questions included.", "");
  return out.join("\n");
}
