//! Completion, navigation, hover, problems and quick fixes for the strings
//! of a CodeIgniter 4 project: `view('home')`, `route_to('users.show')`,
//! `config('App')`, `model('UserModel')`, `lang('App.title')`, `env('a.b')`
//! and, in `app/Config/Routes.php`, the `Controller::method` handlers.

use std::path::Path;
use std::sync::Arc;

use super::{Index, Kind, Site, site_at_in, sites_in};
use crate::php::engine::{
    Action, Completion, FileChange, HoverResult, ItemKind, PhpEngine, Problem, Severity, Target,
    byte_offset, position_of,
};
use crate::php::framework::laravel::engine::{Found, describe, named, pick, target_of};
use crate::php::framework::laravel::{Item, Loc};
use crate::php::parse::{ClassInfo, Member, MemberKind};

/// Namespace route handlers are relative to unless the routes change it.
const DEFAULT_NAMESPACE: &str = "App\\Controllers\\";
const MAX_PROBLEMS: usize = 200;

fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::View => "View",
        Kind::Route => "Route",
        Kind::Config => "Config",
        Kind::Env => "Environment variable",
        Kind::Model => "Model",
        Kind::Service => "Service",
        Kind::Lang => "Language key",
        Kind::Helper => "Helper",
        Kind::Handler => "Route handler",
    }
}

fn item_kind(kind: Kind) -> ItemKind {
    match kind {
        Kind::Config | Kind::Model | Kind::Handler => ItemKind::Class,
        Kind::Env => ItemKind::Constant,
        Kind::Lang => ItemKind::Property,
        Kind::Service => ItemKind::Method,
        Kind::View | Kind::Route | Kind::Helper => ItemKind::Function,
    }
}

/// What a name resolves to, without a class behind it: the definition is
/// the place in the file, not the class declaration.
fn plain(item: &Item) -> Found {
    Found {
        detail: item.detail.clone(),
        loc: item.loc.clone(),
        class: None,
    }
}

/// `items`, with `detail` for those that have none.
fn filled(items: &[Item], detail: &str) -> Vec<Item> {
    items
        .iter()
        .map(|item| Item {
            detail: if item.detail.is_empty() {
                detail.to_owned()
            } else {
                item.detail.clone()
            },
            ..item.clone()
        })
        .collect()
}

/// A view name the index can judge: a relative path of the application's
/// own views. Module views (`Vendor\Views`) are not looked up.
fn safe_view(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '\0'])
        && !name.starts_with('/')
        && name.split('/').all(|part| !part.is_empty() && part != "..")
}

fn class_chars(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'\\')
}

