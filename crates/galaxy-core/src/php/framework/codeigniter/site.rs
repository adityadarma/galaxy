//! Finds the strings in PHP text that name something in a CodeIgniter 4
//! project: `view('home')`, `route_to('users.show')`, `config('App')`,
//! `lang('App.title')`, `helper(['url', 'text'])` and, in `Routes.php`, the
//! handler of `$routes->get('path', 'Home::index')`.
//!
//! This is a text scan, not a parse, so it also works on half-typed code.
//! It shares the string and call scanning of the Laravel module.

use std::path::Path;

use super::index::ROUTE_VERBS;
use crate::php::framework::laravel::site::{
    Region, Str, argument_count, around, callee, closers_of, php_regions, strings,
};

/// What a string refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `view('x')`
    View,
    /// `route_to('x')`, `url_to('x')`, `redirect()->route('x')`
    Route,
    /// `config('X')`
    Config,
    /// `env('a.b')`
    Env,
    /// `model('X')`
    Model,
    /// `service('x')`
    Service,
    /// `lang('File.key')`
    Lang,
    /// `helper('x')`, `helper(['a', 'b'])`
    Helper,
    /// `'Controller::method'` in `$routes->verb('path', …)` of `Routes.php`
    Handler,
}

/// A string and what it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub kind: Kind,
    /// Byte range of the contents, without the quotes.
    pub start: usize,
    pub end: usize,
    /// For [`Kind::Env`]: the call passes a fallback value.
    pub has_default: bool,
    /// The `(` of the call the string is an argument of.
    pub paren: Option<usize>,
}

/// `true` for the file whose route handlers [`site_at_in`] and
/// [`sites_in`] recognise.
pub fn is_routes_file(path: &Path) -> bool {
    path.ends_with("Config/Routes.php")
}

/// The function a plain call to `name` stands for.
fn function_kind(name: &str) -> Option<Kind> {
    Some(match name {
        "view" => Kind::View,
        "route_to" | "url_to" => Kind::Route,
        "config" => Kind::Config,
        "env" => Kind::Env,
        "model" => Kind::Model,
        "service" => Kind::Service,
        "lang" => Kind::Lang,
        "helper" => Kind::Helper,
        _ => return None,
    })
}

/// Index of the handler argument of `$routes->name(…)`.
fn handler_index(name: &str) -> usize {
    if name == "match" { 2 } else { 1 }
}

/// What `string` stands for, if anything.
fn classify(
    text: &str,
    string: &Str,
    floor: usize,
    closers: &std::collections::HashMap<usize, usize>,
    openers: &std::collections::HashMap<usize, usize>,
    handlers: bool,
) -> Option<Site> {
    let contents = &text[string.start..string.end];
    // Interpolation builds a name at run time.
    if text.as_bytes()[string.open] == b'"' && contents.contains('$') {
        return None;
    }
    // A key of an array (`'a' => …`) or one side of a concatenation names
    // nothing.
    let after_at = if string.closed {
        string.end + 1
    } else {
        string.end
    };
    let after = text[after_at.min(text.len())..].trim_start();
    if after.starts_with("=>") || after.starts_with('.') {
        return None;
    }
    if text[..string.open].trim_end().ends_with('.') {
        return None;
    }

    let found = around(text, string, floor, closers);
    let paren = found.paren?;
    let call = callee(text, paren)?;
    let site = |kind: Kind| Site {
        kind,
        start: string.start,
        end: string.end,
        has_default: false,
        paren: Some(paren),
    };

    let kind = match call.operator {
        None => function_kind(&call.name),
        Some("->") if call.name == "route" && call.scope.ends_with("redirect()") => {
            Some(Kind::Route)
        }
        Some("->")
            if handlers
                && ROUTE_VERBS.contains(&call.name.as_str())
                && call.scope.ends_with("routes")
                && found.index == handler_index(&call.name)
                && found.array.is_none() =>
        {
            return Some(site(Kind::Handler));
        }
        _ => None,
    }?;

    if found.index != 0 {
        return None;
    }
    if let Some(array) = found.array {
        // Only `helper(['a', 'b'])`: the array is the first argument itself.
        if kind != Kind::Helper || !text[paren + 1..array].trim().is_empty() {
            return None;
        }
    }
    let has_default = kind == Kind::Env && argument_count(text, paren, closers, openers) > 1;
    Some(Site {
        has_default,
        ..site(kind)
    })
}

