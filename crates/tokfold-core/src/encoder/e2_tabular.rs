//! E2 — shape-deduplicated tabular re-encoding (§7): the core compression bet.
//!
//! # What it does
//!
//! Finds arrays whose elements are all JSON objects sharing a key set, hoists the
//! shared keys **once** as a header, and emits each element as a compact row of
//! values. Keys that would otherwise repeat on every element are stated a single
//! time — the source of the 50–90% target on repetitive tool output.
//!
//! Only the *outermost* qualifying array on any path is tabularized: an
//! array-of-objects nested inside a row cell stays compact JSON, so a row never
//! contains a nested table. Everything that is not a tabularized array is emitted
//! as structural-minified JSON (insignificant whitespace removed) with every
//! string, number and literal copied **verbatim** from its source span. Verbatim
//! copying is what makes the transform trivially reversible and preserves
//! [`never_compress`](crate::never_compress) content (compiler errors, HTTP
//! statuses, panic traces, …) byte-for-byte with its position: E2 performs no line
//! dedup, folding or reordering, so a protected line can never be merged or moved.
//!
//! # Body grammar (what [`render`] returns; the caller adds the `tbl` sentinel)
//!
//! The body is structural-minified JSON, except every tabularized array is written,
//! at its exact value position, as a NEWLINE-INTRODUCED table block. A literal
//! newline (`U+000A`) never occurs inside minified JSON — JSON forbids raw control
//! bytes in strings — so a newline unambiguously marks "a table block begins here".
//! A literal tab (`U+0009`) likewise never occurs inside a value, so it is the
//! in-block field separator.
//!
//! A table block replacing an `N`-element array is:
//!
//! ```text
//! \n
//! #<N>\t<key_0>\t<key_1>\t…\t<key_{H-1}>\n   header: element count, then the H hoisted keys
//! <row>\n                                     exactly N rows follow
//! …
//! ```
//!
//! Keys are raw JSON string lexemes (quotes included). After the `N`-th row the
//! surrounding minified JSON resumes immediately: the count `N` terminates the
//! block, there is no end marker.
//!
//! Each row is one of:
//!
//! * **plain** — `+\t<v_0>\t<v_1>\t…\t<v_{H-1}>`. The element's keys equal the
//!   header keys in order; the `H` cells are its values, in header order.
//! * **deviating** — `*\t<m>\t<k_0>\t<v_0>\t…\t<k_{m-1}>\t<v_{m-1}>`. The element is
//!   still an object but its ordered key list differs (missing, extra, reordered or
//!   duplicated keys); it is written self-describingly as its own `m` key/value
//!   pairs in source order.
//!
//! Each `<v>` is a minified-JSON value (verbatim scalars; compact `{…}` / `[…]` for
//! nested containers). No cell can contain a tab or a newline, so a row is
//! reconstructed by splitting on tabs.
//!
//! # Reversibility
//!
//! Every value survives exactly once; only syntax changed. From the body alone a
//! reader reconstructs the original array — read the header keys, then for each row
//! pair keys with cells (plain) or read the listed pairs (deviating). Object key
//! order, duplicate keys and number lexemes are preserved; strings are byte-verbatim.
//!
//! # Determinism (§10)
//!
//! Shapes are interned in an `FxHashMap<u64, ShapeId>` keyed by a hash of the
//! ordered key list; the header is the most frequent shape, ties broken by first
//! appearance. `std::collections::HashMap` is avoided on purpose: its per-process
//! random seed would make iteration order — and thus output — vary between runs and
//! silently kill the provider's prompt cache. Selection and emission iterate `Vec`s
//! in source order; a map is never iterated to produce output, so identical input
//! yields byte-identical output.
//!
//! # When it declines (returns `None`)
//!
//! No array qualifies: an array needs ≥ 2 elements, all objects, with a shared
//! shape — the dominant key set must occur ≥ 2 times and be non-empty. Scalar
//! arrays and single-element arrays never qualify. Whether the tabular form is
//! actually shorter than the input is decided afterwards by
//! [`select`](super::select) under the token estimator, exactly as for E1.

use core::hash::Hasher;

use rustc_hash::{FxHashMap, FxHasher};

