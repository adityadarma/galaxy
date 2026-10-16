//! Auto close tag and auto rename tag for HTML-like markup: HTML, PHP
//! templates and JSX/TSX.
//!
//! Code being typed is rarely valid, so a syntax tree is not reliable here.
//! A small tokenizer reads the text instead. It understands comments,
//! doctypes, PHP blocks, `<script>`/`<style>` bodies, quoted attributes, and
//! JSX `{...}` expressions (so the `>` of `=>` does not end a tag).
//!
//! Known limits: a `<` inside a string or comment of embedded JavaScript can
//! be mistaken for a tag, and HTML tags with optional end tags (`<li>`,
//! `<p>`) left unclosed stay on the open-tag stack.

use crate::document::Document;
use crate::syntax::SyntaxLanguage;

/// How far back typing `>` looks for the start of its tag.
const WINDOW: usize = 2_000;
/// Documents larger than this skip whole-document scans.
const MAX_DOC_CHARS: usize = 500_000;
/// How many `<` candidates are tried when typing `>`.
const MAX_CANDIDATES: usize = 8;

const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
const RAW_TEXT: [&str; 2] = ["script", "style"];

/// Which markup rules apply to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flavor {
    /// Void elements, case-insensitive names, raw `<script>`/`<style>` text.
    html: bool,
    /// `{...}` expressions in attributes, `<>` fragments, `Array<T>` guard.
    jsx: bool,
    /// `<?php ... ?>` blocks are code, not markup.
    php: bool,
}

impl Flavor {
    #[cfg(test)]
    pub(crate) const HTML: Self = Self {
        html: true,
        jsx: false,
        php: false,
    };

