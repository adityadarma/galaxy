//! Lexical state and bracket nesting before the cursor, read from the text
//! alone so that unfinished code still works.

/// What the cursor is inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lex {
    Code,
    /// An interpreted string; the byte of its opening quote.
    Str(usize),
    Raw,
    Rune,
    Comment,
}

#[derive(Debug, Clone)]
pub(super) struct Scan {
    pub lex: Lex,
    /// Open brackets (`(`, `[`, `{`) with their byte offsets, innermost last.
    pub stack: Vec<(u8, usize)>,
}

/// What kind of name the position wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pos {
    /// An expression or statement.
    No,
    /// A type.
    Type,
    /// A name being declared; nothing to complete.
    Name,
}

pub(super) fn scan(src: &str, end: usize) -> Scan {
    let b = src.as_bytes();
    let end = end.min(b.len());
    let mut lex = Lex::Code;
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut i = 0;
    while i < end {
        let c = b[i];
        match lex {
            Lex::Code => match c {
                b'/' if b.get(i + 1) == Some(&b'/') => match src[i..end].find('\n') {
                    Some(n) => {
                        i += n + 1;
                        continue;
                    }
                    None => {
                        lex = Lex::Comment;
                        break;
                    }
                },
                b'/' if b.get(i + 1) == Some(&b'*') => match src[i + 2..end].find("*/") {
                    Some(n) => {
                        i += n + 4;
                        continue;
                    }
                    None => {
                        lex = Lex::Comment;
                        break;
                    }
                },
                b'"' => lex = Lex::Str(i),
                b'`' => match src[i + 1..end].find('`') {
                    Some(n) => {
                        i += n + 2;
                        continue;
                    }
                    None => {
                        lex = Lex::Raw;
                        break;
                    }
                },
                b'\'' => lex = Lex::Rune,
                b'(' | b'[' | b'{' => stack.push((c, i)),
                b')' | b']' | b'}' => {
                    stack.pop();
                }
                _ => {}
            },
            Lex::Str(_) | Lex::Rune => {
                let quote = if lex == Lex::Rune { b'\'' } else { b'"' };
                if c == b'\\' {
                    i += 1;
                } else if c == quote || c == b'\n' {
                    lex = Lex::Code;
                }
            }
            Lex::Raw | Lex::Comment => {}
        }
        i += 1;
    }
    Scan { lex, stack }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifier ending `text`.
pub(super) fn word_before(text: &str) -> &str {
    let start = text
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(text.len(), |(i, _)| i);
    &text[start..]
}

/// The bracket opening the group closed at `close`.
pub(super) fn matching_open(src: &str, close: usize) -> Option<usize> {
    let b = src.as_bytes();
    let mut depth = 0usize;
    let lowest = close.saturating_sub(4000);
    let mut i = close + 1;
    while i > lowest {
        i -= 1;
        match b[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paren {
    Params,
    /// `func (r T)`: the group before a method name.
    Receiver,
    Other,
}

/// Whether the group opened at `open` holds function parameters.
fn paren_kind(src: &str, open: usize) -> Paren {
    let before = &src[..open];
    let mut t = before.trim_end();
    if word_before(t) == "func" {
        return if before.len() > t.len() {
            Paren::Receiver
        } else {
            Paren::Params
        };
    }
    if t.ends_with(']')
        && let Some(o) = matching_open(src, t.len() - 1)
    {
        t = src[..o].trim_end();
    }
    let w = word_before(t);
    if !w.is_empty() {
        let t2 = t[..t.len() - w.len()].trim_end();
        if word_before(t2) == "func" {
            return Paren::Params;
        }
        if t2.ends_with(')')
            && let Some(o) = matching_open(src, t2.len() - 1)
            && paren_kind(src, o) == Paren::Receiver
        {
            return Paren::Params;
        }
        return Paren::Other;
    }
    if t.ends_with(')')
        && let Some(o) = matching_open(src, t.len() - 1)
        && paren_kind(src, o) == Paren::Params
    {
        return Paren::Params;
    }
    Paren::Other
}

fn is_names(text: &str) -> bool {
    text.split(',').all(|part| {
        let part = part.trim();
        !part.is_empty()
            && part.chars().all(is_word)
            && !part.starts_with(|c: char| c.is_ascii_digit())
    })
}

/// `var`, `const` and `type` lines, also inside their blocks.
fn decl(src: &str, start: usize, stack: &[(u8, usize)]) -> Option<Pos> {
    let before = &src[..start];
    let line = &before[before.rfind('\n').map_or(0, |i| i + 1)..];
    let l = line.trim_start();
    let rest = ["var", "const", "type"]
        .iter()
        .find_map(|kw| {
            l.strip_prefix(kw)
                .filter(|r| r.starts_with(char::is_whitespace))
        })
        .or_else(|| {
            let &(b'(', open) = stack.last()? else {
                return None;
            };
            matches!(
                word_before(src[..open].trim_end()),
                "var" | "const" | "type"
            )
            .then_some(l)
        })?;
    if rest.trim().is_empty() {
        return Some(Pos::Name);
    }
    let trimmed = rest.trim_end();
    if trimmed.ends_with(',') {
        return Some(Pos::Name);
    }
    (rest.ends_with(char::is_whitespace) && is_names(trimmed)).then_some(Pos::Type)
}

/// Whether the identifier starting at `start` is a type, a name being
/// declared, or neither.
pub(super) fn type_position(src: &str, start: usize, stack: &[(u8, usize)]) -> Pos {
    let before = &src[..start];
    let t = before.trim_end();
    if t.ends_with("...") {
        return Pos::Type;
    }
    if t.ends_with('.') {
        return Pos::No;
    }
    if let Some(pos) = decl(src, start, stack) {
        return pos;
    }
    match word_before(t) {
        "func" | "package" | "import" => return Pos::Name,
        "chan" => return Pos::Type,
        _ => {}
    }
    if t.ends_with('*') && type_position(src, t.len() - 1, stack) == Pos::Type {
        return Pos::Type;
    }
    if t.ends_with(']')
        && let Some(open) = matching_open(src, t.len() - 1)
    {
        let head = &src[..open];
        if open + 2 == t.len()
            || head.ends_with("map")
            || !head
                .chars()
                .next_back()
                .is_some_and(|c| is_word(c) || c == ')' || c == ']')
        {
            return Pos::Type;
        }
    }
    if t.ends_with(')')
        && let Some(open) = matching_open(src, t.len() - 1)
    {
        match paren_kind(src, open) {
            Paren::Params => return Pos::Type,
            Paren::Receiver => return Pos::Name,
            Paren::Other => {}
        }
    }
    let Some(&(ch, open)) = stack.last() else {
        return Pos::No;
    };
    let head = src[..open].trim_end();
    match ch {
        b'[' if head.ends_with("map") => Pos::Type,
        b'(' => match paren_kind(src, open) {
            Paren::Params | Paren::Receiver => Pos::Type,
            Paren::Other => {
                let call = matches!(word_before(head), "make" | "new");
                if call && src[open + 1..start].trim().is_empty() {
                    Pos::Type
                } else {
                    Pos::No
                }
            }
        },
        b'{' if matches!(word_before(head), "struct" | "interface") => {
            let seg = &src[open + 1..start];
            let line = seg.rsplit('\n').next().unwrap_or("").trim_start();
            if line.is_empty() || line.trim_end().ends_with(',') {
                Pos::Name
            } else if line.ends_with(char::is_whitespace) && is_names(line.trim_end()) {
                Pos::Type
            } else {
                Pos::No
            }
        }
        _ => Pos::No,
    }
}
