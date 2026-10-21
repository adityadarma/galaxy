//! Completion, navigation, hover, problems and quick fixes for the strings
//! of a Symfony project: `->generateUrl('home')`, `->render('a/b.html.twig')`,
//! `#[Autowire(service: 'app.mailer')]`, `'%app.locale%'`, `%env(APP_ENV)%`,
//! `->trans('greeting', [], 'messages')` and the rest.
//!
//! Problems are conservative: the index reads files only, so anything a
//! bundle or a recipe registers at run time is invisible to it. A name is
//! flagged only when the project's own files are the only place it could
//! come from.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use regex::Regex;

use super::{Index, Item, Kind, Loc, Site, site_at, sites, yaml};
use crate::php::engine::{
    Action, Completion, FileChange, HoverResult, ItemKind, PhpEngine, Problem, Severity, Target,
    byte_offset, fuzzy, position_of, rank,
};
use crate::php::framework::common::complete::relative;

const MAX_ITEMS: usize = 200;
const MAX_PROBLEMS: usize = 200;

/// First segments of parameter and service names that bundles define. A
/// name with one of these is never judged, even when the project shares
/// the prefix.
const VENDOR_PREFIXES: &[&str] = &[
    "framework",
    "twig",
    "twig_extra",
    "doctrine",
    "doctrine_migrations",
    "security",
    "monolog",
    "mailer",
    "messenger",
    "validator",
    "router",
    "session",
    "cache",
    "kernel",
    "debug",
    "translator",
    "web_profiler",
    "maker",
    "asset",
    "assets",
    "form",
    "http_client",
    "serializer",
    "property_info",
    "property_access",
    "uid",
    "lock",
    "mime",
    "notifier",
    "secrets",
    "stimulus",
    "ux",
    "turbo",
    "webpack_encore",
    "api_platform",
    "lexik_jwt_authentication",
    "knp_paginator",
    "sensio_framework_extra",
    "nelmio_cors",
    "nelmio_api_doc",
    "fos_rest",
    "fos_user",
    "stof_doctrine_extensions",
    "vich_uploader",
    "sonata",
    "easyadmin",
    "easy_admin",
    "liip_imagine",
    "league",
    "sentry",
    "scheb_2fa",
    "dama_doctrine_test",
    "test",
];

/// What a name resolves to.
struct Found {
    detail: String,
    loc: Option<Loc>,
}

fn found(item: &Item) -> Found {
    Found {
        detail: item.detail.clone(),
        loc: item.loc.clone(),
    }
}

fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::Route => "Route",
        Kind::Template => "Template",
        Kind::Service => "Service",
        Kind::Parameter => "Parameter",
        Kind::Env => "Environment variable",
        Kind::Translation => "Translation",
    }
}

fn item_kind(kind: Kind) -> ItemKind {
    match kind {
        Kind::Route | Kind::Template => ItemKind::Function,
        Kind::Service => ItemKind::Class,
        Kind::Parameter | Kind::Env => ItemKind::Constant,
        Kind::Translation => ItemKind::Property,
    }
}

/// Completions for the items whose name fits `typed`.
fn named(typed: &str, items: &[Item], kind: ItemKind, from: u32) -> Vec<Completion> {
    let mut out: Vec<Completion> = items
        .iter()
        .filter(|item| fuzzy(typed, &item.name))
        .map(|item| Completion {
            label: item.name.clone(),
            detail: (!item.detail.is_empty()).then(|| item.detail.clone()),
            insert_text: item.name.clone(),
            is_snippet: false,
            kind,
            sort: rank(typed, &item.name, '0'),
            extra: Vec::new(),
            replace_from: Some(from),
        })
        .collect();
    out.sort_by(|a, b| a.sort.cmp(&b.sort));
    out.dedup_by(|a, b| a.label == b.label);
    out.truncate(MAX_ITEMS);
    out
}

fn target_of(loc: &Loc) -> Target {
    Target {
        path: loc.path.clone(),
        line: loc.line,
        column: loc.column_utf16,
    }
}

fn first_segment(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

/// `true` when the project defines names that start with the first segment
/// of `name` itself (not in `config/packages`, which configures bundles),
/// or `name` starts with `app.`. Only such names are the project's to
/// define, so only those can be judged missing.
fn project_owned(items: &[Item], name: &str) -> bool {
    if name.starts_with("app.") {
        return true;
    }
    if !name.contains('.') {
        return false;
    }
    let first = first_segment(name);
    if VENDOR_PREFIXES.contains(&first) {
        return false;
    }
    items.iter().any(|item| {
        item.name.contains('.')
            && first_segment(&item.name) == first
            && item.loc.as_ref().is_some_and(|loc| {
                !loc.path
                    .components()
                    .any(|c| c.as_os_str() == std::ffi::OsStr::new("packages"))
            })
    })
}

/// `twig.paths` adds template directories the index does not read.
fn twig_has_extra_paths(root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(root.join("config/packages/twig.yaml")) else {
        return false;
    };
    let entries = yaml::parse(&text);
    yaml::find(&entries, &["twig", "paths"]).is_some()
}

/// A template name that is a plain path below the template directory.
fn plain_template_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['@', '/', '!'])
        && !name.split('/').any(|part| part == ".." || part.is_empty())
        && !name.contains(['\\', ':', '{', '$'])
}

