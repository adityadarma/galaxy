//! Finds the strings in PHP and Blade text that name something in a Laravel
//! project: `config('app.name')`, `route('home')`, `@include('layout')`,
//! `->middleware('auth')`, `[UserController::class, 'index']` and so on.
//!
//! This is a text scan, not a parse, so it also works on the half-typed
//! code completion is asked about and on Blade, which no grammar covers.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

/// What a string refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Config,
    Env,
    Route,
    View,
    Translation,
    Asset,
    /// A file below the project folder named for Vite.
    Vite,
    Middleware,
    Ability,
    Binding,
    Page,
    Livewire,
    Rule,
    /// A column of the model a query is built on.
    Column,
    /// A relationship method of the model a query is built on.
    Relation,
    /// A method of the controller named before it in `[X::class, '…']`.
    Action,
    /// A key one level below `section` in a config file, such as
    /// `filesystems.disks` for `Storage::disk('…')`.
    Section(&'static str),
    /// A path below the project folder, which the helper names (`app_path`).
    Path(&'static str),
}

/// A string and what it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub kind: Kind,
    /// Byte range of the contents, without the quotes.
    pub start: usize,
    pub end: usize,
    /// For [`Kind::Action`]: the class as written, before `::class`.
    pub class: Option<String>,
    /// For [`Kind::Env`]: the call passes a fallback value.
    pub has_default: bool,
    /// The `(` of the call the string is an argument of.
    pub paren: Option<usize>,
}

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

/// Directives that name a view, a translation and so on, with the index of
/// the argument that does.
const DIRECTIVES: &[(&str, Kind, usize)] = &[
    ("@include", Kind::View, 0),
    ("@includeif", Kind::View, 0),
    ("@includewhen", Kind::View, 1),
    ("@includeunless", Kind::View, 1),
    ("@includefirst", Kind::View, 0),
    ("@extends", Kind::View, 0),
    ("@each", Kind::View, 0),
    ("@component", Kind::View, 0),
    ("@lang", Kind::Translation, 0),
    ("@choice", Kind::Translation, 0),
    ("@can", Kind::Ability, 0),
    ("@cannot", Kind::Ability, 0),
    ("@cant", Kind::Ability, 0),
    ("@canany", Kind::Ability, 0),
    ("@elsecan", Kind::Ability, 0),
    ("@elsecannot", Kind::Ability, 0),
    ("@livewire", Kind::Livewire, 0),
    ("@vite", Kind::Vite, 0),
];

/// Helpers that take the name as their first argument.
fn function_kind(name: &str) -> Option<Kind> {
    Some(match name {
        "config" => Kind::Config,
        "env" => Kind::Env,
        "route" | "to_route" => Kind::Route,
        "view" => Kind::View,
        "trans" | "__" | "trans_choice" | "lang" => Kind::Translation,
        "asset" | "secure_asset" | "mix" => Kind::Asset,
        "app" | "resolve" => Kind::Binding,
        "inertia" => Kind::Page,
        "base_path" => Kind::Path(""),
        "app_path" => Kind::Path("app"),
        "config_path" => Kind::Path("config"),
        "database_path" => Kind::Path("database"),
        "public_path" => Kind::Path("public"),
        "resource_path" => Kind::Path("resources"),
        "storage_path" => Kind::Path("storage"),
        "lang_path" => Kind::Path("lang"),
        _ => return None,
    })
}

/// `Facade::method('…')` calls.
fn static_kind(scope: &str, name: &str) -> Option<Kind> {
    Some(match (scope, name) {
        (
            "config",
            "get" | "has" | "string" | "integer" | "float" | "boolean" | "array" | "set",
        ) => Kind::Config,
        ("route", "has") => Kind::Route,
        ("redirect" | "url", "route" | "signedroute" | "temporarysignedroute") => Kind::Route,
        ("route", "middleware" | "withoutmiddleware") => Kind::Middleware,
        ("view", "make" | "exists" | "first" | "composer" | "creator") => Kind::View,
        ("lang", "get" | "has" | "choice" | "hasforlocale") => Kind::Translation,
        (
            "gate",
            "allows" | "denies" | "check" | "any" | "none" | "authorize" | "inspect" | "has",
        ) => Kind::Ability,
        ("storage", "disk" | "fake" | "persistentfake") => Kind::Section("filesystems.disks"),
        ("db", "connection") => Kind::Section("database.connections"),
        ("cache", "store" | "driver") => Kind::Section("cache.stores"),
        ("log", "channel") => Kind::Section("logging.channels"),
        ("auth", "guard") => Kind::Section("auth.guards"),
        ("mail", "mailer") => Kind::Section("mail.mailers"),
        ("queue", "connection") => Kind::Section("queue.connections"),
        ("inertia", "render") => Kind::Page,
        ("vite", "asset") => Kind::Vite,
        ("livewire", "test") => Kind::Livewire,
        ("app", "make" | "bound" | "makewith") => Kind::Binding,
        ("validator", "make") => Kind::Rule,
        _ => return None,
    })
}