fn region_of(regions: &[Region], at: usize) -> Option<&Region> {
    regions
        .iter()
        .find(|r| r.start <= at && at < r.end.max(r.start + 1))
}

fn site_at_with(text: &str, byte: usize, handlers: bool) -> Option<Site> {
    let regions = php_regions(text);
    let strs = strings(text, &regions, false);
    let string = strs
        .iter()
        .find(|s| s.start <= byte && byte <= s.end)
        .copied()?;
    let region = region_of(&regions, string.open)?;
    let (closers, openers) = closers_of(&strs);
    classify(text, &string, region.start, &closers, &openers, handlers)
}

fn sites_with(text: &str, handlers: bool) -> Vec<Site> {
    let regions = php_regions(text);
    let strs = strings(text, &regions, true);
    let (closers, openers) = closers_of(&strs);
    strs.iter()
        .filter(|s| s.closed)
        .filter_map(|s| {
            let region = region_of(&regions, s.open)?;
            classify(text, s, region.start, &closers, &openers, handlers)
        })
        .collect()
}

/// The string at `byte` and what it stands for. Route handlers are not
/// recognised; use [`site_at_in`] for `Routes.php`.
pub fn site_at(text: &str, byte: usize) -> Option<Site> {
    site_at_with(text, byte, false)
}

/// Every string of the text that stands for something, except route
/// handlers (see [`sites_in`]).
pub fn sites(text: &str) -> Vec<Site> {
    sites_with(text, false)
}

/// [`site_at`] for the file at `path`; also recognises route handlers when
/// it is `app/Config/Routes.php`.
pub fn site_at_in(path: &Path, text: &str, byte: usize) -> Option<Site> {
    site_at_with(text, byte, is_routes_file(path))
}

/// [`sites`] for the file at `path`; also lists route handlers when it is
/// `app/Config/Routes.php`.
pub fn sites_in(path: &Path, text: &str) -> Vec<Site> {
    sites_with(text, is_routes_file(path))
}

