//! E5 — `grid`: a tabular re-encoding with a union header, flattened sub-objects
//! and constant columns.
//!
//! # What it does
//!
//! Like [E2](super::e2_tabular), E5 replaces an array of objects with a header and
//! one row per element, so the keys are stated once instead of on every element. Its
//! syntax is lighter than E2's — a key is written without its quotes when its text
//! allows (see the grammar below), and a plain row's cells are not wrapped in
//! brackets — and it differs from E2 in three more ways, each measured on a real
//! Claude Code corpus in the local analysis of 2026-11-12 (items B1, B3 and B4
//! there):
//!
//! * **Union header (B1).** E2 hoists the single most frequent key list and writes
//!   every other element as a deviating row. E5 hoists the ordered *union* of every
//!   element's keys, so an element that merely lacks some keys is still a plain row,
//!   with `~` in the columns it lacks.
//! * **Flattened sub-objects (B3).** When every present value of a key is a non-empty
//!   object, that key does not become one column holding an object: its own keys
//!   become columns, named by a dotted path (`user.id`). This recurses for keys at
//!   the first [`MAX_FLATTEN_LEVEL`] levels of the element.
//! * **Constant columns (B4).** A column whose cell is the same in every plain row
//!   is written once, in the header, as `path=<cell>`, and dropped from the rows.
//!
//! Only the *outermost* qualifying array on any path becomes a grid: a cell is
//! compact JSON and never contains a grid of its own. Everything outside a grid is
//! structural-minified JSON with every lexeme copied **verbatim** from its source
//! span, exactly as in E2.
//!
//! # Relation to `never_compress`
//!
//! E5 does **not** consult [`never_compress`](crate::never_compress), and what keeps a
//! protected *value* faithful is the same as in E2: every value lexeme is copied
//! byte-for-byte, and rows are emitted in source order with no dedup and no
//! reordering. E5 moves more than E2 does, though. A constant column's value is
//! written once, in the header, instead of at each row, and a flattened key is
//! written once, as a path segment. A protected string among those values or keys
//! therefore appears once, at the header. The verbatim-with-position question E2's
//! docs record for hoisted keys applies here to constant values too, and is not
//! settled here either. `never_compress` is a fidelity safeguard; nothing in this
//! module treats it as anything else.
//!
//! # Body grammar (what [`render`] returns; the caller adds the `grid` sentinel)
//!
//! The body is structural-minified JSON in which each grid replaces its array, at
//! the array's value position, with a newline-introduced block. Minified JSON holds
//! no raw newline, so a newline is unambiguously a block or row boundary:
//!
//! ```text
//! \n
//! #<N>[<col_0>,<col_1>,…]\n   header: element count, then the leaf columns in order
//! <row>\n                     exactly N rows follow
//! …
//! ```
//!
//! After the `N`-th row the surrounding JSON resumes; the count is the block's only
//! terminator, as in E2.
//!
//! A column is `<path>` or `<path>=<cell>`. A path is one or more segments joined by
//! `.`. A segment is a key's lexeme: written bare when the text between its quotes
//! matches `[A-Za-z_][A-Za-z0-9_-]*` — no escape can occur in such a text, so the
//! quotes restore the lexeme byte for byte — and as the verbatim quoted lexeme
//! otherwise. A bare segment holds neither `.`, `=`, `,` nor a quote, so a path ends
//! at the first of `=`, `,` or `]` outside a quoted segment. `=<cell>` marks a
//! constant column; `=~` is a column absent from every plain row.
//!
//! A cell is the compact JSON of a value (verbatim lexemes) or `~` for an absent key.
//! `~` cannot begin a JSON value, so it is unambiguous.
//!
//! Each row is one of:
//!
//! * **plain** — `+<cell>,<cell>,…`: one cell per non-constant column, in header
//!   order, separated by `,` at the top level. A row whose every column is constant
//!   is the bare `+`.
//! * **deviating** — `*<object>`: the element as compact JSON. An element deviates
//!   when its keys do not appear in strictly increasing header order at some level —
//!   a duplicated key, or two keys in the opposite order to the union's — and that
//!   is the only reason one does.
//!
//! # Reversibility
//!
//! From the body alone a reader rebuilds the array: for each plain row, take each
//! column's cell (from the header when constant), drop the `~` ones, and nest the
//! rest by path, keeping first-seen order. A flattened key whose every leaf is `~`
//! is absent: a flattened key's present value always has at least one member, so an
//! all-`~` subtree can only mean the key was missing. Key order is preserved because
//! a plain row's keys are, by definition, in header order; duplicate keys force a
//! deviating row, which is written verbatim. The test module's reference decoder
//! does exactly this and is held to the minified input byte for byte.
//!
//! # Determinism
//!
//! The union is built with an `FxHashMap` keyed by the full key lexeme — unlike E2's
//! shape table, the lookup compares the key text, so a hash collision cannot merge two
//! keys. The map is only ever probed, never iterated: the column order comes from a
//! linked list threaded through the union in insertion order, and rows are emitted
//! in source order. Identical input therefore yields identical output.
//!
//! The union order: for each element in source order, walk its keys with a cursor
//! that starts before the first column. A key already in the union moves the cursor
//! to it; a new key is inserted right after the cursor and becomes the cursor.
//!
//! # Size bound
//!
//! A plain row writes a cell for every non-constant column, present or not, so a
//! sparse array — every element carrying a key no other element has — would cost
//! rows × columns bytes for an input of rows + columns. E5 declines an array when
//! its plain rows times its leaf columns exceed the array's own source bytes, which
//! keeps every grid linear in the input. The check runs before any cell is built.
//!
//! # When it declines (returns `None`)
//!
//! When no array qualifies. An array qualifies when it has ≥ 2 elements, all objects,
//! whose key union is not empty, with ≥ 2 plain rows, and is within the size bound.
//! `None` also comes from defensive guards — a node index out of bounds, an index
//! step that overflows, a span that does not resolve, a member position whose node is
//! not a `Key`, a key that is missing from a union built from the same keys, a
//! flattened value that is not an object — which hold for a tape and the input it was
//! parsed from, and exist so that a tape paired with another input degrades to
//! passthrough rather than to a malformed body.
//!
//! Whether the grid is shorter than the input, or than E2's table, is decided
//! afterwards by [`select`](super::select), as for every encoder.