/// `$x->method('…')` calls; `receiver` is the text before the arrow.
fn method_kind(receiver: &str, name: &str) -> Option<Kind> {
    Some(match name {
        "middleware" | "withoutmiddleware" => Kind::Middleware,
        "can" | "cannot" | "cant" | "authorize" | "canany" => Kind::Ability,
        "route" if receiver.ends_with("redirect()") || receiver.ends_with("url()") => Kind::Route,
        "make" | "bound" | "makewith"
            if receiver.ends_with("app()") || receiver.ends_with("->app") || receiver == "$app" =>
        {
            Kind::Binding
        }
        "disk" if receiver.ends_with("storage") => Kind::Section("filesystems.disks"),
        _ => return None,
    })
}

/// Query builder and model methods whose first argument names a column or
/// a relationship.
fn eloquent_kind(name: &str) -> Option<Kind> {
    Some(match name {
        "where" | "orwhere" | "wherein" | "wherenotin" | "orwherein" | "wherenull"
        | "wherenotnull" | "ornwherenull" | "wherebetween" | "wherenotbetween" | "wheredate"
        | "wheremonth" | "whereday" | "whereyear" | "wheretime" | "wherecolumn" | "orderby"
        | "orderbydesc" | "latest" | "oldest" | "pluck" | "groupby" | "value" | "increment"
        | "decrement" | "sum" | "avg" | "min" | "max" | "wherelike" | "wherenot" => Kind::Column,
        "with" | "load" | "loadmissing" | "has" | "orhas" | "doesnthave" | "ordoesnthave"
        | "wherehas" | "orwherehas" | "wheredoesnthave" | "withcount" | "withsum" | "withavg"
        | "withmin" | "withmax" | "withexists" | "loadcount" | "without" | "withwhererelation"
        | "wherehasmorph" | "whererelation" => Kind::Relation,
        _ => return None,
    })
}

/// Methods whose arguments hold validation rules.
const RULE_MARKERS: &[&str] = &[
    "function rules",
    "validate(",
    "validatewithbag(",
    "validator::make",
    "$rules",
];

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

/// PHP stretches of a Blade file: echoes, `@php` blocks, `<?php` tags and
/// the argument lists of directives.
fn blade_regions(text: &str) -> Vec<Region> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &text[i..];
        if rest.starts_with("{{--") {
            i += rest.find("--}}").map_or(rest.len(), |n| n + 4);
        } else if rest.starts_with("{{") || rest.starts_with("{!!") {
            let (skip, close) = if rest.starts_with("{{") {
                (2, "}}")
            } else {
                (3, "!!}")
            };
            let end = rest[skip..]
                .find(close)
                .map_or(text.len(), |n| i + skip + n);
            out.push(Region {
                start: i + skip,
                end,
            });
            i = (end + close.len()).min(text.len());
        } else if rest.starts_with("<?") {
            out.extend(php_regions(&text[i..]).into_iter().take(1).map(|r| Region {
                start: i + r.start,
                end: i + r.end,
            }));
            i += rest.find("?>").map_or(rest.len(), |n| n + 2);
        } else if bytes[i] == b'@' && i + 1 < bytes.len() && bytes[i + 1] == b'@' {
            i += 2;
        } else if bytes[i] == b'@' && (i == 0 || !is_word(bytes[i - 1])) {
            let name_len = rest[1..]
                .bytes()
                .take_while(|c| c.is_ascii_alphanumeric())
                .count();
            let after = i + 1 + name_len;
            if name_len == 0 {
                i += 1;
                continue;
            }
            let name = rest[..1 + name_len].to_ascii_lowercase();
            if name == "@php" && !text[after..].trim_start().starts_with('(') {
                let end = text[after..]
                    .to_ascii_lowercase()
                    .find("@endphp")
                    .map_or(text.len(), |n| after + n);
                out.push(Region { start: after, end });
                i = end;
                continue;
            }
            // `@name (` may have spaces before the parenthesis.
            let gap = text[after..]
                .bytes()
                .take_while(|c| *c == b' ' || *c == b'\t')
                .count();
            if text.as_bytes().get(after + gap) == Some(&b'(') {
                let open = after + gap;
                let close = matching_paren(text, open);
                out.push(Region {
                    start: open + 1,
                    end: close,
                });
                i = (close + 1).min(text.len());
            } else {
                i = after;
            }
        } else {
            i += 1;
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

fn class_before_element() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*\\?([\w\\]+)::class\s*,\s*$").expect("valid"))
}

