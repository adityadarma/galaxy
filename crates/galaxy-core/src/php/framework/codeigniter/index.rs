//! What a CodeIgniter 4 project defines, read from its files: views, route
//! names, paths and handlers, config classes, models, controllers, services,
//! language keys, helpers and `.env` keys. PHP never runs, so a route made
//! at run time (`resource`, auto-routing, a package) is not in here;
//! [`Index::routes_complete`] says when that may be the case.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

use crate::php::framework::common::files::{
    Item, Loc, at, file_loc, flatten_config, position, read,
};
use crate::php::framework::common::scan::matching_paren;

/// Most files one directory walk collects.
pub const MAX_FILES: usize = 20_000;
const MAX_DEPTH: usize = 8;

/// Route methods of `$routes->…` that define a single route.
pub(super) const ROUTE_VERBS: &[&str] = &[
    "get", "post", "put", "delete", "patch", "options", "head", "add", "match", "cli", "view",
];

#[derive(Debug, Default)]
pub struct Index {
    pub root: PathBuf,
    /// `app/Views/**/*.php`, named by the path below `app/Views` without
    /// `.php`, `/` separated.
    pub views: Vec<Item>,
    /// Routes named with `['as' => 'name']`; `detail` is the full path.
    pub routes: Vec<Item>,
    /// Every route path with its group prefixes; `detail` is the handler.
    pub route_paths: Vec<Item>,
    /// Handler strings (`Home::index`); `detail` is the route path and
    /// `class` the controller as written.
    pub handlers: Vec<Item>,
    /// `false` when routes the index cannot see may exist: `resource`,
    /// `presenter`, `map`, a dynamic path, auto-routing or no `Routes.php`.
    pub routes_complete: bool,
    /// `app/Config/*.php`, named by class.
    pub config: Vec<Item>,
    /// `app/Models/*.php`; `class` is `App\Models\<Name>`.
    pub models: Vec<Item>,
    /// `app/Controllers/**/*.php`, named with `\` for folders; `class` is
    /// `App\Controllers\<Name>`.
    pub controllers: Vec<Item>,
    /// Public static methods of `app/Config/Services.php`.
    pub services: Vec<Item>,
    /// Keys of `app/Language/<locale>/<File>.php` as `File.key`.
    pub lang: Vec<Item>,
    /// File names (`App` for `App.php`) of the default locale.
    pub lang_files: HashSet<String>,
    /// `$defaultLocale` of `app/Config/App.php`, else `en`.
    pub locale: String,
    /// `app/Helpers/*_helper.php`, named without the `_helper` suffix.
    pub helpers: Vec<Item>,
    /// Keys of `.env`, else `env`.
    pub env: Vec<Item>,
    pub has_env: bool,
}

/// Files below `dir`, sorted, skipping hidden entries, at most `cap`.
fn walk(dir: &Path, cap: usize) -> Vec<PathBuf> {
    fn go(dir: &Path, depth: usize, cap: usize, out: &mut Vec<PathBuf>) {
        if depth == 0 || out.len() >= cap {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if out.len() >= cap {
                return;
            }
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                go(&path, depth - 1, cap, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    go(dir, MAX_DEPTH, cap, &mut out);
    out
}

/// `.php` files directly in `dir`, sorted, at most `cap`.
fn php_files(dir: &Path, cap: usize) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_php(p) && p.is_file())
        .collect();
    paths.sort();
    paths.truncate(cap);
    paths
}

fn is_php(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("php")
        && !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_owned()
}

/// `a/b/C.php` below `base` as `a/b/C`.
fn relative(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?.to_str()?;
    Some(rel.strip_suffix(".php")?.replace('\\', "/"))
}

/// `true` when `root` has a `spark` script and either `app/Config/App.php`
/// or a Composer requirement on the CodeIgniter 4 framework or app starter.
fn is_codeigniter(root: &Path) -> bool {
    if !root.join("spark").is_file() {
        return false;
    }
    if root.join("app/Config/App.php").is_file() {
        return true;
    }
    std::fs::read_to_string(root.join("composer.json")).is_ok_and(|text| {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return false;
        };
        ["require", "require-dev"].iter().any(|section| {
            value
                .get(section)
                .and_then(|v| v.as_object())
                .is_some_and(|deps| {
                    deps.keys()
                        .any(|n| n == "codeigniter4/framework" || n == "codeigniter4/appstarter")
                })
        })
    })
}

