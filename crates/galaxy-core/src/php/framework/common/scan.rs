//! Text scanners for PHP source shared by every framework: PHP regions of
//! a file, string literals, the call or array a string sits in, and
//! bracket matching. They scan text instead of parsing, so they work on
//! half-typed code.

use std::collections::HashMap;

/// A string literal: `open` is the quote, `start..end` the contents.
#[derive(Debug, Clone, Copy)]
pub(in crate::php) struct Str {
    pub(in crate::php) open: usize,
    pub(in crate::php) start: usize,
    pub(in crate::php) end: usize,
    pub(in crate::php) closed: bool,
}

/// A stretch of text that is PHP. `call` is the open parenthesis of the
/// Blade directive that owns it.
#[derive(Debug, Clone, Copy)]
pub(in crate::php) struct Region {
    pub(in crate::php) start: usize,
    pub(in crate::php) end: usize,
}

/// PHP stretches of a plain `.php` file: from each open tag to its `?>`.
pub(in crate::php) fn php_regions(text: &str) -> Vec<Region> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(open) = text[at..].find("<?") {
        let mut start = at + open + 2;
        if text[start..].starts_with("php") {
            start += 3;
        } else if text[start..].starts_with('=') {
            start += 1;
        }
        let end = text[start..].find("?>").map_or(text.len(), |i| start + i);
        out.push(Region { start, end });
        at = end;
        if at >= text.len() {
            break;
        }
    }
    out
}

pub(in crate::php) fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The `)` that closes the `(` at `open`, or the end of the text.
pub(in crate::php) fn matching_paren(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// String literals in the PHP stretches. With `multiline` off, a string
/// that is not closed on its line ends there, so one unfinished quote does
/// not swallow the rest of the file while code is being typed.
pub(in crate::php) fn strings(text: &str, regions: &[Region], multiline: bool) -> Vec<Str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for region in regions {
        let end = region.end.min(bytes.len());
        let mut i = region.start;
        while i < end {
            let c = bytes[i];
            match c {
                b'\'' | b'"' => {
                    let mut j = i + 1;
                    let mut closed = false;
                    while j < end {
                        if bytes[j] == b'\\' {
                            j += 2;
                            continue;
                        }
                        if bytes[j] == c {
                            closed = true;
                            break;
                        }
                        if bytes[j] == b'\n' && !multiline {
                            break;
                        }
                        j += 1;
                    }
                    let content_end = j.min(end);
                    out.push(Str {
                        open: i,
                        start: i + 1,
                        end: content_end,
                        closed,
                    });
                    i = if closed {
                        j + 1
                    } else {
                        content_end.max(i + 1)
                    };
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    i += text[i..end].find('\n').map_or(end - i, |n| n);
                }
                b'#' if bytes.get(i + 1) != Some(&b'[') => {
                    i += text[i..end].find('\n').map_or(end - i, |n| n);
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    i += text[i..end].find("*/").map_or(end - i, |n| n + 2);
                }
                b'<' if text[i..end].starts_with("<<<") => {
                    i = skip_heredoc(text, i, end);
                }
                _ => i += 1,
            }
        }
    }
    out
}

/// Index after the heredoc or nowdoc that starts at `at`.
fn skip_heredoc(text: &str, at: usize, end: usize) -> usize {
    let rest = &text[at + 3..end];
    let label: String = rest
        .trim_start_matches([' ', '\'', '"'])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if label.is_empty() {
        return at + 3;
    }
    let body_start = rest.find('\n').map_or(rest.len(), |n| n + 1);
    for (offset, line) in line_offsets(&rest[body_start..]) {
        if line.trim_start().starts_with(label.as_str()) {
            return at + 3 + body_start + offset + line.len();
        }
    }
    end
}

fn line_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split_inclusive('\n').map(move |line| {
        let at = offset;
        offset += line.len();
        (at, line.trim_end_matches('\n'))
    })
}

/// How a string sits in the code around it.
pub(in crate::php) struct Around {
    /// The `(` of the call whose argument it is.
    pub(in crate::php) paren: Option<usize>,
    /// Arguments before it.
    pub(in crate::php) index: usize,
    /// The `[` of the array it is an element of, if any.
    pub(in crate::php) array: Option<usize>,
}

