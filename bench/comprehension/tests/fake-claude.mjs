// A stand-in for the `claude` CLI, used by harness.test.mjs. It never contacts
// a model. Every invocation except `--version` is appended to FAKE_CLAUDE_LOG
// (one JSON line of argv), so a test can count the calls the harness made.
//
//   FAKE_CLAUDE_MODE  answer (default) | fail | garbage
//   FAKE_CLAUDE_KEYS  questions.json written by the harness; `answer` mode
//                     replies with these keys, so every answer is correct
//   FAKE_CLAUDE_LOG   file to append invocations to

import fs from "node:fs";

const argv = process.argv.slice(2);
if (argv[0] === "--version") {
  process.stdout.write("0.0.0 (fake claude)\n");
  process.exit(0);
}
if (process.env.FAKE_CLAUDE_LOG) fs.appendFileSync(process.env.FAKE_CLAUDE_LOG, JSON.stringify(argv) + "\n");
const prompt = fs.readFileSync(0, "utf8");
const mode = process.env.FAKE_CLAUDE_MODE ?? "answer";
if (mode === "fail") {
  process.stderr.write("fake failure\n");
  process.exit(1);
}
const model = argv[argv.indexOf("--model") + 1];
let result = "I cannot answer that.";
if (mode === "answer") {
  const keys = JSON.parse(fs.readFileSync(process.env.FAKE_CLAUDE_KEYS, "utf8"));
  const asked = [...prompt.matchAll(/^(D\d+Q\d+):/gm)].map((m) => m[1]);
  result = "```json\n" + JSON.stringify(Object.fromEntries(asked.map((id) => [id, keys.find((k) => k.id === id).answer]))) + "\n```";
}
process.stdout.write(
  JSON.stringify({
    type: "result",
    subtype: "success",
    is_error: false,
    result,
    total_cost_usd: 0.001,
    usage: { input_tokens: 10, cache_creation_input_tokens: 5, cache_read_input_tokens: 1, output_tokens: 3 },
    modelUsage: { [`fake-${model}`]: { costUSD: 0.001 } },
  }) + "\n",
);
