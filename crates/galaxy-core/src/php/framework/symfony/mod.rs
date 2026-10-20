//! Symfony for the built-in PHP engine: the strings of a project that name
//! routes, templates, services, parameters, environment variables and
//! translation keys, and what they point to. Everything is read from the
//! project's files; PHP never runs.

mod engine;
pub mod index;
pub mod site;
mod yaml;

use std::path::Path;
use std::sync::Arc;

pub use index::{Index, Item, Loc};
pub use site::{Kind, Site, in_string, site_at, sites};

use super::Framework;
use crate::php::engine::{Action, Completion, HoverResult, PhpEngine, Problem, Target};

/// Symfony as a [`Framework`].
#[derive(Debug)]
pub struct SymfonyFramework {
    index: Arc<Index>,
}

impl SymfonyFramework {
    pub fn new(index: Arc<Index>) -> Self {
        Self { index }
    }

    /// `Some` when `root` is a Symfony project: `composer.json` requires
    /// `symfony/framework-bundle` or `symfony/symfony`, `bin/console` and
    /// `config/bundles.php` exist, or there is a `symfony.lock`.
    pub fn detect(root: &Path) -> Option<Self> {
        Index::detect(root).map(|index| Self::new(Arc::new(index)))
    }

    /// What the project defines.
    pub fn index(&self) -> &Arc<Index> {
        &self.index
    }
}