use rustc_hash::FxHashMap;

use super::e2_tabular::{RenderFrame, emit_compact, open_key, open_value, push_usize, span_str};
use crate::tape::{Node, NodeKind, Tape};

/// Block and row boundary.
const BLOCK: char = '\n';
/// Header marker.
const HEADER: char = '#';
/// Plain-row marker.
const PLAIN: char = '+';
/// Deviating-row marker.
const DEVIATED: char = '*';
/// An absent key's cell.
const ABSENT: &str = "~";
/// Separates a constant column's path from its cell.
const CONSTANT: char = '=';
/// Joins the segments of a flattened path.
const PATH_SEP: char = '.';
/// Keys at element levels `0..MAX_FLATTEN_LEVEL` may be flattened, so a path has at
/// most `MAX_FLATTEN_LEVEL + 1` segments. The value the local analysis measured.
const MAX_FLATTEN_LEVEL: usize = 3;

/// Render the tape as an E5 body, or `None` when no array becomes a grid.
// `pub(crate)` is the contract the sealed-encoder design specifies for this entry
// point; it is stated explicitly rather than inferred from the private module.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn render(tape: &Tape, input: &str) -> Option<String> {
    let nodes = tape.nodes();
    let ends = matching_ends(nodes)?;
    let ctx = Ctx {
        nodes,
        input,
        ends: &ends,
    };

    let mut out = String::with_capacity(input.len());
    let mut frames: Vec<RenderFrame> = Vec::new();
    let mut emitted_grid = false;
    let mut i = 0usize;

    while i < nodes.len() {
        let node = *nodes.get(i)?;
        match node.kind {
            NodeKind::ArrayStart { .. } => {
                if let Planned::Grid(plan) = plan_grid(&ctx, i)? {
                    open_value(&mut frames, &mut out);
                    emit_grid(&ctx, &plan, &mut out)?;
                    emitted_grid = true;
                    i = ctx.end_of(i)?.checked_add(1)?;
                    continue;
                }
                open_value(&mut frames, &mut out);
                out.push('[');
                frames.push(RenderFrame::array());
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
            NodeKind::ArrayEnd => {
                out.push(']');
                frames.pop();
            }
        }
        i = i.checked_add(1)?;
    }

    // A body with no grid in it is a plain minification, which is E1's job; paying
    // for a `grid` sentinel on it would only cost tokens.
    if emitted_grid { Some(out) } else { None }
}

// ---------------------------------------------------------------------------
// Tape access.
// ---------------------------------------------------------------------------

/// The tape, its source, and each node's matching end, shared by every helper.
struct Ctx<'a> {
    nodes: &'a [Node],
    input: &'a str,
    /// `ends[i]` is the index of the node closing the container opened at `i`, and
    /// `i` itself for every other node.
    ends: &'a [usize],
}