/// `text` with comments blanked out (newlines kept), so offsets still match
/// the original.
fn mask_comments(text: &str) -> String {
    fn blank(out: &mut [u8], from: usize, to: usize) {
        for b in &mut out[from..to] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                let end = bytes[i..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(bytes.len(), |n| i + n);
                blank(&mut out, i, end);
                i = end;
            }
            b'#' if bytes.get(i + 1) != Some(&b'[') => {
                let end = bytes[i..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(bytes.len(), |n| i + n);
                blank(&mut out, i, end);
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |n| i + 2 + n + 2);
                blank(&mut out, i, end);
                i = end;
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// A `$routes->name(…)` call: where it starts and its argument spans.
struct Call {
    name: String,
    start: usize,
    args: Vec<(usize, usize)>,
}

/// Argument spans (trimmed) between `from` and `to`, split at top-level
/// commas. A trailing comma adds no argument.
fn split_args(text: &str, from: usize, to: usize) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let trim = |mut s: usize, mut e: usize| {
        while s < e && b[s].is_ascii_whitespace() {
            s += 1;
        }
        while e > s && b[e - 1].is_ascii_whitespace() {
            e -= 1;
        }
        (s, e)
    };
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = from;
    let mut i = from;
    while i < to {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < to && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(trim(start, i));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let last = trim(start, to);
    if last.0 < last.1 {
        out.push(last);
    }
    out
}

fn route_calls(masked: &str) -> Vec<Call> {
    static CALL: OnceLock<Regex> = OnceLock::new();
    let re = CALL.get_or_init(|| Regex::new(r"\$\w+\s*->\s*(\w+)\s*\(").expect("valid"));
    re.captures_iter(masked)
        .filter_map(|caps| {
            let name = &caps[1];
            let known = name == "group"
                || ROUTE_VERBS.contains(&name)
                || matches!(name, "resource" | "presenter" | "map");
            if !known {
                return None;
            }
            let whole = caps.get(0)?;
            let open = whole.end() - 1;
            let close = matching_paren(masked, open).min(masked.len());
            Some(Call {
                name: name.to_owned(),
                start: whole.start(),
                args: split_args(masked, open + 1, close),
            })
        })
        .collect()
}

/// A plain string literal filling `span`: byte of its contents and them.
fn literal(text: &str, (s, e): (usize, usize)) -> Option<(usize, &str)> {
    let b = text.as_bytes();
    let q = *b.get(s)?;
    if (q != b'\'' && q != b'"') || e < s + 2 || b[e - 1] != q {
        return None;
    }
    let mut i = s + 1;
    while i < e - 1 {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == q {
            return None;
        }
        i += 1;
    }
    if i != e - 1 {
        return None;
    }
    let inner = &text[s + 1..e - 1];
    if q == b'"' && inner.contains('$') {
        return None;
    }
    Some((s + 1, inner))
}

/// The handler a route call names: `'Home::index'` or
/// `[Home::class, 'index']` (as `Home::index`), with the byte it starts at.
fn handler_of(text: &str, span: (usize, usize)) -> Option<(usize, String)> {
    if let Some((start, inner)) = literal(text, span) {
        return Some((start - 1, inner.to_owned()));
    }
    static ARRAY: OnceLock<Regex> = OnceLock::new();
    let re = ARRAY.get_or_init(|| {
        Regex::new(r#"^\[\s*\\?([\w\\]+)::class\s*,\s*['"](\w+)['"]\s*,?\s*\]$"#).expect("valid")
    });
    let caps = re.captures(&text[span.0..span.1])?;
    Some((span.0, format!("{}::{}", &caps[1], &caps[2])))
}

fn join_path(prefixes: &[&str], path: &str) -> String {
    let parts: Vec<&str> = prefixes
        .iter()
        .copied()
        .chain(std::iter::once(path))
        .map(|p| p.trim_matches('/'))
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        "/".to_owned()
    } else {
        parts.join("/")
    }
}

impl Index {
    /// The index of the CodeIgniter 4 project at `root`; `None` for any
    /// other project.
    pub fn detect(root: &Path) -> Option<Self> {
        is_codeigniter(root).then(|| Self::build(root))
    }

    pub fn build(root: &Path) -> Self {
        Self::build_with_cap(root, MAX_FILES)
    }

    /// [`build`](Self::build) reading at most `cap` files per directory
    /// walk.
    pub fn build_with_cap(root: &Path, cap: usize) -> Self {
        let mut index = Index {
            root: root.to_path_buf(),
            locale: "en".to_owned(),
            ..Index::default()
        };
        index.read_locale();
        index.read_views(cap);
        index.read_routes(cap);
        index.read_classes(cap);
        index.read_services();
        index.read_lang(cap);
        index.read_helpers(cap);
        index.read_env();
        index
    }

    fn read_locale(&mut self) {
        static LOCALE: OnceLock<Regex> = OnceLock::new();
        let re = LOCALE.get_or_init(|| {
            Regex::new(r#"\$defaultLocale\s*=\s*['"]([\w-]+)['"]"#).expect("valid")
        });
        let Some(text) = read(&self.root.join("app/Config/App.php")) else {
            return;
        };
        if let Some(caps) = re.captures(&mask_comments(&text)) {
            self.locale = caps[1].to_owned();
        }
    }

    fn read_views(&mut self, cap: usize) {
        let base = self.root.join("app/Views");
        for path in walk(&base, cap) {
            if !is_php(&path) {
                continue;
            }
            let Some(name) = relative(&base, &path) else {
                continue;
            };
            self.views.push(Item {
                name,
                loc: file_loc(&path),
                ..Item::default()
            });
        }
    }

    fn read_routes(&mut self, cap: usize) {
        let path = self.root.join("app/Config/Routes.php");
        let Some(text) = read(&path) else { return };
        self.routes_complete = true;
        let masked = mask_comments(&text);
        if masked.contains("setAutoRoute(true)") {
            self.routes_complete = false;
        }
        let calls = route_calls(&masked);

        // Prefix and extent of every group.
        let mut groups: Vec<(String, usize, usize)> = Vec::new();
        for call in calls.iter().filter(|c| c.name == "group") {
            let prefix = match call.args.first().and_then(|a| literal(&masked, *a)) {
                Some((_, prefix)) => prefix.to_owned(),
                None => {
                    self.routes_complete = false;
                    String::new()
                }
            };
            let end = call.args.last().map_or(call.start, |a| a.1);
            groups.push((prefix, call.start, end));
        }

        static AS: OnceLock<Regex> = OnceLock::new();
        let as_re =
            AS.get_or_init(|| Regex::new(r#"['"]as['"]\s*=>\s*['"]([^'"]+)['"]"#).expect("valid"));

        for call in &calls {
            if self.route_paths.len() >= cap {
                break;
            }
            let name = call.name.as_str();
            if matches!(name, "resource" | "presenter" | "map") {
                self.routes_complete = false;
                continue;
            }
            if !ROUTE_VERBS.contains(&name) {
                continue;
            }
            let path_i = usize::from(name == "match");
            let Some(&span) = call.args.get(path_i) else {
                continue;
            };
            let Some((pstart, raw)) = literal(&masked, span) else {
                self.routes_complete = false;
                continue;
            };
            let prefixes: Vec<&str> = groups
                .iter()
                .filter(|g| g.1 < call.start && call.start < g.2)
                .map(|g| g.0.as_str())
                .collect();
            let full = join_path(&prefixes, raw);
            let handler = if name == "view" {
                None
            } else {
                call.args
                    .get(path_i + 1)
                    .and_then(|s| handler_of(&masked, *s))
            };
            self.route_paths.push(Item {
                name: full.clone(),
                detail: handler.as_ref().map_or(String::new(), |h| h.1.clone()),
                loc: at(&path, &text, pstart - 1),
                class: None,
            });
            if let Some((hstart, handler)) = handler {
                let class = handler
                    .split_once("::")
                    .map(|(c, _)| c.trim_start_matches('\\').to_owned())
                    .filter(|c| !c.is_empty());
                self.handlers.push(Item {
                    name: handler,
                    detail: full.clone(),
                    loc: at(&path, &text, hstart),
                    class,
                });
            }
            if let Some(&(os, oe)) = call.args.get(path_i + 2)
                && let Some(caps) = as_re.captures(&masked[os..oe])
                && let Some(m) = caps.get(1)
            {
                self.routes.push(Item {
                    name: m.as_str().to_owned(),
                    detail: full,
                    loc: at(&path, &text, os + m.start() - 1),
                    class: None,
                });
            }
        }
    }

    fn read_classes(&mut self, cap: usize) {
        let config = self.root.join("app/Config");
        for path in php_files(&config, cap) {
            let name = file_stem(&path);
            self.config.push(Item {
                class: Some(format!("Config\\{name}")),
                name,
                loc: file_loc(&path),
                ..Item::default()
            });
        }
        let models = self.root.join("app/Models");
        for path in php_files(&models, cap) {
            let name = file_stem(&path);
            self.models.push(Item {
                class: Some(format!("App\\Models\\{name}")),
                name,
                loc: file_loc(&path),
                ..Item::default()
            });
        }
        let base = self.root.join("app/Controllers");
        for path in walk(&base, cap) {
            if !is_php(&path) {
                continue;
            }
            let Some(rel) = relative(&base, &path) else {
                continue;
            };
            let name = rel.replace('/', "\\");
            self.controllers.push(Item {
                class: Some(format!("App\\Controllers\\{name}")),
                name,
                loc: file_loc(&path),
                ..Item::default()
            });
        }
    }

    fn read_services(&mut self) {
        static METHOD: OnceLock<Regex> = OnceLock::new();
        let re = METHOD
            .get_or_init(|| Regex::new(r"public\s+static\s+function\s+(\w+)\s*\(").expect("valid"));
        let path = self.root.join("app/Config/Services.php");
        let Some(text) = read(&path) else { return };
        let masked = mask_comments(&text);
        for caps in re.captures_iter(&masked) {
            let Some(m) = caps.get(1) else { continue };
            self.services.push(Item {
                name: m.as_str().to_owned(),
                detail: format!("{}()", m.as_str()),
                loc: at(&path, &text, m.start()),
                class: None,
            });
        }
    }

    fn read_lang(&mut self, cap: usize) {
        let dir = self.root.join("app/Language").join(&self.locale);
        for path in php_files(&dir, cap) {
            let Some(text) = read(&path) else { continue };
            let file = file_stem(&path);
            self.lang_files.insert(file.clone());
            for entry in flatten_config(&text) {
                if entry.is_array {
                    continue;
                }
                self.lang.push(Item {
                    name: format!("{file}.{}", entry.key),
                    detail: entry.value,
                    loc: Some(Loc {
                        path: path.clone(),
                        line: entry.line as u32,
                        column: entry.column_utf16 as u32,
                    }),
                    class: None,
                });
            }
        }
    }

    fn read_helpers(&mut self, cap: usize) {
        let dir = self.root.join("app/Helpers");
        for path in php_files(&dir, cap) {
            let stem = file_stem(&path);
            let Some(name) = stem.strip_suffix("_helper") else {
                continue;
            };
            self.helpers.push(Item {
                name: name.to_owned(),
                loc: file_loc(&path),
                ..Item::default()
            });
        }
    }

    fn read_env(&mut self) {
        static LINE: OnceLock<Regex> = OnceLock::new();
        let re = LINE.get_or_init(|| {
            Regex::new(r"^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_.]*)\s*=\s*(.*?)\s*$")
                .expect("valid")
        });
        let (path, text) = [".env", "env"]
            .iter()
            .map(|f| self.root.join(f))
            .find_map(|p| read(&p).map(|t| (p, t)))
            .map_or((PathBuf::new(), None), |(p, t)| (p, Some(t)));
        let Some(text) = text else { return };
        self.has_env = true;
        let mut seen = HashSet::new();
        for (row, line) in text.lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            let Some(caps) = re.captures(line) else {
                continue;
            };
            let key = caps[1].to_owned();
            if !seen.insert(key.clone()) {
                continue;
            }
            let value = caps[2].trim_matches(['"', '\'']).to_owned();
            let upper = key.to_ascii_uppercase();
            let secret = ["PASSWORD", "SECRET", "KEY", "TOKEN"]
                .iter()
                .any(|w| upper.contains(w));
            let (_, column) = position(line, caps.get(1).map_or(0, |m| m.start()));
            self.env.push(Item {
                detail: if secret && !value.is_empty() {
                    "••••••".into()
                } else {
                    value
                },
                name: key,
                loc: Some(Loc {
                    path: path.clone(),
                    line: row as u32,
                    column,
                }),
                class: None,
            });
        }
    }

    /// Names of `items`, for tests and quick lookups.
    pub fn names(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    const ROUTES: &str = r#"<?php
use CodeIgniter\Router\RouteCollection;

/** @var RouteCollection $routes */
$routes->get('/', 'Home::index', ['as' => 'home']);
// $routes->get('commented', 'Nope::nope', ['as' => 'nope']);
$routes->post('login', [Auth::class, 'attempt'], ['as' => 'login.post']);
$routes->group('admin', ['filter' => 'auth'], static function ($routes) {
    $routes->get('users', 'Admin\Users::index', ['as' => 'admin.users']);
    $routes->group('deep', static function ($routes) {
        $routes->put('x/(:num)', 'Admin\Deep::update/$1');
    });
});
$routes->match(['get', 'post'], 'contact', 'Home::contact', ['as' => 'contact']);
$routes->cli('tasks', 'Tasks::run');
"#;

    fn ci() -> tempfile::TempDir {
        project(&[
            ("spark", "<?php"),
            (
                "app/Config/App.php",
                "<?php\nclass App {\n    // public string $defaultLocale = 'xx';\n    public string $defaultLocale = 'fr';\n}\n",
            ),
            ("app/Config/Routes.php", ROUTES),
            (
                "app/Config/Services.php",
                "<?php\nclass Services extends BaseService {\n    public static function mailer($shared = true) {}\n    private static function hidden() {}\n    // public static function nope() {}\n    public static function cart() {}\n}\n",
            ),
            ("app/Config/Filters.php", "<?php"),
            ("app/Views/welcome.php", "<?php"),
            ("app/Views/admin/users/index.php", "<?php"),
            ("app/Views/notes.txt", "x"),
            (
                "app/Language/fr/App.php",
                "<?php\nreturn [\n    'title' => 'Titre',\n    'nested' => ['deep' => 'Profond'],\n];\n",
            ),
            (
                "app/Language/en/App.php",
                "<?php\nreturn ['en_only' => 'x'];",
            ),
            ("app/Controllers/Home.php", "<?php"),
            ("app/Controllers/Admin/Users.php", "<?php"),
            ("app/Models/UserModel.php", "<?php"),
            ("app/Helpers/text_helper.php", "<?php"),
            ("app/Helpers/other.php", "<?php"),
            (
                ".env",
                "# comment\nCI_ENVIRONMENT = development\napp.baseURL = 'http://localhost'\ndatabase.default.password = hunter2\nencryption.key =\n",
            ),
        ])
    }

    fn named(items: &[Item]) -> Vec<&str> {
        Index::names(items)
    }

    #[test]
    fn detects_with_spark_and_app_config() {
        let dir = project(&[("spark", ""), ("app/Config/App.php", "<?php")]);
        assert!(Index::detect(dir.path()).is_some());
    }

    #[test]
    fn detects_with_spark_and_composer_requirement() {
        for body in [
            r#"{"require":{"codeigniter4/framework":"^4"}}"#,
            r#"{"require":{"codeigniter4/appstarter":"^4"}}"#,
            r#"{"require-dev":{"codeigniter4/framework":"^4"}}"#,
        ] {
            let dir = project(&[("spark", ""), ("composer.json", body)]);
            assert!(Index::detect(dir.path()).is_some(), "{body}");
        }
    }

    #[test]
    fn rejects_projects_without_the_markers() {
        let no_spark = project(&[("app/Config/App.php", "<?php")]);
        assert!(Index::detect(no_spark.path()).is_none());
        let bare_spark = project(&[("spark", "")]);
        assert!(Index::detect(bare_spark.path()).is_none());
        let other = project(&[
            ("spark", ""),
            ("composer.json", r#"{"require":{"monolog/monolog":"^3"}}"#),
        ]);
        assert!(Index::detect(other.path()).is_none());
        let broken = project(&[("spark", ""), ("composer.json", "{not json")]);
        assert!(Index::detect(broken.path()).is_none());
    }

    #[test]
    fn views_are_named_by_relative_path() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(named(&index.views), ["admin/users/index", "welcome"]);
        assert!(index.views.iter().all(|v| v.loc.is_some()));
    }

    #[test]
    fn named_routes_carry_group_prefixes() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(
            named(&index.routes),
            ["home", "login.post", "admin.users", "contact"]
        );
        let users = index
            .routes
            .iter()
            .find(|r| r.name == "admin.users")
            .unwrap();
        assert_eq!(users.detail, "admin/users");
    }

    #[test]
    fn route_paths_include_nested_groups_and_skip_comments() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(
            named(&index.route_paths),
            [
                "/",
                "login",
                "admin/users",
                "admin/deep/x/(:num)",
                "contact",
                "tasks"
            ]
        );
        assert!(!index.routes.iter().any(|r| r.name == "nope"));
    }

    #[test]
    fn handlers_come_from_strings_and_class_arrays() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(
            named(&index.handlers),
            [
                "Home::index",
                "Auth::attempt",
                "Admin\\Users::index",
                "Admin\\Deep::update/$1",
                "Home::contact",
                "Tasks::run"
            ]
        );
        assert_eq!(index.handlers[1].class.as_deref(), Some("Auth"));
    }

    #[test]
    fn route_locations_point_into_routes_php() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        let home = index.routes.iter().find(|r| r.name == "home").unwrap();
        let loc = home.loc.as_ref().unwrap();
        assert!(loc.path.ends_with("app/Config/Routes.php"));
        assert_eq!(loc.line, 4);
        let line = ROUTES.lines().nth(4).unwrap();
        assert_eq!(loc.column as usize, line.find("'home'").unwrap());
    }

    #[test]
    fn plain_routes_file_is_complete() {
        let dir = ci();
        assert!(Index::detect(dir.path()).unwrap().routes_complete);
    }

    #[test]
    fn resource_presenter_map_and_dynamic_paths_mark_routes_incomplete() {
        for call in [
            "$routes->resource('photos');",
            "$routes->presenter('posts');",
            "$routes->map(['a' => 'B::c']);",
            "$routes->get($path, 'A::b');",
            "$routes->group($prefix, static function ($routes) {});",
            "$routes->setAutoRoute(true);",
        ] {
            let dir = project(&[
                ("spark", ""),
                ("app/Config/App.php", "<?php"),
                ("app/Config/Routes.php", &format!("<?php\n{call}\n")),
            ]);
            let index = Index::detect(dir.path()).unwrap();
            assert!(!index.routes_complete, "{call}");
        }
    }

    #[test]
    fn missing_routes_file_is_incomplete() {
        let dir = project(&[("spark", ""), ("app/Config/App.php", "<?php")]);
        let index = Index::detect(dir.path()).unwrap();
        assert!(!index.routes_complete);
        assert!(index.route_paths.is_empty());
    }

    #[test]
    fn config_models_and_controllers_are_listed() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(
            named(&index.config),
            ["App", "Filters", "Routes", "Services"]
        );
        assert_eq!(index.config[0].class.as_deref(), Some("Config\\App"));
        assert_eq!(named(&index.models), ["UserModel"]);
        assert_eq!(
            index.models[0].class.as_deref(),
            Some("App\\Models\\UserModel")
        );
        assert_eq!(named(&index.controllers), ["Admin\\Users", "Home"]);
        assert_eq!(
            index.controllers[0].class.as_deref(),
            Some("App\\Controllers\\Admin\\Users")
        );
    }

    #[test]
    fn services_are_the_public_static_methods() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(named(&index.services), ["mailer", "cart"]);
        let loc = index.services[0].loc.as_ref().unwrap();
        assert_eq!(loc.line, 2);
    }

    #[test]
    fn language_keys_use_the_default_locale() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(index.locale, "fr");
        assert_eq!(named(&index.lang), ["App.title", "App.nested.deep"]);
        assert!(index.lang_files.contains("App"));
        assert_eq!(index.lang[0].detail, "Titre");
    }

    #[test]
    fn language_falls_back_to_en() {
        let dir = project(&[
            ("spark", ""),
            ("app/Config/App.php", "<?php class App {}"),
            ("app/Language/en/Msg.php", "<?php return ['hi' => 'Hi'];"),
        ]);
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(index.locale, "en");
        assert_eq!(named(&index.lang), ["Msg.hi"]);
    }

    #[test]
    fn helpers_need_the_suffix() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(named(&index.helpers), ["text"]);
    }

    #[test]
    fn env_keys_are_dotted_and_secrets_hidden() {
        let dir = ci();
        let index = Index::detect(dir.path()).unwrap();
        assert!(index.has_env);
        assert_eq!(
            named(&index.env),
            [
                "CI_ENVIRONMENT",
                "app.baseURL",
                "database.default.password",
                "encryption.key"
            ]
        );
        assert_eq!(index.env[1].detail, "http://localhost");
        assert_eq!(index.env[2].detail, "••••••");
        assert_eq!(index.env[3].detail, "");
        assert_eq!(index.env[0].loc.as_ref().unwrap().line, 1);
    }

    #[test]
    fn env_falls_back_to_the_env_file() {
        let dir = project(&[
            ("spark", ""),
            ("app/Config/App.php", "<?php"),
            ("env", "app.name = Demo\n"),
        ]);
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(named(&index.env), ["app.name"]);
    }

    #[test]
    fn project_without_env_has_none() {
        let dir = project(&[("spark", ""), ("app/Config/App.php", "<?php")]);
        let index = Index::detect(dir.path()).unwrap();
        assert!(!index.has_env);
        assert!(index.env.is_empty());
    }

    #[test]
    fn walks_are_capped() {
        let mut files: Vec<(String, &str)> =
            vec![("spark".into(), ""), ("app/Config/App.php".into(), "<?php")];
        for n in 0..10 {
            files.push((format!("app/Views/v{n}.php"), "<?php"));
            files.push((format!("app/Models/M{n}.php"), "<?php"));
            files.push((format!("app/Controllers/C{n}.php"), "<?php"));
        }
        let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), *b)).collect();
        let dir = project(&refs);
        let index = Index::build_with_cap(dir.path(), 3);
        assert_eq!(index.views.len(), 3);
        assert_eq!(index.models.len(), 3);
        assert_eq!(index.controllers.len(), 3);
        let full = Index::build(dir.path());
        assert_eq!(full.views.len(), 10);
    }

    #[test]
    fn route_count_is_capped() {
        let mut routes = String::from("<?php\n");
        for n in 0..10 {
            routes.push_str(&format!("$routes->get('r{n}', 'A::b{n}');\n"));
        }
        let dir = project(&[
            ("spark", ""),
            ("app/Config/App.php", "<?php"),
            ("app/Config/Routes.php", &routes),
        ]);
        let index = Index::build_with_cap(dir.path(), 4);
        assert_eq!(index.route_paths.len(), 4);
    }

    #[test]
    fn masking_keeps_offsets_and_strings() {
        let src = "a // x\n'//' /* y\nz */ #w\nb";
        let masked = mask_comments(src);
        assert_eq!(masked.len(), src.len());
        assert!(masked.contains("'//'"));
        assert!(!masked.contains('x') && !masked.contains('y') && !masked.contains('w'));
        assert_eq!(masked.matches('\n').count(), src.matches('\n').count());
    }
}
