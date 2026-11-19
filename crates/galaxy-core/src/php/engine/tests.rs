use super::*;

struct Project {
    dir: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (path, body) in [
            (
                "composer.json",
                r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
            ),
            (
                "app/Models/Base.php",
                "<?php\nnamespace App\\Models;\n\nclass Base\n{\n    public int $id = 0;\n    protected string $secret = '';\n\n    public function save(): static { return $this; }\n}\n",
            ),
            (
                "app/Models/User.php",
                "<?php\nnamespace App\\Models;\n\n/**\n * @property-read string $label\n * @method static User find(int $id)\n */\nclass User extends Base\n{\n    public const ROLE = 'admin';\n    public static int $count = 0;\n    public ?Profile $profile = null;\n\n    /** Display name of the user. */\n    public function name(): string { return ''; }\n    public function profile(): Profile { return $this->profile; }\n    private function hidden(): void {}\n}\n",
            ),
            (
                "app/Models/Profile.php",
                "<?php\nnamespace App\\Models;\n\nclass Profile\n{\n    public function avatar(): string { return ''; }\n}\n",
            ),
            (
                "app/Support/Container.php",
                "<?php\nnamespace App\\Support;\n\n/**\n * @template T\n * @param class-string<T> $class\n * @return T\n */\nfunction make(string $class) { return new $class; }\n",
            ),
            (
                "app/Support/Greeter.php",
                "<?php\nnamespace App\\Support;\n\ninterface Greeter {}\n",
            ),
        ] {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        std::fs::create_dir_all(root.join("vendor/composer")).unwrap();
        std::fs::write(
            root.join("vendor/composer/autoload_files.php"),
            "<?php\nreturn array(\n    'abc' => $baseDir . '/app/Support/Container.php',\n);\n",
        )
        .unwrap();
        std::fs::write(
            root.join("vendor/composer/autoload_psr4.php"),
            "<?php\nreturn array(\n    'Illuminate\\\\Pagination\\\\' => array($vendorDir . '/laravel/framework/src/Illuminate/Pagination'),\n);\n",
        )
        .unwrap();
        let pagination = root.join("vendor/laravel/framework/src/Illuminate/Pagination");
        std::fs::create_dir_all(&pagination).unwrap();
        std::fs::write(
            pagination.join("AbstractPaginator.php"),
            "<?php\nnamespace Illuminate\\Pagination;\nclass AbstractPaginator\n{\n    public function perPage(): int { return 15; }\n}\n",
        )
        .unwrap();
        std::fs::write(
            pagination.join("LengthAwarePaginator.php"),
            "<?php\nnamespace Illuminate\\Pagination;\nclass LengthAwarePaginator extends AbstractPaginator\n{\n    public function total(): int { return 0; }\n}\n",
        )
        .unwrap();
        Self { dir }
    }

    fn file(&self) -> PathBuf {
        self.dir.path().join("app/Http/Page.php")
    }

    /// Completions at the `|` marker of `source`.
    fn complete(&self, source: &str) -> Vec<Completion> {
        let (text, line, column) = marker(source);
        let mut engine = PhpEngine::new(self.dir.path());
        engine.complete(&self.file(), &text, line, column)
    }
}

fn marker(source: &str) -> (String, u32, u32) {
    let at = source.find('|').expect("marker");
    let text = source.replacen('|', "", 1);
    let (line, column, _) = position_of(&text, at);
    (text, line, column)
}

fn labels(items: &[Completion]) -> Vec<&str> {
    items.iter().map(|c| c.label.as_str()).collect()
}

const HEAD: &str = "<?php\nnamespace App\\Http;\n\nuse App\\Models\\User;\n\nclass Page extends \\App\\Models\\Base\n{\n    private User $owner;\n    public function show(User $user)\n    {\n";

#[test]
fn completes_members_of_a_typed_parameter() {
    let project = Project::new();
    let items = project.complete(&format!("{HEAD}        $user->|\n    }}\n}}\n"));
    let names = labels(&items);
    assert!(names.contains(&"name"), "{names:?}");
    assert!(names.contains(&"profile"));
    assert!(names.contains(&"save"), "inherited method");
    assert!(names.contains(&"id"), "inherited property");
    assert!(names.contains(&"label"), "@property");
    assert!(!names.contains(&"hidden"), "private");
    assert!(
        names.contains(&"secret"),
        "protected, declared in a shared ancestor"
    );
    assert!(
        !names.contains(&"ROLE"),
        "constants are not instance members"
    );
    assert!(
        items
            .iter()
            .any(|c| c.label == "name" && c.insert_text == "name($0)" && c.is_snippet)
    );
}

#[test]
fn filters_by_typed_prefix() {
    let project = Project::new();
    let items = project.complete(&format!("{HEAD}        $user->na|\n    }}\n}}\n"));
    assert_eq!(labels(&items), ["name"]);
}

#[test]
fn completion_uses_case_sensitive_prefix_not_subsequence() {
    let project = Project::new();
    let source = format!("{HEAD}        $user->na|\n    }}\n}}\n");
    assert_eq!(labels(&project.complete(&source)), ["name"]);
    assert!(
        project
            .complete(&format!("{HEAD}        $user->Na|\n    }}\n}}\n"))
            .is_empty()
    );
    assert!(
        project
            .complete(&format!("{HEAD}        $user->ame|\n    }}\n}}\n"))
            .is_empty()
    );

    let classes = project.complete(&format!("{HEAD}        new User|\n    }}\n}}\n"));
    assert!(labels(&classes).contains(&"User"));
    assert!(
        !labels(&project.complete(&format!("{HEAD}        new user|\n    }}\n}}\n")))
            .contains(&"User")
    );

    let functions = project.complete(&format!("{HEAD}        log|\n    }}\n}}\n"));
    assert!(labels(&functions).contains(&"log"));
    assert!(
        !labels(&project.complete(&format!("{HEAD}        Log|\n    }}\n}}\n"))).contains(&"log")
    );
}

#[test]
fn definition_of_trait_method_call_points_to_trait() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/Traits")).unwrap();
    std::fs::write(
        dir.path().join("composer.json"),
        r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/Traits/Response.php"),
        "<?php\nnamespace App\\Traits;\ntrait Response { public function responseJson(): array { return []; } }\n",
    )
    .unwrap();
    let path = dir.path().join("app/Controller.php");
    let source = "<?php\nnamespace App;\nuse App\\Traits\\Response;\nclass Controller { use Response; public function show(): array { return $this->responseJson|(); } }\n";
    let (text, line, column) = marker(source);
    let mut engine = PhpEngine::new(dir.path());

    let target = engine.definition(&path, &text, line, column);
    assert_eq!(target.len(), 1);
    assert!(target[0].path.ends_with("app/Traits/Response.php"));
    assert_eq!(target[0].line, 2);
}

#[test]
fn definition_of_parameter_use_points_to_parameter() {
    let project = Project::new();
    let source = format!("{HEAD}        return $user|->name();\n    }}\n}}\n");
    let (text, line, column) = marker(&source);
    let mut engine = PhpEngine::new(project.dir.path());

    let target = engine.definition(&project.file(), &text, line, column);
    assert_eq!(target.len(), 1);
    assert_eq!(target[0].line, 8);
    assert_eq!(target[0].column, 30);
}

