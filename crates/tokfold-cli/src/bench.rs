//! `tokfold bench` — measure what the engine would save on a directory of Claude Code
//! transcripts, per tool and as a share of the bill those transcripts record.
//!
//! It answers one question before anyone installs the hook: is this workload one where
//! tool results are a large, compressible part of the bill, or one where they are not?
//! The answer is a report and nothing else. **The engine is not changed by it**: every
//! result is compressed by the same [`Compressor`] `compress` uses, with the profile
//! named, and the do-no-harm gate, the encoder choice and the archive format are the
//! engine's own. `--tokenizer` only chooses what the before and after texts are
//! *counted* with, as it does for `stats`.
//!
//! # What it reads, and what it prints
//!
//! It reads the regular `*.jsonl` files under the path it is given and nothing else:
//! no standard input, no network, no file written. It prints aggregates — counts, token
//! sums, ratios, shares — and the names of the tools whose results it measured, which
//! come from the transcripts. It never prints a tool result, a message, a path inside
//! the tree, or any other content. A tool name is untrusted text, so the text report
//! prints it through [`str::escape_debug`]: a control character in a name cannot move
//! the terminal's cursor or change its colours.
//!
//! The walk opens nothing that is not a regular file. Entries are classified with
//! [`fs::DirEntry::file_type`], which does not follow symbolic links, so a link is
//! skipped rather than followed (no loop, no escape from the tree), and so is a FIFO,
//! a socket or a device, which could block an `open` or never reach end of file. Each
//! is counted as a skipped entry. A file is read up to the length it had when it was
//! opened, so a transcript a live session is still appending to cannot keep the read
//! going; a record cut at that length fails to parse and is counted as an unparsed
//! line.
//!
//! One race is narrowed, not closed: an entry classified as a regular file can be
//! replaced between that check and the `open`. A replacement by a link to a device is
//! caught after the `open` — the opened file's own metadata must still say regular
//! file, or it is not read — but a replacement by a FIFO blocks the `open` itself
//! until a writer appears. Closing that needs an `O_NONBLOCK` open, which this crate
//! cannot ask for without a platform constant it does not carry. The same race exists,
//! and is documented, on `compress --archive`.
//!
//! # The model
//!
//! The analysis this follows is the 2026-11-12 measurement of a real Claude Code
//! corpus; the arithmetic is the same, so a run here reproduces its shape on the
//! caller's own data.
//!
//! * **Main chain only.** A record with a truthy `isSidechain` is skipped and counted.
//!   Subagent transcripts are mostly sidechain records, so their requests and results
//!   are out of the bill and out of the table alike.
//! * **A tool result** is a `tool_result` block in a record's `message.content`. Its
//!   text is the block's `content` when that is a string, or the concatenation of the
//!   `text` of its `"type": "text"` parts when it is a list; other parts (images) are
//!   ignored, and an empty text is not a result. Its tool is the `name` of the
//!   `tool_use` block in the same file whose `id` is its `tool_use_id`, or
//!   [`UNKNOWN_TOOL`].
//! * **The replacement rule** is `tokfold hook`'s: a result is replaced only when the
//!   engine accepts it and an encoder other than passthrough wins with a rendering
//!   shorter in bytes than the result. A replaced result's tokens after are the
//!   rendering's; every other result's tokens after are its tokens before. So a ratio
//!   below `1` is a saving the hook would deliver, and a ratio above `1` is possible
//!   only under an exact tokenizer that disagrees with the heuristic the gate prices
//!   with.
//! * **The hook's reach** is its default matcher, `mcp__.*`: Claude Code hands an MCP
//!   tool's result to the hook as a string, which it can replace, and a built-in
//!   tool's as an object, which it leaves alone. The `all tools` row is therefore what
//!   the engine *could* save if every result reached it; the `mcp__` row is what the
//!   installed hook reaches.
//! * **Requests** are assistant records carrying `message.usage`, identified by
//!   `requestId` or, failing that, `message.id`. One request spans several records, so
//!   usage is kept once per identifier, the last record's winning. The bill sums
//!   `input_tokens`, `output_tokens`, `cache_read_input_tokens` and the two
//!   `cache_creation` buckets (`ephemeral_1h_input_tokens`,
//!   `ephemeral_5m_input_tokens`), reading `cache_creation_input_tokens` as 5-minute
//!   writes when there is no `cache_creation` object. It is priced in base-input units
//!   at the multipliers in [`MULTIPLIERS`].
//! * **Compaction** ends every earlier result's life. A record with a truthy
//!   `isCompactSummary` or `compactMetadata`, or with `subtype` `compact_boundary`, is
//!   a boundary; consecutive boundaries count once.
//! * **A result's cost.** A result followed by `N ≥ 1` requests before the next
//!   boundary (or the end of its file) is written once, by the first of them, and read
//!   from the cache by the other `N − 1`. One of its tokens costs `W + r × (N − 1)`,
//!   where `r` is the cache-read multiplier and `W` the write multiplier the bill
//!   itself shows — uncached input, 5-minute and 1-hour writes, weighted by their
//!   volume. A result no request followed costs nothing. (The analysis wrote
//!   `W + r × N`, counting the writing request as a read too; the difference is one
//!   cache read per token.)
//! * **Shares** divide those costs by the bill. They mix units, and say so: tool
//!   results are counted with the chosen tokenizer, the bill is in the tokens Claude
//!   reported.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, ensure};
use clap::ValueEnum as _;
use tokfold_core::{Compressor, EncoderId, HeuristicEstimator, TokenEstimator};
use tokfold_mcp::json::{self, Object, Value};