impl Framework for SymfonyFramework {
    fn name(&self) -> &'static str {
        "symfony"
    }

    fn owns_string(&self, text: &str, byte: usize) -> bool {
        site_at(text, byte).is_some()
    }

    fn complete(
        &self,
        engine: &mut PhpEngine,
        _path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Completion>> {
        engine.symfony_complete(self.index.clone(), text, line, column)
    }

    fn definition(
        &self,
        engine: &mut PhpEngine,
        _path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Target>> {
        engine.symfony_definition(self.index.clone(), text, line, column)
    }

    fn hover(
        &self,
        engine: &mut PhpEngine,
        _path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        engine.symfony_hover(self.index.clone(), text, line, column)
    }

    fn problems(&self, engine: &mut PhpEngine, _path: &Path, text: &str) -> Vec<Problem> {
        engine.symfony_problems(self.index.clone(), text)
    }

    fn actions(
        &self,
        engine: &mut PhpEngine,
        _path: &Path,
        text: &str,
        byte: usize,
        out: &mut Vec<Action>,
    ) {
        engine.symfony_actions(self.index.clone(), text, byte, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOSER: &str = r#"{"require":{"php":">=8.2","symfony/framework-bundle":"7.1.*"}}"#;

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

    #[Route('/unnamed')]
    public function unnamed(): Response {}

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

    /**
     * @Route("/legacy", name="legacy", methods={"DELETE"})
     */
    public function legacy(): Response {}

    #[Route('/skip')]
    public function skipped(): Response {}
}
"#;

    const SERVICES: &str = r#"parameters:
    app.locale: 'en'
    app.admin_email: admin@example.com # trailing

services:
    _defaults:
        autowire: true
        autoconfigure: true

    App\:
        resource: '../src/'
        exclude:
            - '../src/Entity/'

    app.mailer:
        class: App\Service\Mailer
        arguments: ['%app.locale%']

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

localized:
    path:
        en: /hello
        fr: /bonjour
    controller: App\Controller\HelloController
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
                    "    <target>Invalid &amp; wrong</target>\n",
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

    fn built() -> (tempfile::TempDir, Index) {
        let dir = mock(&[]);
        let index = Index::detect(dir.path()).expect("detected");
        (dir, index)
    }

    fn names(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.name.as_str()).collect()
    }

    fn item<'a>(items: &'a [Item], name: &str) -> &'a Item {
        Index::find(items, name).unwrap_or_else(|| panic!("missing {name}"))
    }

    // Detection

    #[test]
    fn detects_by_composer_requirement() {
        for body in [
            r#"{"require":{"symfony/framework-bundle":"^6.4"}}"#,
            r#"{"require":{"symfony/symfony":"^5"}}"#,
            r#"{"require-dev":{"symfony/framework-bundle":"^7"}}"#,
        ] {
            let dir = project(&[("composer.json", body)]);
            assert!(SymfonyFramework::detect(dir.path()).is_some(), "{body}");
        }
    }

    #[test]
    fn detects_by_console_and_bundles_or_lock() {
        let dir = project(&[("bin/console", "<?php"), ("config/bundles.php", "<?php")]);
        assert!(Index::detect(dir.path()).is_some());
        let dir = project(&[("symfony.lock", "{}")]);
        assert!(Index::detect(dir.path()).is_some());
    }

    #[test]
    fn does_not_detect_other_projects() {
        for files in [
            vec![("composer.json", r#"{"require":{"monolog/monolog":"^3"}}"#)],
            vec![("composer.json", r#"{"require":{"symfony/console":"^7"}}"#)],
            vec![("bin/console", "<?php")],
            vec![("config/bundles.php", "<?php")],
            vec![("artisan", "<?php")],
            vec![("composer.json", "not json")],
            vec![],
        ] {
            let dir = project(&files);
            assert!(SymfonyFramework::detect(dir.path()).is_none(), "{files:?}");
        }
    }

    #[test]
    fn framework_name_and_owned_strings() {
        let (dir, _) = built();
        let framework = SymfonyFramework::detect(dir.path()).unwrap();
        assert_eq!(framework.name(), "symfony");
        let code = "<?php\n$this->generateUrl('home');\n$x = 'plain';\n";
        assert!(framework.owns_string(code, code.find("home").unwrap()));
        assert!(!framework.owns_string(code, code.find("plain").unwrap()));
        assert!(!framework.owns_string(code, 2));
        assert!(Arc::strong_count(framework.index()) >= 1);
    }

    // Routes

    #[test]
    fn attribute_routes_join_class_prefixes() {
        let (_dir, index) = built();
        let routes = &index.routes;
        assert_eq!(item(routes, "app_home").detail, "GET /");
        assert_eq!(item(routes, "app_about").detail, "ANY /about");
        assert_eq!(item(routes, "app_contact").detail, "GET|POST /contact");
        assert_eq!(item(routes, "admin_user_index").detail, "GET /admin/users");
        assert_eq!(
            item(routes, "admin_user_edit").detail,
            "GET|POST /admin/users/{id}/edit"
        );
    }

    #[test]
    fn legacy_annotations_are_read() {
        let (_dir, index) = built();
        let legacy = item(&index.routes, "admin_user_legacy");
        assert_eq!(legacy.detail, "DELETE /admin/users/legacy");
    }

    #[test]
    fn routes_without_a_name_are_skipped() {
        let (_dir, index) = built();
        assert!(Index::find(&index.routes, "unnamed").is_none());
        assert!(
            index
                .routes
                .iter()
                .all(|r| !r.detail.contains("/unnamed") && !r.detail.contains("/skip"))
        );
    }

    #[test]
    fn route_locations_point_at_the_attribute() {
        let (dir, index) = built();
        let home = item(&index.routes, "app_home").loc.clone().unwrap();
        assert_eq!(
            home.path,
            dir.path().join("src/Controller/HomeController.php")
        );
        assert_eq!((home.line, home.column_utf16), (8, 6));
        let edit = item(&index.routes, "admin_user_edit").loc.clone().unwrap();
        assert_eq!(
            edit.path,
            dir.path().join("src/Controller/Admin/UserController.php")
        );
        assert_eq!((edit.line, edit.column_utf16), (11, 6));
    }

    #[test]
    fn yaml_routes_are_read() {
        let (dir, index) = built();
        let health = item(&index.routes, "healthcheck");
        assert_eq!(health.detail, "GET /health");
        let loc = health.loc.clone().unwrap();
        assert_eq!(loc.path, dir.path().join("config/routes.yaml"));
        assert_eq!((loc.line, loc.column_utf16), (6, 0));
        assert_eq!(item(&index.routes, "localized").detail, "ANY /hello");
    }

    #[test]
    fn routes_in_the_routes_directory_are_read() {
        let dir = mock(&[("config/routes/api.yaml", "api_ping:\n    path: /ping\n")]);
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(item(&index.routes, "api_ping").detail, "ANY /ping");
        assert!(index.routes_complete);
    }

    #[test]
    fn plain_controller_import_keeps_routes_complete() {
        let (_dir, index) = built();
        assert!(index.routes_complete);
        let dir = mock(&[(
            "config/routes.yaml",
            "controllers:\n    resource: ../src/Controller/\n    type: attribute\n",
        )]);
        assert!(Index::detect(dir.path()).unwrap().routes_complete);
    }

    #[test]
    fn other_route_imports_make_routes_incomplete() {
        for body in [
            "api:\n    resource: '@AcmeBundle/config/routes.yaml'\n",
            "api:\n    resource: ../src/Controller/\n",
            "api:\n    resource: ../vendor/acme/src/\n    type: attribute\n",
            "api:\n    resource: routes/api.xml\n    type: xml\n",
        ] {
            let dir = mock(&[("config/routes.yaml", body)]);
            assert!(
                !Index::detect(dir.path()).unwrap().routes_complete,
                "{body}"
            );
        }
        let dir = mock(&[("config/routes/legacy.xml", "<routes/>")]);
        assert!(!Index::detect(dir.path()).unwrap().routes_complete);
    }

    #[test]
    fn attribute_route_shapes() {
        let controller = r#"<?php
#[Route(path: '/pre', name: 'pre_', methods: ['GET'])]
class A {
    #[Route(name: 'one', path: '/one')]
    public function one() {}

    #[IsGranted('ROLE_X'), Route('/two', 'two', methods: [Request::METHOD_POST])]
    public function two() {}

    #[Route('/three', name: 'three', methods: ['PUT'])]
    #[Cache(maxage: 3600)]
    public function three() {}

    public function other() { return Foo::class; }

    #[Route('/four', name: 'four_' . 'x')]
    public function four() {}
}
"#;
        let dir = mock(&[("src/Controller/A.php", controller)]);
        let index = Index::detect(dir.path()).unwrap();
        let routes = &index.routes;
        assert_eq!(item(routes, "pre_one").detail, "GET /pre/one");
        assert_eq!(item(routes, "pre_two").detail, "POST /pre/two");
        assert_eq!(item(routes, "pre_three").detail, "PUT /pre/three");
        assert!(Index::find(routes, "pre_four_x").is_none());
        assert!(Index::find(routes, "four_").is_none());
    }

    // Templates

    #[test]
    fn templates_are_relative_to_templates() {
        let (dir, index) = built();
        assert_eq!(
            names(&index.templates),
            ["base.html.twig", "home/index.html.twig"]
        );
        assert_eq!(index.template_dir, dir.path().join("templates"));
        let loc = index.templates[0].loc.clone().unwrap();
        assert_eq!(loc.path, dir.path().join("templates/base.html.twig"));
    }

    #[test]
    fn twig_default_path_is_honored() {
        let dir = mock(&[
            (
                "config/packages/twig.yaml",
                "twig:\n    default_path: '%kernel.project_dir%/views'\n",
            ),
            ("views/page.html.twig", ""),
        ]);
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(names(&index.templates), ["page.html.twig"]);
        let dir = mock(&[
            (
                "config/packages/twig.yaml",
                "twig:\n    default_path: other\n",
            ),
            ("other/x.html.twig", ""),
        ]);
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(names(&index.templates), ["x.html.twig"]);
    }

    // Services

    #[test]
    fn explicit_services_are_listed() {
        let (_dir, index) = built();
        let mailer = item(&index.services, "app.mailer");
        assert_eq!(mailer.detail, "App\\Service\\Mailer");
        assert_eq!(mailer.loc.clone().unwrap().line, 14);
        assert_eq!(
            item(&index.services, "App\\Service\\Alias").detail,
            "app.mailer"
        );
        assert!(Index::find(&index.services, "_defaults").is_none());
    }

    #[test]
    fn resource_blocks_make_services_incomplete() {
        let (_dir, index) = built();
        assert!(!index.services_complete);
        assert!(Index::find(&index.services, "App\\").is_none());
        let dir = mock(&[(
            "config/services.yaml",
            "services:\n    a.b:\n        class: A\n",
        )]);
        let index = Index::detect(dir.path()).unwrap();
        assert!(index.services_complete);
        assert_eq!(names(&index.services), ["a.b"]);
    }

    // Parameters

    #[test]
    fn parameters_come_from_services_and_packages() {
        let (_dir, index) = built();
        assert_eq!(item(&index.parameters, "app.locale").detail, "en");
        assert_eq!(
            item(&index.parameters, "app.admin_email").detail,
            "admin@example.com"
        );
        assert_eq!(item(&index.parameters, "twig.extra").detail, "1");
        let loc = item(&index.parameters, "app.locale").loc.clone().unwrap();
        assert_eq!((loc.line, loc.column_utf16), (1, 4));
    }

    #[test]
    fn kernel_parameters_are_built_in() {
        let (_dir, index) = built();
        for name in [
            "kernel.project_dir",
            "kernel.environment",
            "kernel.debug",
            "kernel.cache_dir",
            "kernel.logs_dir",
            "kernel.build_dir",
            "kernel.charset",
            "kernel.container_class",
            "kernel.runtime_environment",
        ] {
            assert!(item(&index.parameters, name).loc.is_none(), "{name}");
        }
    }

    // Environment

    #[test]
    fn env_names_come_from_dotenv_files() {
        let (dir, index) = built();
        assert_eq!(
            names(&index.env),
            ["APP_ENV", "APP_SECRET", "DATABASE_URL", "MAILER_DSN"]
        );
        let secret = item(&index.env, "APP_SECRET").loc.clone().unwrap();
        assert_eq!(secret.path, dir.path().join(".env"));
        assert_eq!((secret.line, secret.column_utf16), (2, 0));
        let dsn = item(&index.env, "MAILER_DSN").loc.clone().unwrap();
        assert_eq!(dsn.path, dir.path().join(".env.local"));
        assert_eq!((dsn.line, dsn.column_utf16), (1, 7));
    }

    #[test]
    fn env_files_for_an_environment_and_dist_are_read() {
        let dir = mock(&[
            (".env.test", "TEST_ONLY=1\n"),
            (".env.dist", "FROM_DIST=1\n"),
            (".env.local.php", "<?php return ['NOPE' => 1];"),
        ]);
        let index = Index::detect(dir.path()).unwrap();
        for name in ["TEST_ONLY", "FROM_DIST"] {
            assert!(Index::find(&index.env, name).is_some(), "{name}");
        }
        assert!(Index::find(&index.env, "NOPE").is_none());
    }

    // Translations

    #[test]
    fn yaml_translations_are_flattened() {
        let (dir, index) = built();
        let keys = index.translation_keys(None);
        assert_eq!(names(keys), ["greeting", "menu.home", "menu.admin.users"]);
        let users = item(keys, "menu.admin.users");
        assert_eq!(users.detail, "Users");
        let loc = users.loc.clone().unwrap();
        assert_eq!(loc.path, dir.path().join("translations/messages.en.yaml"));
        assert_eq!((loc.line, loc.column_utf16), (4, 8));
    }

    #[test]
    fn xliff_translations_use_the_source() {
        let (_dir, index) = built();
        let keys = index.translation_keys(Some("validators"));
        assert_eq!(names(keys), ["form.invalid"]);
        assert_eq!(keys[0].detail, "Invalid & wrong");
        let loc = keys[0].loc.clone().unwrap();
        assert_eq!((loc.line, loc.column_utf16), (2, 2));
        assert!(index.translation_keys(Some("missing")).is_empty());
    }

    #[test]
    fn translation_domains_and_locales_merge() {
        let dir = mock(&[
            (
                "translations/messages.fr.yml",
                "greeting: Bonjour\nfarewell: Salut\n",
            ),
            ("translations/messages+intl-icu.en.yaml", "icu: x\n"),
            (
                "translations/admin.en.xliff",
                "<xliff><unit id=\"u1\"><segment/></unit></xliff>",
            ),
            ("translations/notes.txt", "x"),
            ("translations/nolocale.yaml", "a: b\n"),
        ]);
        let index = Index::detect(dir.path()).unwrap();
        let keys = index.translation_keys(None);
        assert_eq!(
            names(keys),
            [
                "icu",
                "greeting",
                "menu.home",
                "menu.admin.users",
                "farewell"
            ]
        );
        assert_eq!(names(index.translation_keys(Some("admin"))), ["u1"]);
        assert!(!index.translations.contains_key("nolocale"));
    }

    // Caps

    #[test]
    fn walks_are_capped() {
        let dir = project(&[("symfony.lock", "{}")]);
        let templates = dir.path().join("templates");
        std::fs::create_dir_all(&templates).unwrap();
        for i in 0..20_050 {
            std::fs::write(templates.join(format!("t{i:05}.twig")), "").unwrap();
        }
        let index = Index::detect(dir.path()).unwrap();
        assert_eq!(index.templates.len(), 20_000);
    }
}
