//! A strict reference decoder for the E5 `grid` body grammar, shared by the encoder's
//! unit tests and the golden-file integration tests.
//!
//! It rebuilds the structural-minified JSON a body was rendered from, byte for byte,
//! from the body alone: nothing here reads the tape or the input. It is written from
//! the grammar in the `e5_grid` module docs rather than from the encoder, and it
//! panics on anything the grammar does not allow, so a body that only *happens* to
//! decode cannot pass.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    dead_code
)]

/// Decode a full rendering: the `#tkfd1 grid` sentinel line, then the body.
pub fn decode_grid_rendering(rendering: &str) -> String {
    let body = rendering
        .strip_prefix("#tkfd1 grid\n")
        .unwrap_or_else(|| panic!("not a grid rendering: {rendering:?}"));
    decode_grid_body(body)
}

/// Decode an E5 body (no sentinel line) into minified JSON.
pub fn decode_grid_body(body: &str) -> String {
    let lines: Vec<&str> = body.split('\n').collect();
    let mut out = String::new();
    assert_json_text(lines[0]);
    out.push_str(lines[0]);
    let mut i = 1;
    let mut blocks = 0;
    while i < lines.len() {
        let header = lines[i];
        let (count, columns) = parse_header(header);
        let rows = lines
            .get(i + 1..i + 1 + count)
            .unwrap_or_else(|| panic!("block {header:?} wants {count} rows: {body:?}"));
        out.push('[');
        for (n, row) in rows.iter().enumerate() {
            if n > 0 {
                out.push(',');
            }
            out.push_str(&decode_row(row, &columns));
        }
        out.push(']');
        let text = lines
            .get(i + 1 + count)
            .unwrap_or_else(|| panic!("no text after block {header:?}: {body:?}"));
        assert_json_text(text);
        out.push_str(text);
        i += count + 2;
        blocks += 1;
    }
    assert!(blocks > 0, "a grid body holds at least one block: {body:?}");
    out
}

/// A line of surrounding JSON must not look like a block line, or a miscounted row
/// would be read as text.
fn assert_json_text(text: &str) {
    assert!(
        !text.starts_with(['#', '+', '*']),
        "a block line where JSON text belongs: {text:?}"
    );
}

/// One header column: its path as key lexemes (quotes included), and its constant
/// cell when it has one (`Some("~")` for constant-absent).
struct Column {
    path: Vec<String>,
    constant: Option<String>,
}

fn parse_header(line: &str) -> (usize, Vec<Column>) {
    let rest = line
        .strip_prefix('#')
        .unwrap_or_else(|| panic!("not a header: {line:?}"));
    let open = rest.find('[').expect("a header column list");
    let digits = &rest[..open];
    assert!(
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        "bad row count in {line:?}"
    );
    assert!(
        digits == "0" || !digits.starts_with('0'),
        "row count with a leading zero in {line:?}"
    );
    let count: usize = digits.parse().unwrap();
    let inner = rest[open + 1..]
        .strip_suffix(']')
        .unwrap_or_else(|| panic!("unterminated header {line:?}"));
    let columns: Vec<Column> = split_top_level(inner)
        .into_iter()
        .map(parse_column)
        .collect();
    assert!(!columns.is_empty(), "a header with no columns: {line:?}");
    (count, columns)
}

fn parse_column(text: &str) -> Column {
    let bytes = text.as_bytes();
    let mut path = Vec::new();
    let mut i = 0;
    loop {
        let start = i;
        if bytes.get(i) == Some(&b'"') {
            i = string_end(bytes, i);
            path.push(text[start..i].to_owned());
        } else {
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-')
            {
                i += 1;
            }
            let segment = &text[start..i];
            assert!(
                segment
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_'),
                "bad bare segment in column {text:?}"
            );
            path.push(format!("\"{segment}\""));
        }
        match bytes.get(i) {
            Some(b'.') => i += 1,
            Some(b'=') => {
                let cell = &text[i + 1..];
                assert!(!cell.is_empty(), "empty constant in column {text:?}");
                return Column {
                    path,
                    constant: Some(cell.to_owned()),
                };
            }
            None => {
                return Column {
                    path,
                    constant: None,
                };
            }
            Some(_) => panic!("unexpected byte after a segment in column {text:?}"),
        }
    }
}

/// The index one past the closing quote of the string opening at `start`.
fn string_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    loop {
        match bytes.get(i) {
            Some(b'\\') => i += 2,
            Some(b'"') => return i + 1,
            Some(_) => i += 1,
            None => panic!("unterminated string"),
        }
    }
}

/// Split at commas outside strings and brackets.
fn split_top_level(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i = string_end(bytes, i);
                continue;
            }
            b'[' | b'{' => depth += 1,
            b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&s[start..]);
    parts
}

/// A rebuilt object, members in first-seen order.
enum Value {
    Leaf(String),
    Object(Vec<(String, Self)>),
}

fn decode_row(row: &str, columns: &[Column]) -> String {
    if let Some(element) = row.strip_prefix('*') {
        assert!(
            element.starts_with('{') && element.ends_with('}'),
            "a deviating row is an object: {row:?}"
        );
        return element.to_owned();
    }
    let rest = row
        .strip_prefix('+')
        .unwrap_or_else(|| panic!("not a row: {row:?}"));
    let varying = columns.iter().filter(|c| c.constant.is_none()).count();
    let cells: Vec<&str> = if rest.is_empty() {
        Vec::new()
    } else {
        split_top_level(rest)
    };
    assert_eq!(cells.len(), varying, "cell count in row {row:?}");
    let mut cells = cells.into_iter();

    let mut members: Vec<(String, Value)> = Vec::new();
    for column in columns {
        let cell = column
            .constant
            .as_deref()
            .map_or_else(|| cells.next().unwrap(), |c| c);
        assert!(!cell.is_empty(), "an empty cell in row {row:?}");
        if cell == "~" {
            continue;
        }
        insert(&mut members, &column.path, cell.to_owned());
    }
    let mut out = String::new();
    write_object(&members, &mut out);
    out
}

fn insert(members: &mut Vec<(String, Value)>, path: &[String], cell: String) {
    let (key, rest) = path.split_first().expect("a non-empty path");
    let existing = members.iter_mut().position(|(k, _)| k == key);
    if rest.is_empty() {
        assert!(existing.is_none(), "key {key} written twice");
        members.push((key.clone(), Value::Leaf(cell)));
        return;
    }
    let index = existing.unwrap_or_else(|| {
        members.push((key.clone(), Value::Object(Vec::new())));
        members.len() - 1
    });
    match &mut members[index].1 {
        Value::Object(inner) => insert(inner, rest, cell),
        Value::Leaf(_) => panic!("key {key} is both a leaf and a flattened object"),
    }
}

fn write_object(members: &[(String, Value)], out: &mut String) {
    out.push('{');
    for (n, (key, value)) in members.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push_str(key);
        out.push(':');
        match value {
            Value::Leaf(cell) => out.push_str(cell),
            Value::Object(inner) => write_object(inner, out),
        }
    }
    out.push('}');
}