fn action_string() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([\w\\]+)@(\w*)$").expect("valid"))
}

/// What `string` stands for, if anything.
fn classify(
    text: &str,
    string: &Str,
    floor: usize,
    closers: &HashMap<usize, usize>,
    openers: &HashMap<usize, usize>,
) -> Option<Site> {
    let contents = &text[string.start..string.end];
    let site = |kind: Kind| Site {
        kind,
        start: string.start,
        end: string.end,
        class: None,
        has_default: false,
        paren: None,
    };
    // A key of an array (`'a' => …`) names nothing.
    let after = text[string.end..]
        .trim_start_matches(['\'', '"'])
        .trim_start();
    if after.starts_with("=>") {
        return None;
    }
    let before = text[..string.open].trim_end();
    let found = around(text, string, floor, closers);

    // `[Controller::class, '…']`
    if let Some(array) = found.array
        && let Some(caps) = class_before_element().captures(&text[array + 1..string.open])
    {
        return Some(Site {
            class: Some(caps[1].to_owned()),
            ..site(Kind::Action)
        });
    }

    if let Some(paren) = found.paren
        && let Some(call) = callee(text, paren)
    {
        let in_array = found.array.is_some();
        let hit = if call.name.starts_with('@') {
            DIRECTIVES
                .iter()
                .find(|(name, _, _)| *name == call.name)
                .map(|(_, kind, index)| (*kind, Some(*index)))
        } else {
            match call.operator {
                None => function_kind(&call.name).map(|k| (k, Some(0))),
                Some("::") => static_kind(&call.scope, &call.name).map(|k| (k, Some(0))),
                Some(_) => method_kind(&call.scope, &call.name).map(|k| (k, Some(0))),
            }
        };
        let hit = hit.or_else(|| {
            call.operator
                .and_then(|_| eloquent_kind(&call.name))
                .map(|kind| (kind, Some(0)))
        });
        if let Some((kind, index)) = hit {
            let any_index = matches!(kind, Kind::Middleware);
            let wanted = if call.name == "make" && kind == Kind::Rule {
                1
            } else {
                index.unwrap_or(0)
            };
            let array_ok = matches!(
                kind,
                Kind::Middleware
                    | Kind::Ability
                    | Kind::View
                    | Kind::Asset
                    | Kind::Vite
                    | Kind::Rule
                    | Kind::Column
                    | Kind::Relation
            );
            if (any_index || found.index == wanted) && (!in_array || array_ok) {
                let has_default =
                    kind == Kind::Env && argument_count(text, paren, closers, openers) > 1;
                return Some(Site {
                    has_default,
                    paren: Some(paren),
                    ..site(kind)
                });
            }
        }
        // `Route::get('/x', 'Controller@method')`
        if call.operator == Some("::")
            && call.scope == "route"
            && found.index == 1
            && !in_array
            && let Some(caps) = action_string().captures(contents)
        {
            return Some(Site {
                class: Some(caps[1].to_owned()),
                ..site(Kind::Action)
            });
        }
        if call.name == "validate" || call.name == "validatewithbag" {
            return value_rule(before, &site(Kind::Rule));
        }
    }

    // `'field' => 'required|email'` or `['required', 'string']` in rules.
    if before.ends_with("=>") || found.array.is_some() {
        let window_start = string.open.saturating_sub(4000);
        let window_start = (window_start..=string.open)
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(string.open);
        let window = text[window_start..string.open].to_ascii_lowercase();
        if RULE_MARKERS.iter().any(|m| window.contains(m)) {
            if before.ends_with("=>") {
                return Some(site(Kind::Rule));
            }
            if let Some(array) = found.array {
                let outer = text[..array].trim_end();
                if outer.ends_with("=>") {
                    return Some(site(Kind::Rule));
                }
            }
        }
    }
    None
}