#[test]
fn treats_blade_named_files_as_php_without_laravel() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::write(
        dir.path().join("composer.json"),
        r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/User.php"),
        "<?php\nnamespace App;\nclass User { public function name(): string { return ''; } }\n",
    )
    .unwrap();
    let path = dir.path().join("views/example.blade.php");
    let source = "<?php\nnamespace App;\nclass Page {\n    public function show(User $user) {\n        $user->na|\n    }\n}\n";
    let (text, line, column) = marker(source);
    let mut engine = PhpEngine::new(dir.path());

    let items = engine.complete(&path, &text, line, column);
    assert_eq!(labels(&items), ["name"]);
}

#[test]
fn treats_blade_named_files_as_php_with_eloquent_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app")).unwrap();
    std::fs::write(
        dir.path().join("composer.json"),
        r#"{"autoload":{"psr-4":{"App\\":"app/"}},"require":{"illuminate/database":"^12"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("app/User.php"),
        "<?php\nnamespace App;\nclass User { public function name(): string { return ''; } }\n",
    )
    .unwrap();
    let path = dir.path().join("views/example.blade.php");
    let source = "<?php\nnamespace App;\nclass Page {\n    public function show(User $user) {\n        $user->na|\n    }\n}\n";
    let (text, line, column) = marker(source);
    let mut engine = PhpEngine::new(dir.path());

    assert!(engine.is_framework("laravel"));
    assert_eq!(
        labels(&engine.complete(&path, &text, line, column)),
        ["name"]
    );
}

#[test]
fn completes_static_members_and_magic_methods() {
    let project = Project::new();
    let items = project.complete(&format!("{HEAD}        User::|\n    }}\n}}\n"));
    let names = labels(&items);
    assert!(names.contains(&"ROLE"));
    assert!(names.contains(&"$count"));
    assert!(names.contains(&"find"), "@method static");
    assert!(names.contains(&"class"));
    assert!(!names.contains(&"name"), "instance method");
}

#[test]
fn follows_chains_assignments_and_this() {
    let project = Project::new();
    let items = project.complete(&format!(
        "{HEAD}        $p = $user->profile();\n        $p->|\n    }}\n}}\n"
    ));
    assert_eq!(labels(&items), ["avatar"]);

    let items = project.complete(&format!("{HEAD}        $user->profile()->|\n    }}\n}}\n"));
    assert_eq!(labels(&items), ["avatar"]);

    let items = project.complete(&format!("{HEAD}        $this->owner->|\n    }}\n}}\n"));
    assert!(labels(&items).contains(&"name"));

    let items = project.complete(&format!("{HEAD}        $this->|\n    }}\n}}\n"));
    let names = labels(&items);
    assert!(names.contains(&"owner"), "private of own class");
    assert!(names.contains(&"secret"), "protected of parent");
    assert!(names.contains(&"show"));

    let items = project.complete(&format!(
        "{HEAD}        $user\n            ->save()\n            ->|\n    }}\n}}\n"
    ));
    assert!(
        labels(&items).contains(&"name"),
        "static return and multi-line chain"
    );
}

#[test]
fn follows_untyped_property_injected_by_constructor() {
    let project = Project::new();
    let service = project.dir.path().join("app/Services/PhoneQcService.php");
    std::fs::create_dir_all(service.parent().unwrap()).unwrap();
    std::fs::write(
            &service,
            "<?php\nnamespace App\\Services;\nclass PhoneQcService { public function handleListPhoneQcOfDatatable(): array { return []; } }\n",
        )
        .unwrap();
    let (text, line, column) = marker(
        "<?php\nnamespace App\\Http;\nuse App\\Services\\PhoneQcService;\nclass PhoneQcController {\n    protected $phoneQcService;\n    public function __construct(PhoneQcService $phoneQcService) { $this->phoneQcService = $phoneQcService; }\n    public function datatable() { $this->phoneQcService->handleListPhoneQcOfDatata|ble(); }\n}\n",
    );
    let mut engine = PhpEngine::new(project.dir.path());
    let targets = engine.definition(&project.file(), &text, line, column);
    assert_eq!(targets.len(), 1);
    assert!(targets[0].path.ends_with("app/Services/PhoneQcService.php"));
}

#[test]
fn resolves_class_string_templates_and_new() {
    let project = Project::new();
    let items = project.complete(&format!(
        "{HEAD}        \\App\\Support\\make(User::class)->|\n    }}\n}}\n"
    ));
    assert!(labels(&items).contains(&"name"));

    let items = project.complete(&format!("{HEAD}        (new User)->|\n    }}\n}}\n"));
    assert!(labels(&items).contains(&"name"));

    let items = project.complete(&format!(
        "{HEAD}        /** @var User $x */\n        $x = foo();\n        $x->|\n    }}\n}}\n"
    ));
    assert!(
        labels(&items).contains(&"name"),
        "docblock beats unknown call"
    );

    let items = project.complete(&format!(
            "{HEAD}        if ($thing instanceof User) {{\n            $thing->|\n        }}\n    }}\n}}\n"
        ));
    assert!(labels(&items).contains(&"name"), "instanceof narrowing");
}

#[test]
fn narrows_this_property_from_instanceof() {
    let project = Project::new();
    let items = project.complete(
        "<?php\nnamespace App\\Http;\nuse App\\Models\\User;\nclass Page {\n    private $result;\n    public function show() {\n        if ($this->result instanceof User) {\n            $this->result->na|\n        }\n    }\n}\n",
    );
    assert!(
        labels(&items).contains(&"name"),
        "property instanceof narrowing"
    );
}

#[test]
fn finds_vendor_method_on_this_property_narrowed_by_instanceof() {
    let project = Project::new();
    let (source, line, column) = marker(
        "<?php\nnamespace App\\Http;\nuse Illuminate\\Pagination\\LengthAwarePaginator;\nclass Page {\n    private $result;\n    public function show() {\n        if ($this->result instanceof LengthAwarePaginator) {\n            $this->result->to|tal();\n        }\n    }\n}\n",
    );
    let mut engine = PhpEngine::new(project.dir.path());
    let target = engine.definition(&project.file(), &source, line, column);

    assert_eq!(target.len(), 1);
    assert!(
        target[0].path.ends_with(
            "vendor/laravel/framework/src/Illuminate/Pagination/LengthAwarePaginator.php"
        ),
        "{target:?}"
    );
    assert_eq!(target[0].line, 4);
}

#[test]
fn finds_inherited_vendor_method_on_this_property_narrowed_by_instanceof() {
    let project = Project::new();
    let (source, line, column) = marker(
        "<?php\nnamespace App\\Http;\nuse Illuminate\\Pagination\\LengthAwarePaginator;\nclass Page {\n    private $result;\n    public function show() {\n        if ($this->result instanceof LengthAwarePaginator) {\n            $this->result->per|Page();\n        }\n    }\n}\n",
    );
    let mut engine = PhpEngine::new(project.dir.path());
    let target = engine.definition(&project.file(), &source, line, column);

    assert_eq!(target.len(), 1);
    assert!(
        target[0]
            .path
            .ends_with("vendor/laravel/framework/src/Illuminate/Pagination/AbstractPaginator.php"),
        "{target:?}"
    );
    assert_eq!(target[0].line, 4);
}

