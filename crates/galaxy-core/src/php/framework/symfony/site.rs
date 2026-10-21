//! Finds the strings in PHP text that name something in a Symfony project:
//! `->generateUrl('home')`, `->render('a/b.html.twig')`,
//! `#[Autowire(param: 'kernel.debug')]`, `->trans('key', [], 'domain')` and
//! so on.
//!
//! This is a text scan, not a parse, so it also works on half-typed code.
//! It shares the string scanner from `framework::common`.

use std::collections::HashMap;

use crate::php::framework::common::scan::{
    Str, around, callee, closers_of, is_word, php_regions, strings,
};

/// What a string refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Route,
    Template,
    Service,
    Parameter,
    /// An environment variable.
    Env,
    Translation,
}

/// A string and what it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub kind: Kind,
    /// Byte range of the name, without the quotes. For `%name%` and
    /// `%env(NAME)%` strings it is the name between the percent signs.
    pub start: usize,
    pub end: usize,
    /// For [`Kind::Translation`]: the domain when the call passes a literal
    /// one. `None` means the default domain, `messages`.
    pub domain: Option<String>,
    /// The `(` of the call the string is an argument of.
    pub paren: Option<usize>,
}

/// One top-level argument of a call.
struct CallArg {
    name: Option<String>,
    start: usize,
    end: usize,
    /// Byte range of the contents when the argument is one string literal.
    literal: Option<(usize, usize)>,
}

/// The start of the (possibly namespaced) name before `paren`.
fn name_start(text: &str, paren: usize) -> usize {
    let before = text[..paren].trim_end();
    let bytes = before.as_bytes();
    let mut start = before.len();
    while start > 0 && (is_word(bytes[start - 1]) || bytes[start - 1] == b'\\') {
        start -= 1;
    }
    start
}

/// `new` comes right before the name of the call.
fn after_new(text: &str, start: usize) -> bool {
    let head = text[..start].trim_end();
    head.ends_with("new")
        && !head.as_bytes()[..head.len() - 3]
            .last()
            .is_some_and(|c| is_word(*c) || *c == b'$')
}

/// The text before `from` sits inside `#[ … ]`.
fn in_attribute(text: &str, from: usize, floor: usize, closers: &HashMap<usize, usize>) -> bool {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut i = from;
    while i > floor {
        i -= 1;
        if let Some(open) = closers.get(&i) {
            i = *open;
            continue;
        }
        match bytes[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'{' => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            b'[' => {
                if depth == 0 {
                    return i > 0 && bytes[i - 1] == b'#';
                }
                depth -= 1;
            }
            b';' if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

/// `name:` right before a string, as in `service: 'id'`.
fn named_arg(before: &str) -> Option<&str> {
    let head = before.strip_suffix(':')?;
    if head.ends_with(':') {
        return None;
    }
    let head = head.trim_end();
    let word = head.bytes().rev().take_while(|c| is_word(*c)).count();
    if word == 0 {
        return None;
    }
    let (rest, name) = head.split_at(head.len() - word);
    if name.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    matches!(rest.trim_end().as_bytes().last(), Some(b'(' | b',')).then_some(name)
}

/// The argument is the one a call takes the name from.
fn arg_is(named: Option<&str>, index: usize, wanted: usize, names: &[&str]) -> bool {
    match named {
        Some(name) => names.contains(&name),
        None => index == wanted,
    }
}

/// `name:` at the start of an argument; returns the name and the offset of
/// the value.
fn leading_name(arg: &str) -> Option<(&str, usize)> {
    let len = arg.bytes().take_while(|c| is_word(*c)).count();
    if len == 0 || arg.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let rest = &arg[len..];
    let trimmed = rest.trim_start();
    (trimmed.starts_with(':') && !trimmed.starts_with("::"))
        .then(|| (&arg[..len], len + (rest.len() - trimmed.len()) + 1))
}

/// The top-level arguments of the call at `paren`. Stops at the closing
/// parenthesis or, for unfinished code, at the end of the statement.
fn call_args(text: &str, paren: usize, openers: &HashMap<usize, usize>) -> Vec<CallArg> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut depth = 0i32;
    let mut from = paren + 1;
    let mut i = from;
    let mut closed = false;
    while i < bytes.len() {
        if let Some(close) = openers.get(&i) {
            i = *close + 1;
            continue;
        }
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    ranges.push((from, i));
                    closed = true;
                    break;
                }
                depth -= 1;
            }
            b',' if depth == 0 => {
                ranges.push((from, i));
                from = i + 1;
            }
            b';' if depth == 0 => {
                ranges.push((from, i));
                closed = true;
                break;
            }
            _ => {}
        }
        i += 1;
    }
    if !closed {
        ranges.push((from, bytes.len()));
    }
    let mut out = Vec::new();
    for (from, to) in ranges {
        let slice = &text[from..to];
        let lead = slice.len() - slice.trim_start().len();
        let start = from + lead;
        let end = from + slice.trim_end().len();
        if start >= end {
            continue;
        }
        let (name, value_at) = match leading_name(&text[start..end]) {
            Some((name, offset)) => (Some(name.to_owned()), start + offset),
            None => (None, start),
        };
        let value = &text[value_at..end];
        let value_start = value_at + (value.len() - value.trim_start().len());
        let literal = openers.get(&value_start).and_then(|close| {
            text[close + 1..end]
                .trim()
                .is_empty()
                .then_some((value_start + 1, *close))
        });
        out.push(CallArg {
            name,
            start: value_start,
            end,
            literal,
        });
    }
    out
}

