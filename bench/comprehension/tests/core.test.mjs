// Tests for the pure parts of the comprehension harness: fixtures, question
// keys, answer parsing and grading, the call cap, and the summary arithmetic.

import assert from "node:assert/strict";
import test from "node:test";

import {
  HARD_CAP,
  KINDS,
  buildPrompt,
  capError,
  findList,
  isCorrect,
  makeQuestions,
  normalize,
  parseAnswers,
  planCalls,
  signTest,
  summarize,
} from "../lib/core.mjs";
import { generateDocuments } from "../lib/fixtures.mjs";

test("generated documents are reproducible from their seed and differ across seeds", () => {
  const a = generateDocuments(7, 8);
  const b = generateDocuments(7, 8);
  const c = generateDocuments(8, 8);
  assert.deepEqual(a, b);
  assert.notDeepEqual(a.map((d) => d.text), c.map((d) => d.text));
  assert.deepEqual(a.map((d) => d.id), ["D01", "D02", "D03", "D04", "D05", "D06", "D07", "D08"]);
  for (const d of a) assert.doesNotThrow(() => JSON.parse(d.text), d.id);
});

test("the default synthetic corpus yields every question kind for every document", () => {
  for (const [i, d] of generateDocuments(20261115, 40).entries()) {
    const qs = makeQuestions(d.id, JSON.parse(d.text), 20261115 + i + 1);
    assert.deepEqual(qs.map((q) => q.kind), KINDS, d.id);
  }
});

test("question keys are computed from the parsed document", () => {
  const rows = [
    { id: "a", n: 1, s: "x", flag: true },
    { id: "b", n: 9, s: "y", flag: false, extra: 1 },
    { id: "c", n: 3, s: "x", flag: true },
    { id: "d", n: 4, s: "y", flag: true },
    { id: "e", n: 5, s: "x", flag: false, extra: 2 },
    { id: "f", n: 2, s: "x", flag: true },
  ];
  const doc = { meta: { page: 1 }, result: { rows } };
  const qs = makeQuestions("T", doc, 1);
  const by = Object.fromEntries(qs.map((q) => [q.kind, q]));
  assert.equal(by.count.answer, "6");
  assert.match(by.count.text, /the "rows" list/);

  const lookup = /whose "id" is "(\w)", what is the value of "(\w+)"/.exec(by.lookup.text);
  const row = rows.find((x) => x.id === lookup[1]);
  assert.equal(by.lookup.answer, String(row[lookup[2]]));

  const cbv = /"(\w+)" equal to (\S+)\?/.exec(by.count_by_value.text);
  assert.equal(by.count_by_value.answer, String(rows.filter((x) => JSON.stringify(x[cbv[1]]) === cbv[2]).length));

  assert.equal(by.absent_field.answer, "b,e");
  assert.match(by.argmax.text, /largest "n"/);
  assert.equal(by.argmax.answer, "b");

  const conj = /"(\w+)" equal to (\S+) and "(\w+)" equal to (\S+)\?/.exec(by.count_conjunction.text);
  const expected = rows.filter((x) => JSON.stringify(x[conj[1]]) === conj[2] && JSON.stringify(x[conj[3]]) === conj[4]).length;
  assert.ok(expected >= 1);
  assert.equal(by.count_conjunction.answer, String(expected));
  assert.deepEqual(qs.map((q) => q.id), qs.map((_, i) => `TQ${i + 1}`));
});

test("a document without a usable list yields no questions", () => {
  assert.deepEqual(makeQuestions("T", { rows: [{ a: 1 }, { a: 2 }] }, 1), []);
  const noUnique = { rows: Array.from({ length: 6 }, () => ({ a: 1, b: true })) };
  assert.deepEqual(makeQuestions("T", noUnique, 1), []);
  assert.equal(findList([1, 2, 3, 4, 5, 6]), null);
});

