//! Minimal indentation-based YAML reader for Symfony config files.
//!
//! Reads the subset the index needs: nested mapping keys, scalar values,
//! quoted scalars, comments, and the line and column of each key. Lists,
//! anchors, flow collections and multi-line scalars are not interpreted:
//! the entries below a list item are skipped, a flow collection stays a raw
//! scalar, and a block scalar (`|`, `>`) has an empty value.

/// One `key: value` line, with the keys above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Keys from the top level down to this one.
    pub path: Vec<String>,
    /// The scalar after the colon; `None` when the key opens a mapping or
    /// has no value.
    pub value: Option<String>,
    /// 0-based line of the key.
    pub line: u32,
    /// 0-based UTF-16 column of the first character of the key, without
    /// its quote.
    pub column: u32,
}

impl Entry {
    /// The last key of the path.
    pub fn key(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }

    /// The path joined with dots.
    pub fn dotted(&self) -> String {
        self.path.join(".")
    }
}

/// Reads every mapping entry of `text`, in document order.
pub fn parse(text: &str) -> Vec<Entry> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    // `None` marks a list item: everything below it is skipped.
    let mut stack: Vec<(usize, Option<String>)> = Vec::new();
    let mut block: Option<usize> = None;
    for (n, raw) in text.lines().enumerate() {
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        let body = raw[indent..].trim_end();
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        if let Some(parent) = block {
            if indent > parent {
                continue;
            }
            block = None;
        }
        if body == "---" || body.starts_with("--- ") || body == "..." {
            stack.clear();
            continue;
        }
        if body.starts_with("%YAML") || body.starts_with("%TAG") {
            continue;
        }
        if body == "-" || body.starts_with("- ") {
            while stack
                .last()
                .is_some_and(|(i, key)| *i > indent || (*i == indent && key.is_none()))
            {
                stack.pop();
            }
            stack.push((indent, None));
            continue;
        }
        let Some((key, key_col, rest)) = split_key(body) else {
            continue;
        };
        while stack.last().is_some_and(|(i, _)| *i >= indent) {
            stack.pop();
        }
        let under_list = stack.iter().any(|(_, k)| k.is_none());
        if !under_list {
            let mut path: Vec<String> = stack.iter().filter_map(|(_, k)| k.clone()).collect();
            path.push(key.clone());
            out.push(Entry {
                path,
                value: scalar(rest),
                line: n as u32,
                column: (indent + key_col) as u32,
            });
        }
        if is_block_marker(rest) {
            block = Some(indent);
        }
        stack.push((indent, Some(key)));
    }
    out
}

/// The entry at exactly `path`.
pub fn find<'a>(entries: &'a [Entry], path: &[&str]) -> Option<&'a Entry> {
    entries.iter().find(|e| same_path(&e.path, path))
}

/// The scalar value of the entry at `path`.
pub fn value<'a>(entries: &'a [Entry], path: &[&str]) -> Option<&'a str> {
    find(entries, path).and_then(|e| e.value.as_deref())
}

/// Entries one level below `path`.
pub fn children<'a>(
    entries: &'a [Entry],
    path: &'a [&'a str],
) -> impl Iterator<Item = &'a Entry> + 'a {
    entries
        .iter()
        .filter(move |e| e.path.len() == path.len() + 1 && same_path(&e.path[..path.len()], path))
}

fn same_path(have: &[String], want: &[&str]) -> bool {
    have.len() == want.len() && have.iter().zip(want).all(|(a, b)| a == b)
}

/// Splits `key: rest` into the key, its offset in `body` and `rest`.
fn split_key(body: &str) -> Option<(String, usize, &str)> {
    let b = body.as_bytes();
    if b[0] == b'\'' || b[0] == b'"' {
        let quote = b[0];
        let mut i = 1;
        loop {
            if i >= b.len() {
                return None;
            }
            if quote == b'"' && b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == quote {
                if quote == b'\'' && b.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                break;
            }
            i += 1;
        }
        let after = body[i + 1..].trim_start_matches(' ');
        let rest = after.strip_prefix(':')?;
        if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
            return None;
        }
        return Some((unquote(&body[1..i], quote), 1, rest));
    }
    if matches!(b[0], b'{' | b'[' | b'*' | b'?' | b'!' | b'&' | b'|' | b'>') {
        return None;
    }
    let mut at = 0;
    while let Some(i) = body[at..].find(':') {
        let colon = at + i;
        let next = body.as_bytes().get(colon + 1);
        if next.is_none() || matches!(next, Some(b' ' | b'\t')) {
            let key = body[..colon].trim_end();
            if key.is_empty() || key.contains(" #") {
                return None;
            }
            return Some((key.to_owned(), 0, &body[colon + 1..]));
        }
        at = colon + 1;
    }
    None
}