impl<'a> Ctx<'a> {
    /// The last node of the value starting at `v`, or `None` for a key or an index
    /// out of bounds.
    fn end_of(&self, v: usize) -> Option<usize> {
        if matches!(self.nodes.get(v)?.kind, NodeKind::Key) {
            return None;
        }
        self.ends.get(v).copied()
    }

    /// The source text of node `i`.
    fn text(&self, i: usize) -> Option<&'a str> {
        span_str(self.input, self.nodes.get(i)?.span)
    }

    /// The members of the object opened at `obj`, as `(key node, value start, value
    /// end)` triples in source order.
    fn members(&self, obj: usize) -> Option<Vec<(usize, usize, usize)>> {
        let end = self.end_of(obj)?;
        let mut list = Vec::new();
        let mut j = obj.checked_add(1)?;
        while j < end {
            if !matches!(self.nodes.get(j)?.kind, NodeKind::Key) {
                return None;
            }
            let value = j.checked_add(1)?;
            let value_end = self.end_of(value)?;
            list.push((j, value, value_end));
            j = value_end.checked_add(1)?;
        }
        Some(list)
    }

    /// The element start indices of the array opened at `arr`.
    fn elements(&self, arr: usize) -> Option<Vec<usize>> {
        let end = self.end_of(arr)?;
        let mut list = Vec::new();
        let mut v = arr.checked_add(1)?;
        while v < end {
            list.push(v);
            v = self.end_of(v)?.checked_add(1)?;
        }
        Some(list)
    }

    /// Whether node `i` opens an object with at least one member.
    fn is_non_empty_object(&self, i: usize) -> Option<bool> {
        Some(
            matches!(self.nodes.get(i)?.kind, NodeKind::ObjectStart { .. })
                && self.end_of(i)? > i.checked_add(1)?,
        )
    }
}

/// Each node's matching end, computed once per render so that finding the end of a
/// value is a lookup rather than a scan.
fn matching_ends(nodes: &[Node]) -> Option<Vec<usize>> {
    let mut ends: Vec<usize> = (0..nodes.len()).collect();
    let mut open: Vec<usize> = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        match node.kind {
            NodeKind::ObjectStart { .. } | NodeKind::ArrayStart { .. } => open.push(i),
            NodeKind::ObjectEnd | NodeKind::ArrayEnd => {
                let start = open.pop()?;
                *ends.get_mut(start)? = i;
            }
            _ => {}
        }
    }
    if open.is_empty() { Some(ends) } else { None }
}

// ---------------------------------------------------------------------------
// Planning: the column tree.
// ---------------------------------------------------------------------------

/// One level of the column tree: the union of the keys of a set of objects.
struct Level<'a> {
    columns: Vec<Column<'a>>,
    /// Key lexeme to its position in `columns`. Probed, never iterated.
    rank: FxHashMap<&'a str, usize>,
}

/// One key of a level: a leaf column, or a flattened key with a level of its own.
struct Column<'a> {
    /// The key's lexeme, quotes included.
    key: &'a str,
    /// The flattened key's own keys, or `None` for a leaf.
    children: Option<Level<'a>>,
    /// Index of this key's first leaf column (its own, for a leaf).
    first_leaf: usize,
}

/// Build the level for the objects opened at `objects`, numbering its leaf columns
/// from `*next_leaf`.
fn build_level<'a>(
    ctx: &Ctx<'a>,
    objects: &[usize],
    level: usize,
    next_leaf: &mut usize,
) -> Option<Level<'a>> {
    // The union as a linked list through `keys`/`next`; slot 0 is the head.
    const NIL: usize = usize::MAX;
    let mut keys: Vec<&'a str> = vec![""];
    let mut next: Vec<usize> = vec![NIL];
    let mut values: Vec<Vec<usize>> = vec![Vec::new()];
    let mut slot_of: FxHashMap<&'a str, usize> = FxHashMap::default();

    for &obj in objects {
        let mut cursor = 0usize;
        for (key, value, _) in ctx.members(obj)? {
            let text = ctx.text(key)?;
            let slot = if let Some(&slot) = slot_of.get(text) {
                slot
            } else {
                let slot = keys.len();
                keys.push(text);
                next.push(*next.get(cursor)?);
                values.push(Vec::new());
                *next.get_mut(cursor)? = slot;
                slot_of.insert(text, slot);
                slot
            };
            values.get_mut(slot)?.push(value);
            cursor = slot;
        }
    }

    let mut columns: Vec<Column<'a>> = Vec::new();
    let mut rank: FxHashMap<&'a str, usize> = FxHashMap::default();
    let mut slot = *next.first()?;
    while slot != NIL {
        let key = *keys.get(slot)?;
        let present = values.get(slot)?;
        let first_leaf = *next_leaf;
        let mut flatten = level < MAX_FLATTEN_LEVEL;
        if flatten {
            for &v in present {
                if !ctx.is_non_empty_object(v)? {
                    flatten = false;
                    break;
                }
            }
        }
        let children = if flatten {
            Some(build_level(ctx, present, level.checked_add(1)?, next_leaf)?)
        } else {
            *next_leaf = next_leaf.checked_add(1)?;
            None
        };
        rank.insert(key, columns.len());
        columns.push(Column {
            key,
            children,
            first_leaf,
        });
        slot = *next.get(slot)?;
    }
    Some(Level { columns, rank })
}