#[test]
fn finds_vendor_method_in_elseif_narrowed_by_instanceof() {
    let project = Project::new();
    let (source, line, column) = marker(
        "<?php\nnamespace App\\Http;\nuse Illuminate\\Pagination\\LengthAwarePaginator;\nclass Page {\n    private $result;\n    public function show() {\n        if ($this->result instanceof \\stdClass) {\n        } elseif ($this->result instanceof LengthAwarePaginator) {\n            $this->result->to|tal();\n        }\n    }\n}\n",
    );
    let mut engine = PhpEngine::new(project.dir.path());
    let target = engine.definition(&project.file(), &source, line, column);

    assert_eq!(target.len(), 1, "{target:?}");
    assert!(
        target[0].path.ends_with(
            "vendor/laravel/framework/src/Illuminate/Pagination/LengthAwarePaginator.php"
        ),
        "{target:?}"
    );
}

#[test]
fn suggests_classes_with_import() {
    let project = Project::new();
    let items = project.complete(&format!("{HEAD}        $x = new Prof|\n    }}\n}}\n"));
    let profile = items
        .iter()
        .find(|c| c.label == "Profile")
        .expect("Profile");
    assert_eq!(profile.detail.as_deref(), Some("App\\Models"));
    assert_eq!(
        profile.extra,
        [Insertion {
            line: 4,
            column: 0,
            text: "use App\\Models\\Profile;\n".into()
        }]
    );
    assert!(
        !items.iter().any(|c| c.label == "Greeter"),
        "interfaces cannot be instantiated"
    );

    let items = project.complete(&format!("{HEAD}        $x = new Us|\n    }}\n}}\n"));
    let user = items.iter().find(|c| c.label == "User").expect("User");
    assert!(user.extra.is_empty(), "already imported");
}

#[test]
fn completes_use_statements() {
    let project = Project::new();
    let items = project.complete("<?php\nuse App\\Mo|\n");
    assert_eq!(labels(&items), ["Models"]);
    assert_eq!(items[0].insert_text, "Models\\");

    let items = project.complete("<?php\nuse App\\Models\\Us|\n");
    assert_eq!(labels(&items), ["User"]);
}

#[test]
fn suggests_variables_and_functions() {
    let project = Project::new();
    let items = project.complete(&format!("{HEAD}        $us|\n    }}\n}}\n"));
    assert!(labels(&items).contains(&"$user"));
    let items = project.complete(&format!("{HEAD}        ma|\n    }}\n}}\n"));
    assert!(
        items
            .iter()
            .any(|c| c.label == "make" && c.kind == ItemKind::Function)
    );
}

#[test]
fn finds_definitions_and_hover() {
    let project = Project::new();
    let source = format!("{HEAD}        $user->name();\n        User::find(1);\n    }}\n}}\n");
    let mut engine = PhpEngine::new(project.dir.path());
    let line = source
        .lines()
        .position(|l| l.contains("$user->name"))
        .unwrap() as u32;
    let column = source
        .lines()
        .nth(line as usize)
        .unwrap()
        .find("name")
        .unwrap() as u32
        + 1;

    let target = engine.definition(&project.file(), &source, line, column);
    assert_eq!(target.len(), 1);
    assert!(target[0].path.ends_with("app/Models/User.php"));
    assert_eq!(target[0].line, 14);

    let hover = engine
        .hover(&project.file(), &source, line, column)
        .unwrap();
    assert!(
        hover.markdown.contains("function User::name(): string"),
        "{}",
        hover.markdown
    );
    assert!(hover.markdown.contains("Display name of the user."));

    let column = source
        .lines()
        .nth(line as usize + 1)
        .unwrap()
        .find("User")
        .unwrap() as u32
        + 1;
    let target = engine.definition(&project.file(), &source, line + 1, column);
    assert!(target[0].path.ends_with("app/Models/User.php"));
    assert_eq!(target[0].line, 7);
}

#[test]
fn works_without_composer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Thing.php"),
        "<?php\nclass Thing { public function run(): int { return 1; } }\n",
    )
    .unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let text = "<?php\n$t = new Thing();\n$t->";
    let items = engine.complete(&dir.path().join("main.php"), text, 2, 4);
    assert_eq!(labels(&items), ["run"]);
}

#[test]
fn vendor_files_use_the_application_composer_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = root.join("vendor/acme/source/src/UsesTarget.php");
    let target = root.join("vendor/acme/target/src/Target.php");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::create_dir_all(root.join("vendor/composer")).unwrap();
    std::fs::write(
        root.join("vendor/composer/autoload_psr4.php"),
        "<?php\nreturn array(\n    'Acme\\\\Source\\\\' => array($vendorDir . '/acme/source/src'),\n    'Acme\\\\Target\\\\' => array($vendorDir . '/acme/target/src'),\n);\n",
    )
    .unwrap();
    std::fs::write(
        &source,
        "<?php\nnamespace Acme\\Source;\nuse Acme\\Target\\Target;\nnew Target();\n",
    )
    .unwrap();
    std::fs::write(&target, "<?php\nnamespace Acme\\Target;\nclass Target {}\n").unwrap();

    assert_eq!(project_root(&source), root);
    let mut engine = PhpEngine::new(root);
    let text = std::fs::read_to_string(&source).unwrap();
    let found = engine.definition(&source, &text, 3, 5);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, target);
}

#[test]
fn knows_phps_own_classes_and_functions() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("main.php");

    // Inherited from the built-in Exception.
    let text = "<?php\nclass Boom extends Exception {}\n$b = new Boom();\n$b->";
    let items = engine.complete(&file, text, 3, 4);
    let names = labels(&items);
    assert!(names.contains(&"getMessage"), "{names:?}");
    assert!(names.contains(&"getTraceAsString"), "{names:?}");

    // Built-in function and its return type.
    let text = "<?php\n$d = new DateTime();\n$d->";
    let items = engine.complete(&file, text, 2, 4);
    assert!(labels(&items).contains(&"format"));

    let text = "<?php\nstrle";
    let items = engine.complete(&file, text, 1, 5);
    assert!(labels(&items).contains(&"strlen"));

    // Class names come with the index.
    let text = "<?php\nnew ArrayIt";
    let items = engine.complete(&file, text, 1, 10);
    assert!(labels(&items).contains(&"ArrayIterator"));

    // Hover shows the official php-src declaration; definition materializes it.
    let text = "<?php\nstrlen('a');";
    let hover = engine.hover(&file, text, 1, 2).expect("hover on strlen");
    assert!(hover.markdown.contains("strlen"), "{}", hover.markdown);
    let found = engine.definition(&file, text, 1, 2);
    assert_eq!(found.len(), 1);
    assert!(found[0].path.is_file());
    assert!(
        found[0]
            .path
            .to_string_lossy()
            .contains("galaxy-php-stubs/8.5/")
    );
}