/// `Some(domain)` for the call's translation domain: the literal one, or
/// the default for a missing or `null` one. `None` when the domain is only
/// known at run time.
fn translation_domain(
    text: &str,
    paren: usize,
    openers: &HashMap<usize, usize>,
) -> Option<Option<String>> {
    let args = call_args(text, paren, openers);
    let arg = args
        .iter()
        .find(|a| a.name.as_deref() == Some("domain"))
        .or_else(|| {
            args.iter()
                .enumerate()
                .find_map(|(i, a)| (i == 2 && a.name.is_none()).then_some(a))
        });
    let Some(arg) = arg else {
        return Some(None);
    };
    if let Some((start, end)) = arg.literal {
        return Some(Some(text[start..end].to_owned()));
    }
    text[arg.start..arg.end]
        .trim()
        .eq_ignore_ascii_case("null")
        .then_some(None)
}

/// The kind of the name inside an `Autowire('%…%')` string.
fn percent_site(string: &Str, contents: &str, paren: usize) -> Option<Site> {
    let rest = contents.strip_prefix('%')?;
    let (inner, ended) = match rest.strip_suffix('%') {
        Some(inner) => (inner, true),
        None => (rest, false),
    };
    if string.closed && !ended {
        return None;
    }
    let site = |kind, start, end| Site {
        kind,
        start,
        end,
        domain: None,
        paren: Some(paren),
    };
    let name_char = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-');
    if ended && let Some(args) = inner.strip_prefix("env(").and_then(|r| r.strip_suffix(')')) {
        // `%env(json:file:NAME)%`: processors come before the name.
        let name = args.rfind(':').map_or(0, |i| i + 1);
        if args[name..].is_empty() {
            return None;
        }
        let start = string.start + 1 + 4 + name;
        return Some(site(Kind::Env, start, start + args.len() - name));
    }
    if inner.is_empty() || !inner.bytes().all(name_char) {
        return None;
    }
    let start = string.start + 1;
    Some(site(Kind::Parameter, start, start + inner.len()))
}