/// A rule string in the data array of `validate([...])` is a value after
/// `=>`.
fn value_rule(before: &str, site: &Site) -> Option<Site> {
    before.ends_with("=>").then(|| site.clone())
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

fn regions_of(text: &str, blade: bool) -> Vec<Region> {
    if blade {
        blade_regions(text)
    } else {
        php_regions(text)
    }
}

/// The string at `byte` and what it stands for.
pub fn site_at(text: &str, byte: usize, blade: bool) -> Option<Site> {
    let regions = regions_of(text, blade);
    let strs = strings(text, &regions, false);
    let string = strs
        .iter()
        .find(|s| s.start <= byte && byte <= s.end)
        .copied()?;
    let region = regions
        .iter()
        .find(|r| r.start <= string.open && string.open < r.end.max(r.start + 1))?;
    let (closers, openers) = closers_of(&strs);
    let floor = if blade {
        region.start.saturating_sub(1)
    } else {
        region.start
    };
    classify(text, &string, floor, &closers, &openers)
}

/// The cursor is inside a string literal of the PHP in `text`.
pub fn in_string(text: &str, byte: usize, blade: bool) -> bool {
    let regions = regions_of(text, blade);
    strings(text, &regions, false)
        .iter()
        .any(|s| s.start <= byte && byte <= s.end)
}

/// Every string of the text that stands for something.
pub fn sites(text: &str, blade: bool) -> Vec<Site> {
    let regions = regions_of(text, blade);
    let strs = strings(text, &regions, true);
    let (closers, openers) = closers_of(&strs);
    let mut out = Vec::new();
    for string in &strs {
        if !string.closed {
            continue;
        }
        let Some(region) = regions
            .iter()
            .find(|r| r.start <= string.open && string.open < r.end.max(r.start + 1))
        else {
            continue;
        };
        let floor = if blade {
            region.start.saturating_sub(1)
        } else {
            region.start
        };
        if let Some(site) = classify(text, string, floor, &closers, &openers) {
            out.push(site);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, blade: bool) -> Option<(Kind, String)> {
        let cursor = text.find('|').expect("cursor");
        let source = text.replacen('|', "", 1);
        site_at(&source, cursor, blade).map(|s| (s.kind, source[s.start..s.end].to_owned()))
    }

    #[test]
    fn helpers_and_facades_name_things() {
        let php = |code: &str| at(&format!("<?php\n{code}\n"), false);
        assert_eq!(
            php("config('app.na|me');"),
            Some((Kind::Config, "app.name".into()))
        );
        assert_eq!(php("$x = env('APP_|');"), Some((Kind::Env, "APP_".into())));
        assert_eq!(php("route('home|');"), Some((Kind::Route, "home".into())));
        assert_eq!(php("view('a.b|')"), Some((Kind::View, "a.b".into())));
        assert_eq!(
            php("__('auth.fa|iled')"),
            Some((Kind::Translation, "auth.failed".into()))
        );
        assert_eq!(
            php("asset('css/a|.css')"),
            Some((Kind::Asset, "css/a.css".into()))
        );
        assert_eq!(
            php("Config::get('x.y|')"),
            Some((Kind::Config, "x.y".into()))
        );
        assert_eq!(
            php("Storage::disk('s|3')"),
            Some((Kind::Section("filesystems.disks"), "s3".into()))
        );
        assert_eq!(
            php("Log::channel('q|')"),
            Some((Kind::Section("logging.channels"), "q".into()))
        );
        assert_eq!(
            php("Gate::allows('edit|')"),
            Some((Kind::Ability, "edit".into()))
        );
        assert_eq!(
            php("$user->can('edit|', $post)"),
            Some((Kind::Ability, "edit".into()))
        );
        assert_eq!(
            php("redirect()->route('h|')"),
            Some((Kind::Route, "h".into()))
        );
        assert_eq!(php("app('cache|')"), Some((Kind::Binding, "cache".into())));
        assert_eq!(
            php("Inertia::render('Users/In|')"),
            Some((Kind::Page, "Users/In".into()))
        );
    }

    #[test]
    fn only_the_right_argument_counts() {
        let php = |code: &str| at(&format!("<?php\n{code}\n"), false);
        assert_eq!(php("config('a', 'b|')"), None);
        assert_eq!(php("route('home', ['id' => 'x|'])"), None);
        assert_eq!(php("$request->route('id|')"), None);
        assert_eq!(php("foo('app.na|me')"), None);
        // Commas inside earlier strings are not arguments.
        assert_eq!(
            php("config('a,b,c|')"),
            Some((Kind::Config, "a,b,c".into()))
        );
        // A call written inside a string is text, not a call.
        assert_eq!(
            php("$q->where('config(|')"),
            Some((Kind::Column, "config(".into()))
        );
        // A key of an array names nothing.
        assert_eq!(php("config(['a.b|' => 1])"), None);
    }

    #[test]
    fn middleware_takes_lists_and_chains() {
        let php = |code: &str| at(&format!("<?php\n{code}\n"), false);
        assert_eq!(
            php("Route::get('/x', fn () => 1)->middleware('au|th');"),
            Some((Kind::Middleware, "auth".into()))
        );
        assert_eq!(
            php("Route::middleware(['auth', 'ro|le'])->group(fn () => 1);"),
            Some((Kind::Middleware, "role".into()))
        );
        assert_eq!(
            php("$this->middleware('x|', ['only' => 'a'])"),
            Some((Kind::Middleware, "x".into()))
        );
    }

    #[test]
    fn controller_actions_know_their_class() {
        let text = "<?php\nRoute::get('x', [UserController::class, 'ind|ex']);\n";
        let cursor = text.find('|').unwrap();
        let source = text.replacen('|', "", 1);
        let site = site_at(&source, cursor, false).expect("site");
        assert_eq!(site.kind, Kind::Action);
        assert_eq!(site.class.as_deref(), Some("UserController"));
        assert_eq!(&source[site.start..site.end], "index");
        // The first element is the class, not a method.
        assert_eq!(at("<?php\nRoute::get('x', ['a|', 'b']);\n", false), None);
        let text = "<?php\nRoute::get('x', 'UserController@sh|ow');\n";
        let cursor = text.find('|').unwrap();
        let source = text.replacen('|', "", 1);
        let site = site_at(&source, cursor, false).expect("site");
        assert_eq!(site.class.as_deref(), Some("UserController"));
    }

    #[test]
    fn rules_are_found_in_validation_code() {
        let text = "<?php\nclass R {\n    public function rules() {\n        return ['name' => 're|quired|max:3', 'tags' => ['req', 'str']];\n    }\n}\n";
        assert_eq!(at(text, false), Some((Kind::Rule, "required|max:3".into())));
        let text = "<?php\nclass R {\n    public function rules() {\n        return ['tags' => ['req|', 'str']];\n    }\n}\n";
        assert_eq!(at(text, false), Some((Kind::Rule, "req".into())));
        // Outside rules the same shape means nothing.
        assert_eq!(at("<?php\n$a = ['name' => 'req|'];\n", false), None);
    }

    #[test]
    fn env_knows_about_a_fallback() {
        let found = sites("<?php\nenv('A');\nenv('B', 'x');\n", false);
        let flags: Vec<bool> = found.iter().map(|s| s.has_default).collect();
        assert_eq!(flags, vec![false, true]);
    }

    #[test]
    fn blade_directives_and_echoes() {
        let blade = |code: &str| at(code, true);
        assert_eq!(
            blade("<div>@extends('layouts.a|pp')</div>"),
            Some((Kind::View, "layouts.app".into()))
        );
        assert_eq!(
            blade("<p>don't {{ config('app.n|ame') }} it's</p>"),
            Some((Kind::Config, "app.name".into()))
        );
        assert_eq!(
            blade("@includeWhen($ok, 'partials.h|eader')"),
            Some((Kind::View, "partials.header".into()))
        );
        assert_eq!(blade("@includeWhen('par|tials', $ok)"), None);
        assert_eq!(
            blade("@can('upd|ate', $post) x @endcan"),
            Some((Kind::Ability, "update".into()))
        );
        assert_eq!(
            blade("<a href=\"{{ route('home|') }}\">"),
            Some((Kind::Route, "home".into()))
        );
        // Plain HTML text with quotes is not PHP.
        assert_eq!(blade("<p>config('app.n|ame')</p>"), None);
        assert_eq!(
            blade("@php $a = __('x.y|'); @endphp"),
            Some((Kind::Translation, "x.y".into()))
        );
    }

    #[test]
    fn comments_and_heredocs_do_not_count() {
        let text = "<?php\n// config('a.b')\n/* route('x') */\n$a = <<<EOT\nview('q')\nEOT;\nconfig('real');\n";
        let found = sites(text, false);
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].start..found[0].end], "real");
    }

    #[test]
    fn an_unfinished_string_does_not_hide_later_ones() {
        let text = "<?php\n$a = config('app.na\n$b = route('home');\n";
        let cursor = text.find("na\n").unwrap() + 2;
        assert_eq!(
            site_at(text, cursor, false).map(|s| s.kind),
            Some(Kind::Config)
        );
    }
}