use crate::tape::{Node, NodeKind, Span, Tape};

/// Table block introducer and in-block separators. These bytes cannot occur inside
/// minified JSON, which is what makes the block boundaries unambiguous.
const BLOCK: char = '\n';
const FIELD: char = '\t';
/// Row-position markers: header, plain row, deviating row.
const HEADER: char = '#';
const PLAIN: char = '+';
const DEVIATED: char = '*';

/// Render the tape as E2 tabular output, or `None` when E2 does not apply.
// `pub(crate)` is the contract the sealed-encoder design specifies for this entry
// point; it is stated explicitly rather than inferred from the private module.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn render(tape: &Tape, input: &str) -> Option<String> {
    let nodes = tape.nodes();
    let plans = analyze(nodes, input);
    if plans.is_empty() {
        // No array shares a key set: nothing to tabularize. Degrade to passthrough
        // (via the caller) rather than emit a body identical to plain minification.
        return None;
    }

    let mut out = String::with_capacity(input.len());
    let mut frames: Vec<RenderFrame> = Vec::new();
    let mut emitted_table = false;
    let mut i = 0usize;

    while i < nodes.len() {
        let node = *nodes.get(i)?;
        match node.kind {
            // A tabularized array: emit its separator like any value, splice in the
            // table block, and skip the whole array subtree.
            NodeKind::ArrayStart { .. } if plans.contains_key(&i) => {
                let plan = plans.get(&i)?;
                open_value(&mut frames, &mut out);
                emit_table(nodes, input, &mut out, i, plan)?;
                emitted_table = true;
                i = plan.end_index.checked_add(1)?;
                continue;
            }
            NodeKind::Null | NodeKind::Bool(_) | NodeKind::Number | NodeKind::String => {
                open_value(&mut frames, &mut out);
                out.push_str(span_str(input, node.span)?);
            }
            NodeKind::Key => {
                open_key(&mut frames, &mut out);
                out.push_str(span_str(input, node.span)?);
                out.push(':');
            }
            NodeKind::ObjectStart { .. } => {
                open_value(&mut frames, &mut out);
                out.push('{');
                frames.push(RenderFrame::object());
            }
            NodeKind::ObjectEnd => {
                out.push('}');
                frames.pop();
            }
            NodeKind::ArrayStart { .. } => {
                open_value(&mut frames, &mut out);
                out.push('[');
                frames.push(RenderFrame::array());
            }
            NodeKind::ArrayEnd => {
                out.push(']');
                frames.pop();
            }
        }
        i = i.checked_add(1)?;
    }

    // `plans` is non-empty and the topmost qualifier on every path is reached in
    // normal mode, so a table was emitted; the guard only protects against a span
    // resolution failure that aborted the walk early.
    if emitted_table { Some(out) } else { None }
}

// ---------------------------------------------------------------------------
// Analysis: decide which arrays become tables and pick each one's header.
// ---------------------------------------------------------------------------

/// The plan for one tabularized array: where it ends and which keys to hoist.
struct TablePlan {
    /// Index of the array's `ArrayEnd` node, used to skip its subtree.
    end_index: usize,
    /// The hoisted keys (raw string lexeme spans) in header order.
    header_keys: Vec<Span>,
}

/// Interned id of a distinct element shape within one array.
#[derive(Copy, Clone)]
struct ShapeId(usize);

/// Accumulated data for one distinct shape: how often it occurs and an exemplar's
/// ordered key spans (the header candidate if this shape wins).
struct ShapeAcc {
    count: u32,
    header_keys: Vec<Span>,
}

/// One open container during analysis.
struct AnalyzeFrame {
    is_array: bool,
    start_index: usize,
    /// Ordered key spans (objects only).
    keys: Vec<Span>,
    /// Whether every element seen so far is an object (arrays only).
    all_objects: bool,
    /// Element count (arrays only).
    elem_count: u32,
    /// Shape interning by ordered-key-list hash (arrays only). Keyed by `u64` on
    /// purpose: see the module determinism note.
    shape_ids: FxHashMap<u64, ShapeId>,
    /// Per-shape accumulators in first-appearance order (arrays only).
    shapes: Vec<ShapeAcc>,
}

