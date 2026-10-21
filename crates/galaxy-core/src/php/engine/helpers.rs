//! Free functions the engine files share: doc text, completion items and
//! small scanners over PHP source.

use super::*;

// ---- helpers ------------------------------------------------------------

pub(super) fn with_doc(head: &str, doc: Option<&str>) -> String {
    let mut out = format!("```php\n{head}\n```");
    if let Some(doc) = doc {
        let cleaned = clean_doc(doc);
        if !cleaned.is_empty() {
            out.push_str("\n\n");
            out.push_str(&cleaned);
        }
    }
    out
}

pub(super) fn clean_doc(doc: &str) -> String {
    doc.lines()
        .map(|line| {
            line.trim()
                .trim_start_matches("/**")
                .trim_end_matches("*/")
                .trim_start_matches('*')
                .trim()
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn member_completion(
    member: &Member,
    owner: &Arc<ClassInfo>,
    own: char,
    typed_dollar: bool,
) -> Completion {
    let (kind, label, insert, snippet) = match member.kind {
        MemberKind::Method => {
            let empty = member.detail.starts_with("()");
            (
                ItemKind::Method,
                member.name.clone(),
                if empty {
                    format!("{}()", member.name)
                } else {
                    format!("{}($0)", member.name)
                },
                !empty,
            )
        }
        MemberKind::Property => (
            ItemKind::Property,
            if member.is_static {
                format!("${}", member.name)
            } else {
                member.name.clone()
            },
            // The query the editor replaces never includes a typed `$`.
            if member.is_static && !typed_dollar {
                format!("${}", member.name)
            } else {
                member.name.clone()
            },
            false,
        ),
        MemberKind::Constant | MemberKind::Case => (
            ItemKind::Constant,
            member.name.clone(),
            member.name.clone(),
            false,
        ),
    };
    let detail = if member.detail.is_empty() {
        owner.name.clone()
    } else {
        member.detail.clone()
    };
    Completion {
        label,
        detail: Some(detail),
        insert_text: insert,
        is_snippet: snippet,
        kind,
        sort: format!("{own}{}", member.name.to_lowercase()),
        extra: Vec::new(),
        replace_from: None,
    }
}

pub(super) fn keyword_items(prefix: &str) -> Vec<Completion> {
    const WORDS: &[&str] = &[
        "abstract",
        "array",
        "case",
        "catch",
        "class",
        "const",
        "echo",
        "else",
        "elseif",
        "enum",
        "extends",
        "false",
        "final",
        "finally",
        "fn",
        "foreach",
        "function",
        "if",
        "implements",
        "instanceof",
        "interface",
        "isset",
        "match",
        "namespace",
        "new",
        "null",
        "parent",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "self",
        "static",
        "switch",
        "throw",
        "trait",
        "true",
        "try",
        "unset",
        "use",
        "while",
    ];
    if prefix.chars().count() < 2 {
        return Vec::new();
    }
    WORDS
        .iter()
        .filter(|w| fuzzy(prefix, w))
        .map(|w| Completion {
            label: (*w).to_owned(),
            detail: None,
            insert_text: (*w).to_owned(),
            is_snippet: false,
            kind: ItemKind::Keyword,
            sort: rank(prefix, w, '3'),
            extra: Vec::new(),
            replace_from: None,
        })
        .collect()
}

/// `0` exact prefix, `1` other match, behind the caller's group digit.
pub(in crate::php) fn rank(prefix: &str, name: &str, group: char) -> String {
    let lower = name.to_lowercase();
    let near = if lower.starts_with(&prefix.to_lowercase()) {
        '0'
    } else {
        '1'
    };
    format!("{near}{group}{lower}")
}

/// Letters of `needle` appear in `haystack` in order, ignoring case.
pub(in crate::php) fn fuzzy(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|n| hay.any(|h| h == n))
}

/// `use` statement that makes `fqn` usable by its short name, or `None`
/// when the file already reaches it.
pub(super) fn import_edit(file: &FileSymbols, fqn: &str) -> Option<Insertion> {
    let (namespace, short) = fqn.rsplit_once('\\').map_or(("", fqn), |(n, s)| (n, s));
    if file.namespace.is_empty() && namespace.is_empty() {
        return None;
    }
    if namespace.eq_ignore_ascii_case(&file.namespace) {
        return None;
    }
    if file.uses.contains_key(&short.to_lowercase()) {
        return None;
    }
    let statement = format!("use {fqn};\n");
    let edit = if let Some(line) = file.last_use_line {
        Insertion {
            line: line + 1,
            column: 0,
            text: statement,
        }
    } else if let Some(line) = file.namespace_line {
        Insertion {
            line: line + 1,
            column: 0,
            text: format!("\n{statement}"),
        }
    } else {
        Insertion {
            line: file.open_tag_line.map_or(0, |l| l + 1),
            column: 0,
            text: format!("\n{statement}"),
        }
    };
    Some(edit)
}

/// A tree for code that is evaluated from a string, not from a position.
pub(super) fn empty_tree() -> &'static Tree {
    static TREE: OnceLock<Tree> = OnceLock::new();
    TREE.get_or_init(|| {
        parse_with_tree(Path::new(""), "")
            .map(|(_, tree)| tree)
            .expect("the PHP grammar loads")
    })
}

/// `unset` or `exit` when `node` is the keyword of such a statement.
pub(super) fn construct_keyword(node: Node<'_>) -> Option<&'static str> {
    let parent = node.parent()?.kind();
    match (node.kind(), parent) {
        ("unset", "unset_statement") => Some("unset"),
        ("exit", "exit_statement") => Some("exit"),
        _ => None,
    }
}

pub(super) fn is_scope(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration"
            | "function_definition"
            | "anonymous_function"
            | "arrow_function"
            | "program"
    )
}

pub(super) fn uses_variable(scope: Node<'_>, var: &str, src: &str) -> bool {
    let mut cursor = scope.walk();
    scope
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "anonymous_function_use_clause")
        .any(|clause| src[clause.byte_range()].contains(var))
}