/// `"…$x…"` and `"…{$x}…"` build a name at run time.
fn interpolates(contents: &str) -> bool {
    let bytes = contents.as_bytes();
    bytes.iter().enumerate().any(|(i, c)| match c {
        b'$' => bytes
            .get(i + 1)
            .is_some_and(|n| n.is_ascii_alphabetic() || *n == b'_' || *n == b'{'),
        b'{' => bytes.get(i + 1) == Some(&b'$'),
        _ => false,
    })
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
    if text.as_bytes()[string.open] == b'"' && interpolates(contents) {
        return None;
    }
    let before = text[..string.open].trim_end();
    let after = if string.closed {
        text[string.end + 1..].trim_start()
    } else {
        ""
    };
    // Concatenation builds the name at run time; an array key names nothing.
    if after.starts_with('.') || after.starts_with("=>") {
        return None;
    }
    if before.ends_with('.') && !before.ends_with("...") {
        return None;
    }
    let site = |kind: Kind, paren: Option<usize>| Site {
        kind,
        start: string.start,
        end: string.end,
        domain: None,
        paren,
    };

    // `$_ENV['X']`, `$_SERVER['X']`
    if let Some(head) = before.strip_suffix('[') {
        let head = head.trim_end();
        let closes = after.starts_with(']') || !string.closed;
        return (closes && (head.ends_with("$_ENV") || head.ends_with("$_SERVER")))
            .then(|| site(Kind::Env, None));
    }

    let found = around(text, string, floor, closers);
    let paren = found.paren?;
    if found.array.is_some() {
        return None;
    }
    let call = callee(text, paren)?;
    let named = named_arg(before);
    let index = found.index;
    let at = Some(paren);
    let start = name_start(text, paren);

    let kind = match call.operator {
        Some("->") => match call.name.as_str() {
            "generateurl" | "redirecttoroute" | "generate"
                if arg_is(named, index, 0, &["route", "name"]) =>
            {
                Kind::Route
            }
            "render" | "renderview" | "renderform" | "renderblock"
                if arg_is(named, index, 0, &["view", "name", "template"]) =>
            {
                Kind::Template
            }
            "htmltemplate" | "texttemplate" if arg_is(named, index, 0, &["template"]) => {
                Kind::Template
            }
            "getparameter" if arg_is(named, index, 0, &["name"]) => Kind::Parameter,
            "get" if arg_is(named, index, 0, &["id", "name"]) => {
                // The last segment decides for `$c->getParameterBag()->get()`.
                let last = call.scope.rsplit(['>', ':']).next().unwrap_or("");
                if last.contains("param") {
                    Kind::Parameter
                } else if call.scope.contains("container") {
                    Kind::Service
                } else {
                    return None;
                }
            }
            "trans" if arg_is(named, index, 0, &["id", "message"]) => {
                return translation(text, string, paren, openers);
            }
            _ => return None,
        },
        None => match call.name.as_str() {
            "template" if in_attribute(text, start, floor, closers) => {
                if !arg_is(named, index, 0, &["template"]) {
                    return None;
                }
                Kind::Template
            }
            "autowire" if in_attribute(text, start, floor, closers) => {
                return match named {
                    Some("service") => Some(site(Kind::Service, at)),
                    Some("param") => Some(site(Kind::Parameter, at)),
                    Some("env") => Some(site(Kind::Env, at)),
                    Some("value") => percent_site(string, contents, paren),
                    Some(_) => None,
                    None if index == 0 => percent_site(string, contents, paren),
                    None => None,
                };
            }
            "getparameter"
                if !after_new(text, start)
                    && !in_attribute(text, start, floor, closers)
                    && arg_is(named, index, 0, &["name"]) =>
            {
                Kind::Parameter
            }
            "translatablemessage"
                if after_new(text, start) && arg_is(named, index, 0, &["message"]) =>
            {
                return translation(text, string, paren, openers);
            }
            "t" if !after_new(text, start)
                && !in_attribute(text, start, floor, closers)
                && arg_is(named, index, 0, &["message"]) =>
            {
                return translation(text, string, paren, openers);
            }
            _ => return None,
        },
        Some(_) => return None,
    };
    Some(site(kind, at))
}

fn translation(
    text: &str,
    string: &Str,
    paren: usize,
    openers: &HashMap<usize, usize>,
) -> Option<Site> {
    let domain = translation_domain(text, paren, openers)?;
    Some(Site {
        kind: Kind::Translation,
        start: string.start,
        end: string.end,
        domain,
        paren: Some(paren),
    })
}

/// The string at `byte` and what it stands for.
pub fn site_at(text: &str, byte: usize) -> Option<Site> {
    let regions = php_regions(text);
    let strs = strings(text, &regions, false);
    let string = strs
        .iter()
        .find(|s| s.start <= byte && byte <= s.end)
        .copied()?;
    let region = regions
        .iter()
        .find(|r| r.start <= string.open && string.open < r.end.max(r.start + 1))?;
    let (closers, openers) = closers_of(&strs);
    classify(text, &string, region.start, &closers, &openers)
}

/// The cursor is inside a string literal of the PHP in `text`.
pub fn in_string(text: &str, byte: usize) -> bool {
    let regions = php_regions(text);
    strings(text, &regions, false)
        .iter()
        .any(|s| s.start <= byte && byte <= s.end)
}