#[test]
fn unsupported_pre_8_0_projects_do_not_expose_bundled_builtin_api() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("composer.json"),
        r#"{"require":{"php":"^7.4"}}"#,
    )
    .unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let items = engine.complete(&dir.path().join("main.php"), "<?php\nstrle", 1, 5);
    assert!(!labels(&items).contains(&"strlen"));
}

#[test]
fn property_reads_resolve_to_a_method_when_the_class_has_get() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("main.php");
    let text = "<?php\n\
class Base { public function __get($key) { return null; } }\n\
class Order extends Base {\n\
    public function items(): array { return []; }\n\
}\n\
class Plain {\n\
    public function items(): array { return []; }\n\
}\n\
$order = new Order();\n\
$order->items;\n\
$plain = new Plain();\n\
$plain->items;\n";

    // `__get` is inherited: the read points at `items()`.
    let found = engine.definition(&file, text, 9, 10);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].line, 3);
    let hover = engine.hover(&file, text, 9, 10).expect("hover");
    assert!(hover.markdown.contains("items"), "{}", hover.markdown);

    // Without `__get` a read is not a call: nothing to jump to.
    assert!(engine.definition(&file, text, 11, 10).is_empty());
}

#[test]
fn helpers_declared_behind_function_exists_are_found() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("vendor/composer")).unwrap();
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::write(root.join("composer.json"), "{}").unwrap();
    std::fs::write(
            root.join("vendor/composer/autoload_files.php"),
            "<?php\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\nreturn array(\n    'abc' => $baseDir . '/lib/helpers.php',\n);\n",
        )
        .unwrap();
    std::fs::write(
            root.join("lib/helpers.php"),
            "<?php\n\nif (! function_exists('now')) {\n    /** The current time. */\n    function now($tz = null)\n    {\n    }\n}\n",
        )
        .unwrap();
    let mut engine = PhpEngine::new(root);
    let file = root.join("main.php");
    let text = "<?php\n$at = now();\n";
    let found = engine.definition(&file, text, 1, 7);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].path.ends_with("lib/helpers.php"));
    assert_eq!(found[0].line, 4);
    let hover = engine.hover(&file, text, 1, 7).expect("hover");
    assert!(
        hover.markdown.contains("The current time"),
        "{}",
        hover.markdown
    );
}

#[test]
fn language_constructs_that_look_like_calls_have_a_target() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("main.php");
    let text = "<?php\nif (empty($a) && isset($b)) {}\n";
    for column in [5, 20] {
        let found = engine.definition(&file, text, 1, column);
        assert_eq!(found.len(), 1, "column {column}: {found:?}");
        assert!(found[0].path.is_file());
    }
    let hover = engine.hover(&file, text, 1, 5).expect("hover");
    assert!(hover.markdown.contains("empty"), "{}", hover.markdown);
}

#[test]
fn unset_exit_and_die_have_a_target() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("main.php");
    let text = "<?php\nunset($a);\nexit(1);\ndie('x');\nEXIT;\n";
    // The cursor inside the word and right after it both count.
    for (line, column) in [(1, 2), (1, 5), (2, 2), (2, 4), (3, 1), (4, 2)] {
        let found = engine.definition(&file, text, line, column);
        assert_eq!(found.len(), 1, "{line}:{column} {found:?}");
        assert!(found[0].path.is_file());
    }
    let hover = engine.hover(&file, text, 1, 2).expect("hover on unset");
    assert!(hover.markdown.contains("unset"), "{}", hover.markdown);
    assert_eq!((hover.start_column, hover.end_column), (0, 5));
    // A variable inside the call is still a variable.
    assert!(engine.definition(&file, text, 1, 8).is_empty());
}

#[test]
fn docblock_methods_open_at_their_tag() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, body) in [
        (
            "composer.json",
            r#"{"autoload":{"psr-4":{"Lib\\":"lib/"}}}"#,
        ),
        (
            "lib/Clock.php",
            "<?php\nnamespace Lib;\n\n/**\n * @method Clock addMinutes(int $value = 1)\n * @method Clock addDays(int $value = 1)\n */\ninterface Clock\n{\n}\n",
        ),
        (
            "lib/helpers.php",
            "<?php\nuse Lib\\Clock;\n\nfunction now(): Clock {}\n",
        ),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    std::fs::create_dir_all(root.join("vendor/composer")).unwrap();
    std::fs::write(
            root.join("vendor/composer/autoload_files.php"),
            "<?php\n$baseDir = dirname(dirname(__DIR__));\nreturn array('a' => $baseDir . '/lib/helpers.php');\n",
        )
        .unwrap();
    let mut engine = PhpEngine::new(root);
    let file = root.join("main.php");
    let text = "<?php\n$a = now()->addMinutes(50);\n$b = now()->addDays(1);\n";
    let found = engine.definition(&file, text, 1, 15);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].path.ends_with("lib/Clock.php"));
    assert_eq!((found[0].line, found[0].column), (4, 17));
    let found = engine.definition(&file, text, 2, 15);
    assert_eq!((found[0].line, found[0].column), (5, 17));
}

#[test]
fn built_in_functions_and_classes_open_a_real_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("main.php");
    let text = "<?php\n$x = json_encode([]);\n$e = new \\Exception('x');\n";
    let found = engine.definition(&file, text, 1, 8);
    assert_eq!(found.len(), 1, "{found:?}");
    let body = std::fs::read_to_string(&found[0].path).unwrap();
    assert!(
        body.contains("function json_encode"),
        "{}",
        found[0].path.display()
    );
    let line = body.lines().nth(found[0].line as usize).unwrap();
    assert!(line.contains("json_encode"), "{line}");

    let found = engine.definition(&file, text, 2, 17);
    assert_eq!(found.len(), 1, "{found:?}");
    let body = std::fs::read_to_string(&found[0].path).unwrap();
    assert!(body.contains("class Exception"));
}

#[test]
fn model_static_calls_resolve_to_the_query_builder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, body) in [
        (
            "composer.json",
            r#"{"autoload":{"psr-4":{"Illuminate\\Database\\Eloquent\\":"lib/","App\\":"app/"}}}"#,
        ),
        (
            "lib/Model.php",
            "<?php\nnamespace Illuminate\\Database\\Eloquent;\n\nabstract class Model\n{\n    public static function __callStatic($method, $parameters) {}\n}\n",
        ),
        (
            "lib/Builder.php",
            "<?php\nnamespace Illuminate\\Database\\Eloquent;\n\nclass Builder\n{\n    public function create(array $attributes = []) {}\n    public function join($table, $first, $operator = null, $second = null) {}\n}\n",
        ),
        (
            "app/Log.php",
            "<?php\nnamespace App;\n\nuse Illuminate\\Database\\Eloquent\\Model;\n\nclass Log extends Model\n{\n}\n",
        ),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let mut engine = PhpEngine::with_frameworks(root, eloquent());
    let main = root.join("app/Service.php");
    let text = "<?php\nnamespace App;\n\nLog::create([]);\nLog::missing();\n";
    let found = engine.definition(&main, text, 3, 8);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].path.ends_with("lib/Builder.php"));
    assert_eq!(found[0].line, 5);
    let hover = engine.hover(&main, text, 3, 8).expect("hover");
    assert!(
        hover.markdown.contains("Builder::create"),
        "{}",
        hover.markdown
    );
    assert!(engine.definition(&main, text, 4, 8).is_empty());

    let items = engine.complete(&main, "<?php\nnamespace App;\nLog::joi", 2, 8);
    let join = items
        .iter()
        .find(|item| item.label == "join")
        .expect("forwarded join completion");
    assert_eq!(join.kind, ItemKind::Method);
    assert!(join.is_snippet);
}