pub(super) fn preceding_comment<'s>(scope: Node<'_>, src: &'s str) -> Option<&'s str> {
    let prev = scope.prev_named_sibling()?;
    (prev.kind() == "comment").then(|| &src[prev.byte_range()])
}

pub(super) fn param_doc_regex(var: &str) -> Regex {
    Regex::new(&format!(r"@param\s+(\S+)\s+{}\b", regex::escape(var)))
        .unwrap_or_else(|_| Regex::new("$^").expect("valid"))
}

pub(super) fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

pub(super) fn is_name_char(c: u8) -> bool {
    is_ident(c) || c == b'\\'
}

pub(super) fn ident_len(s: &str) -> usize {
    s.bytes().take_while(|&c| is_ident(c)).count()
}

pub(super) fn read_name(s: &str) -> usize {
    s.bytes().take_while(|&c| is_name_char(c)).count()
}

pub(super) fn strip_new(s: &str) -> Option<&str> {
    let rest = s.strip_prefix("new")?;
    let trimmed = rest.trim_start();
    (trimmed.len() < rest.len()).then_some(trimmed)
}

/// Index just past the bracket closing the one at `start`.
pub(super) fn skip_balanced(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = Some(c),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

pub(super) fn split_args(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut start = 0;
    let bytes = args.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = Some(c),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => {
                    out.push(args[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if !args[start..].trim().is_empty() {
        out.push(args[start..].trim());
    }
    out
}

/// Byte offset of a line and UTF-16 column.
pub(in crate::php) fn byte_offset(text: &str, line: u32, column: u32) -> Option<usize> {
    let mut start = 0usize;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let mut units = 0u32;
    for (i, ch) in text[start..].char_indices() {
        if units >= column || ch == '\n' {
            return Some(start + i);
        }
        units += ch.len_utf16() as u32;
    }
    Some(text.len())
}

/// Line and UTF-16 column of a byte offset.
pub(in crate::php) fn position_of(text: &str, byte: usize) -> (u32, u32, u32) {
    let before = &text[..byte.min(text.len())];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = text[line_start..before.len()].encode_utf16().count() as u32;
    (line, column, 0)
}

pub(super) fn word_start(text: &str, byte: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = byte;
    while i > 0 && (is_ident(bytes[i - 1]) || bytes[i - 1] == b'$') {
        i -= 1;
    }
    i
}

/// If `before` ends with `->`, `?->` or `::`, the byte where the receiver
/// ends and whether the operator is `::`.
pub(in crate::php) fn member_operator(before: &str) -> Option<(usize, bool)> {
    let text = before.trim_end();
    if let Some(rest) = text.strip_suffix("?->") {
        return Some((rest.len(), false));
    }
    if let Some(rest) = text.strip_suffix("->") {
        return Some((rest.len(), false));
    }
    if let Some(rest) = text.strip_suffix("::") {
        return Some((rest.len(), true));
    }
    None
}

/// Start of the chain expression that ends at `end`, scanning backwards
/// over calls, indexes and `->`/`::` links (also across lines).
pub(in crate::php) fn receiver_start(text: &str, end: usize) -> usize {
    let bytes = text.as_bytes();
    let skip_ws = |mut i: usize| {
        while i > 0 && bytes[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        i
    };
    let floor = end.saturating_sub(4000);
    let mut i = skip_ws(end);
    loop {
        i = skip_ws(i);
        if i <= floor {
            return floor.max(i);
        }
        let c = bytes[i - 1];
        if c == b')' || c == b']' {
            let mut depth = 0i32;
            let mut j = i;
            let mut opened = None;
            while j > floor {
                j -= 1;
                match bytes[j] {
                    b')' | b']' => depth += 1,
                    b'(' | b'[' => {
                        depth -= 1;
                        if depth == 0 {
                            opened = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(open) = opened else { return i };
            i = open;
            let k = skip_ws(i);
            if k > 0 && (is_name_char(bytes[k - 1]) || bytes[k - 1] == b'$') {
                i = name_start(bytes, k);
            } else {
                return i;
            }
        } else if is_name_char(c) || c == b'$' {
            i = name_start(bytes, i);
        } else {
            return i;
        }
        let k = skip_ws(i);
        if k >= 3 && &bytes[k - 3..k] == b"?->" {
            i = k - 3;
            continue;
        }
        if k >= 2 && (&bytes[k - 2..k] == b"->" || &bytes[k - 2..k] == b"::") {
            i = k - 2;
            continue;
        }
        if k >= 3 && &bytes[k - 3..k] == b"new" && (k == 3 || !is_ident(bytes[k - 4])) {
            return k - 3;
        }
        return i;
    }
}

pub(super) fn name_start(bytes: &[u8], mut i: usize) -> usize {
    while i > 0 && (is_name_char(bytes[i - 1]) || bytes[i - 1] == b'$') {
        i -= 1;
    }
    i
}

/// Class declarations of a project that has no Composer autoload data.
pub(super) fn scan_sources(root: &Path) -> Vec<(String, PathBuf)> {
    static NAMESPACE: OnceLock<Regex> = OnceLock::new();
    static CLASS: OnceLock<Regex> = OnceLock::new();
    let namespace = NAMESPACE
        .get_or_init(|| Regex::new(r"(?m)^\s*namespace\s+([\w\\]+)\s*[;{]").expect("valid"));
    let class = CLASS.get_or_init(|| {
        Regex::new(
            r"(?m)^\s*(?:(?:abstract|final|readonly)\s+)*(?:class|interface|trait|enum)\s+(\w+)",
        )
        .expect("valid")
    });
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut budget = 20_000usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < 12
                    && !name.starts_with('.')
                    && !matches!(name, "vendor" | "node_modules" | "storage" | "cache")
                {
                    stack.push((entry.path(), depth + 1));
                }
            } else if name.ends_with(".php") && !framework::is_template_name(name) {
                budget = budget.saturating_sub(1);
                if budget == 0 {
                    return out;
                }
                let Ok(text) = std::fs::read_to_string(entry.path()) else {
                    continue;
                };
                let ns = namespace
                    .captures(&text)
                    .map(|c| c[1].to_owned())
                    .unwrap_or_default();
                for caps in class.captures_iter(&text) {
                    let fqn = if ns.is_empty() {
                        caps[1].to_owned()
                    } else {
                        format!("{ns}\\{}", &caps[1])
                    };
                    out.push((fqn, entry.path()));
                }
            }
        }
    }
    out
}