    /// `None` for languages without tags (TypeScript has generics, not JSX).
    pub fn for_language(language: SyntaxLanguage) -> Option<Self> {
        match language {
            SyntaxLanguage::Html => Some(Self {
                html: true,
                jsx: false,
                php: false,
            }),
            SyntaxLanguage::Php | SyntaxLanguage::Blade => Some(Self {
                html: true,
                jsx: false,
                php: true,
            }),
            SyntaxLanguage::Tsx | SyntaxLanguage::JavaScript => Some(Self {
                html: false,
                jsx: true,
                php: false,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Open,
    Close,
    /// `<br>`, `<img />`: nothing to close.
    Standalone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tag {
    kind: Kind,
    /// Name as a char range; empty for a JSX fragment.
    name: (usize, usize),
}

#[derive(Debug, Default)]
struct Scan {
    tags: Vec<Tag>,
    /// A tag that was still being written when the text ended.
    pending: Option<(usize, usize)>,
    /// The text ends inside a PHP block.
    in_php: bool,
}

pub fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')
}

fn name_end(chars: &[char], start: usize) -> usize {
    let mut i = start;
    while i < chars.len() && is_name_char(chars[i]) {
        i += 1;
    }
    i
}

fn starts_with(chars: &[char], at: usize, pattern: &str) -> bool {
    pattern
        .chars()
        .enumerate()
        .all(|(k, p)| chars.get(at + k) == Some(&p))
}

fn starts_with_ci(chars: &[char], at: usize, pattern: &str) -> bool {
    pattern.chars().enumerate().all(|(k, p)| {
        chars
            .get(at + k)
            .is_some_and(|c| c.eq_ignore_ascii_case(&p))
    })
}

fn find(chars: &[char], from: usize, pattern: &str) -> Option<usize> {
    (from..chars.len()).find(|&i| starts_with(chars, i, pattern))
}

fn find_ci(chars: &[char], from: usize, pattern: &str) -> Option<usize> {
    (from..chars.len()).find(|&i| starts_with_ci(chars, i, pattern))
}

fn lower_name(chars: &[char], name: (usize, usize)) -> String {
    chars[name.0..name.1]
        .iter()
        .collect::<String>()
        .to_ascii_lowercase()
}

fn same_name(chars: &[char], a: (usize, usize), b: (usize, usize), flavor: Flavor) -> bool {
    if a.1 - a.0 != b.1 - b.0 {
        return false;
    }
    chars[a.0..a.1].iter().zip(&chars[b.0..b.1]).all(|(x, y)| {
        if flavor.html {
            x.eq_ignore_ascii_case(y)
        } else {
            x == y
        }
    })
}

/// In JSX, `<` right after these is a generic or a comparison, not a tag.
fn blocks_jsx_tag(prev: char) -> bool {
    prev.is_alphanumeric() || matches!(prev, '_' | '$' | ')' | ']')
}

/// Index of the `>` ending a tag whose attributes start at `i`, or `None`
/// when the text ends first.
fn tag_end(chars: &[char], mut i: usize, flavor: Flavor) -> Option<usize> {
    let mut depth = 0usize;
    while i < chars.len() {
        match chars[i] {
            quote @ ('"' | '\'') => {
                i += 1;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
                if i >= chars.len() {
                    return None;
                }
            }
            '`' if depth > 0 => {
                i += 1;
                while i < chars.len() && chars[i] != '`' {
                    i += 1;
                }
                if i >= chars.len() {
                    return None;
                }
            }
            '{' if flavor.jsx => depth += 1,
            '}' if flavor.jsx => depth = depth.saturating_sub(1),
            '>' if depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

fn scan(chars: &[char], flavor: Flavor) -> Scan {
    let n = chars.len();
    let mut out = Scan::default();
    let mut i = 0;
    while i < n {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        if starts_with(chars, i, "<!--") {
            i = find(chars, i + 4, "-->").map_or(n, |end| end + 3);
            continue;
        }
        if flavor.php && starts_with(chars, i, "<?") {
            match find(chars, i + 2, "?>") {
                Some(end) => {
                    i = end + 2;
                    continue;
                }
                None => {
                    out.in_php = true;
                    break;
                }
            }
        }
        if starts_with(chars, i, "<!") || starts_with(chars, i, "<?") {
            i = find(chars, i + 2, ">").map_or(n, |end| end + 1);
            continue;
        }
        if chars.get(i + 1) == Some(&'/') {
            let start = i + 2;
            let end = name_end(chars, start);
            let fragment = flavor.jsx && end == start && chars.get(start) == Some(&'>');
            if end == start && !fragment {
                i += 1;
                continue;
            }
            let Some(gt) = find(chars, end, ">") else {
                break;
            };
            if !chars[end..gt].iter().all(|c| c.is_whitespace()) {
                i += 1;
                continue;
            }
            out.tags.push(Tag {
                kind: Kind::Close,
                name: (start, end),
            });
            i = gt + 1;
            continue;
        }
        let start = i + 1;
        let end = name_end(chars, start);
        let fragment = flavor.jsx && end == start && chars.get(start) == Some(&'>');
        let named = chars.get(start).is_some_and(|c| c.is_alphabetic());
        if !named && !fragment {
            i += 1;
            continue;
        }
        if flavor.jsx && i > 0 && blocks_jsx_tag(chars[i - 1]) {
            i += 1;
            continue;
        }
        let Some(gt) = tag_end(chars, end, flavor) else {
            out.pending = Some((start, end));
            break;
        };
        let name = (start, end);
        let lower = lower_name(chars, name);
        let self_closing = gt > end && chars[gt - 1] == '/';
        let kind = if self_closing || (flavor.html && VOID.contains(&lower.as_str())) {
            Kind::Standalone
        } else {
            Kind::Open
        };
        out.tags.push(Tag { kind, name });
        i = gt + 1;
        if flavor.html && kind == Kind::Open && RAW_TEXT.contains(&lower.as_str()) {
            i = find_ci(chars, i, &format!("</{lower}")).unwrap_or(n);
        }
    }
    out
}

/// Open tags with no closing tag yet, outermost first.
fn unclosed(tags: &[Tag], chars: &[char], flavor: Flavor) -> Vec<(usize, usize)> {
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for tag in tags {
        match tag.kind {
            Kind::Open => stack.push(tag.name),
            Kind::Close => {
                if let Some(at) = stack
                    .iter()
                    .rposition(|&open| same_name(chars, open, tag.name, flavor))
                {
                    stack.truncate(at);
                }
            }
            Kind::Standalone => {}
        }
    }
    stack
}

/// For the tag whose name starts at `name_start`: its name range and the
/// range of the name in its opening or closing partner.
#[allow(clippy::type_complexity)]
fn pair_of(
    tags: &[Tag],
    chars: &[char],
    flavor: Flavor,
    name_start: usize,
) -> Option<((usize, usize), (usize, usize))> {
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for tag in tags {
        match tag.kind {
            Kind::Open => stack.push(tag.name),
            Kind::Close => {
                if let Some(at) = stack
                    .iter()
                    .rposition(|&open| same_name(chars, open, tag.name, flavor))
                {
                    let open = stack[at];
                    if open.0 == name_start {
                        return Some((open, tag.name));
                    }
                    if tag.name.0 == name_start {
                        return Some((tag.name, open));
                    }
                    stack.truncate(at);
                }
            }
            Kind::Standalone => {}
        }
    }
    None
}

fn chars_between(doc: &Document, from: usize, to: usize) -> Vec<char> {
    doc.rope().slice(from..to).chars().collect()
}

/// The closing tag to add after `>` is typed at `caret`: `</div>` for
/// `<div`, `</>` for a JSX `<`. `None` when the `>` should stay plain.
pub fn closing_after_gt(doc: &Document, caret: usize, flavor: Flavor) -> Option<String> {
    let before = chars_between(doc, caret.saturating_sub(WINDOW), caret);
    if before.last() == Some(&'/') {
        return None;
    }
    let after: String = doc
        .rope()
        .slice(caret..doc.len_chars().min(caret + 64))
        .chars()
        .collect();
    if before.last() == Some(&'<') {
        let blocked = before.len() >= 2 && blocks_jsx_tag(before[before.len() - 2]);
        return (flavor.jsx && !blocked && !after.starts_with("</>")).then(|| "</>".to_owned());
    }

    let name = if flavor.php {
        if doc.len_chars() > MAX_DOC_CHARS || !before.contains(&'<') {
            return None;
        }
        let all = chars_between(doc, 0, caret);
        let found = scan(&all, flavor);
        if found.in_php {
            return None;
        }
        let (start, end) = found.pending?;
        all[start..end].iter().collect::<String>()
    } else {
        pending_name(&before, flavor)?
    };
    if name.is_empty() {
        return None;
    }
    if flavor.html && VOID.contains(&name.to_ascii_lowercase().as_str()) {
        return None;
    }
    let same_close = format!("</{name}");
    if after
        .to_ascii_lowercase()
        .starts_with(&same_close.to_ascii_lowercase())
    {
        return None;
    }
    Some(format!("</{name}>"))
}

/// Name of the tag still open at the end of `before`. The whole window is
/// read first so comments and strings keep their meaning; only when an
/// earlier stray `<` swallowed the rest (`a <b ... <div`) does it retry from
/// the nearest `<`.
fn pending_name(before: &[char], flavor: Flavor) -> Option<String> {
    let (start, end) = scan(before, flavor).pending?;
    let nearest = before.iter().rposition(|&c| c == '<')?;
    if start == nearest + 1 {
        return Some(before[start..end].iter().collect());
    }
    let mut upto = before.len();
    for _ in 0..MAX_CANDIDATES {
        let lt = before[..upto].iter().rposition(|&c| c == '<')?;
        let base = lt.saturating_sub(1);
        let slice = &before[base..];
        if let Some((s, e)) = scan(slice, flavor).pending
            && s == lt - base + 1
        {
            return Some(slice[s..e].iter().collect());
        }
        upto = lt;
    }
    None
}

/// The text completing `</` just typed (the `<` is at `caret - 1`): the name
/// of the nearest unclosed tag, e.g. `/div>`.
pub fn closing_after_slash(doc: &Document, caret: usize, flavor: Flavor) -> Option<String> {
    if caret == 0 || doc.len_chars() > MAX_DOC_CHARS || doc.rope().char(caret - 1) != '<' {
        return None;
    }
    // No generic check here: `</` can only start a closing tag, even right
    // after text (`<p>hello</`).
    let all = chars_between(doc, 0, caret - 1);
    let found = scan(&all, flavor);
    if found.in_php || found.pending.is_some() {
        return None;
    }
    let (start, end) = *unclosed(&found.tags, &all, flavor).last()?;
    let name: String = all[start..end].iter().collect();
    let after_name = doc
        .rope()
        .slice(caret..doc.len_chars().min(caret + 64))
        .to_string();
    if !name.is_empty()
        && after_name
            .to_ascii_lowercase()
            .starts_with(&name.to_ascii_lowercase())
    {
        return None;
    }
    Some(format!("/{name}>"))
}

/// When the caret is in a tag name, the name range there and the range of
/// its partner's name (open <-> close).
#[allow(clippy::type_complexity)]
pub fn linked_names(
    doc: &Document,
    caret: usize,
    flavor: Flavor,
) -> Option<((usize, usize), (usize, usize))> {
    let len = doc.len_chars();
    if len > MAX_DOC_CHARS {
        return None;
    }
    let rope = doc.rope();
    let mut start = caret;
    while start > 0 && is_name_char(rope.char(start - 1)) {
        start -= 1;
    }
    let mut end = caret;
    while end < len && is_name_char(rope.char(end)) {
        end += 1;
    }
    if start == end || !rope.char(start).is_alphabetic() {
        return None;
    }
    let after_lt = start > 0
        && (rope.char(start - 1) == '<'
            || (rope.char(start - 1) == '/' && start > 1 && rope.char(start - 2) == '<'));
    if !after_lt {
        return None;
    }
    let chars: Vec<char> = rope.chars().collect();
    let found = scan(&chars, flavor);
    pair_of(&found.tags, &chars, flavor, start)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HTML: Flavor = Flavor {
        html: true,
        jsx: false,
        php: false,
    };
    const JSX: Flavor = Flavor {
        html: false,
        jsx: true,
        php: false,
    };
    const PHP: Flavor = Flavor {
        html: true,
        jsx: false,
        php: true,
    };

    /// Splits at `|`: a document and the caret in it.
    fn at(marked: &str) -> (Document, usize) {
        let caret = marked.chars().take_while(|c| *c != '|').count();
        (Document::from_text(&marked.replacen('|', "", 1)), caret)
    }

    fn after_gt(marked: &str, flavor: Flavor) -> Option<String> {
        let (doc, caret) = at(marked);
        closing_after_gt(&doc, caret, flavor)
    }

    fn after_slash(marked: &str, flavor: Flavor) -> Option<String> {
        let (doc, caret) = at(marked);
        closing_after_slash(&doc, caret, flavor)
    }

    #[test]
    fn language_support() {
        assert!(Flavor::for_language(SyntaxLanguage::Html).is_some());
        assert!(Flavor::for_language(SyntaxLanguage::Tsx).is_some());
        assert!(Flavor::for_language(SyntaxLanguage::JavaScript).is_some());
        assert!(Flavor::for_language(SyntaxLanguage::Php).is_some());
        assert!(Flavor::for_language(SyntaxLanguage::TypeScript).is_none());
        assert!(Flavor::for_language(SyntaxLanguage::Rust).is_none());
    }

    #[test]
    fn closes_the_tag_being_typed() {
        assert_eq!(after_gt("<div|", HTML).as_deref(), Some("</div>"));
        assert_eq!(
            after_gt("<div class=\"a\" id='b'|", HTML).as_deref(),
            Some("</div>")
        );
        assert_eq!(after_gt("<p>text <span|", HTML).as_deref(), Some("</span>"));
        assert_eq!(
            after_gt("<my-element|", HTML).as_deref(),
            Some("</my-element>")
        );
        assert_eq!(after_gt("<DIV|", HTML).as_deref(), Some("</DIV>"));
        assert_eq!(
            after_gt("<a title=\"x > y\"|", HTML).as_deref(),
            Some("</a>")
        );
    }

    #[test]
    fn leaves_void_self_closing_and_already_closed_tags_alone() {
        assert_eq!(after_gt("<br|", HTML), None);
        assert_eq!(after_gt("<IMG src=x|", HTML), None);
        assert_eq!(after_gt("<input type=\"text\"|", HTML), None);
        assert_eq!(after_gt("<div /|", HTML), None);
        assert_eq!(after_gt("<Foo bar={1}/|", JSX), None);
        assert_eq!(after_gt("<div|</div>", HTML), None);
        assert_eq!(after_gt("<!DOCTYPE html|", HTML), None);
        assert_eq!(after_gt("<!-- <div| -->", HTML), None);
        assert_eq!(after_gt("<div>|", HTML), None, "`>` already ended the tag");
        assert_eq!(after_gt("text|", HTML), None);
    }

    #[test]
    fn jsx_expressions_arrows_and_generics() {
        assert_eq!(
            after_gt("const a = <div onClick={() => go()}|", JSX).as_deref(),
            Some("</div>")
        );
        assert_eq!(
            after_gt("<Foo.Bar x={a > b}|", JSX).as_deref(),
            Some("</Foo.Bar>")
        );
        assert_eq!(after_gt("return <|", JSX).as_deref(), Some("</>"));
        assert_eq!(after_gt("const [a] = useState<string|", JSX), None);
        assert_eq!(after_gt("let x: Array<|", JSX), None);
        assert_eq!(after_gt("if (a < b|", JSX), None);
        assert_eq!(after_gt("<|", HTML), None, "no fragments in HTML");
    }

    #[test]
    fn comparison_before_the_tag_does_not_hide_it() {
        assert_eq!(
            after_gt("<div title={a < b ? 1 : 2}|", JSX).as_deref(),
            Some("</div>")
        );
    }

    #[test]
    fn php_code_is_not_markup() {
        assert_eq!(after_gt("<?php if ($a <b|", PHP), None);
        assert_eq!(
            after_gt("<?php echo 1; ?>\n<div|", PHP).as_deref(),
            Some("</div>")
        );
        assert_eq!(after_gt("<?php $x->y|", PHP), None);
    }

    #[test]
    fn completes_a_closing_tag_with_the_nearest_open_name() {
        assert_eq!(
            after_slash("<ul><li>a</li><|", HTML).as_deref(),
            Some("/ul>")
        );
        assert_eq!(
            after_slash("<div><span>x<|", HTML).as_deref(),
            Some("/span>")
        );
        assert_eq!(
            after_slash("<div><br><img src=x><|", HTML).as_deref(),
            Some("/div>")
        );
        assert_eq!(
            after_slash("<div><!-- <b> --><|", HTML).as_deref(),
            Some("/div>")
        );
        assert_eq!(after_slash("<div></div><|", HTML), None, "nothing open");
        assert_eq!(after_slash("<>a<|", JSX).as_deref(), Some("/>"));
        assert_eq!(after_slash("<A><B/><|", JSX).as_deref(), Some("/A>"));
        assert_eq!(
            after_slash("<div>hello<|", JSX).as_deref(),
            Some("/div>"),
            "text right before `</`"
        );
        assert_eq!(after_slash("<p>hi<|", HTML).as_deref(), Some("/p>"));
        assert_eq!(after_slash("a<|", JSX), None, "nothing open");
        assert_eq!(
            after_slash("<div>x</|", HTML),
            None,
            "slash not typed after `<`"
        );
    }

    #[test]
    fn script_bodies_are_skipped() {
        assert_eq!(
            after_slash("<body><script>if (a<b) { x = '<div>'; }</script><|", HTML).as_deref(),
            Some("/body>")
        );
    }

    #[test]
    fn php_blocks_hide_their_less_than_signs() {
        assert_eq!(
            after_slash("<div><?php if ($a <b) { ?>x<?php } ?><|", PHP).as_deref(),
            Some("/div>")
        );
        assert_eq!(after_slash("<div><?php foo(<|", PHP), None);
    }

    fn names(marked: &str, flavor: Flavor) -> Option<(String, String)> {
        let (doc, caret) = at(marked);
        let (own, partner) = linked_names(&doc, caret, flavor)?;
        let text = doc.text();
        let pick = |r: (usize, usize)| text.chars().skip(r.0).take(r.1 - r.0).collect::<String>();
        Some((pick(own), pick(partner)))
    }

    #[test]
    fn links_open_and_close_names_in_both_directions() {
        let both = Some(("div".to_owned(), "div".to_owned()));
        assert_eq!(names("<di|v>x</div>", HTML), both);
        assert_eq!(names("<div|>x</div>", HTML), both);
        assert_eq!(names("<|div>x</div>", HTML), both);
        assert_eq!(names("<div>x</d|iv>", HTML), both);
        assert_eq!(names("<div>x</div|>", HTML), both);
    }

    #[test]
    fn linking_pairs_by_nesting_and_ignores_everything_else() {
        let (doc, caret) = at("<div|><div></div></div>");
        let (own, partner) = linked_names(&doc, caret, HTML).unwrap();
        assert_eq!(own, (1, 4));
        assert_eq!(partner, (18, 21), "outer open pairs with the last close");

        assert_eq!(names("<div|>never closed", HTML), None);
        assert_eq!(
            names("<div class=\"ab|c\">x</div>", HTML),
            None,
            "attribute, not a name"
        );
        assert_eq!(names("<br|>", HTML), None);
        assert_eq!(names("<p>te|xt</p>", HTML), None);
        assert_eq!(names("<Foo|>x</Foo>", JSX).unwrap().1, "Foo");
        assert_eq!(names("<>x|</>", JSX), None, "fragments have no name");
        assert_eq!(
            names("<DIV|>x</div>", HTML).unwrap().1,
            "div",
            "HTML names ignore case"
        );
        assert_eq!(
            names("<DIV|>x</div>", JSX),
            None,
            "JSX names are case sensitive"
        );
    }
}