#[test]
fn relation_chains_keep_resolving_through_the_query_builder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, body) in [
        (
            "composer.json",
            r#"{"autoload":{"psr-4":{"Illuminate\\Database\\Eloquent\\":"lib/","App\\":"app/"}}}"#,
        ),
        (
            "lib/Model.php",
            "<?php\nnamespace Illuminate\\Database\\Eloquent;\n\nabstract class Model\n{\n    public static function __callStatic($method, $parameters) {}\n    public function save(): bool { return true; }\n}\n",
        ),
        (
            "lib/Builder.php",
            r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel of \Illuminate\Database\Eloquent\Model
 */
class Builder
{
    /** @return $this */
    public function where($column) { return $this; }

    /** @return TModel|null */
    public function first() {}

    /** @return TModel */
    public function create(array $attributes = []) {}

    /**
     * @return ($id is array ? array : TModel|null)
     */
    public function find($id) {}
}
"#,
        ),
        (
            "lib/Relations/Relation.php",
            "<?php\nnamespace Illuminate\\Database\\Eloquent\\Relations;\n\n/**\n * @template TRelatedModel of \\Illuminate\\Database\\Eloquent\\Model\n * @template TDeclaringModel of \\Illuminate\\Database\\Eloquent\\Model\n *\n * @mixin \\Illuminate\\Database\\Eloquent\\Builder<TRelatedModel>\n */\nabstract class Relation\n{\n}\n",
        ),
        (
            "lib/Relations/HasMany.php",
            "<?php\nnamespace Illuminate\\Database\\Eloquent\\Relations;\n\n/**\n * @template TRelatedModel of \\Illuminate\\Database\\Eloquent\\Model\n * @template TDeclaringModel of \\Illuminate\\Database\\Eloquent\\Model\n *\n * @extends \\Illuminate\\Database\\Eloquent\\Relations\\Relation<TRelatedModel, TDeclaringModel>\n */\nclass HasMany extends Relation\n{\n}\n",
        ),
        (
            "app/Otp.php",
            "<?php\nnamespace App;\n\nuse Illuminate\\Database\\Eloquent\\Model;\n\nclass Otp extends Model\n{\n}\n",
        ),
        (
            "app/User.php",
            "<?php\nnamespace App;\n\nuse Illuminate\\Database\\Eloquent\\Model;\nuse Illuminate\\Database\\Eloquent\\Relations\\HasMany;\n\nclass User extends Model\n{\n    /** @return HasMany<Otp, $this> */\n    public function otps(): HasMany {}\n}\n",
        ),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let mut engine = PhpEngine::with_frameworks(root, eloquent());
    let main = root.join("app/Run.php");
    let text = "<?php\nnamespace App;\n\nclass Run\n{\n    public function go()\n    {\n        $u = User::find(1);\n        $u->otps()->where('a')->first()->save();\n        $u->otps()->create([])->save();\n    }\n}\n";
    let at = |engine: &mut PhpEngine, line: u32, needle: &str| {
        let row = text.lines().nth(line as usize).unwrap();
        let column = row.find(needle).unwrap() as u32 + 1;
        engine.definition(&main, text, line, column)
    };
    let otps = at(&mut engine, 8, "otps");
    assert!(otps[0].path.ends_with("app/User.php"), "{otps:?}");
    let r#where = at(&mut engine, 8, "where");
    assert!(r#where[0].path.ends_with("lib/Builder.php"), "{where:?}");
    let first = at(&mut engine, 8, "first");
    assert!(first[0].path.ends_with("lib/Builder.php"), "{first:?}");
    let save = at(&mut engine, 8, "save");
    assert!(save[0].path.ends_with("lib/Model.php"), "{save:?}");
    let create = at(&mut engine, 9, "create");
    assert!(create[0].path.ends_with("lib/Builder.php"), "{create:?}");
    let save = at(&mut engine, 9, "save");
    assert!(save[0].path.ends_with("lib/Model.php"), "{save:?}");
}

/// Eloquent as Laravel declares it: `first()` lives in the
/// `BuildsQueries` trait, whose `TValue` the builder binds with a
/// `@use` docblock on its `use` statement.
fn query_builder_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, body) in [
        (
            "composer.json",
            r#"{"autoload":{"psr-4":{"Illuminate\\Database\\":"lib/","App\\":"app/"}}}"#,
        ),
        (
            "lib/Eloquent/Model.php",
            "<?php\nnamespace Illuminate\\Database\\Eloquent;\n\nabstract class Model\n{\n    public function __call($method, $parameters) {}\n    public static function __callStatic($method, $parameters) {}\n    public function save(): bool { return true; }\n    /** @return \\Illuminate\\Database\\Eloquent\\Builder<static> */\n    public static function query() {}\n}\n",
        ),
        (
            "lib/Concerns/BuildsQueries.php",
            r#"<?php
namespace Illuminate\Database\Concerns;

/**
 * @template TValue
 *
 * @mixin \Illuminate\Database\Query\Builder
 */
trait BuildsQueries
{
    /**
     * @param  array|string  $columns
     * @return TValue|null
     */
    public function first($columns = ['*']) {}
}
"#,
        ),
        (
            "lib/Query/Builder.php",
            "<?php\nnamespace Illuminate\\Database\\Query;\n\nclass Builder\n{\n    public function toSql() {}\n}\n",
        ),
        (
            "lib/Eloquent/Collection.php",
            r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TKey of array-key
 * @template TModel of \Illuminate\Database\Eloquent\Model
 *
 * @implements \IteratorAggregate<TKey, TModel>
 */
class Collection implements \IteratorAggregate
{
    /** @return TModel|null */
    public function first() {}

    public function getIterator(): \Traversable {}
}
"#,
        ),
        (
            "lib/Eloquent/Builder.php",
            r#"<?php
namespace Illuminate\Database\Eloquent;

use Illuminate\Database\Concerns\BuildsQueries;

/**
 * @template TModel of \Illuminate\Database\Eloquent\Model
 *
 * @mixin \Illuminate\Database\Query\Builder
 */
class Builder
{
    /** @use \Illuminate\Database\Concerns\BuildsQueries<TModel> */
    use BuildsQueries;

    /** @return $this */
    public function where($column, $operator = null) { return $this; }

    /** @return $this */
    public function when($value, $callback) { return $this; }

    /** @return $this */
    public function latest($column = null) { return $this; }

    /** @return TModel */
    public function firstOrFail($columns = ['*']) {}

    /**
     * @return ($id is array ? \Illuminate\Database\Eloquent\Collection<int, TModel> : TModel|null)
     */
    public function find($id, $columns = ['*']) {}

    /** @return \Illuminate\Database\Eloquent\Collection<int, TModel> */
    public function get($columns = ['*']) {}
}
"#,
        ),
        (
            "lib/Eloquent/Relations/Relation.php",
            "<?php\nnamespace Illuminate\\Database\\Eloquent\\Relations;\n\n/**\n * @template TRelatedModel of \\Illuminate\\Database\\Eloquent\\Model\n * @template TDeclaringModel of \\Illuminate\\Database\\Eloquent\\Model\n *\n * @mixin \\Illuminate\\Database\\Eloquent\\Builder<TRelatedModel>\n */\nabstract class Relation\n{\n}\n",
        ),
        (
            "lib/Eloquent/Relations/HasMany.php",
            "<?php\nnamespace Illuminate\\Database\\Eloquent\\Relations;\n\n/**\n * @template TRelatedModel of \\Illuminate\\Database\\Eloquent\\Model\n * @template TDeclaringModel of \\Illuminate\\Database\\Eloquent\\Model\n *\n * @extends \\Illuminate\\Database\\Eloquent\\Relations\\Relation<TRelatedModel, TDeclaringModel>\n */\nclass HasMany extends Relation\n{\n}\n",
        ),
        (
            "app/Post.php",
            "<?php\nnamespace App;\n\nuse Illuminate\\Database\\Eloquent\\Model;\n\nclass Post extends Model\n{\n    public function publish(): bool { return true; }\n}\n",
        ),
        (
            "app/User.php",
            "<?php\nnamespace App;\n\nuse Illuminate\\Database\\Eloquent\\Model;\nuse Illuminate\\Database\\Eloquent\\Relations\\HasMany;\n\nclass User extends Model\n{\n    /** @return HasMany<Post, $this> */\n    public function posts(): HasMany {}\n\n    public function isActive(): bool { return true; }\n}\n",
        ),
    ] {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    dir
}