test("the prompt carries the rendering and every question id", () => {
  const p = buildPrompt("⟦tkfd:v1:tbl⟧\nbody\n", [{ id: "D1Q1", text: "How many?" }]);
  assert.match(p, /<document>\n⟦tkfd:v1:tbl⟧\nbody\n<\/document>/);
  assert.match(p, /\nD1Q1: How many\?\n$/);
});

test("answers are parsed bare, fenced or embedded in prose", () => {
  assert.deepEqual(parseAnswers('{"Q1":"3"}'), { Q1: "3" });
  assert.deepEqual(parseAnswers('```json\n{"Q1": 3}\n```'), { Q1: 3 });
  assert.deepEqual(parseAnswers('Here you go: {"Q1":"a"} done'), { Q1: "a" });
  assert.equal(parseAnswers("no json here"), null);
  assert.equal(parseAnswers("[1,2]"), null);
  assert.equal(parseAnswers(undefined), null);
});

test("grading normalizes case, quotes, spacing, list order and number spelling", () => {
  assert.ok(isCorrect("b, a", "a,b"));
  assert.ok(isCorrect('"False"', "false"));
  assert.ok(isCorrect(3, "3"));
  assert.ok(isCorrect("3.0", "3"));
  assert.ok(isCorrect(["x", "y"], "y,x"));
  assert.ok(!isCorrect("a", "a,b"));
  assert.ok(!isCorrect(undefined, "1"));
  assert.ok(!isCorrect("", "0"));
  assert.equal(normalize(null), "");
});

test("the call cap refuses a plan above --max-calls and a --max-calls above the hard cap", () => {
  assert.equal(HARD_CAP, 200);
  assert.equal(capError(200, 200), null);
  assert.equal(capError(10, 10), null);
  assert.match(capError(11, 10), /needs 11 model calls, more than the cap of 10/);
  assert.match(capError(1, 201), /exceeds the hard cap of 200/);
  assert.match(capError(1, 0), /positive integer/);
});

test("the plan is one call per document, model, rendering and repeat, document-major", () => {
  const plan = planCalls(["D1", "D2"], ["m1", "m2"], ["json", "tokfold"], 2);
  assert.equal(plan.length, 16);
  assert.deepEqual(plan.slice(0, 4).map((c) => `${c.doc}${c.model}${c.rendering}`), ["D1m1json", "D1m1tokfold", "D1m2json", "D1m2tokfold"]);
  assert.equal(plan[8].repeat, 2);
});

test("the sign test matches hand-computed values", () => {
  assert.equal(signTest(0, 0), 1);
  assert.equal(signTest(5, 0), 0.0625);
  assert.equal(signTest(0, 5), 0.0625);
  assert.equal(signTest(1, 1), 1);
  assert.equal(signTest(6, 1), 0.125);
});

test("the summary counts each cell and the discordant pairs", () => {
  const rec = (rendering, correct) => ({
    doc: "D1",
    model: "m",
    rendering,
    repeat: 1,
    attempts: 1,
    ok: true,
    parsed: true,
    costUsd: 0.5,
    inputTokens: 100,
    outputTokens: 7,
    modelIds: ["m-1"],
    grades: correct.map((c, i) => ({ qid: `D1Q${i + 1}`, kind: KINDS[i], correct: c })),
  });
  const { cells, pairs } = summarize([rec("json", [true, true, false]), rec("tokfold", [true, false, true])], ["m"], ["json", "tokfold"]);
  assert.equal(cells.length, 2);
  assert.deepEqual([cells[0].correct, cells[0].total, cells[0].calls, cells[0].costUsd, cells[0].modelIds], [2, 3, 1, 0.5, ["m-1"]]);
  assert.deepEqual(cells[1].byKind.lookup, { correct: 0, total: 1 });
  assert.deepEqual([pairs[0].both, pairs[0].onlyFirst, pairs[0].onlySecond, pairs[0].neither], [1, 1, 1, 0]);
});