use crate::BenchArgs;

/// The tool name a result is filed under when no `tool_use` in its file names it.
///
/// Parenthesised so it cannot be mistaken for a tool really called `unknown`: Claude
/// Code tool names are identifiers, and an MCP tool's starts with `mcp__`.
pub const UNKNOWN_TOOL: &str = "(unknown)";

/// The prefix every tool name the default hook matcher `mcp__.*` selects starts with.
pub const HOOK_MATCHER_PREFIX: &str = "mcp__";

/// How the bill is priced, in base-input units per token.
///
/// List-price multipliers: an uncached input token is the unit, a 5-minute cache
/// write costs `1.25`, a 1-hour write `2`, a cache read `0.10` and an output token `5`.
/// They are stated in every report rather than taken on trust.
pub const MULTIPLIERS: Multipliers = Multipliers {
    input: 1.0,
    write_5m: 1.25,
    write_1h: 2.0,
    cache_read: 0.10,
    output: 5.0,
};

/// The price of each usage bucket in base-input units; see [`MULTIPLIERS`].
#[derive(Debug, Clone, Copy)]
pub struct Multipliers {
    /// Uncached input.
    pub input: f64,
    /// A 5-minute cache write.
    pub write_5m: f64,
    /// A 1-hour cache write.
    pub write_1h: f64,
    /// A cache read.
    pub cache_read: f64,
    /// Output.
    pub output: f64,
}

/// `bench`: walk the path, measure every main-chain tool result, print the report.
///
/// `--tokenizer` is resolved first, before anything is read, so a build without the
/// `tiktoken` feature refuses `cl100k` and `o200k` with exit `2` exactly as `stats`
/// does. A path that is neither a directory nor a regular file, a root directory that
/// cannot be listed, and a tree with no `*.jsonl` file in it also exit `2`. A file or
/// subdirectory that cannot be read is counted in the report, not fatal.
pub fn run(args: &BenchArgs) -> Result<ExitCode> {
    let counter: Box<dyn TokenEstimator> =
        crate::exact_tokenizer(args.tokenizer)?.unwrap_or_else(|| Box::new(HeuristicEstimator));
    let mut walk = collect(&args.path)?;
    ensure!(
        !walk.files.is_empty(),
        "no .jsonl transcript under {}",
        args.path.display()
    );
    let compressor = Compressor::new(crate::config_for(args.profile));
    let mut tally = Tally::new(&compressor, counter.as_ref());
    for path in &walk.files {
        let read = read_file(path, &mut tally);
        tally.end_file();
        if read.is_err() {
            walk.unreadable += 1;
        }
    }
    let profile = args
        .profile
        .to_possible_value()
        .map_or_else(String::new, |value| value.get_name().to_owned());
    let report = Report {
        tokenizer_id: counter.tokenizer_id(),
        profile,
        files: walk.files.len(),
        skipped: walk.skipped,
        unreadable: walk.unreadable,
        tally,
    };
    let out = if args.json {
        report.json()
    } else {
        report.text()?
    };
    crate::write_stdout(out.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

/// What the walk found: the transcripts to read, in sorted order, and the entries it
/// would not open.
#[derive(Debug, Default)]
struct Walk {
    files: Vec<PathBuf>,
    /// Symbolic links and special files, never opened.
    skipped: u64,
    /// Subdirectories that could not be listed, entries that could not be classified,
    /// and transcripts that could not be read to the end.
    unreadable: u64,
}

/// Find the transcripts under `root`.
///
/// `root` itself is `stat`ed, which follows a link the caller typed: a named regular
/// file is read whatever its extension, a directory is walked, and anything else is
/// refused before it is opened. Inside the tree nothing is followed; see the module
/// docs.
fn collect(root: &Path) -> Result<Walk> {
    let meta = fs::metadata(root).with_context(|| format!("reading {}", root.display()))?;
    let mut walk = Walk::default();
    if meta.is_file() {
        walk.files.push(root.to_path_buf());
        return Ok(walk);
    }
    ensure!(
        meta.is_dir(),
        "{} is neither a directory nor a regular file",
        root.display()
    );
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if dir == root => {
                return Err(err).with_context(|| format!("listing {}", root.display()));
            }
            Err(_) => {
                walk.unreadable += 1;
                continue;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                walk.unreadable += 1;
                continue;
            };
            let Ok(kind) = entry.file_type() else {
                walk.unreadable += 1;
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                if path.extension() == Some(OsStr::new("jsonl")) {
                    walk.files.push(path);
                }
            } else {
                walk.skipped += 1;
            }
        }
    }
    walk.files.sort();
    Ok(walk)
}