/// The cursor is inside a string literal of the PHP in `text`.
pub fn in_string(text: &str, byte: usize) -> bool {
    let regions = php_regions(text);
    strings(text, &regions, false)
        .iter()
        .any(|s| s.start <= byte && byte <= s.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_in(path: &str, text: &str) -> Option<(Kind, String)> {
        let cursor = text.find('|').expect("cursor");
        let source = text.replacen('|', "", 1);
        site_at_in(Path::new(path), &source, cursor)
            .map(|s| (s.kind, source[s.start..s.end].to_owned()))
    }

    fn php(code: &str) -> Option<(Kind, String)> {
        at_in("app/Controllers/Home.php", &format!("<?php\n{code}\n"))
    }

    fn routes(code: &str) -> Option<(Kind, String)> {
        at_in("/p/app/Config/Routes.php", &format!("<?php\n{code}\n"))
    }

    fn hit(kind: Kind, name: &str) -> Option<(Kind, String)> {
        Some((kind, name.to_owned()))
    }

    #[test]
    fn helper_functions_name_things() {
        for (code, kind, name) in [
            ("return view('adm|in/home');", Kind::View, "admin/home"),
            (
                "$u = route_to('users.sh|ow', 1);",
                Kind::Route,
                "users.show",
            ),
            ("$u = url_to('home|');", Kind::Route, "home"),
            ("$c = config('Ap|p');", Kind::Config, "App"),
            ("$e = env('app.base|URL');", Kind::Env, "app.baseURL"),
            ("$m = model('UserMo|del');", Kind::Model, "UserModel"),
            ("$s = service('mail|er');", Kind::Service, "mailer"),
            ("echo lang('App.ti|tle');", Kind::Lang, "App.title"),
            ("helper('ur|l');", Kind::Helper, "url"),
        ] {
            assert_eq!(php(code), hit(kind, name), "{code}");
        }
    }

    #[test]
    fn redirect_route_names_a_route() {
        assert_eq!(
            php("return redirect()->route('ho|me');"),
            hit(Kind::Route, "home")
        );
        assert_eq!(
            php("return \\redirect()->route('ho|me')->with('a', 'b');"),
            hit(Kind::Route, "home")
        );
    }

    #[test]
    fn redirect_other_methods_name_nothing() {
        assert_eq!(php("return redirect()->to('ho|me');"), None);
        assert_eq!(php("return $this->route('ho|me');"), None);
    }

    #[test]
    fn helper_accepts_an_array_of_names() {
        assert_eq!(php("helper(['url', 'te|xt']);"), hit(Kind::Helper, "text"));
        assert_eq!(php("helper(['ur|l', 'text']);"), hit(Kind::Helper, "url"));
        assert_eq!(php("helper( [ 'ur|l' ] );"), hit(Kind::Helper, "url"));
    }

    #[test]
    fn arrays_are_not_names_for_other_calls() {
        assert_eq!(php("view(['a|']);"), None);
        assert_eq!(php("config(['a|']);"), None);
        assert_eq!(php("helper(['a', ['b|']]);"), None);
    }

    #[test]
    fn only_the_first_argument_names_something() {
        assert_eq!(php("view('a', ['k' => 'v|']);"), None);
        assert_eq!(php("view('a', 'b|');"), None);
        assert_eq!(php("env('a.b', 'fall|back');"), None);
        assert_eq!(php("route_to('a', 'b|');"), None);
        assert_eq!(php("helper('a', 'b|');"), None);
    }

    #[test]
    fn env_reports_a_fallback_value() {
        let with = "<?php env('app.name', 'x');";
        let without = "<?php env('app.name');";
        let a = site_at(with, with.find("app").unwrap()).unwrap();
        let b = site_at(without, without.find("app").unwrap()).unwrap();
        assert!(a.has_default);
        assert!(!b.has_default);
        assert_eq!(a.kind, Kind::Env);
    }

    #[test]
    fn strings_outside_those_calls_name_nothing() {
        for code in [
            "$x = 'vi|ew';",
            "echo 'hel|lo';",
            "strtoupper('vi|ew');",
            "$this->view('ho|me');",
            "Foo::view('ho|me');",
            "$a = ['view' => 'va|lue'];",
            "foo(view('a'), 'b|');",
        ] {
            assert_eq!(php(code), None, "{code}");
        }
    }

    #[test]
    fn array_keys_name_nothing() {
        assert_eq!(php("view(['ke|y' => 1]);"), None);
    }

    #[test]
    fn concatenation_names_nothing() {
        assert_eq!(php("view('admin/' . $name|);"), None);
        assert_eq!(php("view('admin|/' . $name);"), None);
        assert_eq!(php("view($dir . 'ho|me');"), None);
        assert_eq!(php("route_to($a. 'ho|me');"), None);
    }

    #[test]
    fn interpolated_double_quotes_name_nothing() {
        assert_eq!(php("view(\"admin/$na|me\");"), None);
        assert_eq!(php("view(\"adm|in/home\");"), hit(Kind::View, "admin/home"));
    }

    #[test]
    fn comments_name_nothing() {
        for code in [
            "// view('ho|me');",
            "# view('ho|me');",
            "/* view('ho|me'); */",
            "/** @see config('Ap|p') */",
        ] {
            assert_eq!(php(code), None, "{code}");
        }
    }

    #[test]
    fn escaped_quotes_stay_inside_the_string() {
        assert_eq!(php(r"view('it\'s|');"), hit(Kind::View, r"it\'s"));
        assert_eq!(php(r"view('a\'b') . foo('c|');"), None);
    }

    #[test]
    fn namespaced_and_spaced_calls_count() {
        assert_eq!(php("\\view('ho|me');"), hit(Kind::View, "home"));
        assert_eq!(php("view ( 'ho|me' );"), hit(Kind::View, "home"));
    }

    #[test]
    fn half_typed_calls_are_recognised() {
        assert_eq!(php("view('|"), hit(Kind::View, ""));
        assert_eq!(php("route_to('us|"), hit(Kind::Route, "us"));
    }

    #[test]
    fn handlers_are_recognised_in_routes_php_only() {
        assert_eq!(
            routes("$routes->get('users', 'Users::in|dex');"),
            hit(Kind::Handler, "Users::index")
        );
        assert_eq!(
            routes("$routes->post('users', 'Users::cr|eate', ['as' => 'x']);"),
            hit(Kind::Handler, "Users::create")
        );
        assert_eq!(
            at_in(
                "app/Controllers/Home.php",
                "<?php\n$routes->get('users', 'Users::in|dex');"
            ),
            None
        );
        let text = "<?php $routes->get('a', 'A::b');";
        assert_eq!(site_at(text, text.find("A::").unwrap()), None);
    }

    #[test]
    fn handler_position_depends_on_the_method() {
        assert_eq!(routes("$routes->get('us|ers', 'Users::index');"), None);
        assert_eq!(routes("$routes->match(['get'], 'us|ers', 'U::i');"), None);
        assert_eq!(
            routes("$routes->match(['get', 'post'], 'users', 'Users::in|dex');"),
            hit(Kind::Handler, "Users::index")
        );
        assert_eq!(routes("$routes->get('a', 'A::b', ['as' => 'n|']);"), None);
        assert_eq!(routes("$routes->get('a', ['A', 'in|dex']);"), None);
        assert_eq!(routes("$routes->get('a', $h . 'A::i|');"), None);
    }

    #[test]
    fn handlers_inside_groups_are_recognised() {
        let code = "$routes->group('admin', static function ($routes) {\n    $routes->get('u', 'Admin\\Users::in|dex');\n});";
        assert_eq!(routes(code), hit(Kind::Handler, "Admin\\Users::index"));
    }

    #[test]
    fn view_helpers_still_work_in_routes_php() {
        assert_eq!(
            routes("$routes->get('a', static fn () => view('ho|me'));"),
            hit(Kind::View, "home")
        );
    }

    #[test]
    fn sites_lists_every_recognised_string() {
        let text = "<?php\nview('a');\n$x = 'plain';\nlang('App.t');\n// env('no')\nhelper(['url', 'text']);\nview('p' . $q);\n";
        let found: Vec<(Kind, &str)> = sites(text)
            .iter()
            .map(|s| (s.kind, &text[s.start..s.end]))
            .collect();
        assert_eq!(
            found,
            [
                (Kind::View, "a"),
                (Kind::Lang, "App.t"),
                (Kind::Helper, "url"),
                (Kind::Helper, "text"),
            ]
        );
    }

    #[test]
    fn sites_in_routes_php_include_handlers() {
        let text = "<?php\n$routes->get('a', 'A::b');\n$routes->get('c', 'C::d');\n";
        let found: Vec<(Kind, &str)> = sites_in(Path::new("app/Config/Routes.php"), text)
            .iter()
            .map(|s| (s.kind, &text[s.start..s.end]))
            .collect();
        assert_eq!(found, [(Kind::Handler, "A::b"), (Kind::Handler, "C::d")]);
        assert!(sites(text).is_empty());
    }

    #[test]
    fn text_outside_php_tags_has_no_sites() {
        let text = "view('a') <?php echo 1; ?> view('b')";
        assert!(sites(text).is_empty());
        assert_eq!(site_at(text, 6), None);
    }

    #[test]
    fn in_string_reports_any_literal() {
        let text = "<?php $a = 'abc'; $b = 1;";
        let at = text.find("abc").unwrap();
        assert!(in_string(text, at + 1));
        assert!(!in_string(text, text.find("$b").unwrap()));
        assert!(!in_string("plain 'abc'", 8));
    }

    #[test]
    fn routes_file_is_matched_by_path_suffix() {
        assert!(is_routes_file(Path::new("/x/app/Config/Routes.php")));
        assert!(is_routes_file(Path::new("Config/Routes.php")));
        assert!(!is_routes_file(Path::new("/x/app/Config/Routes.php.bak")));
        assert!(!is_routes_file(Path::new("/x/app/Routes.php")));
    }
}