/// A grid ready to emit.
struct GridPlan<'a> {
    elements: Vec<usize>,
    root: Level<'a>,
    leaves: usize,
    /// Per element: whether it is a plain row.
    plain: Vec<bool>,
}

/// What [`plan_grid`] decided about an array that no defensive guard stopped.
enum Planned<'a> {
    /// The array becomes a grid.
    Grid(GridPlan<'a>),
    /// The array does not qualify and is rendered as minified JSON.
    Declined,
}

/// Plan the grid for the array opened at `arr`: [`Planned::Declined`] when the array
/// does not qualify, `None` from a defensive guard.
fn plan_grid<'a>(ctx: &Ctx<'a>, arr: usize) -> Option<Planned<'a>> {
    let elements = ctx.elements(arr)?;
    if elements.len() < 2 {
        return Some(Planned::Declined);
    }
    for &e in &elements {
        if !matches!(ctx.nodes.get(e)?.kind, NodeKind::ObjectStart { .. }) {
            return Some(Planned::Declined);
        }
    }
    let mut leaves = 0usize;
    let root = build_level(ctx, &elements, 0, &mut leaves)?;
    if leaves == 0 {
        return Some(Planned::Declined);
    }

    let mut plain = Vec::with_capacity(elements.len());
    let mut plain_rows = 0usize;
    for &e in &elements {
        let is_plain = in_header_order(ctx, e, &root)?;
        if is_plain {
            plain_rows = plain_rows.checked_add(1)?;
        }
        plain.push(is_plain);
    }
    if plain_rows < 2 {
        return Some(Planned::Declined);
    }

    let source = ctx.nodes.get(ctx.end_of(arr)?)?.span.end;
    let source = source.checked_sub(ctx.nodes.get(arr)?.span.start)? as usize;
    match plain_rows.checked_mul(leaves) {
        Some(cells) if cells <= source => {}
        _ => return Some(Planned::Declined),
    }

    Some(Planned::Grid(GridPlan {
        elements,
        root,
        leaves,
        plain,
    }))
}

/// Whether the object at `obj` lists its keys in strictly increasing column order at
/// this level and, recursively, inside every flattened key.
fn in_header_order(ctx: &Ctx<'_>, obj: usize, level: &Level<'_>) -> Option<bool> {
    let mut last: Option<usize> = None;
    for (key, value, _) in ctx.members(obj)? {
        let rank = *level.rank.get(ctx.text(key)?)?;
        if last.is_some_and(|l| rank <= l) {
            return Some(false);
        }
        last = Some(rank);
        if let Some(sub) = &level.columns.get(rank)?.children {
            if !in_header_order(ctx, value, sub)? {
                return Some(false);
            }
        }
    }
    Some(true)
}

// ---------------------------------------------------------------------------
// Emission.
// ---------------------------------------------------------------------------

/// The plain rows' cells: `(leaf column, start, end)` into one text buffer, sparse
/// (absent keys hold no entry) and in column order within each row.
struct Cells {
    text: String,
    spans: Vec<(usize, usize, usize)>,
    /// Per plain row, its range in `spans`.
    rows: Vec<(usize, usize)>,
}

impl Cells {
    fn get(&self, cell: (usize, usize, usize)) -> Option<&str> {
        self.text.get(cell.1..cell.2)
    }
}