impl AnalyzeFrame {
    fn object(start_index: usize) -> Self {
        Self {
            is_array: false,
            start_index,
            keys: Vec::new(),
            all_objects: true,
            elem_count: 0,
            shape_ids: FxHashMap::default(),
            shapes: Vec::new(),
        }
    }

    fn array(start_index: usize) -> Self {
        Self {
            is_array: true,
            start_index,
            keys: Vec::new(),
            all_objects: true,
            elem_count: 0,
            shape_ids: FxHashMap::default(),
            shapes: Vec::new(),
        }
    }

    /// Record one element object's shape, interning by the hash of its key list.
    fn intern(&mut self, hash: u64, keys: &[Span]) {
        if let Some(&id) = self.shape_ids.get(&hash) {
            if let Some(acc) = self.shapes.get_mut(id.0) {
                acc.count = acc.count.saturating_add(1);
            }
        } else {
            let id = ShapeId(self.shapes.len());
            self.shape_ids.insert(hash, id);
            self.shapes.push(ShapeAcc {
                count: 1,
                header_keys: keys.to_vec(),
            });
        }
    }
}

/// Walk the tape once and return a plan for every qualifying array, keyed by the
/// array's `ArrayStart` node index.
fn analyze(nodes: &[Node], input: &str) -> FxHashMap<usize, TablePlan> {
    let mut plans: FxHashMap<usize, TablePlan> = FxHashMap::default();
    let mut stack: Vec<AnalyzeFrame> = Vec::new();

    for (i, node) in nodes.iter().enumerate() {
        match node.kind {
            NodeKind::ObjectStart { .. } => stack.push(AnalyzeFrame::object(i)),
            NodeKind::ArrayStart { .. } => stack.push(AnalyzeFrame::array(i)),
            NodeKind::Key => {
                if let Some(top) = stack.last_mut() {
                    top.keys.push(node.span);
                }
            }
            NodeKind::Null | NodeKind::Bool(_) | NodeKind::Number | NodeKind::String => {
                // A scalar directly inside an array is a non-object element.
                if let Some(top) = stack.last_mut() {
                    if top.is_array {
                        top.elem_count = top.elem_count.saturating_add(1);
                        top.all_objects = false;
                    }
                }
            }
            NodeKind::ObjectEnd => {
                let Some(done) = stack.pop() else { continue };
                // If this object is an array element, record its shape.
                if let Some(parent) = stack.last_mut() {
                    if parent.is_array {
                        parent.elem_count = parent.elem_count.saturating_add(1);
                        let hash = shape_hash(&done.keys, input);
                        parent.intern(hash, &done.keys);
                    }
                }
            }
            NodeKind::ArrayEnd => {
                let Some(done) = stack.pop() else { continue };
                if let Some(plan) = finish_array(&done, i) {
                    plans.insert(done.start_index, plan);
                }
                // A nested array is itself a non-object element of its parent.
                if let Some(parent) = stack.last_mut() {
                    if parent.is_array {
                        parent.elem_count = parent.elem_count.saturating_add(1);
                        parent.all_objects = false;
                    }
                }
            }
        }
    }

    plans
}

/// Decide whether a closed array qualifies and, if so, build its plan.
fn finish_array(frame: &AnalyzeFrame, end_index: usize) -> Option<TablePlan> {
    if !frame.is_array || !frame.all_objects || frame.elem_count < 2 {
        return None;
    }

    // Dominant shape = highest count, ties broken by first appearance (the `>` keeps
    // the earlier `Vec` entry on equal counts). Iterating the `Vec`, never the map,
    // keeps this deterministic.
    let mut best: Option<&ShapeAcc> = None;
    for shape in &frame.shapes {
        if best.is_none_or(|current| shape.count > current.count) {
            best = Some(shape);
        }
    }
    let best = best?;

    // Require real sharing (≥ 2 elements of one shape) and something to hoist.
    if best.count < 2 || best.header_keys.is_empty() {
        return None;
    }

    Some(TablePlan {
        end_index,
        header_keys: best.header_keys.clone(),
    })
}

