//! Blade template highlighting layered over the HTML grammar.
//!
//! There is no Tree-sitter Blade grammar bundled, so `.blade.php` files are
//! parsed as HTML and this scanner adds what HTML cannot see: `{{ }}` and
//! `{!! !!}` echoes, `{{-- --}}` comments, `@directives` with their
//! arguments, `@php` blocks and `<?php ?>` tags. The PHP inside those spans is
//! colored by a small token scanner (strings, variables, numbers, calls, a few
//! keywords), not a full PHP parse.
//!
//! The scan works on bytes of the document and returns two things: the byte
//! ranges Blade owns (`claimed`) and the colors inside them (`spans`). The
//! caller removes HTML styling from claimed ranges so, for example,
//! `href="{{ asset('a') }}"` keeps green quotes around a Blade expression
//! instead of one flat string.

use crate::syntax::HighlightScope;

/// `true` for the file name of a Blade template.
pub fn is_blade_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".blade.php")
}

/// A colored byte range of the scanned text.
pub type Span = (usize, usize, HighlightScope);

#[derive(Debug, Default)]
pub struct Overlay {
    /// Sorted, non-overlapping byte ranges that Blade (not HTML) styles.
    pub claimed: Vec<(usize, usize)>,
    pub spans: Vec<Span>,
}

/// Directives Blade compiles. Anything else after `@` (`@media`, `@font-face`,
/// `@Component`) is left alone so CSS and JS at-rules keep their own colors.
const DIRECTIVES: &[&str] = &[
    "if",
    "elseif",
    "else",
    "endif",
    "unless",
    "endunless",
    "isset",
    "endisset",
    "empty",
    "endempty",
    "auth",
    "endauth",
    "guest",
    "endguest",
    "production",
    "endproduction",
    "env",
    "endenv",
    "hasSection",
    "sectionMissing",
    "switch",
    "case",
    "default",
    "endswitch",
    "for",
    "endfor",
    "foreach",
    "endforeach",
    "forelse",
    "endforelse",
    "while",
    "endwhile",
    "continue",
    "break",
    "include",
    "includeIf",
    "includeWhen",
    "includeUnless",
    "includeFirst",
    "each",
    "extends",
    "yield",
    "section",
    "endsection",
    "show",
    "stop",
    "overwrite",
    "append",
    "parent",
    "push",
    "endpush",
    "pushIf",
    "endPushIf",
    "pushOnce",
    "endPushOnce",
    "prepend",
    "endprepend",
    "prependOnce",
    "endPrependOnce",
    "stack",
    "slot",
    "endslot",
    "component",
    "endcomponent",
    "props",
    "aware",
    "class",
    "style",
    "checked",
    "selected",
    "disabled",
    "readonly",
    "required",
    "csrf",
    "method",
    "error",
    "enderror",
    "once",
    "endonce",
    "php",
    "endphp",
    "verbatim",
    "endverbatim",
    "json",
    "js",
    "vite",
    "viteReactRefresh",
    "lang",
    "endlang",
    "inject",
    "can",
    "cannot",
    "canany",
    "elsecan",
    "elsecannot",
    "elsecanany",
    "endcan",
    "endcannot",
    "endcanany",
    "session",
    "endsession",
    "fragment",
    "endfragment",
    "livewire",
    "livewireStyles",
    "livewireScripts",
    "livewireScriptConfig",
    "dd",
    "dump",
    "use",
    "teleport",
    "endteleport",
    "bool",
    "unlessauth",
    "choice",
    "persist",
    "endpersist",
    "island",
    "endisland",
];

const KEYWORDS: &[&str] = &[
    "as",
    "new",
    "fn",
    "function",
    "return",
    "use",
    "and",
    "or",
    "xor",
    "instanceof",
    "isset",
    "empty",
    "echo",
    "print",
    "match",
    "static",
    "self",
    "parent",
    "if",
    "else",
    "elseif",
    "foreach",
    "for",
    "while",
    "unset",
    "list",
    "global",
    "clone",
    "throw",
    "yield",
];

const CONSTANTS: &[&str] = &["true", "false", "null"];