/// Append the cells of a plain row's object at `obj`, at `level`.
fn collect_cells(ctx: &Ctx<'_>, obj: usize, level: &Level<'_>, cells: &mut Cells) -> Option<()> {
    for (key, value, value_end) in ctx.members(obj)? {
        let column = level.columns.get(*level.rank.get(ctx.text(key)?)?)?;
        if let Some(sub) = &column.children {
            collect_cells(ctx, value, sub, cells)?;
        } else {
            let start = cells.text.len();
            emit_compact(ctx.nodes, ctx.input, &mut cells.text, value, value_end)?;
            cells
                .spans
                .push((column.first_leaf, start, cells.text.len()));
        }
    }
    Some(())
}

/// What the header says about one leaf column.
enum Constancy {
    /// The column varies; each plain row carries its cell.
    Varies,
    /// Absent from every plain row.
    Absent,
    /// The same cell in every plain row: the `(leaf, start, end)` of its first one.
    Value((usize, usize, usize)),
}

/// Classify every leaf column over the plain rows.
fn constancy(cells: &Cells, leaves: usize) -> Option<Vec<Constancy>> {
    let mut count = vec![0usize; leaves];
    let mut first: Vec<Option<(usize, usize, usize)>> = vec![None; leaves];
    let mut varies = vec![false; leaves];
    for &cell in &cells.spans {
        let leaf = cell.0;
        *count.get_mut(leaf)? += 1;
        match *first.get(leaf)? {
            None => *first.get_mut(leaf)? = Some(cell),
            Some(seen) => {
                if cells.get(seen)? != cells.get(cell)? {
                    *varies.get_mut(leaf)? = true;
                }
            }
        }
    }
    let rows = cells.rows.len();
    let mut out = Vec::with_capacity(leaves);
    for leaf in 0..leaves {
        let n = *count.get(leaf)?;
        out.push(if n == 0 {
            Constancy::Absent
        } else if n == rows && !*varies.get(leaf)? {
            Constancy::Value((*first.get(leaf)?)?)
        } else {
            Constancy::Varies
        });
    }
    Some(out)
}

/// Emit the block for a planned grid.
fn emit_grid(ctx: &Ctx<'_>, plan: &GridPlan<'_>, out: &mut String) -> Option<()> {
    let mut cells = Cells {
        text: String::new(),
        spans: Vec::new(),
        rows: Vec::new(),
    };
    for (&e, &is_plain) in plan.elements.iter().zip(&plan.plain) {
        if is_plain {
            let start = cells.spans.len();
            collect_cells(ctx, e, &plan.root, &mut cells)?;
            cells.rows.push((start, cells.spans.len()));
        }
    }
    let constancy = constancy(&cells, plan.leaves)?;

    out.push(BLOCK);
    out.push(HEADER);
    push_usize(out, plan.elements.len());
    out.push('[');
    let mut path: Vec<&str> = Vec::new();
    let mut first = true;
    emit_columns(&plan.root, &mut path, &constancy, &cells, &mut first, out)?;
    out.push(']');
    out.push(BLOCK);

    let mut rows = cells.rows.iter();
    for (&e, &is_plain) in plan.elements.iter().zip(&plan.plain) {
        if is_plain {
            let &(lo, hi) = rows.next()?;
            out.push(PLAIN);
            let mut present = cells.spans.get(lo..hi)?.iter().peekable();
            let mut sep = false;
            for (leaf, kind) in constancy.iter().enumerate() {
                let cell = present.next_if(|c| c.0 == leaf);
                if !matches!(kind, Constancy::Varies) {
                    continue;
                }
                if sep {
                    out.push(',');
                }
                sep = true;
                match cell {
                    Some(&c) => out.push_str(cells.get(c)?),
                    None => out.push_str(ABSENT),
                }
            }
        } else {
            out.push(DEVIATED);
            emit_compact(ctx.nodes, ctx.input, out, e, ctx.end_of(e)?)?;
        }
        out.push(BLOCK);
    }
    Some(())
}

/// Emit the header columns of `level` in depth-first order, each as its path and,
/// when constant, `=` and its cell.
fn emit_columns<'a>(
    level: &Level<'a>,
    path: &mut Vec<&'a str>,
    constancy: &[Constancy],
    cells: &Cells,
    first: &mut bool,
    out: &mut String,
) -> Option<()> {
    for column in &level.columns {
        path.push(column.key);
        if let Some(sub) = &column.children {
            emit_columns(sub, path, constancy, cells, first, out)?;
        } else {
            if !*first {
                out.push(',');
            }
            *first = false;
            for (i, segment) in path.iter().enumerate() {
                if i > 0 {
                    out.push(PATH_SEP);
                }
                push_segment(segment, out);
            }
            match constancy.get(column.first_leaf)? {
                Constancy::Varies => {}
                Constancy::Absent => {
                    out.push(CONSTANT);
                    out.push_str(ABSENT);
                }
                Constancy::Value(cell) => {
                    out.push(CONSTANT);
                    out.push_str(cells.get(*cell)?);
                }
            }
        }
        path.pop();
    }
    Some(())
}