#[test]
fn query_builder_chains_resolve_to_the_model() {
    let dir = query_builder_project();
    let mut engine = PhpEngine::with_frameworks(dir.path(), eloquent());
    let main = dir.path().join("app/Run.php");
    let text = "<?php
namespace App;

class Run
{
    public function go($request, User $owner)
    {
        $a = User::where('email', $request->email)->first();
        $a->isActive();
        $b = User::where('email', 'x')->firstOrFail();
        $b->isActive();
        $c = User::latest()->first();
        $c->isActive();
        $d = User::query()->where('a', 1)->first();
        $d->isActive();
        $e = User::query()->where('a', 1)->get()->first();
        $e->isActive();
        $f = User::where('a', 1)->find(3);
        $f->isActive();
        $g = $owner->posts()->where('a', 1)->first();
        $g->publish();
        $h = $owner->posts()->latest()->first();
        $h->publish();
        foreach (User::query()->where('a', 1)->get() as $item) {
            $item->isActive();
        }
        $i = User::find(1);
        $i->isActive();
        $j = User::where('a', 1)->first()->isActive();
    }
}

";
    let lines: Vec<&str> = text.lines().collect();
    let cases = [
        ("$a->isActive", "first()"),
        ("$b->isActive", "firstOrFail()"),
        ("$c->isActive", "latest()->first()"),
        ("$d->isActive", "query()->where()->first()"),
        ("$e->isActive", "get()->first()"),
        ("$f->isActive", "where()->find()"),
        ("$i->isActive", "find()"),
        ("$item->isActive", "foreach over get()"),
        (
            "$j = User::where('a', 1)->first()->isActive",
            "first()->isActive()",
        ),
    ];
    let mut failures = Vec::new();
    for (needle, label) in cases {
        let row = lines.iter().position(|l| l.contains(needle)).unwrap();
        let column = if needle.starts_with("$j") {
            lines[row].rfind("isActive").unwrap()
        } else {
            lines[row].find("isActive").unwrap()
        } as u32
            + 1;
        let found = engine.definition(&main, text, row as u32, column);
        if !found
            .first()
            .is_some_and(|f| f.path.ends_with("app/User.php"))
        {
            failures.push(format!("{label}: {found:?}"));
        }
    }
    for (needle, label) in [
        ("$g->publish", "relation where()->first()"),
        ("$h->publish", "relation latest()->first()"),
    ] {
        let row = lines.iter().position(|l| l.contains(needle)).unwrap();
        let column = lines[row].find("publish").unwrap() as u32 + 1;
        let found = engine.definition(&main, text, row as u32, column);
        if !found
            .first()
            .is_some_and(|f| f.path.ends_with("app/Post.php"))
        {
            failures.push(format!("{label}: {found:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn query_builder_callbacks_resolve_untyped_parameters() {
    let dir = query_builder_project();
    let mut engine = PhpEngine::with_frameworks(dir.path(), eloquent());
    let main = dir.path().join("app/Run.php");
    let text = "<?php
namespace App;

class Run
{
    public function go()
    {
        User::query()->where(function ($query) {
            $query->where('active', true);
            $query->when(true, fn ($q) => $q->where('published', true));
        });
    }
}
";
    let at = |engine: &mut PhpEngine, line: usize, needle: &str| {
        let source_line = text.lines().nth(line).unwrap();
        let column = source_line.rfind(needle).unwrap() as u32;
        engine.definition(&main, text, line as u32, column)
    };
    let where_method = at(&mut engine, 8, "where");
    assert!(
        where_method[0].path.ends_with("lib/Eloquent/Builder.php"),
        "{where_method:?}"
    );
    let when = at(&mut engine, 9, "when");
    assert!(
        when[0].path.ends_with("lib/Eloquent/Builder.php"),
        "{when:?}"
    );
    let nested_where = at(&mut engine, 9, "where");
    assert!(
        nested_where[0].path.ends_with("lib/Eloquent/Builder.php"),
        "{nested_where:?}"
    );
}

/// The mock projects below have no `artisan` script or Laravel
/// requirement, so Eloquent forwarding is registered by hand.
fn eloquent() -> Vec<Arc<dyn Framework>> {
    vec![Arc::new(framework::laravel::LaravelFramework::new(None))]
}

/// A small copy of how Eloquent declares relations.
fn eloquent_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, body) in [
        (
            "composer.json",
            r#"{"autoload":{"psr-4":{"Lib\\":"lib/","App\\":"app/"}}}"#,
        ),
        (
            "lib/Model.php",
            r#"<?php
namespace Lib;

class Model
{
    use HasRelationships;
    public function __get($key) { return null; }
    public function save(): bool { return true; }
}
"#,
        ),
        (
            "lib/HasRelationships.php",
            r#"<?php
namespace Lib;

trait HasRelationships
{
    /**
     * @template TRelatedModel of \Lib\Model
     * @param  class-string<TRelatedModel>  $related
     * @return \Lib\HasOne<TRelatedModel, $this>
     */
    public function hasOne($related) {}

    /**
     * @template TRelatedModel of \Lib\Model
     * @param  class-string<TRelatedModel>  $related
     * @return \Lib\HasMany<TRelatedModel, $this>
     */
    public function hasMany($related) {}
}
"#,
        ),
        (
            "lib/Relation.php",
            r#"<?php
namespace Lib;

/**
 * @template TRelatedModel of \Lib\Model
 * @template TDeclaringModel of \Lib\Model
 * @template TResult
 */
abstract class Relation
{
    /** @return TResult */
    abstract public function getResults();
}
"#,
        ),
        (
            "lib/HasOneOrMany.php",
            r#"<?php
namespace Lib;

/**
 * @template TRelatedModel of \Lib\Model
 * @template TDeclaringModel of \Lib\Model
 * @template TResult
 *
 * @extends \Lib\Relation<TRelatedModel, TDeclaringModel, TResult>
 */
abstract class HasOneOrMany extends Relation
{
    /** @return TRelatedModel */
    public function create(array $attributes = []) {}

    /** @return $this */
    public function latestOfMany() { return $this; }
}
"#,
        ),
        (
            "lib/HasOne.php",
            r#"<?php
namespace Lib;

/**
 * @template TRelatedModel of \Lib\Model
 * @template TDeclaringModel of \Lib\Model
 *
 * @extends \Lib\HasOneOrMany<TRelatedModel, TDeclaringModel, ?TRelatedModel>
 */
class HasOne extends HasOneOrMany
{
    /** @inheritDoc */
    public function getResults() {}
}
"#,
        ),
        (
            "lib/HasMany.php",
            r#"<?php
namespace Lib;

/**
 * @template TRelatedModel of \Lib\Model
 * @template TDeclaringModel of \Lib\Model
 *
 * @extends \Lib\HasOneOrMany<TRelatedModel, TDeclaringModel, \Lib\Collection<int, TRelatedModel>>
 */
class HasMany extends HasOneOrMany
{
    /** @inheritDoc */
    public function getResults() {}
}
"#,
        ),
        (
            "lib/Collection.php",
            r#"<?php
namespace Lib;

/**
 * @template TKey of array-key
 * @template TValue
 *
 * @implements \IteratorAggregate<TKey, TValue>
 */
class Collection implements \IteratorAggregate
{
    public function getIterator(): \Traversable {}
}
"#,
        ),
        (
            "app/Order.php",
            r#"<?php
namespace App;

use Lib\HasMany;
use Lib\HasOne;
use Lib\Model;

class Order extends Model
{
    public function payment(): HasOne
    {
        return $this->hasOne(Payment::class)->latestOfMany();
    }

    public function lines(): HasMany
    {
        return $this->hasMany(Line::class);
    }
}
"#,
        ),
        (
            "app/Payment.php",
            "<?php\nnamespace App;\n\nuse Lib\\Model;\n\nclass Payment extends Model\n{\n    public function details(): \\Lib\\HasMany\n    {\n        return $this->hasMany(Line::class);\n    }\n}\n",
        ),
        (
            "app/Line.php",
            "<?php\nnamespace App;\n\nuse Lib\\Model;\n\nclass Line extends Model\n{\n    public function product(): string { return ''; }\n}\n",
        ),
    ] {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    dir
}

#[test]
fn follows_relations_through_generics() {
    let dir = eloquent_project();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("app/Service.php");

    // `create()` on a relation gives the related model.
    let text = "<?php\nnamespace App;\nclass Service {\n    public function run(Order $order) {\n        $payment = $order->payment()->create([]);\n        $payment->\n    }\n}\n";
    let items = engine.complete(&file, text, 5, 18);
    let names = labels(&items);
    assert!(names.contains(&"details"), "{names:?}");
    assert!(names.contains(&"save"), "{names:?}");
    assert!(!names.contains(&"product"), "{names:?}");

    // Next call through the new model's own relation.
    let text = "<?php\nnamespace App;\nclass Service {\n    public function run(Order $order) {\n        $payment = $order->payment()->create([]);\n        $payment->details()->create([])->\n    }\n}\n";
    let items = engine.complete(&file, text, 5, 50);
    assert!(labels(&items).contains(&"product"), "{:?}", labels(&items));

    // `foreach` over a has-many read gives the related model.
    let text = "<?php\nnamespace App;\nclass Service {\n    public function run(Order $order) {\n        foreach ($order->lines as $line) {\n            $line->\n    }\n}\n";
    let items = engine.complete(&file, text, 5, 19);
    assert!(labels(&items).contains(&"product"), "{:?}", labels(&items));

    // Clicking `create` on the chain reaches the declaration.
    let text = "<?php\nnamespace App;\nclass Service {\n    public function run(Order $order) {\n        $order->payment()->create([]);\n    }\n}\n";
    let found = engine.definition(&file, text, 4, 28);
    assert_eq!(found.len(), 1);
    assert!(
        found[0].path.ends_with("lib/HasOneOrMany.php"),
        "{:?}",
        found[0].path
    );
}

#[test]
fn laravel_dotted_relation_strings_follow_each_model() {
    let dir = eloquent_project();
    std::fs::write(dir.path().join("artisan"), "#!/usr/bin/env php\n").unwrap();
    std::fs::create_dir_all(dir.path().join("bootstrap")).unwrap();
    std::fs::write(dir.path().join("bootstrap/app.php"), "<?php").unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("app/Service.php");
    let text = "<?php\nnamespace App;\nclass Service {\n    public function run() {\n        Order::with('payment.details');\n    }\n}\n";

    let payment = engine.definition(&file, text, 4, 22);
    assert_eq!(payment.len(), 1);
    assert!(payment[0].path.ends_with("app/Order.php"), "{payment:?}");

    let details = engine.definition(&file, text, 4, 30);
    assert_eq!(details.len(), 1);
    assert!(details[0].path.ends_with("app/Payment.php"), "{details:?}");
}

#[test]
fn laravel_relation_arrays_resolve_on_multiline_chains() {
    let dir = eloquent_project();
    std::fs::write(dir.path().join("artisan"), "#!/usr/bin/env php\n").unwrap();
    std::fs::create_dir_all(dir.path().join("bootstrap")).unwrap();
    std::fs::write(dir.path().join("bootstrap/app.php"), "<?php").unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("app/Service.php");
    let text = "<?php\nnamespace App;\nclass Service {\n    public function run() {\n        $orders = Order::select([\n            'id',\n            'name',\n        ])\n            ->with([\n                'lines',\n                'payment',\n            ])\n            ->get();\n    }\n}\n";

    let payment = engine.definition(&file, text, 10, 20);
    assert_eq!(payment.len(), 1, "{payment:?}");
    assert!(payment[0].path.ends_with("app/Order.php"), "{payment:?}");

    let lines = engine.definition(&file, text, 9, 20);
    assert_eq!(lines.len(), 1, "{lines:?}");
}

#[test]
fn laravel_relation_array_on_one_line_resolves() {
    let dir = eloquent_project();
    std::fs::write(dir.path().join("artisan"), "#!/usr/bin/env php\n").unwrap();
    std::fs::create_dir_all(dir.path().join("bootstrap")).unwrap();
    std::fs::write(dir.path().join("bootstrap/app.php"), "<?php").unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("app/Service.php");
    let text = "<?php\nnamespace App;\nclass Service {\n    public function run() {\n        Order::with(['payment'])->where('id', 1);\n    }\n}\n";
    let found = engine.definition(&file, text, 4, 24);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].path.ends_with("app/Order.php"), "{found:?}");
}

#[test]
fn laravel_relations_without_return_types_resolve() {
    let dir = eloquent_project();
    std::fs::write(dir.path().join("artisan"), "#!/usr/bin/env php\n").unwrap();
    std::fs::create_dir_all(dir.path().join("bootstrap")).unwrap();
    std::fs::write(dir.path().join("bootstrap/app.php"), "<?php").unwrap();
    std::fs::write(
        dir.path().join("app/Untyped.php"),
        "<?php\nnamespace App;\nclass Untyped extends \\Lib\\Model\n{\n    public function payment()\n    {\n        return $this->hasOne(Payment::class, 'untyped_id', 'id')\n            ->withDefault(['x' => 1]);\n    }\n}\n",
    )
    .unwrap();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("app/Service.php");
    let text = "<?php\nnamespace App;\nclass Service {\n    public function run() {\n        Untyped::with(['payment'])->get();\n        Untyped::with('payment.details');\n    }\n}\n";

    let found = engine.definition(&file, text, 4, 24);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].path.ends_with("app/Untyped.php"), "{found:?}");

    let details = engine.definition(&file, text, 5, 33);
    assert_eq!(details.len(), 1, "{details:?}");
    assert!(details[0].path.ends_with("app/Payment.php"), "{details:?}");
}