/// Feed one transcript to `tally`, a line at a time, up to the length it had when it
/// was opened.
fn read_file(path: &Path, tally: &mut Tally<'_>) -> io::Result<()> {
    let file = fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut reader = BufReader::new(file.take(meta.len()));
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        tally.line(&line);
    }
}

/// Python's truthiness, which is how the analysis read the transcript's flags: absent,
/// `null`, `false`, `0`, `""`, `[]` and `{}` are false.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(n)) => *n != 0,
        Some(Value::Float(x)) => *x != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(fields)) => !fields.is_empty(),
    }
}

/// A non-negative integer field, or `0`.
fn count_field(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(0)
}

/// One request's `usage`, in Claude tokens.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Usage {
    input: u64,
    output: u64,
    cache_read: u64,
    write_1h: u64,
    write_5m: u64,
}

impl Usage {
    fn from_value(usage: &Value) -> Self {
        let creation = usage.get("cache_creation").filter(|c| truthy(Some(c)));
        let (hour, five_minutes) = creation.map_or_else(
            || (0, count_field(usage, "cache_creation_input_tokens")),
            |c| {
                (
                    count_field(c, "ephemeral_1h_input_tokens"),
                    count_field(c, "ephemeral_5m_input_tokens"),
                )
            },
        );
        Self {
            input: count_field(usage, "input_tokens"),
            output: count_field(usage, "output_tokens"),
            cache_read: count_field(usage, "cache_read_input_tokens"),
            write_1h: hour,
            write_5m: five_minutes,
        }
    }

    fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.write_1h = self.write_1h.saturating_add(other.write_1h);
        self.write_5m = self.write_5m.saturating_add(other.write_5m);
    }

    /// The bill in base-input units.
    fn units(&self, m: &Multipliers) -> f64 {
        priced(&[
            (self.input, m.input),
            (self.write_5m, m.write_5m),
            (self.write_1h, m.write_1h),
            (self.cache_read, m.cache_read),
            (self.output, m.output),
        ])
    }

    /// The write multiplier this bill shows: what one token entering the context costs,
    /// averaged over uncached input and the two cache-write buckets by their volume.
    /// `None` when the bill has none of the three.
    #[allow(clippy::cast_precision_loss)]
    fn write_multiplier(&self, m: &Multipliers) -> Option<f64> {
        let tokens = self
            .input
            .saturating_add(self.write_5m)
            .saturating_add(self.write_1h);
        (tokens > 0).then(|| {
            priced(&[
                (self.input, m.input),
                (self.write_5m, m.write_5m),
                (self.write_1h, m.write_1h),
            ]) / tokens as f64
        })
    }
}

/// The sum of `tokens × multiplier` over `terms`, in base-input units.
#[allow(clippy::cast_precision_loss)]
fn priced(terms: &[(u64, f64)]) -> f64 {
    terms.iter().fold(0.0, |sum, &(tokens, multiplier)| {
        (tokens as f64).mul_add(multiplier, sum)
    })
}

/// One side (before or after) of a set of results, with what their cost needs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Tokens {
    /// Every token.
    total: u64,
    /// The tokens of results some later request carried, so written once.
    written: u64,
    /// Token reads from the cache: each written result's tokens times `N − 1`.
    reread: u64,
    /// Tokens times `N`, for the token-weighted mean of later requests.
    turns: u64,
}

impl Tokens {
    fn add(&mut self, tokens: u64, later: u64) {
        self.total = self.total.saturating_add(tokens);
        self.turns = self.turns.saturating_add(tokens.saturating_mul(later));
        if later > 0 {
            self.written = self.written.saturating_add(tokens);
            self.reread = self.reread.saturating_add(tokens.saturating_mul(later - 1));
        }
    }

    fn merge(&mut self, other: &Self) {
        self.total = self.total.saturating_add(other.total);
        self.written = self.written.saturating_add(other.written);
        self.reread = self.reread.saturating_add(other.reread);
        self.turns = self.turns.saturating_add(other.turns);
    }