/// Text of a quoted scalar without its quotes, escapes resolved.
fn unquote(inner: &str, quote: u8) -> String {
    if quote == b'\'' {
        return inner.replace("''", "'");
    }
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn is_block_marker(rest: &str) -> bool {
    let t = rest.trim();
    t.starts_with(['|', '>']) && !t.starts_with("|#") && !t.starts_with(">#")
}

/// The scalar in `rest` (the text after the colon).
fn scalar(rest: &str) -> Option<String> {
    let t = rest.trim();
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    if is_block_marker(rest) {
        return Some(String::new());
    }
    let b = t.as_bytes();
    if b[0] == b'\'' || b[0] == b'"' {
        let quote = b[0];
        let mut i = 1;
        while i < b.len() {
            if quote == b'"' && b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == quote {
                if quote == b'\'' && b.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                return Some(unquote(&t[1..i], quote));
            }
            i += 1;
        }
        return Some(unquote(&t[1..], quote));
    }
    let cut = t.find(" #").map_or(t, |i| &t[..i]).trim_end();
    if (cut.starts_with('&') || cut.starts_with('!')) && !cut.contains(' ') {
        return None;
    }
    (!cut.is_empty()).then(|| cut.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(Entry::dotted).collect()
    }

    #[test]
    fn reads_nested_keys_with_positions() {
        let entries = parse("a:\n    b:\n        c: 1\n    d: two\n");
        assert_eq!(paths(&entries), ["a", "a.b", "a.b.c", "a.d"]);
        let c = find(&entries, &["a", "b", "c"]).unwrap();
        assert_eq!((c.line, c.column), (2, 8));
        assert_eq!(c.value.as_deref(), Some("1"));
        assert_eq!(value(&entries, &["a", "d"]), Some("two"));
        assert_eq!(value(&entries, &["a"]), None);
    }

    #[test]
    fn quoted_scalars_lose_their_quotes() {
        let entries = parse("a: 'it''s'\nb: \"x\\\"y\"\nc: \"# not a comment\"\nd: '  '\n");
        assert_eq!(value(&entries, &["a"]), Some("it's"));
        assert_eq!(value(&entries, &["b"]), Some("x\"y"));
        assert_eq!(value(&entries, &["c"]), Some("# not a comment"));
        assert_eq!(value(&entries, &["d"]), Some("  "));
    }

    #[test]
    fn quoted_keys_point_at_their_contents() {
        let entries = parse("'app.name': x\n  \"b c\": y\n");
        assert_eq!(entries[0].key(), "app.name");
        assert_eq!(entries[0].column, 1);
        assert_eq!(entries[1].key(), "b c");
        assert_eq!(entries[1].column, 3);
    }

    #[test]
    fn comments_are_ignored() {
        let entries = parse("# top\na: 1 # trailing\nb: # only a comment\n  c: x#y\n");
        assert_eq!(value(&entries, &["a"]), Some("1"));
        assert_eq!(value(&entries, &["b"]), None);
        assert_eq!(value(&entries, &["b", "c"]), Some("x#y"));
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn colon_inside_a_plain_scalar_does_not_split() {
        let entries = parse("url: http://localhost:8000/x\nApp\\Foo:\n  class: A\\B\n");
        assert_eq!(value(&entries, &["url"]), Some("http://localhost:8000/x"));
        assert_eq!(value(&entries, &["App\\Foo", "class"]), Some("A\\B"));
    }

    #[test]
    fn list_items_and_their_children_are_skipped() {
        let text = "a:\n    - x: 1\n      y: 2\n    - z\nb: 3\nc:\n- q: 1\nd: 4\n";
        let entries = parse(text);
        assert_eq!(paths(&entries), ["a", "b", "c", "d"]);
    }

    #[test]
    fn block_scalars_hide_their_body() {
        let entries = parse("a: |\n  not: a key\n  more\nb: >-\n  text\nc: 1\n");
        assert_eq!(paths(&entries), ["a", "b", "c"]);
        assert_eq!(value(&entries, &["a"]), Some(""));
        assert_eq!(value(&entries, &["c"]), Some("1"));
    }

    #[test]
    fn children_are_direct_only() {
        let entries = parse("p:\n  a: 1\n  b:\n    c: 2\n  d: 3\nq:\n  e: 4\n");
        let keys: Vec<&str> = children(&entries, &["p"]).map(Entry::key).collect();
        assert_eq!(keys, ["a", "b", "d"]);
    }

    #[test]
    fn flow_values_stay_raw_and_blank_or_odd_lines_are_ignored() {
        let entries = parse("\n---\nm: [GET, POST]\n\n   \n{ a: b }\n.\nn: { x: 1 }\n");
        assert_eq!(value(&entries, &["m"]), Some("[GET, POST]"));
        assert_eq!(value(&entries, &["n"]), Some("{ x: 1 }"));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn crlf_and_bom_are_handled() {
        let entries = parse("\u{feff}a:\r\n  b: 1\r\n");
        assert_eq!(paths(&entries), ["a", "a.b"]);
        assert_eq!(entries[0].column, 0);
        assert_eq!(value(&entries, &["a", "b"]), Some("1"));
    }

    #[test]
    fn dedent_returns_to_the_right_parent() {
        let entries = parse("a:\n  b:\n    c: 1\n  d: 2\ne: 3\n");
        assert_eq!(paths(&entries), ["a", "a.b", "a.b.c", "a.d", "e"]);
    }
}