/// Hash the ordered key list into the shape key. A trailing sentinel byte after each
/// key keeps `["ab","c"]` distinct from `["a","bc"]`.
fn shape_hash(keys: &[Span], input: &str) -> u64 {
    let mut hasher = FxHasher::default();
    for span in keys {
        if let Some(text) = span_str(input, *span) {
            hasher.write(text.as_bytes());
        }
        hasher.write_u8(0xff);
    }
    hasher.finish()
}

// ---------------------------------------------------------------------------
// Table emission.
// ---------------------------------------------------------------------------

/// Emit the newline-introduced table block for the array at `arr_start`.
fn emit_table(
    nodes: &[Node],
    input: &str,
    out: &mut String,
    arr_start: usize,
    plan: &TablePlan,
) -> Option<()> {
    let elems = elements(nodes, arr_start, plan.end_index)?;

    out.push(BLOCK);
    out.push(HEADER);
    push_usize(out, elems.len());
    for key in &plan.header_keys {
        out.push(FIELD);
        out.push_str(span_str(input, *key)?);
    }
    out.push(BLOCK);

    for (elem_start, elem_end) in elems {
        emit_row(nodes, input, out, elem_start, elem_end, &plan.header_keys)?;
    }
    Some(())
}

/// Emit one element as a plain or deviating row, terminated by a newline.
fn emit_row(
    nodes: &[Node],
    input: &str,
    out: &mut String,
    elem_start: usize,
    elem_end: usize,
    header: &[Span],
) -> Option<()> {
    // The qualifying condition guarantees every element is an object; bail to
    // passthrough rather than misrender if that ever fails to hold.
    if !matches!(nodes.get(elem_start)?.kind, NodeKind::ObjectStart { .. }) {
        return None;
    }
    let members = object_members(nodes, elem_start, elem_end)?;

    if members_match_header(&members, header, input) {
        out.push(PLAIN);
        for (_, val_lo, val_hi) in &members {
            out.push(FIELD);
            emit_compact(nodes, input, out, *val_lo, *val_hi)?;
        }
    } else {
        out.push(DEVIATED);
        out.push(FIELD);
        push_usize(out, members.len());
        for (key, val_lo, val_hi) in &members {
            out.push(FIELD);
            out.push_str(span_str(input, *key)?);
            out.push(FIELD);
            emit_compact(nodes, input, out, *val_lo, *val_hi)?;
        }
    }

    out.push(BLOCK);
    Some(())
}

/// Whether the element's ordered keys equal the header keys, lexeme for lexeme.
/// Exact comparison (not the hash) decides plain vs deviating, so a hash collision
/// can only cost compression, never correctness.
fn members_match_header(members: &[(Span, usize, usize)], header: &[Span], input: &str) -> bool {
    if members.len() != header.len() {
        return false;
    }
    for (member, hspan) in members.iter().zip(header) {
        let (kspan, _, _) = *member;
        match (span_str(input, kspan), span_str(input, *hspan)) {
            (Some(a), Some(b)) if a == b => {}
            _ => return false,
        }
    }
    true
}

/// The direct children of an array `[arr_start, arr_end)` as `(start, end)` node
/// index pairs, one per element.
fn elements(nodes: &[Node], arr_start: usize, arr_end: usize) -> Option<Vec<(usize, usize)>> {
    let mut list: Vec<(usize, usize)> = Vec::new();
    let mut v = arr_start.checked_add(1)?;
    while v < arr_end {
        let end = value_end(nodes, v)?;
        list.push((v, end));
        v = end.checked_add(1)?;
    }
    Some(list)
}

/// The members of an object `[obj_start, obj_end)` as `(key_span, value_start,
/// value_end)` triples, in source order (duplicate keys preserved).
fn object_members(
    nodes: &[Node],
    obj_start: usize,
    obj_end: usize,
) -> Option<Vec<(Span, usize, usize)>> {
    let mut list: Vec<(Span, usize, usize)> = Vec::new();
    let mut j = obj_start.checked_add(1)?;
    while j < obj_end {
        let key = nodes.get(j)?;
        if !matches!(key.kind, NodeKind::Key) {
            return None;
        }
        let value_start = j.checked_add(1)?;
        let value_end = value_end(nodes, value_start)?;
        list.push((key.span, value_start, value_end));
        j = value_end.checked_add(1)?;
    }
    Some(list)
}