/// Every string of the text that stands for something.
pub fn sites(text: &str) -> Vec<Site> {
    let regions = php_regions(text);
    let strs = strings(text, &regions, true);
    let (closers, openers) = closers_of(&strs);
    let mut out = Vec::new();
    for string in strs.iter().filter(|s| s.closed) {
        let Some(region) = regions
            .iter()
            .find(|r| r.start <= string.open && string.open < r.end.max(r.start + 1))
        else {
            continue;
        };
        if let Some(site) = classify(text, string, region.start, &closers, &openers) {
            out.push(site);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The site at the `|` marker as `(kind, name, domain)`.
    fn at(code: &str) -> Option<(Kind, String, Option<String>)> {
        let source = format!("<?php\n{code}\n");
        let cursor = source.find('|').expect("cursor");
        let source = source.replacen('|', "", 1);
        site_at(&source, cursor).map(|s| (s.kind, source[s.start..s.end].to_owned(), s.domain))
    }

    fn hit(code: &str) -> Option<(Kind, String)> {
        at(code).map(|(kind, name, _)| (kind, name))
    }

    fn is(kind: Kind, name: &str) -> Option<(Kind, String)> {
        Some((kind, name.to_owned()))
    }

    #[test]
    fn route_calls() {
        for code in [
            "$this->generateUrl('home|');",
            "return $this->redirectToRoute('home|');",
            "$router->generate('home|', ['id' => 1]);",
            "$this->container->get('router')->generate('home|');",
            "$x?->generate('home|');",
        ] {
            assert_eq!(hit(code), is(Kind::Route, "home"), "{code}");
        }
    }

    #[test]
    fn template_calls() {
        for code in [
            "return $this->render('a/b.html.twig|');",
            "return $this->render('a/b.html.twig|', ['x' => 1]);",
            "$this->renderView('a/b.html.twig|');",
            "$this->renderForm('a/b.html.twig|', ['f' => $f]);",
            "$twig->renderBlock('a/b.html.twig|', 'content');",
            "(new TemplatedEmail())->htmlTemplate('a/b.html.twig|');",
            "$email->textTemplate('a/b.html.twig|');",
            "#[Template('a/b.html.twig|')]",
            "#[\\Symfony\\Bridge\\Twig\\Attribute\\Template('a/b.html.twig|')]",
            "#[Route('/x'), Template(template: 'a/b.html.twig|')]",
        ] {
            assert_eq!(hit(code), is(Kind::Template, "a/b.html.twig"), "{code}");
        }
    }

    #[test]
    fn only_the_name_argument_counts() {
        for code in [
            "$this->render('a.html.twig', ['k' => 'v|']);",
            "$this->render('a.html.twig', 'b|');",
            "$this->generateUrl('home', [], 'x|');",
            "$twig->renderBlock('a.html.twig', 'blo|ck');",
        ] {
            assert_eq!(hit(code), None, "{code}");
        }
    }

    #[test]
    fn container_get_is_a_service() {
        for code in [
            "$this->container->get('app.mailer|');",
            "$container->get('app.mailer|');",
            "$this->getContainer()->get('app.mailer|');",
            "static::getContainer()->get('app.mailer|');",
            "$this->Container->get('app.mailer|');",
        ] {
            assert_eq!(hit(code), is(Kind::Service, "app.mailer"), "{code}");
        }
    }

    #[test]
    fn get_on_other_receivers_is_not_a_site() {
        for code in [
            "$this->get('app.mailer|');",
            "$request->get('page|');",
            "$cache->get('key|');",
            "Foo::get('x|');",
            "get('x|');",
        ] {
            assert_eq!(hit(code), None, "{code}");
        }
    }

    #[test]
    fn parameters() {
        for code in [
            "$this->getParameter('kernel.debug|');",
            "$p = getParameter('kernel.debug|');",
            "$params->get('kernel.debug|');",
            "$this->parameterBag->get('kernel.debug|');",
            "$this->container->getParameter('kernel.debug|');",
            "$this->container->getParameterBag()->get('kernel.debug|');",
            "#[Autowire(param: 'kernel.debug|')]",
            "#[Autowire('%kernel.debug|%')]",
            "#[Autowire(value: '%kernel.debug|%')]",
        ] {
            assert_eq!(hit(code), is(Kind::Parameter, "kernel.debug"), "{code}");
        }
    }

    #[test]
    fn autowire_forms() {
        assert_eq!(
            hit("#[Autowire(service: 'app.mailer|')]"),
            is(Kind::Service, "app.mailer")
        );
        assert_eq!(
            hit(
                "public function __construct(#[Autowire(service: 'app.mailer|')] private Mailer $m) {}"
            ),
            is(Kind::Service, "app.mailer")
        );
        assert_eq!(
            hit("#[Autowire(env: 'APP_SECRET|')]"),
            is(Kind::Env, "APP_SECRET")
        );
        assert_eq!(
            hit("#[Autowire('%env(APP_SECRET|)%')]"),
            is(Kind::Env, "APP_SECRET")
        );
        assert_eq!(
            hit("#[Autowire('%env(string:APP_SECRET|)%')]"),
            is(Kind::Env, "APP_SECRET")
        );
        assert_eq!(
            hit("#[Autowire('%env(json:file:DB|)%')]"),
            is(Kind::Env, "DB")
        );
    }

    #[test]
    fn autowire_ranges_and_non_sites() {
        let code = "<?php\n#[Autowire('%kernel.debug%')]\n";
        let site = site_at(code, code.find("debug").unwrap()).unwrap();
        assert_eq!(&code[site.start..site.end], "kernel.debug");
        assert_eq!(site.kind, Kind::Parameter);
        for code in [
            "#[Target('foo|')]",
            "#[Autowire('@some.service|')]",
            "#[Autowire('%a%/%b|%')]",
            "#[Autowire(lazy: 'x|')]",
            "#[Autowire(param: 'a|', env: 'B')] ",
        ] {
            let got = hit(code);
            if code.contains("param: 'a|'") {
                assert_eq!(got, is(Kind::Parameter, "a"), "{code}");
            } else {
                assert_eq!(got, None, "{code}");
            }
        }
    }

    #[test]
    fn attribute_names_outside_attributes_are_not_sites() {
        for code in [
            "autowire(service: 'x|');",
            "template('a.html.twig|');",
            "$x = new Template('a.html.twig|');",
            "$y = new Autowire('%kernel.debug|%');",
        ] {
            assert_eq!(hit(code), None, "{code}");
        }
    }

    #[test]
    fn env_superglobals() {
        for code in [
            "$x = $_ENV['APP_ENV|'];",
            "$x = $_SERVER[\"APP_ENV|\"];",
            "$x = $_ENV [ 'APP_ENV|' ];",
            "foo($_SERVER['APP_ENV|']);",
        ] {
            assert_eq!(hit(code), is(Kind::Env, "APP_ENV"), "{code}");
        }
        for code in ["$x = $other['APP_ENV|'];", "$x = $_GET['APP_ENV|'];"] {
            assert_eq!(hit(code), None, "{code}");
        }
    }

    #[test]
    fn translation_domains() {
        let cases = [
            ("$t->trans('hello.world|');", None),
            ("$this->translator->trans('hello.world|', []);", None),
            ("$t->trans('hello.world|', [], 'admin');", Some("admin")),
            (
                "$t->trans('hello.world|', ['%n%' => 1], \"admin\", 'fr');",
                Some("admin"),
            ),
            ("$t->trans('hello.world|', [], null);", None),
            ("$t->trans('hello.world|', domain: 'admin');", Some("admin")),
            ("$t->trans('hello.world|', [], 'admin'", Some("admin")),
        ];
        for (code, domain) in cases {
            let (kind, name, got) = at(code).unwrap_or_else(|| panic!("{code}"));
            assert_eq!(
                (kind, name.as_str()),
                (Kind::Translation, "hello.world"),
                "{code}"
            );
            assert_eq!(got.as_deref(), domain, "{code}");
        }
    }

    #[test]
    fn translation_with_a_run_time_domain_is_not_a_site() {
        assert_eq!(at("$t->trans('hello|', [], $domain);"), None);
        assert_eq!(at("$t->trans('hello|', [], self::DOMAIN);"), None);
        assert_eq!(at("$t->trans('hello|', [], 'a' . $b);"), None);
    }

    #[test]
    fn translatable_message_and_t() {
        let (kind, name, domain) =
            at("$m = new TranslatableMessage('greet|', [], 'mail');").unwrap();
        assert_eq!(
            (kind, name.as_str(), domain.as_deref()),
            (Kind::Translation, "greet", Some("mail"))
        );
        let (kind, name, domain) =
            at("$m = new \\Symfony\\Component\\Translation\\TranslatableMessage('greet|');")
                .unwrap();
        assert_eq!(
            (kind, name.as_str(), domain),
            (Kind::Translation, "greet", None)
        );
        let (kind, name, domain) = at("$m = t('greet|', [], 'mail');").unwrap();
        assert_eq!(
            (kind, name.as_str(), domain.as_deref()),
            (Kind::Translation, "greet", Some("mail"))
        );
        assert_eq!(at("$m = new t('greet|');"), None);
        assert_eq!(at("$m = TranslatableMessage('greet|');"), None);
    }

    #[test]
    fn named_arguments() {
        assert_eq!(
            hit("$this->generateUrl(route: 'home|');"),
            is(Kind::Route, "home")
        );
        assert_eq!(
            hit("$this->generateUrl(parameters: [], route: 'home|');"),
            is(Kind::Route, "home")
        );
        assert_eq!(
            hit("$this->render(view: 'a.html.twig|');"),
            is(Kind::Template, "a.html.twig")
        );
        assert_eq!(hit("$this->generateUrl(referenceType: 'home|');"), None);
        assert_eq!(
            hit("$this->getParameter(name: 'p|');"),
            is(Kind::Parameter, "p")
        );
    }

    #[test]
    fn concatenation_and_interpolation_are_not_sites() {
        for code in [
            "$this->generateUrl('admin_' . $name|);",
            "$this->generateUrl('admin_|' . $name);",
            "$this->generateUrl($prefix . 'home|');",
            "$this->generateUrl($prefix.'home|');",
            "$this->generateUrl(\"admin_{$na|me}\");",
            "$this->generateUrl(\"admin_$na|me\");",
        ] {
            assert_eq!(hit(code), None, "{code}");
        }
        assert_eq!(
            hit("$this->generateUrl(\"ho|me\");"),
            is(Kind::Route, "home")
        );
        assert_eq!(hit("$this->generateUrl(...['ho|me']);"), None);
    }

    #[test]
    fn comments_are_skipped() {
        for code in [
            "// $this->generateUrl('home|');",
            "# $this->generateUrl('home|');",
            "/* $this->generateUrl('home|'); */",
            "/**\n * $this->generateUrl('home|');\n */",
        ] {
            assert_eq!(hit(code), None, "{code}");
        }
        let code = "<?php\n// it's\n$this->generateUrl('home');\n";
        assert_eq!(sites(code).len(), 1);
    }

    #[test]
    fn array_keys_and_values_are_not_sites() {
        for code in [
            "$this->render('a.html.twig', ['generateUrl' => 'x|']);",
            "$x = ['home|' => 1];",
            "$this->generateUrl(['home|']);",
            "$other->foo('home|');",
        ] {
            assert_eq!(hit(code), None, "{code}");
        }
    }

    #[test]
    fn in_string_follows_the_php_scanner() {
        let code = "<?php\n$a = 'abc'; // 'no'\n$b = foo;\n";
        assert!(in_string(code, code.find("bc").unwrap()));
        assert!(!in_string(code, code.find("no").unwrap()));
        assert!(!in_string(code, code.find("foo").unwrap()));
        assert!(!in_string("plain 'text'", 8));
    }

    #[test]
    fn sites_lists_every_site_in_order() {
        let code = "<?php\n\
            #[Route('/x', name: 'x')]\n\
            class A {\n\
                public function __construct(#[Autowire(service: 'svc')] private S $s) {}\n\
                #[Template('t.html.twig')]\n\
                public function f() {\n\
                    $a = $this->generateUrl('r');\n\
                    $b = $this->getParameter('p');\n\
                    $c = $_ENV['E'];\n\
                    $d = $t->trans('k', [], 'd');\n\
                    $e = 'plain';\n\
                }\n\
            }\n";
        let found: Vec<(Kind, &str)> = sites(code)
            .iter()
            .map(|s| (s.kind, &code[s.start..s.end]))
            .collect();
        assert_eq!(
            found,
            [
                (Kind::Service, "svc"),
                (Kind::Template, "t.html.twig"),
                (Kind::Route, "r"),
                (Kind::Parameter, "p"),
                (Kind::Env, "E"),
                (Kind::Translation, "k"),
            ]
        );
    }

    #[test]
    fn unclosed_strings_still_complete() {
        assert_eq!(hit("$this->generateUrl('ho|"), is(Kind::Route, "ho"));
        let code = "<?php\n$this->generateUrl('ho\n$x = 1;\n";
        assert!(sites(code).is_empty());
    }

    #[test]
    fn multibyte_text_keeps_byte_offsets() {
        let code = "<?php\n$x = 'é'; $this->generateUrl('hé');\n";
        let found = sites(code);
        assert_eq!(found.len(), 1);
        assert_eq!(&code[found[0].start..found[0].end], "hé");
    }

    #[test]
    fn site_reports_the_call_paren() {
        let code = "<?php\n$this->generateUrl('home');\n";
        let site = site_at(code, code.find("home").unwrap()).unwrap();
        assert_eq!(code.as_bytes()[site.paren.unwrap()], b'(');
    }
}