/// Write a key lexeme as a path segment: bare when its text is an identifier, the
/// lexeme verbatim otherwise.
fn push_segment(lexeme: &str, out: &mut String) {
    match lexeme
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        Some(inner) if is_bare(inner) => out.push_str(inner),
        _ => out.push_str(lexeme),
    }
}

/// Whether `text` matches `[A-Za-z_][A-Za-z0-9_-]*`.
fn is_bare(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
#[path = "../../tests/support/grid.rs"]
mod grid_reference;

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::too_many_lines
    )]

    use proptest::collection::vec as prop_vec;
    use proptest::prelude::{Strategy, proptest};
    use proptest::sample::select;

    use super::grid_reference::decode_grid_body;
    use super::{is_bare, render};
    use crate::encoder::e2_tabular::emit_compact;
    use crate::tape::{self, Tape};

    fn tape_of(input: &str) -> Tape {
        tape::parse(input, 512).unwrap()
    }

    fn e5(input: &str) -> Option<String> {
        render(&tape_of(input), input)
    }

    /// The whole document as minified JSON with verbatim lexemes.
    fn compact(input: &str) -> String {
        let t = tape_of(input);
        let nodes = t.nodes();
        let mut out = String::new();
        emit_compact(nodes, input, &mut out, 0, nodes.len() - 1).unwrap();
        out
    }

    /// E5 must fire, and its body must decode to the minified input byte for byte.
    fn assert_roundtrip(input: &str) -> String {
        let body = e5(input).expect("E5 should apply");
        assert_eq!(
            decode_grid_body(&body),
            compact(input),
            "reconstruction mismatch for {input:?}\nbody: {body:?}"
        );
        body
    }

    #[test]
    fn a_homogeneous_array_is_one_header_and_bare_rows() {
        let body = assert_roundtrip(r#"[{"id":1,"name":"a"},{"id":2,"name":"b"}]"#);
        assert_eq!(body, "\n#2[id,name]\n+1,\"a\"\n+2,\"b\"\n");
    }

    #[test]
    fn a_missing_key_is_a_tilde_in_a_plain_row() {
        let body = assert_roundtrip(r#"[{"a":1,"b":2},{"a":3},{"b":4}]"#);
        assert_eq!(body, "\n#3[a,b]\n+1,2\n+3,~\n+~,4\n");
    }

    #[test]
    fn the_union_inserts_a_new_key_after_the_last_key_seen() {
        // Element 1 puts `c` between `a` and `b`: the cursor is on `a` when `c` is
        // new. Element 2 then adds `d` after `b`, the last key it saw.
        let body = assert_roundtrip(r#"[{"a":1,"b":2},{"a":3,"c":4,"b":5},{"b":6,"d":7}]"#);
        assert_eq!(body, "\n#3[a,c,b,d]\n+1,~,2,~\n+3,4,5,~\n+~,~,6,7\n");
    }

    #[test]
    fn keys_against_the_union_order_make_a_deviating_row() {
        let body = assert_roundtrip(r#"[{"a":1,"b":2},{"a":3,"b":4},{"b":5,"a":6}]"#);
        assert_eq!(body, "\n#3[a,b]\n+1,2\n+3,4\n*{\"b\":5,\"a\":6}\n");
    }

    #[test]
    fn a_duplicate_key_makes_a_deviating_row() {
        let body = assert_roundtrip(r#"[{"a":1,"b":2},{"a":3,"b":4},{"a":5,"a":6}]"#);
        assert_eq!(body, "\n#3[a,b]\n+1,2\n+3,4\n*{\"a\":5,\"a\":6}\n");
    }

    #[test]
    fn a_key_whose_values_are_all_objects_is_flattened_into_paths() {
        let body =
            assert_roundtrip(r#"[{"id":1,"u":{"n":"x","r":1}},{"id":2,"u":{"n":"y"}},{"id":3}]"#);
        assert_eq!(body, "\n#3[id,u.n,u.r]\n+1,\"x\",1\n+2,\"y\",~\n+3,~,~\n");
    }

    #[test]
    fn a_key_with_an_empty_or_scalar_value_is_not_flattened() {
        let body = assert_roundtrip(r#"[{"u":{"n":1}},{"u":{}},{"u":{"n":2}}]"#);
        assert_eq!(body, "\n#3[u]\n+{\"n\":1}\n+{}\n+{\"n\":2}\n");
        let body = assert_roundtrip(r#"[{"u":{"n":1}},{"u":7}]"#);
        assert_eq!(body, "\n#2[u]\n+{\"n\":1}\n+7\n");
    }

    #[test]
    fn flattening_stops_after_three_levels() {
        let body = assert_roundtrip(
            r#"[{"a":{"b":{"c":{"d":{"e":1}}}}},{"a":{"b":{"c":{"d":{"e":2}}}}}]"#,
        );
        assert_eq!(body, "\n#2[a.b.c.d]\n+{\"e\":1}\n+{\"e\":2}\n");
    }

    #[test]
    fn a_constant_column_moves_into_the_header() {
        let body = assert_roundtrip(r#"[{"t":"x","v":1},{"t":"x","v":2},{"t":"x","v":3}]"#);
        assert_eq!(body, "\n#3[t=\"x\",v]\n+1\n+2\n+3\n");
    }

    #[test]
    fn a_column_present_only_in_deviating_rows_is_constant_absent() {
        // `b` occurs only in the third element, which deviates on its duplicate `a`.
        let body = assert_roundtrip(r#"[{"a":1},{"a":2},{"a":3,"b":4,"a":5}]"#);
        assert_eq!(body, "\n#3[a,b=~]\n+1\n+2\n*{\"a\":3,\"b\":4,\"a\":5}\n");
    }

    #[test]
    fn a_row_whose_every_column_is_constant_is_a_bare_marker() {
        let body = assert_roundtrip(r#"[{"a":1},{"a":1},{"a":2,"a":3}]"#);
        assert_eq!(body, "\n#3[a=1]\n+\n+\n*{\"a\":2,\"a\":3}\n");
    }

    #[test]
    fn a_column_present_in_some_rows_with_one_value_is_not_constant() {
        let body = assert_roundtrip(r#"[{"a":1,"b":9},{"a":2},{"a":3,"b":9}]"#);
        assert_eq!(body, "\n#3[a,b]\n+1,9\n+2,~\n+3,9\n");
    }

    #[test]
    fn a_key_that_is_not_an_identifier_keeps_its_quotes() {
        let body = assert_roundtrip(
            r#"[{"a.b":1,"x y":2,"\u0061":3,"":4,"9":5},{"a.b":6,"x y":7,"\u0061":8,"":9,"9":0}]"#,
        );
        assert_eq!(
            body,
            "\n#2[\"a.b\",\"x y\",\"\\u0061\",\"\",\"9\"]\n+1,2,3,4,5\n+6,7,8,9,0\n"
        );
    }

    #[test]
    fn a_quoted_segment_survives_inside_a_flattened_path() {
        let body = assert_roundtrip(r#"[{"u":{"=":1,",":2}},{"u":{"=":3,",":4}}]"#);
        assert_eq!(body, "\n#2[u.\"=\",u.\",\"]\n+1,2\n+3,4\n");
    }

    #[test]
    fn cells_keep_nested_json_and_lexemes_verbatim() {
        let input = r#"[ {"a": [1, {"b": "x,y]"}], "n": 1.50e+3}, {"a": [], "n": -0} ]"#;
        let body = assert_roundtrip(input);
        assert_eq!(body, "\n#2[a,n]\n+[1,{\"b\":\"x,y]\"}],1.50e+3\n+[],-0\n");
    }

    #[test]
    fn the_grid_sits_at_its_value_position_inside_surrounding_json() {
        let body = assert_roundtrip(r#"{"k":[{"a":1},{"a":2}],"z":[{"b":1},{"b":2}],"w":0}"#);
        assert_eq!(
            body,
            "{\"k\":\n#2[a]\n+1\n+2\n,\"z\":\n#2[b]\n+1\n+2\n,\"w\":0}"
        );
    }

    #[test]
    fn only_the_outermost_qualifying_array_becomes_a_grid() {
        let body = assert_roundtrip(r#"[{"r":[{"x":1},{"x":2}]},{"r":[{"x":3},{"x":4}]}]"#);
        assert_eq!(
            body,
            "\n#2[r]\n+[{\"x\":1},{\"x\":2}]\n+[{\"x\":3},{\"x\":4}]\n"
        );
    }

    #[test]
    fn an_inner_array_becomes_a_grid_when_the_outer_one_does_not_qualify() {
        let body = assert_roundtrip(r#"[[{"x":1},{"x":2}],[{"x":3},{"x":4}]]"#);
        assert_eq!(body, "[\n#2[x]\n+1\n+2\n,\n#2[x]\n+3\n+4\n]");
    }

    #[test]
    fn it_declines_what_does_not_qualify() {
        for input in [
            r"[1,2,3]",
            r#"[{"a":1}]"#,
            r#"[{"a":1},2]"#,
            r"[{},{}]",
            r#"{"a":1}"#,
            r#""s""#,
            // One plain row only: the other two deviate.
            r#"[{"a":1,"b":2},{"b":3,"a":4},{"a":5,"a":6}]"#,
        ] {
            assert_eq!(e5(input), None, "{input}");
        }
    }

    #[test]
    fn it_declines_a_grid_larger_than_its_array() {
        // Every element has its own key: 4 plain rows x 4 columns = 16 cells.
        let sparse = r#"[{"a":1},{"b":1},{"c":1},{"d":1}]"#;
        assert!(sparse.len() >= 16, "the bound would not bind");
        // The bound is on rows x columns against the array's source bytes, so make
        // the source too short for it: one-letter keys, 26 elements.
        let keys: Vec<String> = ('a'..='z').map(|c| format!("{{\"{c}\":1}}")).collect();
        let wide = format!("[{}]", keys.join(","));
        assert!(26 * 26 > wide.len());
        assert_eq!(e5(&wide), None);
        // The same shape under the bound still renders.
        assert_roundtrip(sparse);
    }

    #[test]
    fn the_bound_counts_the_array_bytes_not_the_document() {
        // 26 x 26 = 676 cells: padding outside the array must not lift the bound.
        let keys: Vec<String> = ('a'..='z').map(|c| format!("{{\"{c}\":1}}")).collect();
        let doc = format!(
            "{{\"pad\":\"{}\",\"x\":[{}]}}",
            "p".repeat(2000),
            keys.join(",")
        );
        assert_eq!(e5(&doc), None);
    }

    #[test]
    fn a_bare_segment_is_an_identifier_and_nothing_else() {
        for ok in ["a", "_", "A_b-9", "z-"] {
            assert!(is_bare(ok), "{ok}");
        }
        for no in [
            "", "9", "-a", "a.b", "a=b", "a b", "a,b", "\\u0061", "é", "a\"b",
        ] {
            assert!(!is_bare(no), "{no}");
        }
    }

    #[test]
    fn identical_input_renders_identical_bytes() {
        let input = r#"[{"b":{"x":1},"a":2},{"a":3,"c":[1]},{"c":null,"b":{"y":2}}]"#;
        assert_eq!(e5(input), e5(input));
        assert_roundtrip(input);
    }

    /// A random array of objects over a small key and value alphabet, so keys
    /// collide, repeat, go missing, nest and reorder.
    fn arb_value(depth: u32) -> impl Strategy<Value = String> {
        let leaf = select(vec![
            "1".to_owned(),
            "-0".to_owned(),
            "2.5e3".to_owned(),
            "true".to_owned(),
            "null".to_owned(),
            "\"x\"".to_owned(),
            "\"a,b]\"".to_owned(),
            "\"~\"".to_owned(),
            "[]".to_owned(),
            "{}".to_owned(),
        ]);
        leaf.prop_recursive(depth, 24, 4, arb_object)
    }

    /// An object over a five-key alphabet, two of whose keys are not identifiers.
    fn arb_object(value: impl Strategy<Value = String>) -> impl Strategy<Value = String> {
        let keys = select(vec!["\"a\"", "\"b\"", "\"c\"", "\"a.b\"", "\"\\u0061\""]);
        prop_vec((keys, value), 0..4).prop_map(|members| {
            let body: Vec<String> = members
                .into_iter()
                .map(|(k, v)| format!("{k}:{v}"))
                .collect();
            format!("{{{}}}", body.join(","))
        })
    }

    proptest! {
        #[test]
        fn every_rendered_body_decodes_to_the_minified_input(
            elements in prop_vec(arb_object(arb_value(3)), 0..8),
            wrap in proptest::bool::ANY,
        ) {
            let array = format!("[{}]", elements.join(" , "));
            let input = if wrap { format!("{{\"k\": {array}, \"z\": {array}}}") } else { array };
            if let Some(body) = e5(&input) {
                assert_eq!(decode_grid_body(&body), compact(&input), "body: {body:?}");
            }
        }
    }
}