fn identifier(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The routes file changes the namespace handlers are relative to, so the
/// default one says nothing about them.
fn namespace_changed(text: &str) -> bool {
    text.contains("'namespace'")
        || text.contains("\"namespace\"")
        || text.contains("setDefaultNamespace")
}

fn span(text: &str, (start, end): (usize, usize)) -> (u32, u32, u32, u32) {
    let (start_line, start_column, _) = position_of(text, start);
    let (end_line, end_column, _) = position_of(text, end);
    (start_line, start_column, end_line, end_column)
}

/// The parts of a `Controller::method/$1` handler.
struct Handler {
    class_range: (usize, usize),
    method: Option<(String, (usize, usize))>,
}

fn parse_handler(text: &str, site: &Site) -> Option<Handler> {
    let contents = &text[site.start..site.end];
    let (head, rest) = match contents.split_once("::") {
        Some((head, rest)) => (head, Some(rest)),
        None => (contents.split('/').next().unwrap_or(""), None),
    };
    if !class_chars(head) {
        return None;
    }
    let class_range = (site.start, site.start + head.len());
    let method = rest.and_then(|rest| {
        let name = rest.split('/').next().unwrap_or("");
        let start = site.start + head.len() + 2;
        identifier(name).then(|| (name.to_owned(), (start, start + name.len())))
    });
    Some(Handler {
        class_range,
        method,
    })
}

/// What a handler's controller name stands for.
enum Controller {
    Class(Arc<ClassInfo>),
    Indexed(Item),
    /// Nothing by that name, though it would be found if it existed.
    Missing,
    /// Not judged: it may live where the index cannot see.
    Unknown,
}

/// What a handler's method name stands for.
enum Method {
    Found(Arc<ClassInfo>, Member),
    Missing,
    Unknown,
}

enum Verdict {
    Found(Found),
    Missing(String),
}

/// A judged piece of a string, with the byte range it covers.
struct Hit {
    range: (usize, usize),
    verdict: Verdict,
}

/// The hit under `byte`, else the first.
fn hit_at(hits: Vec<Hit>, byte: usize) -> Option<Hit> {
    hits.into_iter()
        .find(|h| h.range.0 <= byte && byte <= h.range.1)
}

impl PhpEngine {
    // ---- lookup --------------------------------------------------------

    fn handler_controller(&mut self, idx: &Index, text: &str, name: &str) -> Controller {
        if let Some(absolute) = name.strip_prefix('\\') {
            return match self.class(absolute) {
                Some(class) => Controller::Class(class),
                None => Controller::Unknown,
            };
        }
        if namespace_changed(text) {
            return Controller::Unknown;
        }
        if let Some(class) = self.class(&format!("{DEFAULT_NAMESPACE}{name}")) {
            return Controller::Class(class);
        }
        if let Some(item) = idx
            .controllers
            .iter()
            .find(|i| i.name.eq_ignore_ascii_case(name))
        {
            return Controller::Indexed(item.clone());
        }
        if name.contains('\\')
            && let Some(class) = self.class(name)
        {
            return Controller::Class(class);
        }
        Controller::Missing
    }

    fn handler_method(&mut self, class: &Arc<ClassInfo>, name: &str) -> Method {
        if let Some((owner, member)) = self.find_member(class, name, true) {
            return Method::Found(owner, member);
        }
        if name.starts_with("__") || !self.fully_known(class) {
            return Method::Unknown;
        }
        let has_call = self.lineage(class).iter().any(|c| {
            c.members
                .iter()
                .any(|m| m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case("__call"))
        });
        if has_call {
            Method::Unknown
        } else {
            Method::Missing
        }
    }

    /// The pieces of a route handler: its controller and its method.
    fn handler_hits(&mut self, idx: &Index, text: &str, site: &Site) -> Vec<Hit> {
        let Some(handler) = parse_handler(text, site) else {
            return Vec::new();
        };
        let (from, to) = handler.class_range;
        let name = &text[from..to];
        let mut out = Vec::new();
        match self.handler_controller(idx, text, name) {
            Controller::Class(class) => {
                let loc = (!class.path.as_os_str().is_empty()).then(|| Loc {
                    path: class.path.clone(),
                    line: class.line,
                    column: class.column,
                });
                out.push(Hit {
                    range: handler.class_range,
                    verdict: Verdict::Found(Found {
                        detail: class.fqn.clone(),
                        loc,
                        class: None,
                    }),
                });
                if let Some((method, range)) = &handler.method {
                    match self.handler_method(&class, method) {
                        Method::Found(owner, member) => out.push(Hit {
                            range: *range,
                            verdict: Verdict::Found(Found {
                                detail: format!("{}{}", member.name, member.detail),
                                loc: Some(Loc {
                                    path: owner.path.clone(),
                                    line: member.line,
                                    column: member.column,
                                }),
                                class: None,
                            }),
                        }),
                        Method::Missing => out.push(Hit {
                            range: *range,
                            verdict: Verdict::Missing(format!(
                                "Method `{method}` not found in `{}`",
                                class.fqn
                            )),
                        }),
                        Method::Unknown => {}
                    }
                }
            }
            Controller::Indexed(item) => out.push(Hit {
                range: handler.class_range,
                verdict: Verdict::Found(Found {
                    detail: item.class.clone().unwrap_or_default(),
                    loc: item.loc.clone(),
                    class: None,
                }),
            }),
            Controller::Missing => out.push(Hit {
                range: handler.class_range,
                verdict: Verdict::Missing(format!("Controller `{name}` not found")),
            }),
            Controller::Unknown => {}
        }
        out
    }

    /// What the string of a non-handler `site` names. `None`: not judged.
    fn verdict(&mut self, idx: &Index, text: &str, site: &Site) -> Option<Verdict> {
        let name = &text[site.start..site.end];
        if name.is_empty() {
            return None;
        }
        let from_list = |items: &[Item], missing: bool| match pick(items, name) {
            Some(item) => Some(Verdict::Found(plain(item))),
            None => missing
                .then(|| Verdict::Missing(format!("{} `{name}` not found", label(site.kind)))),
        };
        match site.kind {
            Kind::View => {
                if !safe_view(name) {
                    return None;
                }
                let key = name.strip_suffix(".php").unwrap_or(name);
                match pick(&idx.views, key) {
                    Some(item) => Some(Verdict::Found(plain(item))),
                    None => Some(Verdict::Missing(format!("View `{name}` not found"))),
                }
            }
            Kind::Route => {
                for items in [&idx.routes, &idx.handlers, &idx.route_paths] {
                    if let Some(item) = pick(items, name) {
                        return Some(Verdict::Found(plain(item)));
                    }
                }
                let judged = idx.routes_complete && !name.contains(['{', '$', '(', ':']);
                judged.then(|| Verdict::Missing(format!("Route `{name}` not found")))
            }
            Kind::Config => from_list(&idx.config, !name.contains('\\')),
            Kind::Model => from_list(&idx.models, !name.contains(['\\', '/'])),
            Kind::Service => from_list(&idx.services, false),
            Kind::Helper => from_list(&idx.helpers, false),
            Kind::Env => {
                if let Some(item) = pick(&idx.env, name) {
                    return Some(Verdict::Found(plain(item)));
                }
                (idx.has_env && !site.has_default).then(|| {
                    Verdict::Missing(format!("Environment variable `{name}` is not set in .env"))
                })
            }
            Kind::Lang => {
                if let Some(item) = pick(&idx.lang, name) {
                    return Some(Verdict::Found(plain(item)));
                }
                let (file, _) = name.split_once('.')?;
                if !idx.lang_files.contains(file) {
                    return None;
                }
                // A key that holds an array of keys is a valid name too.
                let prefix = format!("{name}.");
                if let Some(item) = idx.lang.iter().find(|i| i.name.starts_with(&prefix)) {
                    return Some(Verdict::Found(plain(item)));
                }
                Some(Verdict::Missing(format!("Language key `{name}` not found")))
            }
            Kind::Handler => None,
        }
    }

    /// The judged pieces of the string of `site`.
    fn hits(&mut self, idx: &Index, text: &str, site: &Site) -> Vec<Hit> {
        if site.kind == Kind::Handler {
            return self.handler_hits(idx, text, site);
        }
        match self.verdict(idx, text, site) {
            Some(verdict) => vec![Hit {
                range: (site.start, site.end),
                verdict,
            }],
            None => Vec::new(),
        }
    }

    // ---- completion ----------------------------------------------------

    /// Suggestions for the CodeIgniter string the cursor is in. `None` when
    /// it is not in one, so the caller goes on with plain PHP.
    pub(super) fn codeigniter_complete(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Completion>> {
        let byte = byte_offset(text, line, column)?;
        let site = site_at_in(path, text, byte)?;
        let typed = &text[site.start..byte.min(site.end)];
        let from = position_of(text, site.start).1;
        let kind = item_kind(site.kind);
        let items = match site.kind {
            Kind::View => named(typed, &filled(&idx.views, "View"), kind, from),
            Kind::Route => named(typed, &idx.routes, kind, from),
            Kind::Config => named(typed, &filled(&idx.config, "Config"), kind, from),
            Kind::Env => named(typed, &idx.env, kind, from),
            Kind::Model => named(typed, &filled(&idx.models, "Model"), kind, from),
            Kind::Service => named(typed, &idx.services, kind, from),
            Kind::Lang => named(typed, &idx.lang, kind, from),
            Kind::Helper => named(typed, &filled(&idx.helpers, "Helper"), kind, from),
            Kind::Handler => self.handler_completions(&idx, text, typed, from),
        };
        Some(items)
    }

    /// Controller names before `::`, public methods of the controller after.
    fn handler_completions(
        &mut self,
        idx: &Index,
        text: &str,
        typed: &str,
        from: u32,
    ) -> Vec<Completion> {
        let Some((class, method)) = typed.split_once("::") else {
            let controllers = filled(&idx.controllers, "Controller");
            return named(typed, &controllers, ItemKind::Class, from);
        };
        if !class_chars(class) || method.contains('/') {
            return Vec::new();
        }
        let Controller::Class(class_info) = self.handler_controller(idx, text, class) else {
            return Vec::new();
        };
        let methods = self.action_methods(&class_info, false);
        let start = from + typed[..class.len() + 2].encode_utf16().count() as u32;
        named(method, &methods, ItemKind::Method, start)
    }

    // ---- definition and hover ------------------------------------------

    /// Where the CodeIgniter string under the cursor is defined.
    pub(super) fn codeigniter_definition(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Target>> {
        let byte = byte_offset(text, line, column)?;
        let site = site_at_in(path, text, byte)?;
        let hits = self.hits(&idx, text, &site);
        let Verdict::Found(hit) = hit_at(hits, byte)?.verdict else {
            return None;
        };
        hit.loc.as_ref().map(|loc| vec![target_of(loc)])
    }

    /// Documentation for the CodeIgniter string under the cursor.
    pub(super) fn codeigniter_hover(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        let byte = byte_offset(text, line, column)?;
        let site = site_at_in(path, text, byte)?;
        let hits = self.hits(&idx, text, &site);
        let hit = hit_at(hits, byte)?;
        let Verdict::Found(found) = hit.verdict else {
            return None;
        };
        let name = &text[hit.range.0..hit.range.1];
        let what = match site.kind {
            Kind::Handler if hit.range.0 == site.start => "Controller",
            Kind::Handler => "Controller method",
            kind => label(kind),
        };
        let plain_text = matches!(site.kind, Kind::Lang | Kind::Handler);
        let markdown = describe(what, name, &found, plain_text, &idx.root);
        let (start_line, start_column, _, end_column) = span(text, hit.range);
        Some(HoverResult {
            markdown,
            line: start_line,
            start_column,
            end_column,
        })
    }

    // ---- problems and actions ------------------------------------------

    /// Strings that name something the project does not have.
    pub(super) fn codeigniter_problems(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
    ) -> Vec<Problem> {
        let mut out = Vec::new();
        for site in sites_in(path, text) {
            if out.len() >= MAX_PROBLEMS {
                break;
            }
            let severity = match site.kind {
                Kind::Env | Kind::Lang => Severity::Hint,
                _ => Severity::Warning,
            };
            for hit in self.hits(&idx, text, &site) {
                let Verdict::Missing(message) = hit.verdict else {
                    continue;
                };
                let (start_line, start_column, end_line, end_column) = span(text, hit.range);
                out.push(Problem {
                    start_line,
                    start_column,
                    end_line,
                    end_column,
                    severity,
                    message,
                    unnecessary: false,
                });
            }
        }
        out
    }

    /// The fix for a missing view: create it.
    pub(super) fn codeigniter_actions(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
        byte: usize,
        out: &mut Vec<Action>,
    ) {
        let Some(site) = site_at_in(path, text, byte) else {
            return;
        };
        if site.kind != Kind::View {
            return;
        }
        let Some(Verdict::Missing(_)) = self.verdict(&idx, text, &site) else {
            return;
        };
        let name = &text[site.start..site.end];
        let key = name.strip_suffix(".php").unwrap_or(name);
        out.push(Action {
            title: format!("Create view `{name}`"),
            edits: Vec::new(),
            files: vec![FileChange {
                path: idx.root.join("app/Views").join(format!("{key}.php")),
                line: 0,
                column: 0,
                text: "<div>\n    \n</div>\n".to_owned(),
            }],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::CodeIgniterFramework;
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const ROUTES: &str = r#"<?php
use CodeIgniter\Router\RouteCollection;

/** @var RouteCollection $routes */
$routes->get('/', 'Home::index', ['as' => 'home']);
$routes->get('about', 'Home::about');
$routes->get('users/(:num)', 'Admin\Users::list/$1', ['as' => 'users.list']);
"#;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "spark", "<?php");
        write(
            r,
            "composer.json",
            r#"{"autoload":{"psr-4":{"App\\":"app/","CodeIgniter\\":"lib/CodeIgniter/"}}}"#,
        );
        write(
            r,
            "app/Config/App.php",
            "<?php\nnamespace Config;\n\nclass App\n{\n    public string $defaultLocale = 'en';\n}\n",
        );
        write(r, "app/Config/Routes.php", ROUTES);
        write(
            r,
            "app/Config/Services.php",
            "<?php\nnamespace Config;\n\nclass Services\n{\n    public static function mailer() {}\n}\n",
        );
        write(r, "app/Views/home.php", "<h1>home</h1>");
        write(r, "app/Views/admin/dash.php", "<h1>dash</h1>");
        write(
            r,
            "app/Language/en/App.php",
            "<?php\nreturn [\n    'title' => 'Title',\n    'nested' => ['deep' => 'Deep'],\n];\n",
        );
        write(
            r,
            "app/Controllers/Home.php",
            "<?php\nnamespace App\\Controllers;\n\nclass Home\n{\n    public function index() {}\n\n    public function about() {}\n\n    private function hidden() {}\n}\n",
        );
        write(
            r,
            "app/Controllers/Admin/Users.php",
            "<?php\nnamespace App\\Controllers\\Admin;\n\nclass Users\n{\n    public function list() {}\n}\n",
        );
        write(
            r,
            "app/Controllers/Magic.php",
            "<?php\nnamespace App\\Controllers;\n\nclass Magic\n{\n    public function __call($name, $args) {}\n}\n",
        );
        write(
            r,
            "app/Models/UserModel.php",
            "<?php\nnamespace App\\Models;\n\nuse CodeIgniter\\Model;\n\nclass UserModel extends Model\n{\n    public function active() {}\n}\n",
        );
        write(
            r,
            "app/Models/Plain.php",
            "<?php\nnamespace App\\Models;\n\nclass Plain\n{\n}\n",
        );
        write(r, "app/Helpers/text_helper.php", "<?php\n");
        write(
            r,
            "lib/CodeIgniter/Model.php",
            "<?php\nnamespace CodeIgniter;\n\nclass Model\n{\n    public function __call($name, $args) {}\n\n    public function save($row): bool { return true; }\n}\n",
        );
        write(
            r,
            "lib/CodeIgniter/Database/BaseBuilder.php",
            "<?php\nnamespace CodeIgniter\\Database;\n\nclass BaseBuilder\n{\n    /** @return $this */\n    public function where($key, $value = null) { return $this; }\n\n    /** @return $this */\n    public function orderBy($by) { return $this; }\n\n    /** @return array|object|null */\n    public function first() {}\n\n    protected function resetRun() {}\n}\n",
        );
        write(
            r,
            ".env",
            "CI_ENVIRONMENT = development\napp.baseURL = 'http://localhost'\ndatabase.default.password = hunter2\n",
        );
        dir
    }

    fn engine(dir: &Path) -> PhpEngine {
        let framework = CodeIgniterFramework::detect(dir).expect("detected");
        PhpEngine::with_frameworks(dir, vec![Arc::new(framework)])
    }

    /// `text` with `|` at the cursor: the text and the position.
    fn cursor(text: &str) -> (String, u32, u32) {
        let marker = text.find('|').expect("cursor");
        let before = &text[..marker];
        let line = before.matches('\n').count() as u32;
        let column = before.rsplit('\n').next().unwrap().encode_utf16().count() as u32;
        (text.replacen('|', "", 1), line, column)
    }

    fn labels(items: Vec<Completion>) -> Vec<String> {
        items.into_iter().map(|i| i.label).collect()
    }

    fn complete_php(dir: &Path, code: &str) -> Vec<Completion> {
        let (text, line, column) = cursor(&format!("<?php\n{code}\n"));
        engine(dir).complete(&dir.join("app/Controllers/X.php"), &text, line, column)
    }

    fn complete_routes(dir: &Path, code: &str) -> Vec<Completion> {
        let (text, line, column) = cursor(&format!("<?php\n{code}\n"));
        engine(dir).complete(&dir.join("app/Config/Routes.php"), &text, line, column)
    }

    fn define(dir: &Path, rel: &str, code: &str) -> Vec<Target> {
        let (text, line, column) = cursor(&format!("<?php\n{code}\n"));
        engine(dir).definition(&dir.join(rel), &text, line, column)
    }

    fn define_php(dir: &Path, code: &str) -> Vec<Target> {
        define(dir, "app/Controllers/X.php", code)
    }

    fn define_routes(dir: &Path, code: &str) -> Vec<Target> {
        define(dir, "app/Config/Routes.php", code)
    }

    fn hover_at(dir: &Path, rel: &str, code: &str) -> Option<HoverResult> {
        let (text, line, column) = cursor(&format!("<?php\n{code}\n"));
        engine(dir).hover(&dir.join(rel), &text, line, column)
    }

    fn problems(dir: &Path, rel: &str, body: &str) -> Vec<Problem> {
        engine(dir).diagnostics(&dir.join(rel), &format!("<?php\n{body}\n"))
    }

    fn messages(problems: &[Problem]) -> Vec<&str> {
        problems.iter().map(|p| p.message.as_str()).collect()
    }

    // ---- completion ----------------------------------------------------

    #[test]
    fn completes_view_names_and_replaces_the_string() {
        let dir = project();
        let items = complete_php(dir.path(), "view('|');");
        assert_eq!(labels(items.clone()), vec!["admin/dash", "home"]);
        assert_eq!(items[0].detail.as_deref(), Some("View"));
        assert_eq!(items[0].replace_from, Some(6));
        let items = complete_php(dir.path(), "return view('adm|');");
        assert_eq!(labels(items.clone()), vec!["admin/dash"]);
        assert_eq!(items[0].replace_from, Some(13));
    }

    #[test]
    fn completes_route_names_with_their_path() {
        let dir = project();
        let items = complete_php(dir.path(), "route_to('|');");
        assert_eq!(labels(items.clone()), vec!["home", "users.list"]);
        assert_eq!(items[0].detail.as_deref(), Some("/"));
        assert_eq!(items[1].detail.as_deref(), Some("users/(:num)"));
        let items = complete_php(dir.path(), "return redirect()->route('us|');");
        assert_eq!(labels(items), vec!["users.list"]);
    }

    #[test]
    fn completes_config_model_service_and_helper_names() {
        let dir = project();
        let p = dir.path();
        assert_eq!(
            labels(complete_php(p, "config('|');")),
            vec!["App", "Routes", "Services"]
        );
        assert_eq!(
            labels(complete_php(p, "model('|');")),
            vec!["Plain", "UserModel"]
        );
        assert_eq!(labels(complete_php(p, "service('ma|');")), vec!["mailer"]);
        let items = complete_php(p, "helper(['url', 'te|']);");
        assert_eq!(labels(items.clone()), vec!["text"]);
        assert_eq!(items[0].detail.as_deref(), Some("Helper"));
    }

    #[test]
    fn completes_env_keys_and_language_keys() {
        let dir = project();
        let p = dir.path();
        assert_eq!(
            labels(complete_php(p, "env('app.|');")),
            vec!["app.baseURL"]
        );
        let items = complete_php(p, "lang('App.ti|');");
        assert_eq!(labels(items.clone()), vec!["App.title"]);
        assert_eq!(items[0].detail.as_deref(), Some("Title"));
        assert_eq!(items[0].replace_from, Some(6));
    }

    #[test]
    fn completes_controllers_before_the_separator() {
        let dir = project();
        let items = complete_routes(dir.path(), "$routes->get('x', 'Ad|');");
        assert_eq!(labels(items.clone()), vec!["Admin\\Users"]);
        assert_eq!(items[0].replace_from, Some(19));
        let all = complete_routes(dir.path(), "$routes->get('x', '|');");
        assert_eq!(labels(all), vec!["Admin\\Users", "Home", "Magic"]);
    }

    #[test]
    fn completes_public_methods_after_the_separator() {
        let dir = project();
        let items = complete_routes(dir.path(), "$routes->get('x', 'Home::|');");
        assert_eq!(labels(items.clone()), vec!["about", "index"]);
        assert_eq!(items[0].replace_from, Some(25));
        let items = complete_routes(dir.path(), "$routes->get('x', 'Admin\\Users::l|');");
        assert_eq!(labels(items), vec!["list"]);
        assert!(complete_routes(dir.path(), "$routes->get('x', 'Ghost::|');").is_empty());
    }

    #[test]
    fn handlers_complete_only_in_routes_php() {
        let dir = project();
        let (text, line, column) = cursor("<?php\n$routes->get('x', 'Ho|');\n");
        let items = engine(dir.path()).complete(
            &dir.path().join("app/Controllers/X.php"),
            &text,
            line,
            column,
        );
        assert!(
            items
                .iter()
                .all(|i| i.detail.as_deref() != Some("Controller"))
        );
    }

    // ---- definition ----------------------------------------------------

    #[test]
    fn view_definition_is_the_view_file() {
        let dir = project();
        let found = define_php(dir.path(), "view('admin/da|sh');");
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("app/Views/admin/dash.php"));
        assert_eq!(found[0].line, 0);
        assert!(define_php(dir.path(), "view('gon|e');").is_empty());
    }

    #[test]
    fn route_definition_is_the_route_in_routes_php() {
        let dir = project();
        let found = define_php(dir.path(), "route_to('ho|me');");
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("app/Config/Routes.php"));
        assert_eq!(found[0].line, 4);
    }

    #[test]
    fn config_and_model_definitions_are_their_files() {
        let dir = project();
        let found = define_php(dir.path(), "config('Ap|p');");
        assert!(found[0].path.ends_with("app/Config/App.php"));
        let found = define_php(dir.path(), "model('UserMo|del');");
        assert!(found[0].path.ends_with("app/Models/UserModel.php"));
        assert!(define_php(dir.path(), "model('Nop|e');").is_empty());
    }

    #[test]
    fn env_service_lang_and_helper_definitions_point_at_lines() {
        let dir = project();
        let p = dir.path();
        let found = define_php(p, "env('app.base|URL');");
        assert!(found[0].path.ends_with(".env"));
        assert_eq!(found[0].line, 1);
        let found = define_php(p, "service('mai|ler');");
        assert!(found[0].path.ends_with("app/Config/Services.php"));
        assert_eq!(found[0].line, 5);
        let found = define_php(p, "lang('App.ti|tle');");
        assert!(found[0].path.ends_with("app/Language/en/App.php"));
        assert_eq!(found[0].line, 2);
        let found = define_php(p, "helper('te|xt');");
        assert!(found[0].path.ends_with("app/Helpers/text_helper.php"));
    }

    #[test]
    fn handler_definition_goes_to_the_class_and_the_method() {
        let dir = project();
        let p = dir.path();
        let found = define_routes(p, "$routes->get('x', 'Ho|me::index');");
        assert!(found[0].path.ends_with("app/Controllers/Home.php"));
        assert_eq!(found[0].line, 3);
        let found = define_routes(p, "$routes->get('x', 'Home::ab|out');");
        assert!(found[0].path.ends_with("app/Controllers/Home.php"));
        assert_eq!(found[0].line, 7);
    }

    #[test]
    fn handler_definition_honors_folders_tails_and_fqcn() {
        let dir = project();
        let p = dir.path();
        let found = define_routes(p, "$routes->get('x', 'Admin\\Users::li|st/$1');");
        assert!(found[0].path.ends_with("app/Controllers/Admin/Users.php"));
        assert_eq!(found[0].line, 5);
        let found = define_routes(p, "$routes->get('x', 'Admin\\Us|ers::list/$1');");
        assert_eq!(found[0].line, 3);
        let found = define_routes(p, "$routes->get('x', '\\App\\Controllers\\Ho|me::index');");
        assert!(found[0].path.ends_with("app/Controllers/Home.php"));
        let found = define_routes(p, "$routes->get('x', '\\App\\Controllers\\Home::in|dex');");
        assert_eq!(found[0].line, 5);
    }

    #[test]
    fn handler_definition_of_unknown_names_is_empty() {
        let dir = project();
        let p = dir.path();
        assert!(define_routes(p, "$routes->get('x', 'Gh|ost::index');").is_empty());
        assert!(define_routes(p, "$routes->get('x', 'Home::no|pe');").is_empty());
        assert!(define_routes(p, "$routes->get('x', 'Magic::any|thing');").is_empty());
    }

    #[test]
    fn handler_definition_needs_routes_php() {
        let dir = project();
        let found = define_php(dir.path(), "$routes->get('x', 'Ho|me::index');");
        assert!(found.is_empty());
    }

    // ---- hover ---------------------------------------------------------

    #[test]
    fn view_hover_shows_the_path() {
        let dir = project();
        let hover = hover_at(dir.path(), "app/Controllers/X.php", "view('ho|me');").unwrap();
        assert!(
            hover.markdown.contains("app/Views/home.php"),
            "{}",
            hover.markdown
        );
        assert_eq!(
            (hover.line, hover.start_column, hover.end_column),
            (1, 6, 10)
        );
    }

    #[test]
    fn route_and_lang_hover_show_values() {
        let dir = project();
        let hover = hover_at(
            dir.path(),
            "app/Controllers/X.php",
            "route_to('users.li|st');",
        )
        .unwrap();
        assert!(
            hover.markdown.contains("`users/(:num)`"),
            "{}",
            hover.markdown
        );
        let hover = hover_at(dir.path(), "app/Controllers/X.php", "lang('App.ti|tle');").unwrap();
        assert!(hover.markdown.contains("Title"), "{}", hover.markdown);
        assert!(!hover.markdown.contains("`Title`"));
    }

    #[test]
    fn env_hover_hides_secrets() {
        let dir = project();
        let hover = hover_at(
            dir.path(),
            "app/Controllers/X.php",
            "env('database.default.pass|word');",
        )
        .unwrap();
        assert!(hover.markdown.contains("••••••"), "{}", hover.markdown);
        assert!(!hover.markdown.contains("hunter2"));
    }

    #[test]
    fn handler_hover_covers_the_part_under_the_cursor() {
        let dir = project();
        let rel = "app/Config/Routes.php";
        let hover = hover_at(dir.path(), rel, "$routes->get('x', 'Ho|me::index');").unwrap();
        assert!(
            hover.markdown.contains("**Controller**"),
            "{}",
            hover.markdown
        );
        assert!(hover.markdown.contains("App\\Controllers\\Home"));
        assert_eq!((hover.start_column, hover.end_column), (19, 23));
        let hover = hover_at(dir.path(), rel, "$routes->get('x', 'Home::in|dex');").unwrap();
        assert!(hover.markdown.contains("Controller method"));
        assert_eq!((hover.start_column, hover.end_column), (25, 30));
        assert!(hover_at(dir.path(), rel, "$routes->get('x', 'Gh|ost::index');").is_none());
    }

    // ---- problems ------------------------------------------------------

    #[test]
    fn unknown_views_are_warnings() {
        let dir = project();
        let found = problems(
            dir.path(),
            "app/Controllers/X.php",
            "view('home');\nview('gone');\nview('');\nview('Pkg\\Views\\x');",
        );
        assert_eq!(messages(&found), vec!["View `gone` not found"]);
        assert_eq!(found[0].severity, Severity::Warning);
        assert_eq!((found[0].start_line, found[0].start_column), (2, 6));
        assert_eq!(found[0].end_column, 10);
    }

    #[test]
    fn routes_are_judged_only_when_the_list_is_complete() {
        let dir = project();
        let found = problems(
            dir.path(),
            "app/Controllers/X.php",
            "route_to('home');\nroute_to('nowhere');\nroute_to('Home::index');",
        );
        assert_eq!(messages(&found), vec!["Route `nowhere` not found"]);

        write(
            dir.path(),
            "app/Config/Routes.php",
            "<?php\n$routes->resource('photos');\n$routes->get('/', 'Home::index', ['as' => 'home']);\n",
        );
        let found = problems(dir.path(), "app/Controllers/X.php", "route_to('nowhere');");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn unknown_controllers_and_methods_are_flagged() {
        let dir = project();
        let body = "$routes->get('a', 'Home::index');\n$routes->get('b', 'Ghost::index');\n$routes->get('c', 'Home::nope');\n$routes->get('d', 'Magic::anything');\n$routes->get('e', 'Admin\\Users::list/$1');";
        let found = problems(dir.path(), "app/Config/Routes.php", body);
        assert_eq!(
            messages(&found),
            vec![
                "Controller `Ghost` not found",
                "Method `nope` not found in `App\\Controllers\\Home`"
            ]
        );
        assert_eq!((found[0].start_line, found[0].start_column), (2, 19));
        assert_eq!(found[0].end_column, 24);
        assert_eq!((found[1].start_line, found[1].start_column), (3, 25));
    }

    #[test]
    fn handlers_are_left_alone_when_the_namespace_changes() {
        let dir = project();
        let body = "$routes->group('api', ['namespace' => 'App\\Controllers\\Api'], static function ($routes) {\n    $routes->get('a', 'Ghost::index');\n});";
        assert!(problems(dir.path(), "app/Config/Routes.php", body).is_empty());
    }

    #[test]
    fn handlers_with_unresolvable_parents_are_not_judged() {
        let dir = project();
        write(
            dir.path(),
            "app/Controllers/Child.php",
            "<?php\nnamespace App\\Controllers;\n\nclass Child extends \\Vendor\\Base\n{\n}\n",
        );
        let body = "$routes->get('a', 'Child::whatever');";
        assert!(problems(dir.path(), "app/Config/Routes.php", body).is_empty());
    }

    #[test]
    fn config_and_model_names_must_exist() {
        let dir = project();
        let found = problems(
            dir.path(),
            "app/Controllers/X.php",
            "config('App');\nconfig('Nope');\nconfig('Vendor\\Config\\Pkg');\nmodel('UserModel');\nmodel('Ghost');\nmodel('Blog/Post');",
        );
        assert_eq!(
            messages(&found),
            vec!["Config `Nope` not found", "Model `Ghost` not found"]
        );
    }

    #[test]
    fn service_and_helper_names_are_never_flagged() {
        let dir = project();
        let found = problems(
            dir.path(),
            "app/Controllers/X.php",
            "service('unknown');\nhelper('url');\nhelper(['nope']);",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn unknown_env_keys_are_hints() {
        let dir = project();
        let found = problems(
            dir.path(),
            "app/Controllers/X.php",
            "env('app.baseURL');\nenv('NOPE');\nenv('NOPE', 'fallback');",
        );
        assert_eq!(
            messages(&found),
            vec!["Environment variable `NOPE` is not set in .env"]
        );
        assert_eq!(found[0].severity, Severity::Hint);

        std::fs::remove_file(dir.path().join(".env")).unwrap();
        let found = problems(dir.path(), "app/Controllers/X.php", "env('NOPE');");
        assert!(found.is_empty());
    }

    #[test]
    fn missing_language_keys_are_hints_only_for_known_files() {
        let dir = project();
        let found = problems(
            dir.path(),
            "app/Controllers/X.php",
            "lang('App.title');\nlang('App.nested');\nlang('App.gone');\nlang('Other.key');\nlang('plain');",
        );
        assert_eq!(messages(&found), vec!["Language key `App.gone` not found"]);
        assert_eq!(found[0].severity, Severity::Hint);
    }

    #[test]
    fn plain_php_and_comments_give_no_problems() {
        let dir = project();
        let body = "$x = 'view';\n// view('gone');\nfoo('gone');\nview('home' . $a);";
        assert!(problems(dir.path(), "app/Controllers/X.php", body).is_empty());
    }

    // ---- actions -------------------------------------------------------

    #[test]
    fn offers_to_create_a_missing_view() {
        let dir = project();
        let (text, line, column) = cursor("<?php\nview('admin/us|ers');\n");
        let actions = engine(dir.path()).actions(
            &dir.path().join("app/Controllers/X.php"),
            &text,
            line,
            column,
        );
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].title, "Create view `admin/users`");
        let change = &actions[0].files[0];
        assert!(change.path.ends_with("app/Views/admin/users.php"));
        assert_eq!((change.line, change.column), (0, 0));
        assert!(change.text.contains("<div>"));
    }

    #[test]
    fn existing_views_and_other_kinds_get_no_action() {
        let dir = project();
        let at = |code: &str| {
            let (text, line, column) = cursor(&format!("<?php\n{code}\n"));
            engine(dir.path()).actions(
                &dir.path().join("app/Controllers/X.php"),
                &text,
                line,
                column,
            )
        };
        assert!(at("view('ho|me');").is_empty());
        assert!(at("config('No|pe');").is_empty());
        assert!(at("env('NO|PE');").is_empty());
        assert!(at("view('../ou|t');").is_empty());
    }

    // ---- model forwarding ----------------------------------------------

    const CONTROLLER: &str = "<?php\nnamespace App\\Controllers;\n\nuse App\\Models\\UserModel;\nuse App\\Models\\Plain;\n\nclass X\n{\n    public function go()\n    {\n        $userModel = new UserModel();\n        $userModel->where('id', 1)->first();\n        $plain = new Plain();\n        $plain->where('id', 1);\n    }\n}\n";

    fn at_word(
        dir: &Path,
        line: u32,
        needle: &str,
        run: impl FnOnce(&mut PhpEngine, &Path, &str, u32, u32) -> Vec<Target>,
    ) -> Vec<Target> {
        let row = CONTROLLER.lines().nth(line as usize).unwrap();
        let column = row.find(needle).unwrap() as u32 + 1;
        run(
            &mut engine(dir),
            &dir.join("app/Controllers/X.php"),
            CONTROLLER,
            line,
            column,
        )
    }

    #[test]
    fn model_calls_resolve_to_the_builder() {
        let dir = project();
        let found = at_word(dir.path(), 11, "where", |e, p, t, l, c| {
            e.definition(p, t, l, c)
        });
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .path
                .ends_with("lib/CodeIgniter/Database/BaseBuilder.php")
        );
        assert_eq!(found[0].line, 6);
    }

    #[test]
    fn chained_builder_calls_keep_resolving() {
        let dir = project();
        let found = at_word(dir.path(), 11, "first", |e, p, t, l, c| {
            e.definition(p, t, l, c)
        });
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .path
                .ends_with("lib/CodeIgniter/Database/BaseBuilder.php")
        );
        assert_eq!(found[0].line, 12);
    }

    #[test]
    fn model_members_win_over_forwarding() {
        let dir = project();
        let text = CONTROLLER.replace("->where('id', 1)->first()", "->active()");
        let row = text.lines().nth(11).unwrap();
        let column = row.find("active").unwrap() as u32 + 1;
        let found = engine(dir.path()).definition(
            &dir.path().join("app/Controllers/X.php"),
            &text,
            11,
            column,
        );
        assert!(found[0].path.ends_with("app/Models/UserModel.php"));
    }

    #[test]
    fn completes_declared_methods_on_a_model() {
        let dir = project();
        let text = CONTROLLER.replace("->where('id', 1)->first()", "->");
        let column = text.lines().nth(11).unwrap().find("->").unwrap() as u32 + 2;
        let items = engine(dir.path()).complete(
            &dir.path().join("app/Controllers/X.php"),
            &text,
            11,
            column,
        );
        let names = labels(items);
        // Member completion lists what the model declares; forwarded
        // builder methods come from a chained call (next test).
        for expected in ["active", "save"] {
            assert!(names.iter().any(|n| n == expected), "{expected}: {names:?}");
        }
        assert!(!names.iter().any(|n| n == "resetRun"), "{names:?}");
    }

    #[test]
    fn completes_builder_methods_after_a_chained_call() {
        let dir = project();
        let text = CONTROLLER.replace("->first()", "->");
        let column = text.lines().nth(11).unwrap().rfind("->").unwrap() as u32 + 2;
        let items = engine(dir.path()).complete(
            &dir.path().join("app/Controllers/X.php"),
            &text,
            11,
            column,
        );
        let names = labels(items);
        assert!(names.iter().any(|n| n == "first"), "{names:?}");
        assert!(names.iter().any(|n| n == "orderBy"), "{names:?}");
        assert!(!names.iter().any(|n| n == "active"), "{names:?}");
    }

    #[test]
    fn protected_builder_methods_are_not_forwarded() {
        let dir = project();
        let text = CONTROLLER.replace("->where('id', 1)->first()", "->resetRun()");
        let row = text.lines().nth(11).unwrap();
        let column = row.find("resetRun").unwrap() as u32 + 1;
        let found = engine(dir.path()).definition(
            &dir.path().join("app/Controllers/X.php"),
            &text,
            11,
            column,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn other_classes_do_not_forward() {
        let dir = project();
        let found = at_word(dir.path(), 13, "where", |e, p, t, l, c| {
            e.definition(p, t, l, c)
        });
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn forwarding_needs_the_builder_class() {
        let dir = project();
        std::fs::remove_file(dir.path().join("lib/CodeIgniter/Database/BaseBuilder.php")).unwrap();
        let found = at_word(dir.path(), 11, "where", |e, p, t, l, c| {
            e.definition(p, t, l, c)
        });
        assert!(found.is_empty(), "{found:?}");
    }
}