#[test]
fn laravel_relation_strings_resolve_through_builder_chains() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, body) in [
        (
            "composer.json",
            r#"{"autoload":{"psr-4":{"Illuminate\\Database\\Eloquent\\":"lib/","App\\":"app/"}}}"#,
        ),
        ("artisan", "#!/usr/bin/env php\n"),
        ("bootstrap/app.php", "<?php"),
        (
            "lib/Model.php",
            r#"<?php
namespace Illuminate\Database\Eloquent;

abstract class Model
{
    /** @return \Illuminate\Database\Eloquent\Builder<static> */
    public static function query() {}
}
"#,
        ),
        (
            "lib/Builder.php",
            r#"<?php
namespace Illuminate\Database\Eloquent;

/**
 * @template TModel of \Illuminate\Database\Eloquent\Model
 */
class Builder
{
    /** @return $this */
    public function when($value, $callback) { return $this; }
    /** @return $this */
    public function where($column) { return $this; }
    /** @return $this */
    public function select($columns) { return $this; }
    /** @return $this */
    public function with($relations) { return $this; }
    /** @return $this */
    public function whereHas($relation, $callback = null) { return $this; }
}
"#,
        ),
        (
            "app/Product.php",
            "<?php\nnamespace App;\n\nuse Illuminate\\Database\\Eloquent\\Model;\n\nclass Product extends Model\n{\n}\n",
        ),
        (
            "app/Line.php",
            "<?php\nnamespace App;\n\nuse Illuminate\\Database\\Eloquent\\Model;\n\nclass Line extends Model\n{\n    public function product()\n    {\n        return $this->belongsTo(Product::class, 'product_id', 'id');\n    }\n}\n",
        ),
        (
            "app/Order.php",
            "<?php\nnamespace App;\n\nuse Illuminate\\Database\\Eloquent\\Model;\n\nclass Order extends Model\n{\n    public function lines()\n    {\n        return $this->hasMany(Line::class, 'order_id', 'id');\n    }\n\n    public function activeLines()\n    {\n        return $this->lines()->where('active', 1);\n    }\n}\n",
        ),
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let file = root.join("app/Service.php");
    let text = r#"<?php
namespace App;

use Illuminate\Database\Eloquent\Builder;

class Service
{
    public function run($x)
    {
        $base = function () {
            return Order::query()
                ->where('id', 1);
        };
        $a = Order::query()
            ->when($x, function (Builder $query) {
                $query->whereHas('activeLines', function ($query) {
                    $query->where('id', 1);
                });
            });
        $a->select(['id'])
            ->with([
                'lines',
                'lines.product',
            ]);
        $base()->select(['id'])
            ->with([
                'activeLines',
            ]);
    }
}
"#;
    let mut engine = PhpEngine::new(root);
    let mut at = |needle: &str, nth: usize| {
        let (row, line) = text
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(needle))
            .nth(nth)
            .unwrap();
        let column = line.find(needle).unwrap() as u32 + 2;
        engine.definition(&file, text, row as u32, column)
    };

    // `Builder $query` names the class; the call it is passed to names the model.
    let in_callback = at("'activeLines'", 0);
    assert!(
        in_callback
            .iter()
            .any(|t| t.path.ends_with("app/Order.php")),
        "{in_callback:?}"
    );
    // A builder kept in a variable, with the array on lines of its own.
    let in_variable = at("'lines',", 0);
    assert!(
        in_variable
            .iter()
            .any(|t| t.path.ends_with("app/Order.php")),
        "{in_variable:?}"
    );
    let next_segment = at("product'", 0);
    assert!(
        next_segment
            .iter()
            .any(|t| t.path.ends_with("app/Line.php")),
        "{next_segment:?}"
    );
    // A builder returned by a closure held in a variable, and a relation
    // that is built on another one.
    let in_closure = at("'activeLines'", 1);
    assert_eq!(in_closure.len(), 1, "{in_closure:?}");
    assert!(
        in_closure[0].path.ends_with("app/Order.php"),
        "{in_closure:?}"
    );
}