    /// The cost in base-input units at write multiplier `write`.
    fn cost(&self, write: f64, m: &Multipliers) -> f64 {
        priced(&[(self.written, write), (self.reread, m.cache_read)])
    }
}

/// The results of one tool, or of a set of tools.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Row {
    results: u64,
    replaced: u64,
    before: Tokens,
    after: Tokens,
}

impl Row {
    fn merge(&mut self, other: &Self) {
        self.results = self.results.saturating_add(other.results);
        self.replaced = self.replaced.saturating_add(other.replaced);
        self.before.merge(&other.before);
        self.after.merge(&other.after);
    }

    /// Tokens after over tokens before, `1` when there are none (the `stats`
    /// convention).
    #[allow(clippy::cast_precision_loss)]
    fn ratio(&self) -> f64 {
        if self.before.total == 0 {
            1.0
        } else {
            self.after.total as f64 / self.before.total as f64
        }
    }
}

/// A result waiting for its file's next compaction boundary or end.
#[derive(Debug)]
struct Pending {
    tool: String,
    before: u64,
    /// The rendering's tokens when the result is replaced.
    after: Option<u64>,
    /// The file's request count when the result appeared.
    at: u64,
}

/// Everything measured so far, plus the state of the file being read.
struct Tally<'a> {
    compressor: &'a Compressor,
    counter: &'a dyn TokenEstimator,
    tools: BTreeMap<String, Row>,
    usage: HashMap<String, Usage>,
    records: u64,
    sidechain: u64,
    unparsed: u64,
    compactions: u64,
    // Per-file state, reset by `end_file`.
    names: HashMap<String, String>,
    requests_seen: HashSet<String>,
    requests: u64,
    pending: Vec<Pending>,
    at_boundary: bool,
}

/// Saturating `usize` to `u64`, for token counts.
fn to_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

impl<'a> Tally<'a> {
    fn new(compressor: &'a Compressor, counter: &'a dyn TokenEstimator) -> Self {
        Self {
            compressor,
            counter,
            tools: BTreeMap::new(),
            usage: HashMap::new(),
            records: 0,
            sidechain: 0,
            unparsed: 0,
            compactions: 0,
            names: HashMap::new(),
            requests_seen: HashSet::new(),
            requests: 0,
            pending: Vec::new(),
            at_boundary: false,
        }
    }

    /// Take one line of a transcript.
    fn line(&mut self, bytes: &[u8]) {
        let Ok(text) = std::str::from_utf8(bytes) else {
            self.unparsed += 1;
            return;
        };
        let text = text.trim_ascii();
        if text.is_empty() {
            return;
        }
        let Ok(record) = json::parse(text) else {
            self.unparsed += 1;
            return;
        };
        if record.as_object().is_none() {
            self.unparsed += 1;
            return;
        }
        if truthy(record.get("isSidechain")) {
            self.sidechain += 1;
            return;
        }
        self.records += 1;
        if truthy(record.get("isCompactSummary"))
            || record.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
            || truthy(record.get("compactMetadata"))
        {
            self.boundary();
            return;
        }
        let Some(message) = record.get("message").filter(|m| m.as_object().is_some()) else {
            return;
        };
        if record.get("type").and_then(Value::as_str) == Some("assistant") {
            self.request(&record, message);
        }
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let id = block.get("id").and_then(Value::as_str);
                    let name = block.get("name").and_then(Value::as_str);
                    if let (Some(id), Some(name)) = (id, name) {
                        self.names.insert(id.to_owned(), name.to_owned());
                    }
                }
                Some("tool_result") => self.result(block),
                _ => {}
            }
        }
    }

    /// Record an assistant record's request, if it carries usage and an identifier.
    fn request(&mut self, record: &Value, message: &Value) {
        let Some(usage) = message.get("usage").filter(|u| u.as_object().is_some()) else {
            return;
        };
        let id = record
            .get("requestId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .or_else(|| {
                message
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
            });
        let Some(id) = id else {
            return;
        };
        if self.requests_seen.insert(id.to_owned()) {
            self.requests += 1;
            self.at_boundary = false;
        }
        self.usage.insert(id.to_owned(), Usage::from_value(usage));
    }

    /// Measure one `tool_result` block.
    fn result(&mut self, block: &Value) {
        let text: Cow<'_, str> = match block.get("content") {
            Some(Value::Str(text)) => Cow::Borrowed(text),
            Some(Value::Array(parts)) => Cow::Owned(
                parts
                    .iter()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect(),
            ),
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        let tool = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .and_then(|id| self.names.get(id))
            .map_or_else(|| UNKNOWN_TOOL.to_owned(), Clone::clone);
        let before = to_u64(self.counter.estimate(&text));
        let after = match self.compressor.compress(text.as_bytes()) {
            Ok(artifact)
                if artifact.stats.encoder != EncoderId::PASSTHROUGH
                    && artifact.rendering.len() < text.len() =>
            {
                Some(to_u64(self.counter.estimate(&artifact.rendering)))
            }
            _ => None,
        };
        self.pending.push(Pending {
            tool,
            before,
            after,
            at: self.requests,
        });
        self.at_boundary = false;
    }

    /// A compaction boundary: every pending result's life ends here.
    fn boundary(&mut self) {
        if !self.at_boundary {
            self.compactions += 1;
            self.at_boundary = true;
        }
        self.settle();
    }

    /// Close every pending result at the current request count.
    fn settle(&mut self) {
        for pending in self.pending.drain(..) {
            let later = self.requests - pending.at;
            let row = self.tools.entry(pending.tool).or_default();
            row.results += 1;
            if pending.after.is_some() {
                row.replaced += 1;
            }
            row.before.add(pending.before, later);
            row.after
                .add(pending.after.unwrap_or(pending.before), later);
        }
    }

    /// The end of a file: settle what is pending and forget the file's state.
    fn end_file(&mut self) {
        self.settle();
        self.names.clear();
        self.requests_seen.clear();
        self.requests = 0;
        self.at_boundary = false;
    }

    /// The sum of every row whose tool `keep` selects.
    fn sum(&self, keep: impl Fn(&str) -> bool) -> Row {
        let mut sum = Row::default();
        for (tool, row) in &self.tools {
            if keep(tool) {
                sum.merge(row);
            }
        }
        sum
    }

    fn bill(&self) -> Usage {
        let mut bill = Usage::default();
        for usage in self.usage.values() {
            bill.add(usage);
        }
        bill
    }
}