/// Where a token scanner stops.
#[derive(Clone, Copy)]
enum Stop<'a> {
    /// Just before this text, outside strings and comments.
    Str(&'a [u8]),
    /// At the `)` matching an already consumed `(`.
    Paren,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum After {
    None,
    /// Right after `->` or `?->`.
    Arrow,
    /// Right after `::`.
    Static,
}

pub fn scan(src: &str) -> Overlay {
    let b = src.as_bytes();
    let mut out = Overlay::default();
    let mut i = 0;
    while i < b.len() {
        let prev = if i == 0 { None } else { Some(b[i - 1]) };
        i = match b[i] {
            b'{' => brace(b, i, prev, &mut out),
            b'@' => at(b, i, prev, &mut out),
            b'<' => php_tag(b, i, &mut out),
            _ => i + 1,
        };
    }
    out
}

/// Drops the parts of `spans` that Blade owns. `claimed` must be sorted and
/// non-overlapping, as [`scan`] returns it.
pub fn subtract(spans: Vec<Span>, claimed: &[(usize, usize)]) -> Vec<Span> {
    let mut out = Vec::with_capacity(spans.len());
    for (start, end, scope) in spans {
        let mut cursor = start;
        let first = claimed.partition_point(|c| c.1 <= start);
        for &(claim_start, claim_end) in &claimed[first..] {
            if claim_start >= end {
                break;
            }
            if claim_start > cursor {
                out.push((cursor, claim_start, scope));
            }
            cursor = cursor.max(claim_end);
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            out.push((cursor, end, scope));
        }
    }
    out
}

/// `{{ }}`, `{!! !!}` and `{{-- --}}`. Unterminated openers are plain text so
/// a half-typed `{{ $x` does not recolor the rest of the file.
fn brace(b: &[u8], i: usize, prev: Option<u8>, out: &mut Overlay) -> usize {
    // `@{{ name }}` is Blade's escape for a literal echo.
    if prev == Some(b'@') {
        return i + 1;
    }
    let rest = &b[i..];
    let (open, close, comment): (usize, &[u8], bool) = if rest.starts_with(b"{{--") {
        (4, b"--}}", true)
    } else if rest.starts_with(b"{!!") {
        (3, b"!!}", false)
    } else if rest.starts_with(b"{{") {
        (2, b"}}", false)
    } else {
        return i + 1;
    };
    if comment {
        return match find(b, i + open, close) {
            Some(at) => {
                let end = at + close.len();
                out.claimed.push((i, end));
                out.spans.push((i, end, HighlightScope::Comment));
                end
            }
            None => i + open,
        };
    }
    let mark = out.spans.len();
    match code(b, i + open, Stop::Str(close), &mut out.spans) {
        Some(at) => {
            let end = at + close.len();
            out.spans.push((i, i + open, HighlightScope::Punctuation));
            out.spans.push((at, end, HighlightScope::Punctuation));
            out.claimed.push((i, end));
            end
        }
        None => {
            out.spans.truncate(mark);
            i + open
        }
    }
}

/// `@directive`, with `(arguments)` and `@php ... @endphp` bodies.
fn at(b: &[u8], i: usize, prev: Option<u8>, out: &mut Overlay) -> usize {
    // `user@example.com` is not a directive.
    if prev.is_some_and(is_word) {
        return i + 1;
    }
    match b.get(i + 1) {
        Some(c) if c.is_ascii_alphabetic() => {}
        // `@@name` prints a literal `@name`.
        Some(b'@') => return i + 2,
        _ => return i + 1,
    }
    let name_end = i + 1 + b[i + 1..].iter().take_while(|c| is_word(**c)).count();
    let name = &b[i + 1..name_end];
    if !DIRECTIVES.iter().any(|d| d.as_bytes() == name) {
        return name_end;
    }
    out.spans.push((i, name_end, HighlightScope::Keyword));

    if name == b"verbatim" {
        // Blade leaves everything up to `@endverbatim` alone.
        out.claimed.push((i, name_end));
        return find(b, name_end, b"@endverbatim").unwrap_or(b.len());
    }

    let mut end = name_end;
    let mut j = name_end;
    while matches!(b.get(j), Some(b' ' | b'\t')) {
        j += 1;
    }
    let mark = out.spans.len();
    if b.get(j) == Some(&b'(') {
        match code(b, j + 1, Stop::Paren, &mut out.spans) {
            Some(close) => {
                out.spans.push((j, j + 1, HighlightScope::Punctuation));
                out.spans
                    .push((close, close + 1, HighlightScope::Punctuation));
                end = close + 1;
            }
            None => out.spans.truncate(mark),
        }
    } else if name == b"php" {
        match code(b, name_end, Stop::Str(b"@endphp"), &mut out.spans) {
            Some(close) => end = close,
            None => out.spans.truncate(mark),
        }
    }
    out.claimed.push((i, end));
    end
}

/// `<?php ... ?>` and `<?= ... ?>`. Without `?>` the PHP runs to the end of
/// the file, as it does for PHP itself.
fn php_tag(b: &[u8], i: usize, out: &mut Overlay) -> usize {
    let rest = &b[i..];
    let open = if rest.starts_with(b"<?php") && b.get(i + 5).is_none_or(|c| c.is_ascii_whitespace())
    {
        5
    } else if rest.starts_with(b"<?=") {
        3
    } else {
        return i + 1;
    };
    out.spans.push((i, i + open, HighlightScope::Tag));
    match code(b, i + open, Stop::Str(b"?>"), &mut out.spans) {
        Some(at) => {
            out.spans.push((at, at + 2, HighlightScope::Tag));
            out.claimed.push((i, at + 2));
            at + 2
        }
        None => {
            out.claimed.push((i, b.len()));
            b.len()
        }
    }
}

/// Colors PHP-ish tokens from `i` until `stop`. Returns where it stopped, or
/// `None` at the end of the text.
fn code(b: &[u8], mut i: usize, stop: Stop, out: &mut Vec<Span>) -> Option<usize> {
    let mut depth = 0usize;
    let mut after = After::None;
    while i < b.len() {
        if at_stop(b, i, stop) {
            return Some(i);
        }
        // `->` and `::` color only the word right after them.
        let before = std::mem::replace(&mut after, After::None);
        match b[i] {
            quote @ (b'\'' | b'"') => {
                let end = string_end(b, i, quote);
                out.push((i, end, HighlightScope::String));
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let start = i;
                while i < b.len() && b[i] != b'\n' && !at_stop(b, i, stop) {
                    i += 1;
                }
                out.push((start, i, HighlightScope::Comment));
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = find(b, i + 2, b"*/").map_or(b.len(), |at| at + 2);
                out.push((i, end, HighlightScope::Comment));
                i = end;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                if depth == 0 {
                    if matches!(stop, Stop::Paren) {
                        return Some(i);
                    }
                } else {
                    depth -= 1;
                }
                i += 1;
            }
            b'$' => {
                let n = b[i + 1..]
                    .iter()
                    .take_while(|c| is_word(**c) || **c >= 0x80)
                    .count();
                if n > 0 {
                    out.push((i, i + 1 + n, HighlightScope::Variable));
                    i += 1 + n;
                } else {
                    i += 1;
                }
            }
            b'-' if b.get(i + 1) == Some(&b'>') => {
                after = After::Arrow;
                i += 2;
                continue;
            }
            b'?' if b[i..].starts_with(b"?->") => {
                after = After::Arrow;
                i += 3;
                continue;
            }
            b':' if b.get(i + 1) == Some(&b':') => {
                after = After::Static;
                i += 2;
                continue;
            }
            c if c.is_ascii_digit() => {
                let n = b[i..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_' || **c == b'.')
                    .count();
                out.push((i, i + n, HighlightScope::Number));
                i += n;
            }
            c if c.is_ascii_whitespace() => {
                after = before;
                i += 1;
            }
            c if is_word(c) || c >= 0x80 => {
                let n = word_len(b, i);
                let mut k = i + n;
                while matches!(b.get(k), Some(b' ' | b'\t')) {
                    k += 1;
                }
                let paren = b.get(k) == Some(&b'(');
                if let Some(scope) = classify(&b[i..i + n], before, paren) {
                    out.push((i, i + n, scope));
                }
                i += n;
            }
            _ => i += 1,
        }
    }
    None
}

fn classify(word: &[u8], after: After, paren: bool) -> Option<HighlightScope> {
    match after {
        After::Arrow => {
            return Some(if paren {
                HighlightScope::Function
            } else {
                HighlightScope::Property
            });
        }
        After::Static => {
            return if paren {
                Some(HighlightScope::Function)
            } else if word
                .iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
            {
                Some(HighlightScope::Constant)
            } else {
                None
            };
        }
        After::None => {}
    }
    let is = |list: &[&str]| list.iter().any(|w| w.as_bytes().eq_ignore_ascii_case(word));
    if is(CONSTANTS) {
        Some(HighlightScope::Constant)
    } else if is(KEYWORDS) {
        Some(HighlightScope::Keyword)
    } else if paren {
        Some(HighlightScope::Function)
    } else if word[0].is_ascii_uppercase() || word[0] == b'\\' {
        Some(HighlightScope::Type)
    } else {
        None
    }
}

fn at_stop(b: &[u8], i: usize, stop: Stop) -> bool {
    matches!(stop, Stop::Str(text) if b[i..].starts_with(text))
}

/// End of the string starting at `i`, or the end of the text if unterminated.
fn string_end(b: &[u8], i: usize, quote: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            c if c == quote => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// Length of the identifier at `i`, including `\` between namespace segments.
fn word_len(b: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < b.len() {
        let c = b[j];
        let namespace_sep = c == b'\\' && b.get(j + 1).is_some_and(|n| is_word(*n));
        if is_word(c) || c >= 0x80 || namespace_sep {
            j += 1;
        } else {
            break;
        }
    }
    j.max(i + 1) - i
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > b.len() {
        return None;
    }
    b[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| at + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts<'a>(src: &'a str, overlay: &Overlay, scope: HighlightScope) -> Vec<&'a str> {
        let mut spans: Vec<_> = overlay.spans.iter().filter(|s| s.2 == scope).collect();
        spans.sort_by_key(|s| (s.0, s.1));
        spans.iter().map(|s| &src[s.0..s.1]).collect()
    }

    fn claimed<'a>(src: &'a str, overlay: &Overlay) -> Vec<&'a str> {
        overlay.claimed.iter().map(|c| &src[c.0..c.1]).collect()
    }

    #[test]
    fn echo_colors_delimiters_variables_and_strings() {
        let src = "<p>{{ $title ?? 'Good' }}</p>";
        let o = scan(src);
        assert_eq!(texts(src, &o, HighlightScope::Punctuation), ["{{", "}}"]);
        assert_eq!(texts(src, &o, HighlightScope::Variable), ["$title"]);
        assert_eq!(texts(src, &o, HighlightScope::String), ["'Good'"]);
        assert_eq!(claimed(src, &o), ["{{ $title ?? 'Good' }}"]);
    }

    #[test]
    fn raw_echo_and_function_calls() {
        let src = "{!! config('app.name', 'x') !!}";
        let o = scan(src);
        assert_eq!(texts(src, &o, HighlightScope::Punctuation), ["{!!", "!!}"]);
        assert_eq!(texts(src, &o, HighlightScope::Function), ["config"]);
        assert_eq!(
            texts(src, &o, HighlightScope::String),
            ["'app.name'", "'x'"]
        );
    }

    #[test]
    fn closing_braces_inside_strings_do_not_end_the_echo() {
        let src = "{{ '}}' . $a }}";
        let o = scan(src);
        assert_eq!(claimed(src, &o), [src]);
    }

    #[test]
    fn directives_with_arguments() {
        let src = "@yield('custom_style')\n@foreach ($items as $item)\n@endforeach";
        let o = scan(src);
        assert_eq!(
            texts(src, &o, HighlightScope::Keyword),
            ["@yield", "@foreach", "as", "@endforeach"]
        );
        assert_eq!(
            texts(src, &o, HighlightScope::Variable),
            ["$items", "$item"]
        );
        assert_eq!(
            claimed(src, &o),
            [
                "@yield('custom_style')",
                "@foreach ($items as $item)",
                "@endforeach"
            ]
        );
    }

    #[test]
    fn nested_parentheses_and_strings_in_arguments() {
        let src = "@if (in_array($a, ['x)', 'y']) && ($b))\nok\n@endif";
        let o = scan(src);
        assert_eq!(
            claimed(src, &o)[0],
            "@if (in_array($a, ['x)', 'y']) && ($b))"
        );
    }

    #[test]
    fn at_rules_emails_and_escapes_stay_plain() {
        let src = "@media (min-width: 1px) a@b.com @@if @{{ x }}";
        let o = scan(src);
        assert!(o.claimed.is_empty(), "{:?}", claimed(src, &o));
        assert!(o.spans.is_empty());
    }

    #[test]
    fn unterminated_openers_are_plain_text() {
        for src in ["{{ $x", "{!! $x", "{{-- note", "@if ($x", "@php $x = 1;"] {
            let o = scan(src);
            // At most the directive name is claimed; never the rest of the file.
            assert!(o.claimed.iter().all(|c| c.1 <= 4), "{src}: {:?}", o.claimed);
        }
    }

    #[test]
    fn comments_may_span_lines() {
        let src = "a {{-- one\ntwo --}} b";
        let o = scan(src);
        assert_eq!(
            texts(src, &o, HighlightScope::Comment),
            ["{{-- one\ntwo --}}"]
        );
    }

    #[test]
    fn php_block_is_colored_and_claimed() {
        let src = "@php\n  $n = count($list);\n@endphp";
        let o = scan(src);
        assert_eq!(texts(src, &o, HighlightScope::Keyword), ["@php", "@endphp"]);
        assert_eq!(texts(src, &o, HighlightScope::Variable), ["$n", "$list"]);
        assert_eq!(texts(src, &o, HighlightScope::Function), ["count"]);
        assert_eq!(claimed(src, &o)[0], "@php\n  $n = count($list);\n");
    }

    #[test]
    fn verbatim_content_is_left_alone() {
        let src = "@verbatim {{ $x }} @endverbatim";
        let o = scan(src);
        assert_eq!(
            texts(src, &o, HighlightScope::Keyword),
            ["@verbatim", "@endverbatim"]
        );
        assert!(texts(src, &o, HighlightScope::Variable).is_empty());
    }

    #[test]
    fn php_tags() {
        let src = "<?php echo $a; ?>x<?= $b ?>";
        let o = scan(src);
        assert_eq!(
            texts(src, &o, HighlightScope::Tag),
            ["<?php", "?>", "<?=", "?>"]
        );
        assert_eq!(texts(src, &o, HighlightScope::Variable), ["$a", "$b"]);
        assert_eq!(texts(src, &o, HighlightScope::Keyword), ["echo"]);
    }

    #[test]
    fn members_and_constants() {
        let src = "{{ $user->name }} {{ Str::limit($a, 3) }} {{ Foo::BAR }} {{ true }}";
        let o = scan(src);
        assert_eq!(texts(src, &o, HighlightScope::Property), ["name"]);
        assert_eq!(texts(src, &o, HighlightScope::Function), ["limit"]);
        assert_eq!(texts(src, &o, HighlightScope::Type), ["Str", "Foo"]);
        assert_eq!(texts(src, &o, HighlightScope::Constant), ["BAR", "true"]);
        assert_eq!(texts(src, &o, HighlightScope::Number), ["3"]);
    }

    #[test]
    fn subtract_splits_spans_around_claimed_ranges() {
        let spans = vec![(0, 10, HighlightScope::String)];
        let cut = subtract(spans, &[(3, 5), (7, 12)]);
        assert_eq!(
            cut,
            [
                (0, 3, HighlightScope::String),
                (5, 7, HighlightScope::String)
            ]
        );
        assert!(subtract(vec![(4, 5, HighlightScope::Tag)], &[(0, 10)]).is_empty());
        assert_eq!(
            subtract(vec![(0, 2, HighlightScope::Tag)], &[(5, 6)]),
            [(0, 2, HighlightScope::Tag)]
        );
    }

    #[test]
    fn multibyte_text_keeps_offsets_on_char_boundaries() {
        let src = "é {{ 'ü' . $é }} ü";
        let o = scan(src);
        for (s, e, _) in &o.spans {
            assert!(
                src.is_char_boundary(*s) && src.is_char_boundary(*e),
                "{s}..{e}"
            );
        }
    }
}