/// The class and method a PHP route attribute at `line` belongs to.
fn php_controller(path: &Path, line: u32) -> Option<String> {
    static NAMESPACE: OnceLock<Regex> = OnceLock::new();
    static CLASS: OnceLock<Regex> = OnceLock::new();
    static FUNCTION: OnceLock<Regex> = OnceLock::new();
    let namespace =
        NAMESPACE.get_or_init(|| Regex::new(r"(?m)^\s*namespace\s+([\w\\]+)\s*;").expect("valid"));
    let class = CLASS.get_or_init(|| {
        Regex::new(r"(?m)^\s*(?:(?:final|abstract|readonly)\s+)*class\s+(\w+)").expect("valid")
    });
    let function = FUNCTION.get_or_init(|| Regex::new(r"function\s+(\w+)").expect("valid"));
    let text = std::fs::read_to_string(path).ok()?;
    let class_name = class.captures(&text)?[1].to_owned();
    let class_name = match namespace.captures(&text) {
        Some(caps) => format!("{}\\{class_name}", &caps[1]),
        None => class_name,
    };
    let start = byte_offset(&text, line, 0)?;
    let method = function.captures(&text[start..])?[1].to_owned();
    Some(format!("{class_name}::{method}"))
}

/// The `controller:` of the YAML route defined at `line`.
fn yaml_controller(path: &Path, line: u32) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let entries = yaml::parse(&text);
    let top = entries
        .iter()
        .find(|e| e.path.len() == 1 && e.line == line)?;
    yaml::value(&entries, &[top.key(), "controller"])
        .map(|v| v.trim_matches(['\'', '"']).to_owned())
}

impl PhpEngine {
    // ---- lookup --------------------------------------------------------

    /// What the string of `site` names. `None`: not judged, `Some(None)`:
    /// the project has no such thing.
    fn symfony_resolve(&mut self, idx: &Index, text: &str, site: &Site) -> Option<Option<Found>> {
        let name = &text[site.start..site.end];
        if name.is_empty() {
            return None;
        }
        let pick = |items: &[Item]| Index::find(items, name).map(found);
        match site.kind {
            Kind::Route => match pick(&idx.routes) {
                Some(hit) => Some(Some(hit)),
                // `_profiler`, `_wdt` and the like come from bundles.
                None => (idx.routes_complete && !name.starts_with('_')).then_some(None),
            },
            Kind::Template => {
                if let Some(hit) = pick(&idx.templates) {
                    return Some(Some(hit));
                }
                if !plain_template_name(name) || twig_has_extra_paths(&idx.root) {
                    return None;
                }
                Some(None)
            }
            Kind::Service => {
                if let Some(hit) = pick(&idx.services) {
                    return Some(Some(hit));
                }
                if name.contains('\\') {
                    // A class name is a service id when the class exists.
                    let class = self.class(name)?;
                    if class.path.as_os_str().is_empty() {
                        return None;
                    }
                    return Some(Some(Found {
                        detail: class.fqn.clone(),
                        loc: Some(Loc {
                            path: class.path.clone(),
                            line: class.line,
                            column_utf16: class.column,
                        }),
                    }));
                }
                (idx.services_complete && project_owned(&idx.services, name)).then_some(None)
            }
            Kind::Parameter => match pick(&idx.parameters) {
                Some(hit) => Some(Some(hit)),
                None => {
                    let judged = idx.services_complete
                        && !name.starts_with("kernel.")
                        && !name.starts_with("env(")
                        && project_owned(&idx.parameters, name);
                    judged.then_some(None)
                }
            },
            Kind::Env => match pick(&idx.env) {
                Some(hit) => Some(Some(hit)),
                // `$_SERVER['X']` reads the process, not only `.env` files.
                None => (!idx.env.is_empty() && site.paren.is_some()).then_some(None),
            },
            Kind::Translation => {
                let domain = site.domain.as_deref().unwrap_or("messages");
                let keys = idx.translations.get(domain)?;
                match pick(keys) {
                    Some(hit) => Some(Some(hit)),
                    None => Some(None),
                }
            }
        }
    }

    // ---- completion ----------------------------------------------------

