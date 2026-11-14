# Comprehension harness

Asks a model questions about tool-result documents, once with the original JSON
and once with `tokfold compress` output, and grades the answers against keys
computed from the parsed JSON. It measures whether a model reads a tokfold
rendering as accurately as the JSON it replaces, on the documents and questions
it is given. It is not a general reading-comprehension benchmark.

Every model call goes through the Claude Code CLI (`claude -p`), so a run spends
from the caller's Claude subscription or API account. Nothing here runs in CI.

## Running it

```sh
cargo build --release -p tokfold-cli          # the binary whose output is tested
node bench/comprehension/harness.mjs --dry-run  # plan, render, write prompts; no call
node bench/comprehension/harness.mjs            # haiku,sonnet x json,tokfold x 40 documents = 160 calls
```

`--help` lists every option. The defaults are 40 synthetic documents, seed
`20261115`, models `haiku,sonnet`, both renderings, `--profile balanced`, one
repeat, one retry per failed call and four calls in flight.

## The call cap

- A run makes at most `--max-calls` model calls, and `--max-calls` may not exceed
  **200**. Both refusals happen before anything is rendered or called (exit `2`).
- The plan (documents x models x renderings x repeats) must fit under the cap, or
  the run is refused. Retries count as calls and use only headroom the remaining
  planned calls do not need, so every planned call gets its first attempt.
- `--max-cost-usd <x>` additionally stops issuing calls once the cost the CLI
  reports reaches `x`; calls already in flight finish, so the reported total can
  exceed `x`.
- `--dry-run` makes no model call (it does not even query `claude --version`).

## How a model is called

```
claude -p --model <model> --output-format json --tools "" --safe-mode \
  --system-prompt <fixed text> --no-session-persistence --strict-mcp-config
```

The prompt goes in on stdin and the working directory is an empty temporary
directory: no tools, no `CLAUDE.md`, no skills, no MCP servers. The prompt holds
the rendering inside `<document>` tags and the questions, and asks for one JSON
object mapping question ids to answers. No legend explaining the tokfold notation
is given.

The model ids a run records are the keys of the CLI's `modelUsage`, and the CLI
lists there every request it made, including ones it makes on its own. With
Claude Code 2.1.308 that was one extra `claude-haiku-5-5` request, about the size
of the prompt, in 159 of 160 calls, whether `--model` was `haiku` or `sonnet`; a
`sonnet` cell therefore lists both ids. The answer is the main request's: input
and output tokens come from `usage`, which describes that request only, while the
cost the CLI reports covers both.

## Questions and grading

`lib/core.mjs` takes the first list of six or more objects in a document, picks
its first unique scalar column as the record key, and asks up to six questions:
the record count, a field lookup by key, a count of records with one value, the
keys of the records that carry an optional field, the key of the record with the
largest value of a numeric field, and a count of records matching two values.
Choices come from a seeded PRNG, so the same seed asks the same questions. An
answer is correct when it equals the key after normalization (case, surrounding
quotes, whitespace, list order, number spelling). A reply that holds no JSON
object grades every question of that call as wrong and is counted as unparsed.

Note one asymmetry the questions do not hide: the tabular rendering states a
list's row count in its header (`#N[...]`), so the count question is a lookup in
that rendering and a counting task in JSON.

## Documents and privacy

By default the documents are synthetic (`lib/fixtures.mjs`): four tool-result
shapes (a table description, a record query, an instance listing, an audit log)
built from a seeded PRNG and fixed word lists. They contain no data from any real
system or person and are reproduced byte for byte from the seed.

`--docs <dir>` uses the `*.json` files in a local directory instead. Such a
corpus is the caller's data: keep it outside the repository, and note that the
run directory then holds copies of it and of the prompts. The configuration
records it as a local directory that is not committed.

## Output

A run writes to `--out` (default `target/comprehension/<UTC time>/`, ignored by
git):

- `inputs/`: each document and its tokfold rendering;
- `prompts/`: the exact prompt for each document and rendering;
- `questions.json`: the questions with their keys;
- `run.json`: the configuration (document source and seed, per-document encoder
  and bytes, profile, the tokfold binary's path and SHA-256, the repository HEAD
  and whether the tree was dirty, the claude CLI version and arguments, models,
  cap, retries), the totals (calls planned and made, the cost the CLI reported,
  why a run stopped early), per-cell results and every call record with its
  answers, `usage` and the model ids the CLI returned;
- `summary.md`: per-cell accuracy, per question kind, calls, failures, unparsed
  replies, input tokens and cost, and for two renderings a paired comparison per
  model with an exact two-sided sign test on the discordant questions.

Accuracy is a statement about the configuration recorded beside it — these
documents, these questions, this binary and profile, these models — and nothing
wider.

## Tests

```sh
node --test bench/comprehension/tests/*.test.mjs
```

The tests run the harness against fake `claude` and `tokfold` executables, so
they call no model and need no build. They count the fake CLI's invocations to
check the cap, the dry run, retries, the cost limit and the grading.