/// The index of the last node of the value beginning at `v` (inclusive). For a
/// scalar this is `v`; for a container it is the matching `End`.
fn value_end(nodes: &[Node], v: usize) -> Option<usize> {
    match nodes.get(v)?.kind {
        NodeKind::ObjectStart { .. } | NodeKind::ArrayStart { .. } => {
            let mut depth = 0i32;
            let mut k = v;
            loop {
                match nodes.get(k)?.kind {
                    NodeKind::ObjectStart { .. } | NodeKind::ArrayStart { .. } => depth += 1,
                    NodeKind::ObjectEnd | NodeKind::ArrayEnd => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(k);
                        }
                    }
                    _ => {}
                }
                k = k.checked_add(1)?;
            }
        }
        // A key is never the start of a value; other kinds are single-node scalars.
        NodeKind::Key => None,
        _ => Some(v),
    }
}

// ---------------------------------------------------------------------------
// Compact (table-free) rendering of a single value subtree, for row cells.
// ---------------------------------------------------------------------------

/// Emit the value spanning nodes `[lo, hi]` as minified JSON with verbatim lexemes.
/// Never starts a table, so a cell can never contain a nested table block.
fn emit_compact(nodes: &[Node], input: &str, out: &mut String, lo: usize, hi: usize) -> Option<()> {
    let mut frames: Vec<RenderFrame> = Vec::new();
    let mut i = lo;
    while i <= hi {
        let node = *nodes.get(i)?;
        match node.kind {
            NodeKind::Null | NodeKind::Bool(_) | NodeKind::Number | NodeKind::String => {
                open_value(&mut frames, out);
                out.push_str(span_str(input, node.span)?);
            }
            NodeKind::Key => {
                open_key(&mut frames, out);
                out.push_str(span_str(input, node.span)?);
                out.push(':');
            }
            NodeKind::ObjectStart { .. } => {
                open_value(&mut frames, out);
                out.push('{');
                frames.push(RenderFrame::object());
            }
            NodeKind::ObjectEnd => {
                out.push('}');
                frames.pop();
            }
            NodeKind::ArrayStart { .. } => {
                open_value(&mut frames, out);
                out.push('[');
                frames.push(RenderFrame::array());
            }
            NodeKind::ArrayEnd => {
                out.push(']');
                frames.pop();
            }
        }
        i = i.checked_add(1)?;
    }
    Some(())
}

// ---------------------------------------------------------------------------
// Shared minified-JSON emission helpers (mirror E1's separator logic).
// ---------------------------------------------------------------------------

/// One open container during emission; `seen` drives comma placement.
struct RenderFrame {
    is_array: bool,
    seen: bool,
}

impl RenderFrame {
    fn object() -> Self {
        Self {
            is_array: false,
            seen: false,
        }
    }

    fn array() -> Self {
        Self {
            is_array: true,
            seen: false,
        }
    }
}

/// Emit the separator preceding a value: a comma before every array element after
/// the first. In an object the comma belongs before the key, and a top-level value
/// has no frame, so both emit nothing here.
fn open_value(frames: &mut [RenderFrame], out: &mut String) {
    if let Some(frame) = frames.last_mut() {
        if frame.is_array {
            if frame.seen {
                out.push(',');
            }
            frame.seen = true;
        }
    }
}

/// Emit the separator preceding an object key: a comma before every member after
/// the first.
fn open_key(frames: &mut [RenderFrame], out: &mut String) {
    if let Some(frame) = frames.last_mut() {
        if frame.seen {
            out.push(',');
        }
        frame.seen = true;
    }
}

/// Resolve a node span to its source slice, or `None` if it is out of bounds.
fn span_str(input: &str, span: Span) -> Option<&str> {
    input.get(span.start as usize..span.end as usize)
}