pub(in crate::php) fn around(
    text: &str,
    string: &Str,
    floor: usize,
    closers: &HashMap<usize, usize>,
) -> Around {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut index = 0usize;
    let mut array = None;
    let mut i = string.open;
    while i > floor {
        i -= 1;
        // Skip over strings seen earlier, whatever they contain.
        if let Some(open) = closers.get(&i) {
            i = *open;
            continue;
        }
        match bytes[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' => {
                if depth == 0 {
                    return Around {
                        paren: Some(i),
                        index,
                        array,
                    };
                }
                depth -= 1;
            }
            b'[' => {
                if depth == 0 {
                    // Only the innermost array is the string's own.
                    array.get_or_insert(i);
                    index = 0;
                } else {
                    depth -= 1;
                }
            }
            b'{' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b';' if depth == 0 => break,
            b',' if depth == 0 => index += 1,
            _ => {}
        }
    }
    Around {
        paren: None,
        index,
        array,
    }
}

/// The function, method or directive that is called at `paren`.
pub(in crate::php) struct Callee {
    pub(in crate::php) name: String,
    /// `->` or `::`; `None` for plain functions and directives.
    pub(in crate::php) operator: Option<&'static str>,
    /// Last segment of the class for `::`, the text of the object for `->`.
    pub(in crate::php) scope: String,
}

pub(in crate::php) fn callee(text: &str, paren: usize) -> Option<Callee> {
    let before = text[..paren].trim_end();
    let bytes = before.as_bytes();
    let mut start = before.len();
    while start > 0 && (is_word(bytes[start - 1]) || bytes[start - 1] == b'\\') {
        start -= 1;
    }
    let name = &before[start..];
    if name.is_empty() {
        return None;
    }
    if start > 0 && bytes[start - 1] == b'@' {
        return Some(Callee {
            name: format!("@{}", name.to_ascii_lowercase()),
            operator: None,
            scope: String::new(),
        });
    }
    let head = before[..start].trim_end();
    let name = name
        .rsplit('\\')
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    if let Some(object) = head.strip_suffix("?->").or_else(|| head.strip_suffix("->")) {
        let tail_start = object.len().saturating_sub(120);
        let tail_start = (tail_start..=object.len())
            .find(|i| object.is_char_boundary(*i))
            .unwrap_or(object.len());
        let scope: String = object[tail_start..]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        return Some(Callee {
            name,
            operator: Some("->"),
            scope,
        });
    }
    if let Some(class) = head.strip_suffix("::") {
        let scope: String = class
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '\\')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let scope = scope.rsplit('\\').next().unwrap_or("").to_ascii_lowercase();
        return Some(Callee {
            name,
            operator: Some("::"),
            scope,
        });
    }
    Some(Callee {
        name,
        operator: None,
        scope: String::new(),
    })
}

/// Number of arguments the call at `paren` passes (strings are skipped by
/// the caller's table of string ends).
pub(in crate::php) fn argument_count(
    text: &str,
    paren: usize,
    closers: &HashMap<usize, usize>,
    openers: &HashMap<usize, usize>,
) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut any = false;
    let mut i = paren + 1;
    while i < bytes.len() {
        if let Some(close) = openers.get(&i) {
            any = true;
            i = *close + 1;
            continue;
        }
        match bytes[i] {
            b'(' | b'[' | b'{' => {
                depth += 1;
                any = true;
            }
            b')' | b']' | b'}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => commas += 1,
            c if !c.is_ascii_whitespace() => any = true,
            _ => {}
        }
        i += 1;
    }
    let _ = closers;
    if any { commas + 1 } else { 0 }
}

pub(in crate::php) fn closers_of(strs: &[Str]) -> (HashMap<usize, usize>, HashMap<usize, usize>) {
    let mut closers = HashMap::new();
    let mut openers = HashMap::new();
    for s in strs.iter().filter(|s| s.closed) {
        closers.insert(s.end, s.open);
        openers.insert(s.open, s.end);
    }
    (closers, openers)
}