    /// Suggestions for the Symfony string the cursor is in. `None` when it
    /// is not in one, so the caller goes on with plain PHP.
    pub(super) fn symfony_complete(
        &mut self,
        idx: Arc<Index>,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Completion>> {
        let byte = byte_offset(text, line, column)?;
        let site = site_at(text, byte)?;
        let typed = &text[site.start..byte.min(site.end)];
        let from = position_of(text, site.start).1;
        let kind = item_kind(site.kind);
        let items = match site.kind {
            Kind::Route => named(typed, &idx.routes, kind, from),
            Kind::Template => named(typed, &idx.templates, kind, from),
            Kind::Service => named(typed, &idx.services, kind, from),
            Kind::Parameter => named(typed, &idx.parameters, kind, from),
            Kind::Env => named(typed, &idx.env, kind, from),
            Kind::Translation => {
                let keys = idx.translation_keys(site.domain.as_deref());
                named(typed, keys, kind, from)
            }
        };
        Some(items)
    }

    // ---- definition and hover ------------------------------------------

    /// Where the Symfony string under the cursor is defined.
    pub(super) fn symfony_definition(
        &mut self,
        idx: Arc<Index>,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Target>> {
        let byte = byte_offset(text, line, column)?;
        let site = site_at(text, byte)?;
        let hit = self.symfony_resolve(&idx, text, &site)??;
        Some(hit.loc.as_ref().map(target_of).into_iter().collect())
    }

    /// Documentation for the Symfony string under the cursor.
    pub(super) fn symfony_hover(
        &mut self,
        idx: Arc<Index>,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        let byte = byte_offset(text, line, column)?;
        let site = site_at(text, byte)?;
        let hit = self.symfony_resolve(&idx, text, &site)??;
        let name = &text[site.start..site.end];
        let controller = match (site.kind, &hit.loc) {
            (Kind::Route, Some(loc)) => match loc.path.extension().and_then(|e| e.to_str()) {
                Some("php") => php_controller(&loc.path, loc.line),
                Some("yaml" | "yml") => yaml_controller(&loc.path, loc.line),
                _ => None,
            },
            _ => None,
        };
        let markdown = describe(site.kind, name, &hit, controller.as_deref(), &idx.root);
        let (start_line, start_column, _) = position_of(text, site.start);
        let (_, end_column, _) = position_of(text, site.end);
        Some(HoverResult {
            markdown,
            line: start_line,
            start_column,
            end_column,
        })
    }

    // ---- problems and actions ------------------------------------------

    /// Strings that name something the project does not have.
    pub(super) fn symfony_problems(&mut self, idx: Arc<Index>, text: &str) -> Vec<Problem> {
        let mut out = Vec::new();
        for site in sites(text) {
            if out.len() >= MAX_PROBLEMS {
                break;
            }
            let Some(None) = self.symfony_resolve(&idx, text, &site) else {
                continue;
            };
            let name = &text[site.start..site.end];
            let (severity, message) = match site.kind {
                Kind::Route => (Severity::Warning, format!("Unknown route `{name}`")),
                Kind::Template => (Severity::Warning, format!("Template `{name}` not found")),
                Kind::Service => (Severity::Warning, format!("Unknown service `{name}`")),
                Kind::Parameter => (Severity::Warning, format!("Unknown parameter `{name}`")),
                Kind::Env => (
                    Severity::Hint,
                    format!("Environment variable `{name}` is not set in any .env file"),
                ),
                Kind::Translation => (
                    Severity::Hint,
                    format!(
                        "Translation key `{name}` not found in domain `{}`",
                        site.domain.as_deref().unwrap_or("messages")
                    ),
                ),
            };
            let (start_line, start_column, _) = position_of(text, site.start);
            let (end_line, end_column, _) = position_of(text, site.end);
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
        out
    }

    /// A fix for a missing template.
    pub(super) fn symfony_actions(
        &mut self,
        idx: Arc<Index>,
        text: &str,
        byte: usize,
        out: &mut Vec<Action>,
    ) {
        let Some(site) = site_at(text, byte) else {
            return;
        };
        if site.kind != Kind::Template {
            return;
        }
        let Some(None) = self.symfony_resolve(&idx, text, &site) else {
            return;
        };
        let name = text[site.start..site.end].to_owned();
        let dir = if idx.template_dir.as_os_str().is_empty() {
            idx.root.join("templates")
        } else {
            idx.template_dir.clone()
        };
        let has_base = idx.templates.iter().any(|t| t.name == "base.html.twig");
        let body = if has_base {
            "{% extends 'base.html.twig' %}\n\n{% block body %}\n{% endblock %}\n"
        } else {
            "\n"
        };
        out.push(Action {
            title: format!("Create template `{name}`"),
            edits: Vec::new(),
            files: vec![FileChange {
                path: dir.join(&name) as PathBuf,
                line: 0,
                column: 0,
                text: body.to_owned(),
            }],
        });
    }
}

/// Markdown for a hover: what it is, its value and where it is defined.
fn describe(kind: Kind, name: &str, hit: &Found, controller: Option<&str>, root: &Path) -> String {
    let mut out = format!("**{}** `{name}`", label(kind));
    if !hit.detail.is_empty() && kind != Kind::Template {
        if kind == Kind::Translation {
            out.push_str(&format!("\n\n{}", hit.detail));
        } else {
            out.push_str(&format!("\n\n`{}`", hit.detail.replace('`', "'")));
        }
    }
    if let Some(controller) = controller {
        out.push_str(&format!("\n\n`{controller}`"));
    }
    if let Some(loc) = &hit.loc {
        let file = relative(root, &loc.path);
        if loc.line == 0 && loc.column_utf16 == 0 {
            out.push_str(&format!("\n\nDefined in `{file}`"));
        } else {
            out.push_str(&format!("\n\nDefined in `{file}:{}`", loc.line + 1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::SymfonyFramework;
    use super::*;

    const COMPOSER: &str = r#"{"require":{"php":">=8.2","symfony/framework-bundle":"7.1.*"},"autoload":{"psr-4":{"App\\":"src/"}}}"#;

    const HOME: &str = r#"<?php
namespace App\Controller;

use Symfony\Bundle\FrameworkBundle\Controller\AbstractController;
use Symfony\Component\Routing\Attribute\Route;

class HomeController extends AbstractController
{
    #[Route('/', name: 'app_home', methods: ['GET'])]
    public function index(): Response
    {
        return $this->render('home/index.html.twig');
    }

    #[Route('/about', name: 'app_about')]
    public function about(): Response {}

    #[Route('/contact', name: 'app_contact', methods: ['GET', 'POST'])]
    public function contact(): Response {}
}
"#;

    const USER: &str = r#"<?php
namespace App\Controller\Admin;

use Symfony\Component\Routing\Attribute\Route;

#[Route('/admin/users', name: 'admin_user_')]
final class UserController extends AbstractController
{
    #[Route('', name: 'index', methods: ['GET'])]
    public function index(): Response {}

    #[Route('/{id}/edit', name: 'edit', methods: ['GET', 'POST'])]
    public function edit(): Response {}
}
"#;

    const MAILER: &str = "<?php\nnamespace App\\Service;\n\nclass Mailer\n{\n}\n";

    /// No `resource:` and no `imports:`, so the services are all listed.
    const SERVICES: &str = r#"parameters:
    app.locale: 'en'
    app.admin_email: admin@example.com

services:
    _defaults:
        autowire: true

    app.mailer:
        class: App\Service\Mailer

    App\Service\Alias:
        alias: app.mailer
"#;

    const ROUTES: &str = r#"controllers:
    resource:
        path: ../src/Controller/
        namespace: App\Controller
    type: attribute

healthcheck:
    path: /health
    methods: [GET]
    controller: App\Controller\HealthController
"#;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    /// The mock project; `extra` files replace the defaults of the same name.
    fn mock(extra: &[(&str, &str)]) -> tempfile::TempDir {
        let mut files = vec![
            ("composer.json", COMPOSER),
            ("bin/console", "#!/usr/bin/env php\n<?php\n"),
            ("config/bundles.php", "<?php\nreturn [];\n"),
            ("config/services.yaml", SERVICES),
            ("config/routes.yaml", ROUTES),
            (
                "config/packages/twig.yaml",
                "twig:\n    file_name_pattern: '*.twig'\n\nparameters:\n    twig.extra: 1\n",
            ),
            ("src/Controller/HomeController.php", HOME),
            ("src/Controller/Admin/UserController.php", USER),
            ("src/Service/Mailer.php", MAILER),
            ("templates/base.html.twig", "{% block body %}{% endblock %}"),
            (
                "templates/home/index.html.twig",
                "{% extends 'base.html.twig' %}",
            ),
            (
                "translations/messages.en.yaml",
                "greeting: Hello\nmenu:\n    home: Home\n    admin:\n        users: Users\n",
            ),
            (
                "translations/validators.en.xlf",
                concat!(
                    "<?xml version=\"1.0\"?>\n",
                    "<xliff version=\"1.2\"><file source-language=\"en\"><body>\n",
                    "  <trans-unit id=\"abc123\">\n",
                    "    <source>form.invalid</source>\n",
                    "    <target>Invalid</target>\n",
                    "  </trans-unit>\n",
                    "</body></file></xliff>\n",
                ),
            ),
            (
                ".env",
                "# defaults\nAPP_ENV=dev\nAPP_SECRET=\nDATABASE_URL=\"sqlite:///x\"\n",
            ),
            (
                ".env.local",
                "APP_SECRET=abc\nexport MAILER_DSN=null://null\n",
            ),
        ];
        files.extend_from_slice(extra);
        project(&files)
    }

    fn engine_for(dir: &tempfile::TempDir) -> PhpEngine {
        let framework = SymfonyFramework::detect(dir.path()).expect("detected");
        PhpEngine::with_frameworks(dir.path(), vec![Arc::new(framework)])
    }

    /// `text` with `|` at the cursor: the text and the position.
    fn cursor(text: &str) -> (String, u32, u32) {
        let marker = text.find('|').expect("cursor");
        let before = &text[..marker];
        let line = before.matches('\n').count() as u32;
        let column = before.rsplit('\n').next().unwrap().encode_utf16().count() as u32;
        (text.replacen('|', "", 1), line, column)
    }

    /// PHP with `body` as the statements of a controller method, or, when
    /// it starts with `#[`, as the parameters of a constructor.
    fn php(body: &str) -> String {
        if body.starts_with("#[") {
            return format!(
                "<?php\nclass Page\n{{\n    public function __construct(\n        {body}\n    ) {{}}\n}}\n"
            );
        }
        format!(
            "<?php\nclass Page\n{{\n    public function show()\n    {{\n        {body}\n    }}\n}}\n"
        )
    }

    fn labels(engine: &mut PhpEngine, dir: &tempfile::TempDir, code: &str) -> Vec<String> {
        let (text, line, column) = cursor(&php(code));
        let path = dir.path().join("src/Controller/Page.php");
        engine
            .complete(&path, &text, line, column)
            .into_iter()
            .map(|i| i.label)
            .collect()
    }

    fn completions(engine: &mut PhpEngine, dir: &tempfile::TempDir, code: &str) -> Vec<Completion> {
        let (text, line, column) = cursor(&php(code));
        let path = dir.path().join("src/Controller/Page.php");
        engine.complete(&path, &text, line, column)
    }

    fn definition(engine: &mut PhpEngine, dir: &tempfile::TempDir, code: &str) -> Vec<Target> {
        let (text, line, column) = cursor(&php(code));
        let path = dir.path().join("src/Controller/Page.php");
        engine.definition(&path, &text, line, column)
    }

    fn hover(engine: &mut PhpEngine, dir: &tempfile::TempDir, code: &str) -> Option<HoverResult> {
        let (text, line, column) = cursor(&php(code));
        let path = dir.path().join("src/Controller/Page.php");
        engine.hover(&path, &text, line, column)
    }

    fn problems(engine: &mut PhpEngine, dir: &tempfile::TempDir, code: &str) -> Vec<Problem> {
        let path = dir.path().join("src/Controller/Page.php");
        engine.diagnostics(&path, &php(code))
    }

    fn messages(engine: &mut PhpEngine, dir: &tempfile::TempDir, code: &str) -> Vec<String> {
        problems(engine, dir, code)
            .into_iter()
            .map(|p| p.message)
            .collect()
    }

    fn actions(engine: &mut PhpEngine, dir: &tempfile::TempDir, code: &str) -> Vec<Action> {
        let (text, line, column) = cursor(&php(code));
        let path = dir.path().join("src/Controller/Page.php");
        engine.actions(&path, &text, line, column)
    }

    // ---- completion ----------------------------------------------------

    #[test]
    fn completes_route_names_with_method_and_path() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let items = completions(&mut engine, &dir, "$this->generateUrl('app_|');");
        let names: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(names, vec!["app_about", "app_contact", "app_home"]);
        let home = items.iter().find(|i| i.label == "app_home").unwrap();
        assert_eq!(home.detail.as_deref(), Some("GET /"));
        let contact = items.iter().find(|i| i.label == "app_contact").unwrap();
        assert_eq!(contact.detail.as_deref(), Some("GET|POST /contact"));
    }

    #[test]
    fn route_completion_replaces_only_the_string_content() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let items = completions(&mut engine, &dir, "$this->generateUrl('app_ho|');");
        assert_eq!(items[0].label, "app_home");
        // 8 spaces + `$this->generateUrl('` is 28 columns.
        assert_eq!(items[0].replace_from, Some(28));
    }

    #[test]
    fn route_completion_includes_prefixed_and_yaml_routes() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let all = labels(&mut engine, &dir, "$this->redirectToRoute('|');");
        for name in ["admin_user_edit", "admin_user_index", "healthcheck"] {
            assert!(all.iter().any(|l| l == name), "{name} in {all:?}");
        }
    }

    #[test]
    fn completes_template_names_relative_to_the_template_dir() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let all = labels(&mut engine, &dir, "return $this->render('|');");
        assert_eq!(all, vec!["base.html.twig", "home/index.html.twig"]);
        let some = labels(&mut engine, &dir, "return $this->render('home/|');");
        assert_eq!(some, vec!["home/index.html.twig"]);
    }

    #[test]
    fn completes_templates_below_a_configured_default_path() {
        let dir = mock(&[
            (
                "config/packages/twig.yaml",
                "twig:\n    default_path: '%kernel.project_dir%/views'\n",
            ),
            ("views/page.html.twig", ""),
        ]);
        let mut engine = engine_for(&dir);
        let all = labels(&mut engine, &dir, "return $this->render('|');");
        assert_eq!(all, vec!["page.html.twig"]);
    }

    #[test]
    fn completes_service_ids() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let items = completions(&mut engine, &dir, "$x = $this->container->get('app.|');");
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].label, "app.mailer");
        assert_eq!(items[0].detail.as_deref(), Some("App\\Service\\Mailer"));
    }