#[test]
fn variables_are_not_mistaken_for_classes_of_the_same_name() {
    let dir = eloquent_project();
    let mut engine = PhpEngine::new(dir.path());
    let file = dir.path().join("app/Service.php");
    // `$line` has the short name of the class `App\Line`.
    let text = "<?php\nnamespace App;\nclass Service {\n    public function run(Order $order) {\n        foreach ($order->lines as $line) {\n            $line->product();\n        }\n    }\n}\n";
    let hover = engine.hover(&file, text, 5, 14).expect("hover on $line");
    assert!(
        hover.markdown.contains("$line: App\\Line"),
        "{}",
        hover.markdown
    );
    assert_eq!((hover.start_column, hover.end_column), (12, 17));
    assert!(engine.definition(&file, text, 5, 14).is_empty());
    // The method called on the iterated model resolves.
    let found = engine.definition(&file, text, 5, 22);
    assert_eq!(found.len(), 1);
    assert!(
        found[0].path.ends_with("app/Line.php"),
        "{:?}",
        found[0].path
    );
}

#[test]
fn attribute_reads_point_at_the_fillable_entry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("composer.json"),
        r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(
            root.join("app/Item.php"),
            "<?php\nnamespace App;\n\nclass Item\n{\n    protected $fillable = [\n        'name',\n        'price',\n    ];\n\n    public function __get($key) { return null; }\n}\n",
        )
        .unwrap();
    let mut engine = PhpEngine::new(root);
    let file = root.join("main.php");
    let text = "<?php\nuse App\\Item;\nfunction show(Item $item) {\n    return $item->price;\n}\n";
    let found = engine.definition(&file, text, 3, 20);
    assert_eq!(found.len(), 1);
    assert!(found[0].path.ends_with("app/Item.php"));
    assert_eq!((found[0].line, found[0].column), (7, 9));
    let items = engine.complete(
        &file,
        "<?php\nuse App\\Item;\nfunction show(Item $item) {\n    return $item->\n}\n",
        3,
        18,
    );
    assert!(labels(&items).contains(&"name"), "{:?}", labels(&items));
}