/// Whether the default hook matcher reaches `tool`.
fn hook_matches(tool: &str) -> bool {
    tool.starts_with(HOOK_MATCHER_PREFIX)
}

/// What the bill section derives from the tally.
#[derive(Debug, Clone, Copy)]
struct Bill {
    usage: Usage,
    requests: u64,
    units: f64,
    write_multiplier: Option<f64>,
    mean_later_requests: Option<f64>,
    tool_share: Option<f64>,
    saving_share: Option<f64>,
    hook_saving_share: Option<f64>,
}

impl Bill {
    #[allow(clippy::cast_precision_loss)]
    fn new(tally: &Tally<'_>, total: &Row, hooked: &Row) -> Self {
        let m = &MULTIPLIERS;
        let usage = tally.bill();
        let units = usage.units(m);
        let write = usage.write_multiplier(m);
        let share = |cost: f64| write.filter(|_| units > 0.0).map(|_| cost / units);
        let w = write.unwrap_or(0.0);
        Self {
            usage,
            requests: to_u64(tally.usage.len()),
            units,
            write_multiplier: write,
            mean_later_requests: (total.before.total > 0)
                .then(|| total.before.turns as f64 / total.before.total as f64),
            tool_share: share(total.before.cost(w, m)),
            saving_share: share(total.before.cost(w, m) - total.after.cost(w, m)),
            hook_saving_share: share(hooked.before.cost(w, m) - hooked.after.cost(w, m)),
        }
    }
}

/// The finished measurement, ready to print.
struct Report<'a> {
    tokenizer_id: u16,
    profile: String,
    files: usize,
    skipped: u64,
    unreadable: u64,
    tally: Tally<'a>,
}