    #[test]
    fn completes_parameter_names_in_a_call_and_in_percent_signs() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let all = labels(&mut engine, &dir, "$this->getParameter('app.|');");
        assert_eq!(all, vec!["app.admin_email", "app.locale"]);

        let items = completions(&mut engine, &dir, "#[Autowire('%app.lo|%')] $x,");
        // The range covers only the name between the percent signs.
        let locale = items.iter().find(|i| i.label == "app.locale").unwrap();
        assert_eq!(locale.replace_from, Some(21));
    }

    #[test]
    fn completes_parameters_in_an_autowire_attribute() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let all = labels(&mut engine, &dir, "#[Autowire(param: 'kernel.deb|')] $x,");
        assert_eq!(all, vec!["kernel.debug"]);
    }

    #[test]
    fn completes_env_names_from_dotenv_files() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let all = labels(&mut engine, &dir, "#[Autowire(env: 'APP_|')] $x,");
        assert_eq!(all, vec!["APP_ENV", "APP_SECRET"]);
        let items = completions(&mut engine, &dir, "#[Autowire('%env(MAIL|)%')] $x,");
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].label, "MAILER_DSN");
        // `#[Autowire('%env(` is 8 spaces and 17 characters.
        assert_eq!(items[0].replace_from, Some(25));
    }

    #[test]
    fn completes_translation_keys_of_the_default_domain() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let all = labels(&mut engine, &dir, "$t->trans('menu.|');");
        assert_eq!(all, vec!["menu.admin.users", "menu.home"]);
        let items = completions(&mut engine, &dir, "$t->trans('gre|');");
        assert_eq!(items[0].label, "greeting");
        assert_eq!(items[0].detail.as_deref(), Some("Hello"));
    }

    #[test]
    fn completes_translation_keys_of_the_named_domain() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let all = labels(&mut engine, &dir, "$t->trans('|', [], 'validators');");
        assert_eq!(all, vec!["form.invalid"]);
        let none = labels(&mut engine, &dir, "$t->trans('|', [], 'nothing');");
        assert!(none.is_empty(), "{none:?}");
    }

    #[test]
    fn leaves_other_strings_to_plain_php() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let all = labels(&mut engine, &dir, "$x = 'app_|';");
        assert!(!all.iter().any(|l| l.starts_with("app_")), "{all:?}");
    }

    // ---- definition ----------------------------------------------------

    #[test]
    fn route_definition_is_the_attribute_line() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(&mut engine, &dir, "$this->generateUrl('app_ab|out');");
        assert_eq!(targets.len(), 1);
        assert!(
            targets[0]
                .path
                .ends_with("src/Controller/HomeController.php")
        );
        assert_eq!(targets[0].line, 14);
    }

    #[test]
    fn route_definition_works_for_class_prefixed_names() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(&mut engine, &dir, "$this->generateUrl('admin_user_ed|it');");
        assert_eq!(targets.len(), 1);
        assert!(
            targets[0]
                .path
                .ends_with("src/Controller/Admin/UserController.php")
        );
        assert_eq!(targets[0].line, 11);
    }

    #[test]
    fn route_definition_is_the_yaml_key() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(&mut engine, &dir, "$this->generateUrl('health|check');");
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with("config/routes.yaml"));
        assert_eq!(targets[0].line, 6);
        assert_eq!(targets[0].column, 0);
    }

    #[test]
    fn template_definition_is_the_file() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(
            &mut engine,
            &dir,
            "return $this->render('home/ind|ex.html.twig');",
        );
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with("templates/home/index.html.twig"));
        assert_eq!((targets[0].line, targets[0].column), (0, 0));
    }

    #[test]
    fn service_definition_is_the_services_yaml_line() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(&mut engine, &dir, "$this->container->get('app.mai|ler');");
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with("config/services.yaml"));
        assert_eq!(targets[0].line, 8);
    }

    #[test]
    fn service_definition_of_an_existing_class_is_its_file() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(
            &mut engine,
            &dir,
            "$this->container->get('App\\Service\\Mail|er');",
        );
        assert_eq!(targets.len(), 1, "{targets:?}");
        assert!(targets[0].path.ends_with("src/Service/Mailer.php"));
        assert_eq!(targets[0].line, 3);
    }

    #[test]
    fn service_definition_of_a_missing_class_is_empty() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(
            &mut engine,
            &dir,
            "$this->container->get('App\\Gone\\Thi|ng');",
        );
        assert!(targets.is_empty());
    }

    #[test]
    fn parameter_definition_is_the_file_line() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(&mut engine, &dir, "$this->getParameter('app.loc|ale');");
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with("config/services.yaml"));
        assert_eq!(targets[0].line, 1);

        let targets = definition(&mut engine, &dir, "$this->getParameter('twig.ex|tra');");
        assert!(targets[0].path.ends_with("config/packages/twig.yaml"));

        let built_in = definition(&mut engine, &dir, "$this->getParameter('kernel.deb|ug');");
        assert!(built_in.is_empty());
    }

    #[test]
    fn env_definition_is_the_dotenv_line() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(&mut engine, &dir, "#[Autowire(env: 'APP_SEC|RET')] $x,");
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with(".env"));
        assert_eq!(targets[0].line, 2);

        let targets = definition(&mut engine, &dir, "#[Autowire(env: 'MAILER|_DSN')] $x,");
        assert!(targets[0].path.ends_with(".env.local"));
        assert_eq!((targets[0].line, targets[0].column), (1, 7));
    }

    #[test]
    fn translation_definition_follows_the_domain() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let targets = definition(&mut engine, &dir, "$t->trans('menu.ho|me');");
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with("translations/messages.en.yaml"));
        assert_eq!(targets[0].line, 2);

        let targets = definition(
            &mut engine,
            &dir,
            "$t->trans('form.inv|alid', [], 'validators');",
        );
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with("translations/validators.en.xlf"));
        assert_eq!(targets[0].line, 2);
    }

    // ---- hover ---------------------------------------------------------

    #[test]
    fn route_hover_shows_method_path_and_controller() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let result = hover(&mut engine, &dir, "$this->generateUrl('app_ab|out');").expect("hover");
        let md = &result.markdown;
        assert!(md.contains("**Route** `app_about`"), "{md}");
        assert!(md.contains("`ANY /about`"), "{md}");
        assert!(
            md.contains("`App\\Controller\\HomeController::about`"),
            "{md}"
        );
        assert!(md.contains("src/Controller/HomeController.php:15"), "{md}");
        assert_eq!((result.start_column, result.end_column), (28, 37));
    }

    #[test]
    fn route_hover_shows_the_class_prefix_and_yaml_controller() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let md = hover(&mut engine, &dir, "$this->generateUrl('admin_user_ed|it');")
            .expect("hover")
            .markdown;
        assert!(md.contains("`GET|POST /admin/users/{id}/edit`"), "{md}");
        assert!(md.contains("UserController::edit"), "{md}");

        let md = hover(&mut engine, &dir, "$this->generateUrl('health|check');")
            .expect("hover")
            .markdown;
        assert!(md.contains("`GET /health`"), "{md}");
        assert!(md.contains("`App\\Controller\\HealthController`"), "{md}");
    }

    #[test]
    fn template_hover_shows_the_path() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let md = hover(
            &mut engine,
            &dir,
            "return $this->render('base.html.t|wig');",
        )
        .expect("hover")
        .markdown;
        assert!(md.contains("**Template** `base.html.twig`"), "{md}");
        assert!(md.contains("Defined in `templates/base.html.twig`"), "{md}");
    }

    #[test]
    fn service_hover_shows_the_class() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let md = hover(&mut engine, &dir, "$this->container->get('app.mai|ler');")
            .expect("hover")
            .markdown;
        assert!(md.contains("`App\\Service\\Mailer`"), "{md}");
        assert!(md.contains("config/services.yaml:9"), "{md}");

        let md = hover(
            &mut engine,
            &dir,
            "$this->container->get('App\\Service\\Mail|er');",
        )
        .expect("hover")
        .markdown;
        assert!(md.contains("src/Service/Mailer.php:4"), "{md}");
    }

    #[test]
    fn parameter_hover_shows_the_value() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let md = hover(&mut engine, &dir, "$this->getParameter('app.loc|ale');")
            .expect("hover")
            .markdown;
        assert!(md.contains("`en`"), "{md}");
        assert!(md.contains("config/services.yaml:2"), "{md}");
        assert!(hover(&mut engine, &dir, "$this->getParameter('app.nop|e');").is_none());
    }

    #[test]
    fn env_hover_names_the_file_without_the_value() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let md = hover(&mut engine, &dir, "#[Autowire(env: 'DATABASE|_URL')] $x,")
            .expect("hover")
            .markdown;
        assert!(
            md.contains("**Environment variable** `DATABASE_URL`"),
            "{md}"
        );
        assert!(md.contains(".env:4"), "{md}");
        assert!(!md.contains("sqlite"), "{md}");
    }

    #[test]
    fn translation_hover_shows_the_message() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let md = hover(&mut engine, &dir, "$t->trans('gree|ting');")
            .expect("hover")
            .markdown;
        assert!(md.contains("Hello"), "{md}");
        assert!(md.contains("translations/messages.en.yaml"), "{md}");
    }

    // ---- problems ------------------------------------------------------

    #[test]
    fn flags_unknown_routes_only() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = problems(
            &mut engine,
            &dir,
            "$this->generateUrl('app_home');\n        $this->generateUrl('app_gone');",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].message, "Unknown route `app_gone`");
        assert_eq!(found[0].severity, Severity::Warning);
        assert_eq!((found[0].start_line, found[0].start_column), (6, 28));
        assert_eq!(found[0].end_column, 36);
    }

    #[test]
    fn route_imports_silence_unknown_routes() {
        let dir = mock(&[(
            "config/routes.yaml",
            "api:\n    resource: '@ApiBundle/config/routes.yaml'\n",
        )]);
        let mut engine = engine_for(&dir);
        assert!(messages(&mut engine, &dir, "$this->generateUrl('app_gone');").is_empty());
    }

    #[test]
    fn xml_and_php_routes_silence_unknown_routes() {
        let dir = mock(&[("config/routes/extra.xml", "<routes/>")]);
        let mut engine = engine_for(&dir);
        assert!(messages(&mut engine, &dir, "$this->generateUrl('app_gone');").is_empty());
    }

    #[test]
    fn bundle_routes_with_an_underscore_are_not_flagged() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(
            &mut engine,
            &dir,
            "$this->generateUrl('_profiler');\n        $this->generateUrl('_wdt');",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn flags_missing_templates() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = problems(
            &mut engine,
            &dir,
            "$this->render('home/index.html.twig');\n        $this->render('home/gone.html.twig');",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].message, "Template `home/gone.html.twig` not found");
        assert_eq!(found[0].severity, Severity::Warning);
    }

    #[test]
    fn namespaced_templates_are_not_flagged() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(
            &mut engine,
            &dir,
            "$this->render('@Acme/page.html.twig');\n        $this->render('@!Acme/page.html.twig');",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn extra_twig_paths_silence_missing_templates() {
        let dir = mock(&[(
            "config/packages/twig.yaml",
            "twig:\n    paths:\n        '%kernel.project_dir%/extra': Extra\n",
        )]);
        let mut engine = engine_for(&dir);
        assert!(messages(&mut engine, &dir, "$this->render('gone.html.twig');").is_empty());
    }

    #[test]
    fn flags_unknown_project_services_when_the_list_is_complete() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(
            &mut engine,
            &dir,
            "$this->container->get('app.mailer');\n        $this->container->get('app.gone');",
        );
        assert_eq!(found, vec!["Unknown service `app.gone`"]);
    }

    #[test]
    fn class_name_services_are_never_flagged() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(
            &mut engine,
            &dir,
            "$this->container->get('App\\Gone\\Thing');\n        $this->container->get('App\\Service\\Mailer');",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn registered_directories_and_imports_silence_unknown_services() {
        for services in [
            "services:\n    App\\:\n        resource: '../src/'\n",
            "imports:\n    - { resource: 'more.yaml' }\n",
        ] {
            let dir = mock(&[("config/services.yaml", services)]);
            let mut engine = engine_for(&dir);
            let found = messages(&mut engine, &dir, "$this->container->get('app.gone');");
            assert!(found.is_empty(), "{services}: {found:?}");
        }
    }

    #[test]
    fn vendor_looking_services_are_not_flagged() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(
            &mut engine,
            &dir,
            "$this->container->get('logger');\n        $this->container->get('monolog.logger.request');\n        $this->container->get('doctrine.orm.entity_manager');",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn flags_unknown_project_parameters() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(
            &mut engine,
            &dir,
            "$this->getParameter('app.locale');\n        $this->getParameter('app.gone');",
        );
        assert_eq!(found, vec!["Unknown parameter `app.gone`"]);
    }

    #[test]
    fn vendor_looking_parameters_are_not_flagged() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(
            &mut engine,
            &dir,
            concat!(
                "$this->getParameter('twig.default_path');\n",
                "        $this->getParameter('doctrine.orm.auto_generate_proxy_classes');\n",
                "        $this->getParameter('kernel.gone');\n",
                "        $this->getParameter('kernel.debug');\n",
                "        $this->getParameter('router.request_context.host');\n",
                "        $this->getParameter('plain');\n",
                "        $this->getParameter('acme_bundle.thing');",
            ),
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn percent_parameters_are_judged_like_calls() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(
            &mut engine,
            &dir,
            "#[Autowire('%app.locale%')] $a,\n        #[Autowire('%app.gone%')] $b,",
        );
        assert_eq!(found, vec!["Unknown parameter `app.gone`"]);
    }

    #[test]
    fn imports_silence_unknown_parameters() {
        let dir = mock(&[(
            "config/services.yaml",
            "imports:\n    - { resource: 'params.yaml' }\n",
        )]);
        let mut engine = engine_for(&dir);
        assert!(messages(&mut engine, &dir, "$this->getParameter('app.gone');").is_empty());
    }

    #[test]
    fn flags_unknown_env_vars_as_hints() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = problems(
            &mut engine,
            &dir,
            "#[Autowire(env: 'APP_ENV')] $a,\n        #[Autowire(env: 'GONE')] $b,\n        #[Autowire('%env(json:GONE_TOO)%')] $c,",
        );
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].severity, Severity::Hint);
        assert!(found[0].message.contains("`GONE`"));
        assert!(found[1].message.contains("`GONE_TOO`"));
    }

    #[test]
    fn env_is_not_judged_without_dotenv_files_or_for_superglobals() {
        let dir = project(&[("composer.json", COMPOSER)]);
        let mut engine = engine_for(&dir);
        assert!(messages(&mut engine, &dir, "#[Autowire(env: 'GONE')] $a,").is_empty());

        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(&mut engine, &dir, "$x = $_SERVER['REQUEST_URI'];");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn flags_missing_translation_keys_only_in_known_domains() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = problems(
            &mut engine,
            &dir,
            concat!(
                "$t->trans('greeting');\n",
                "        $t->trans('menu.gone');\n",
                "        $t->trans('x', [], 'validators');\n",
                "        $t->trans('form.invalid', [], 'validators');\n",
                "        $t->trans('x', [], 'security');",
            ),
        );
        let text: Vec<(&str, Severity)> = found
            .iter()
            .map(|p| (p.message.as_str(), p.severity))
            .collect();
        assert_eq!(
            text,
            vec![
                (
                    "Translation key `menu.gone` not found in domain `messages`",
                    Severity::Hint
                ),
                (
                    "Translation key `x` not found in domain `validators`",
                    Severity::Hint
                ),
            ]
        );
    }

    #[test]
    fn computed_names_and_plain_strings_get_no_problems() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = messages(
            &mut engine,
            &dir,
            concat!(
                "$a = 'app_gone';\n",
                "        $this->generateUrl($name);\n",
                "        $this->generateUrl('app_' . $x);\n",
                "        $this->generateUrl(\"app_$x\");\n",
                "        $this->render($t);",
            ),
        );
        assert!(found.is_empty(), "{found:?}");
    }

    // ---- actions -------------------------------------------------------

    #[test]
    fn offers_to_create_a_missing_template() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        let found = actions(&mut engine, &dir, "$this->render('admin/da|sh.html.twig');");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].title, "Create template `admin/dash.html.twig`");
        assert!(found[0].edits.is_empty());
        let change = &found[0].files[0];
        assert_eq!(
            change.path,
            dir.path().join("templates/admin/dash.html.twig")
        );
        assert_eq!((change.line, change.column), (0, 0));
        assert!(change.text.contains("{% extends 'base.html.twig' %}"));
    }

    #[test]
    fn created_template_uses_the_configured_directory() {
        let dir = mock(&[(
            "config/packages/twig.yaml",
            "twig:\n    default_path: views\n",
        )]);
        let mut engine = engine_for(&dir);
        let found = actions(&mut engine, &dir, "$this->render('a|.html.twig');");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].files[0].path, dir.path().join("views/a.html.twig"));
        // No base template here, so no `extends`.
        assert!(!found[0].files[0].text.contains("extends"));
    }

    #[test]
    fn offers_nothing_for_known_namespaced_or_other_names() {
        let dir = mock(&[]);
        let mut engine = engine_for(&dir);
        for code in [
            "$this->render('base.html.t|wig');",
            "$this->render('@Acme/pa|ge.html.twig');",
            "$this->render('../esc|ape.twig');",
            "$this->generateUrl('app_go|ne');",
            "$this->getParameter('app.go|ne');",
            "$x = 'plain|';",
        ] {
            let found = actions(&mut engine, &dir, code);
            assert!(found.is_empty(), "{code}: {found:?}");
        }
    }

    #[test]
    fn non_symfony_engines_ignore_these_strings() {
        let dir = mock(&[]);
        let mut engine = PhpEngine::with_frameworks(dir.path(), Vec::new());
        assert!(messages(&mut engine, &dir, "$this->generateUrl('app_gone');").is_empty());
        assert!(definition(&mut engine, &dir, "$this->generateUrl('app_ho|me');").is_empty());
    }
}