/// Append `n` in decimal without allocating a panic path.
fn push_usize(out: &mut String, n: usize) {
    out.push_str(&n.to_string());
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::needless_range_loop,
        clippy::too_many_lines
    )]

    use super::{emit_compact, render};
    use crate::tape::{self, Tape};

    fn tape_of(input: &str) -> Tape {
        tape::parse(input, 512).unwrap()
    }

    fn e2(input: &str) -> Option<String> {
        render(&tape_of(input), input)
    }

    /// The whole document as minified JSON with verbatim lexemes — the canonical
    /// form the reconstructed body must match byte-for-byte.
    fn compact(input: &str) -> String {
        let t = tape_of(input);
        let nodes = t.nodes();
        let mut out = String::new();
        emit_compact(nodes, input, &mut out, 0, nodes.len() - 1).unwrap();
        out
    }

    /// Reference decoder for the documented `tbl` body grammar: rebuild minified
    /// JSON, expanding every table block back into an array. Proves the body is
    /// self-contained reversible from the rendering alone.
    fn reconstruct(body: &str) -> String {
        let lines: Vec<&str> = body.split('\n').collect();
        let mut out = String::new();
        let mut idx = 0;
        while idx < lines.len() {
            let line = lines[idx];
            if let Some(header) = line.strip_prefix('#') {
                let fields: Vec<&str> = header.split('\t').collect();
                let count: usize = fields[0].parse().unwrap();
                let keys = &fields[1..];
                idx += 1;
                out.push('[');
                for r in 0..count {
                    if r > 0 {
                        out.push(',');
                    }
                    let row: Vec<&str> = lines[idx].split('\t').collect();
                    idx += 1;
                    out.push('{');
                    match row[0] {
                        "+" => {
                            for (ki, key) in keys.iter().enumerate() {
                                if ki > 0 {
                                    out.push(',');
                                }
                                out.push_str(key);
                                out.push(':');
                                out.push_str(row[1 + ki]);
                            }
                        }
                        "*" => {
                            let m: usize = row[1].parse().unwrap();
                            for j in 0..m {
                                if j > 0 {
                                    out.push(',');
                                }
                                out.push_str(row[2 + 2 * j]);
                                out.push(':');
                                out.push_str(row[3 + 2 * j]);
                            }
                        }
                        other => panic!("unexpected row marker {other:?}"),
                    }
                    out.push('}');
                }
                out.push(']');
            } else {
                // A minified-JSON segment: copy it verbatim (it holds no newline).
                out.push_str(line);
                idx += 1;
            }
        }
        out
    }

    /// End-to-end: E2 must fire and its body must reconstruct to the canonical
    /// minified original.
    fn assert_roundtrip(input: &str) -> String {
        let body = e2(input).expect("E2 should apply");
        assert_eq!(
            reconstruct(&body),
            compact(input),
            "reconstruction mismatch for {input:?}\nbody: {body:?}"
        );
        body
    }

    #[test]
    fn homogeneous_array_compresses_and_reconstructs() {
        let input = r#"[{"id":1,"name":"a"},{"id":2,"name":"b"},{"id":3,"name":"c"}]"#;
        let body = assert_roundtrip(input);
        // Keys are hoisted once, not repeated on every element.
        assert_eq!(body.matches("\"id\"").count(), 1);
        assert_eq!(body.matches("\"name\"").count(), 1);
        // Header lists the shared keys; three plain rows follow.
        assert!(body.contains("#3\t\"id\"\t\"name\""), "body: {body:?}");
        assert_eq!(body.matches('+').count(), 3);
    }

    #[test]
    fn heterogeneous_shapes_are_annotated_and_reconstruct() {
        // Two elements share {a,b}; the third deviates (missing b, extra c).
        let input = r#"[{"a":1,"b":2},{"a":3,"b":4},{"a":5,"c":6}]"#;
        let body = assert_roundtrip(input);
        assert!(body.contains("#3\t\"a\"\t\"b\""), "body: {body:?}");
        assert_eq!(body.matches('+').count(), 2, "two plain rows");
        assert_eq!(body.matches('*').count(), 1, "one deviating row");
        // The deviating row self-describes its own keys and values.
        assert!(body.contains("*\t2\t\"a\"\t5\t\"c\"\t6"), "body: {body:?}");
    }

    #[test]
    fn array_of_scalars_is_declined() {
        assert_eq!(e2("[1,2,3]"), None);
        assert_eq!(e2(r#"["a","b","c"]"#), None);
        assert_eq!(e2("[true,false,null]"), None);
    }

    #[test]
    fn single_element_array_is_declined() {
        assert_eq!(e2(r#"[{"a":1}]"#), None);
    }

    #[test]
    fn no_array_is_declined() {
        assert_eq!(e2(r#"{"a":1,"b":2}"#), None);
        assert_eq!(e2("42"), None);
        assert_eq!(e2(r#""just a string""#), None);
    }

    #[test]
    fn all_distinct_shapes_are_declined() {
        // No shape occurs twice: nothing to hoist, so E2 declines.
        let input = r#"[{"a":1},{"b":2},{"c":3}]"#;
        assert_eq!(e2(input), None);
    }

    #[test]
    fn number_lexemes_survive_verbatim() {
        // 1.0 stays 1.0, 1e3 stays 1e3 — never round-tripped through f64.
        let input = r#"[{"x":1.0,"y":1e3},{"x":2.5e10,"y":-0}]"#;
        let body = assert_roundtrip(input);
        assert!(body.contains("1.0"), "body: {body:?}");
        assert!(body.contains("1e3"), "body: {body:?}");
        assert!(body.contains("2.5e10"), "body: {body:?}");
        assert!(body.contains("-0"), "body: {body:?}");
        // A 100-digit integer survives byte-for-byte.
        let big = "9".repeat(100);
        let input = format!(r#"[{{"n":{big}}},{{"n":{big}}}]"#);
        let body = assert_roundtrip(&input);
        assert!(body.contains(&big));
    }

    #[test]
    fn duplicate_keys_inside_element_are_preserved() {
        // Homogeneous duplicate keys: the header carries "a" twice, plain rows hold
        // both values in order.
        let input = r#"[{"a":1,"a":2},{"a":3,"a":4}]"#;
        let body = assert_roundtrip(input);
        assert!(body.contains("#2\t\"a\"\t\"a\""), "body: {body:?}");

        // Deviating case: an element with duplicate keys against a single-key header.
        let input = r#"[{"a":1},{"a":2},{"a":3,"a":4}]"#;
        let body = assert_roundtrip(input);
        assert!(body.contains("*\t2\t\"a\"\t3\t\"a\"\t4"), "body: {body:?}");
    }

    #[test]
    fn determinism_render_twice_is_byte_identical() {
        let input = r#"[{"k":"v","n":1},{"k":"w","n":2},{"k":"x","n":3}]"#;
        let a = e2(input).unwrap();
        let b = e2(input).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn nested_array_of_objects_in_a_cell_stays_compact() {
        // The outer array is tabularized; the inner arrays-of-objects sit inside
        // row cells and remain compact JSON, so rows stay single-line.
        let input = r#"[{"a":1,"items":[{"x":1},{"x":2}]},{"a":2,"items":[{"x":3},{"x":4}]}]"#;
        let body = assert_roundtrip(input);
        // Exactly one table (the outer array): one header line.
        assert_eq!(body.matches('#').count(), 1, "body: {body:?}");
        // The inner arrays are rendered inline, not hoisted.
        assert!(body.contains(r#"[{"x":1},{"x":2}]"#), "body: {body:?}");
    }

    #[test]
    fn sibling_arrays_of_objects_are_both_tabularized() {
        let input = r#"[[{"a":1},{"a":2}],[{"b":3},{"b":4}]]"#;
        let body = assert_roundtrip(input);
        // The outer array holds arrays (not objects), so it is not a table; each
        // inner array-of-objects becomes its own table block.
        assert_eq!(body.matches('#').count(), 2, "body: {body:?}");
    }

    #[test]
    fn array_of_objects_under_a_key_reconstructs() {
        let input = r#"{"results":[{"a":1},{"a":2}],"ok":true}"#;
        assert_roundtrip(input);
    }

    #[test]
    fn container_values_in_rows_reconstruct() {
        // Values that are objects/arrays render compactly as cells and round-trip.
        let input = r#"[{"a":{"p":1},"b":[1,2]},{"a":{"p":2},"b":[3,4]}]"#;
        assert_roundtrip(input);
    }
}