/// A `u64` as a JSON integer, saturating at `i64::MAX`.
fn int(n: u64) -> Value {
    Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

/// An optional ratio as a JSON number, or `null`.
fn opt_float(x: Option<f64>) -> Value {
    x.map_or(Value::Null, Value::Float)
}

/// A row's counts as a JSON object, after any fields already in `object`.
fn row_json(object: Object, row: &Row) -> Value {
    object
        .set("results", int(row.results))
        .set("tokensBefore", int(row.before.total))
        .set("tokensAfter", int(row.after.total))
        .set("ratio", Value::Float(row.ratio()))
        .set("replaced", int(row.replaced))
        .build()
}

/// A share as a percentage with two decimals, or `n/a`.
fn percent(x: Option<f64>) -> String {
    x.map_or_else(|| "n/a".to_owned(), |x| format!("{:.2}%", x * 100.0))
}

impl Report<'_> {
    fn tokenizer(&self) -> &'static str {
        crate::tokenizer_name(self.tokenizer_id)
    }

    /// Every tool, most tokens first, ties by name.
    fn tools(&self) -> Vec<(&str, &Row)> {
        let mut tools: Vec<(&str, &Row)> = self
            .tally
            .tools
            .iter()
            .map(|(tool, row)| (tool.as_str(), row))
            .collect();
        tools.sort_by(|a, b| b.1.before.total.cmp(&a.1.before.total).then(a.0.cmp(b.0)));
        tools
    }

    /// One line of JSON.
    fn json(&self) -> String {
        let total = self.tally.sum(|_| true);
        let hooked = self.tally.sum(hook_matches);
        let bill = Bill::new(&self.tally, &total, &hooked);
        let m = &MULTIPLIERS;
        let tools = self
            .tools()
            .into_iter()
            .map(|(tool, row)| {
                row_json(
                    Object::new()
                        .set("tool", Value::string(tool))
                        .set("hookMatcher", Value::Bool(hook_matches(tool))),
                    row,
                )
            })
            .collect();
        let value = Object::new()
            .set("tokenizer", Value::string(self.tokenizer()))
            .set("tokenizerId", Value::Int(i64::from(self.tokenizer_id)))
            .set("profile", Value::string(self.profile.as_str()))
            .set("files", int(to_u64(self.files)))
            .set("skippedEntries", int(self.skipped))
            .set("unreadable", int(self.unreadable))
            .set("records", int(self.tally.records))
            .set("sidechainRecordsSkipped", int(self.tally.sidechain))
            .set("unparsedLines", int(self.tally.unparsed))
            .set("compactions", int(self.tally.compactions))
            .set("tools", Value::Array(tools))
            .set("total", row_json(Object::new(), &total))
            .set("hookMatched", row_json(Object::new(), &hooked))
            .set(
                "bill",
                Object::new()
                    .set("requests", int(bill.requests))
                    .set("inputTokens", int(bill.usage.input))
                    .set("cacheWrite5mTokens", int(bill.usage.write_5m))
                    .set("cacheWrite1hTokens", int(bill.usage.write_1h))
                    .set("cacheReadTokens", int(bill.usage.cache_read))
                    .set("outputTokens", int(bill.usage.output))
                    .set(
                        "multipliers",
                        Object::new()
                            .set("input", Value::Float(m.input))
                            .set("cacheWrite5m", Value::Float(m.write_5m))
                            .set("cacheWrite1h", Value::Float(m.write_1h))
                            .set("cacheRead", Value::Float(m.cache_read))
                            .set("output", Value::Float(m.output))
                            .build(),
                    )
                    .set("units", Value::Float(bill.units))
                    .set("writeMultiplier", opt_float(bill.write_multiplier))
                    .set("meanLaterRequests", opt_float(bill.mean_later_requests))
                    .set("toolResultShare", opt_float(bill.tool_share))
                    .set("savingShare", opt_float(bill.saving_share))
                    .set("hookMatchedSavingShare", opt_float(bill.hook_saving_share))
                    .build(),
            )
            .build();
        format!("{value}\n")
    }

    /// The aligned text report.
    fn text(&self) -> Result<String> {
        let total = self.tally.sum(|_| true);
        let hooked = self.tally.sum(hook_matches);
        let bill = Bill::new(&self.tally, &total, &hooked);
        let mut out = String::new();
        self.text_header(&mut out)?;
        writeln!(out)?;
        self.text_table(&mut out, &total, &hooked)?;
        writeln!(out)?;
        self.text_bill(&mut out, &bill)?;
        Ok(out)
    }

    /// What was measured, how, and what was read.
    fn text_header(&self, out: &mut String) -> fmt::Result {
        let estimate = if self.tokenizer_id == tokfold_core::estimator::ids::HEURISTIC {
            " -- the engine's own estimate, not a real tokenizer"
        } else {
            ""
        };
        writeln!(
            out,
            "tokfold bench: tool results in Claude Code transcripts (main chain only)"
        )?;
        writeln!(
            out,
            "tokenizer: {} (id {}){estimate}",
            self.tokenizer(),
            self.tokenizer_id
        )?;
        writeln!(out, "profile:   {}", self.profile)?;
        writeln!(
            out,
            "replaced:  only when an encoder other than passthrough wins with a rendering \
             shorter in bytes (the tokfold hook rule); otherwise tokens after = tokens before"
        )?;
        writeln!(
            out,
            "read:      {} files, {} records, {} sidechain records skipped, {} unparsed \
             lines, {} entries skipped (links, special files), {} unreadable, {} compactions",
            self.files,
            self.tally.records,
            self.tally.sidechain,
            self.tally.unparsed,
            self.skipped,
            self.unreadable,
            self.tally.compactions
        )
    }

    /// One row per tool, then the two sums.
    fn text_table(&self, out: &mut String, total: &Row, hooked: &Row) -> fmt::Result {
        const TOTAL: &str = "all tools";
        const HOOKED: &str = "mcp__.* (hook matcher)";
        let tools = self.tools();
        let names: Vec<String> = tools
            .iter()
            .map(|(tool, _)| tool.escape_debug().to_string())
            .collect();
        let w = names
            .iter()
            .map(|name| name.chars().count())
            .chain([4, TOTAL.len(), HOOKED.len()])
            .max()
            .unwrap_or(4);
        writeln!(
            out,
            "tool results, counted with {}, profile {}:",
            self.tokenizer(),
            self.profile
        )?;
        writeln!(
            out,
            "{:<w$}  {:>8}  {:>13}  {:>13}  {:>7}  {:>8}  hook",
            "tool", "results", "tokens before", "tokens after", "ratio", "replaced"
        )?;
        let line = |out: &mut String, name: &str, row: &Row, hook: &str| {
            let text = format!(
                "{name:<w$}  {:>8}  {:>13}  {:>13}  {:>7.4}  {:>8}  {hook}",
                row.results,
                row.before.total,
                row.after.total,
                row.ratio(),
                row.replaced
            );
            writeln!(out, "{}", text.trim_end())
        };
        for (name, (tool, row)) in names.iter().zip(&tools) {
            let hook = if hook_matches(tool) { "yes" } else { "no" };
            line(out, name, row, hook)?;
        }
        line(out, TOTAL, total, "")?;
        line(out, HOOKED, hooked, "")
    }

    /// The bill and the shares of it.
    fn text_bill(&self, out: &mut String, bill: &Bill) -> fmt::Result {
        const W: usize = 32;
        let m = &MULTIPLIERS;
        let opt = |x: Option<f64>, decimals: usize| {
            x.map_or_else(|| "n/a".to_owned(), |x| format!("{x:.decimals$}"))
        };
        writeln!(
            out,
            "bill, from the usage Claude reported, one entry per request id, in base-input \
             units (input {}, 5-minute write {}, 1-hour write {}, cache read {:.2}, output {}):",
            m.input, m.write_5m, m.write_1h, m.cache_read, m.output
        )?;
        for (label, value) in [
            ("requests:", bill.requests),
            ("input tokens:", bill.usage.input),
            ("5-minute cache writes:", bill.usage.write_5m),
            ("1-hour cache writes:", bill.usage.write_1h),
            ("cache reads:", bill.usage.cache_read),
            ("output tokens:", bill.usage.output),
        ] {
            writeln!(out, "{label:<W$}{value}")?;
        }
        writeln!(out, "{:<W$}{:.1}", "bill (base-input units):", bill.units)?;
        writeln!(
            out,
            "{:<W$}{}",
            "write multiplier:",
            opt(bill.write_multiplier, 4)
        )?;
        writeln!(
            out,
            "{:<W$}{} (token-weighted, until the next compaction)",
            "later requests per result:",
            opt(bill.mean_later_requests, 2)
        )?;
        writeln!(
            out,
            "shares of the bill mix units: tool results in {} tokens (profile {}), the bill \
             in Claude tokens",
            self.tokenizer(),
            self.profile
        )?;
        for (label, share) in [
            ("tool results:", bill.tool_share),
            ("saving, all tools:", bill.saving_share),
            ("saving, mcp__.* (hook matcher):", bill.hook_saving_share),
        ] {
            writeln!(out, "{label:<W$}{}", percent(share))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::{MULTIPLIERS, Row, Tally, Tokens, UNKNOWN_TOOL, Usage, hook_matches, truthy};
    use tokfold_core::{Compressor, Config, HeuristicEstimator, Profile, TokenEstimator};
    use tokfold_mcp::json::Value;

    /// A JSON array the tabular encoder wins on, as a tool's result text.
    const TABLE: &str = r#"[{"id":0,"name":"item0","active":true},{"id":1,"name":"item1","active":true},{"id":2,"name":"item2","active":true},{"id":3,"name":"item3","active":true}]"#;

    fn compressor() -> Compressor {
        Compressor::new(Config::builder().profile(Profile::Balanced).build())
    }

    fn assistant(request: &str, tool: Option<(&str, &str)>) -> String {
        let content = tool.map_or_else(String::new, |(id, name)| {
            format!(r#"{{"type":"tool_use","id":"{id}","name":"{name}","input":{{}}}}"#)
        });
        format!(
            r#"{{"type":"assistant","requestId":"{request}","message":{{"id":"m-{request}","usage":{{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":100,"cache_creation":{{"ephemeral_1h_input_tokens":40,"ephemeral_5m_input_tokens":0}}}},"content":[{content}]}}}}"#
        )
    }

    fn result(id: &str, text: &str) -> String {
        let text = Value::string(text).to_string();
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":{text}}}]}}}}"#
        )
    }

    fn feed(tally: &mut Tally<'_>, lines: &[String]) {
        for line in lines {
            tally.line(line.as_bytes());
        }
        tally.end_file();
    }

    #[test]
    fn a_result_lives_until_the_next_compaction_and_is_written_once() {
        let compressor = compressor();
        let mut tally = Tally::new(&compressor, &HeuristicEstimator);
        feed(
            &mut tally,
            &[
                assistant("r1", Some(("t1", "mcp__srv__list"))),
                result("t1", TABLE),
                assistant("r2", None),
                assistant("r2", None), // the same request, a second record
                assistant("r3", None),
                r#"{"type":"system","subtype":"compact_boundary"}"#.to_owned(),
                r#"{"isCompactSummary":true,"message":{"content":"summary"}}"#.to_owned(),
                assistant("r4", None),
            ],
        );
        let est = HeuristicEstimator.estimate(TABLE) as u64;
        let row = tally.tools["mcp__srv__list"];
        assert_eq!(row.results, 1);
        assert_eq!(row.replaced, 1);
        // Two requests (r2, r3) followed it before the boundary: written once, read once.
        assert_eq!(
            row.before,
            Tokens {
                total: est,
                written: est,
                reread: est,
                turns: est * 2
            }
        );
        assert!(row.after.total < row.before.total);
        assert_eq!(tally.compactions, 1, "consecutive boundaries count once");
        assert_eq!(tally.usage.len(), 4);
        assert_eq!(tally.bill().write_1h, 160);
    }

    #[test]
    fn sidechain_unparsed_and_unnamed_results_are_counted_apart() {
        let compressor = compressor();
        let mut tally = Tally::new(&compressor, &HeuristicEstimator);
        feed(
            &mut tally,
            &[
                r#"{"isSidechain":true,"type":"assistant","requestId":"side","message":{"usage":{"input_tokens":999},"content":[]}}"#.to_owned(),
                "not json".to_owned(),
                "[1,2]".to_owned(),
                "   ".to_owned(),
                result("nobody", "plain text, not JSON"),
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"x","content":[{"type":"text","text":"a"},{"type":"image"},{"type":"text","text":"b"}]}]}}"#.to_owned(),
                result("nobody", ""),
            ],
        );
        assert_eq!(tally.sidechain, 1);
        assert_eq!(
            tally.unparsed, 2,
            "invalid JSON and a non-object; blank is neither"
        );
        assert_eq!(tally.records, 3);
        assert!(
            tally.usage.is_empty(),
            "the sidechain request is not billed"
        );
        let row = tally.tools[UNKNOWN_TOOL];
        assert_eq!(row.results, 2, "an empty result is not a result");
        assert_eq!(row.replaced, 0);
        let text_parts = HeuristicEstimator.estimate("ab") as u64;
        let plain = HeuristicEstimator.estimate("plain text, not JSON") as u64;
        assert_eq!(row.before.total, plain + text_parts);
        assert_eq!(row.after, row.before);
        assert_eq!(row.before.written, 0, "no request followed either result");
    }

    #[test]
    fn a_passthrough_or_longer_rendering_is_not_a_replacement() {
        let compressor = compressor();
        let mut tally = Tally::new(&compressor, &HeuristicEstimator);
        feed(
            &mut tally,
            &[
                assistant("r1", Some(("t1", "Read"))),
                result("t1", r#"{"a":1}"#),
                assistant("r2", None),
            ],
        );
        let row = tally.tools["Read"];
        assert_eq!((row.results, row.replaced), (1, 0));
        assert_eq!(row.after, row.before);
        assert!(!hook_matches("Read"));
        assert!(hook_matches("mcp__srv__list"));
    }

    #[test]
    fn usage_without_a_cache_creation_object_reads_the_flat_field_as_five_minute() {
        let flat = tokfold_mcp::json::parse(
            r#"{"input_tokens":3,"cache_creation_input_tokens":7,"cache_creation":{}}"#,
        )
        .unwrap();
        let usage = Usage::from_value(&flat);
        assert_eq!((usage.write_5m, usage.write_1h, usage.input), (7, 0, 3));
        let w = usage.write_multiplier(&MULTIPLIERS).unwrap();
        assert!((w - 11.75 / 10.0).abs() < 1e-12, "(3 + 7 x 1.25) / 10");
        assert!(Usage::default().write_multiplier(&MULTIPLIERS).is_none());
    }

    #[test]
    fn truthiness_matches_the_analysis() {
        for (text, want) in [
            ("null", false),
            ("false", false),
            ("0", false),
            ("\"\"", false),
            ("[]", false),
            ("{}", false),
            ("true", true),
            ("1", true),
            ("\"x\"", true),
            ("{\"a\":1}", true),
        ] {
            let value = tokfold_mcp::json::parse(text).unwrap();
            assert_eq!(truthy(Some(&value)), want, "{text}");
        }
        assert!(!truthy(None));
    }

    #[test]
    fn a_row_without_tokens_has_ratio_one() {
        assert!((Row::default().ratio() - 1.0).abs() < f64::EPSILON);
    }
}
