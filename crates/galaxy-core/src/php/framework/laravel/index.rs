//! What a Laravel project defines, read from its files: config keys,
//! environment variables, route names, views, translations, middleware
//! aliases, abilities, container bindings, Livewire and Blade components,
//! Inertia pages and validation rules. PHP never runs, so a value made at
//! run time (a route registered by a package, say) is not in here.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

pub use crate::php::framework::common::files::{Item, Loc};
use crate::php::framework::common::files::{
    at, close_of, file_loc, flatten_config, read, resolve, stem, uses, walk,
};

/// A database table and the columns migrations give it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Table {
    pub name: String,
    pub columns: Vec<Item>,
}

#[derive(Debug, Default)]
pub struct Index {
    pub root: PathBuf,
    pub config: Vec<Item>,
    pub env: Vec<Item>,
    pub routes: Vec<Item>,
    pub views: Vec<Item>,
    pub translations: Vec<Item>,
    pub middleware: Vec<Item>,
    pub abilities: Vec<Item>,
    pub bindings: Vec<Item>,
    pub livewire: Vec<Item>,
    pub components: Vec<Item>,
    pub pages: Vec<Item>,
    pub rules: Vec<Item>,
    pub tables: Vec<Table>,
    /// First segment of config keys whose file exists in `config/`.
    pub config_files: HashSet<String>,
    /// Group names (`messages` for `lang/en/messages.php`).
    pub translation_groups: HashSet<String>,
    pub has_views: bool,
    pub has_pages: bool,
    pub has_env: bool,
    pub has_middleware_source: bool,
}

/// `a/b/c.blade.php` below `base` as `a.b.c`.
fn dotted(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?.to_str()?;
    let rel = rel
        .strip_suffix(".blade.php")
        .or_else(|| rel.strip_suffix(".php"))?;
    Some(rel.replace(['/', '\\'], "."))
}

fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with(['-', '.']) {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

impl Index {
    /// The index of the Laravel application at `root`; `None` for any other
    /// project (without Laravel's application structure).
    pub fn detect(root: &Path) -> Option<Self> {
        (root.join("artisan").is_file() && root.join("bootstrap/app.php").is_file())
            .then(|| Self::build(root))
    }

    pub fn build(root: &Path) -> Self {
        let mut index = Index {
            root: root.to_path_buf(),
            ..Index::default()
        };
        index.read_config();
        index.read_env();
        index.read_routes();
        index.read_views();
        index.read_translations();
        index.read_middleware();
        index.read_providers();
        index.read_classes();
        index.read_pages();
        index.rules = builtin_rules();
        index.read_custom_rules();
        index.read_migrations();
        index
    }

    fn read_config(&mut self) {
        for path in walk(&self.root.join("config")) {
            if path.extension().and_then(|e| e.to_str()) != Some("php") {
                continue;
            }
            let Some(text) = read(&path) else { continue };
            let file = stem(&path);
            self.config_files.insert(file.clone());
            self.config.push(Item {
                name: file.clone(),
                detail: String::new(),
                loc: file_loc(&path),
                class: None,
            });
            for entry in flatten_config(&text) {
                self.config.push(Item {
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

    fn read_env(&mut self) {
        static LINE: OnceLock<Regex> = OnceLock::new();
        let re = LINE.get_or_init(|| {
            Regex::new(r#"^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_.]*)\s*=\s*(.*?)\s*$"#)
                .expect("valid")
        });
        let mut seen = HashSet::new();
        for file in [".env", ".env.example"] {
            let path = self.root.join(file);
            let Some(text) = read(&path) else { continue };
            self.has_env = true;
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
                let secret = [
                    "PASSWORD",
                    "SECRET",
                    "KEY",
                    "TOKEN",
                    "PRIVATE",
                    "CREDENTIAL",
                ]
                .iter()
                .any(|word| key.to_ascii_uppercase().contains(word));
                self.env.push(Item {
                    detail: if secret && file == ".env" && !value.is_empty() {
                        "••••••".into()
                    } else {
                        value
                    },
                    name: key,
                    loc: Some(Loc {
                        path: path.clone(),
                        line: row as u32,
                        column: 0,
                    }),
                    class: None,
                });
            }
        }
    }

    fn read_routes(&mut self) {
        self.routes = routes_in(&self.root);
    }

    /// Where the route called `name` is defined in the Laravel application
    /// at `root`, read straight from `routes/*.php` so callers outside the
    /// PHP engine (a Vue file calling Ziggy's `route()`) need no full index.
    /// `None` for any other project or an unknown name.
    pub fn find_route(root: &Path, name: &str) -> Option<Item> {
        if !root.join("artisan").is_file() {
            return None;
        }
        routes_in(root).into_iter().find(|item| item.name == name)
    }

    fn read_views(&mut self) {
        let base = self.root.join("resources/views");
        self.has_views = base.is_dir();
        for path in walk(&base) {
            let Some(name) = dotted(&base, &path) else {
                continue;
            };
            let components = base.join("components");
            if let Some(component) = dotted(&components, &path)
                && path.starts_with(&components)
            {
                let component = component
                    .strip_suffix(".index")
                    .map_or(component.clone(), str::to_owned);
                self.components.push(Item {
                    name: component,
                    detail: "Blade component".into(),
                    loc: file_loc(&path),
                    class: None,
                });
            }
            self.views.push(Item {
                name,
                detail: path
                    .strip_prefix(&self.root)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                loc: file_loc(&path),
                class: None,
            });
        }
    }

    fn read_translations(&mut self) {
        static JSON_KEY: OnceLock<Regex> = OnceLock::new();
        let key_re = JSON_KEY.get_or_init(|| {
            Regex::new(r#"^\s*"((?:[^"\\]|\\.)*)"\s*:\s*"((?:[^"\\]|\\.)*)""#).expect("valid")
        });
        let mut seen = HashSet::new();
        for lang in [self.root.join("lang"), self.root.join("resources/lang")] {
            let Ok(entries) = fs::read_dir(&lang) else {
                continue;
            };
            let mut entries: Vec<_> = entries.flatten().collect();
            // The default locale first, so its text is the one shown.
            entries.sort_by_key(|e| (e.file_name() != "en", e.file_name()));
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    for file in walk(&path) {
                        if file.extension().and_then(|e| e.to_str()) != Some("php") {
                            continue;
                        }
                        let Some(text) = read(&file) else { continue };
                        let group = stem(&file);
                        self.translation_groups.insert(group.clone());
                        for entry in flatten_config(&text) {
                            let name = format!("{group}.{}", entry.key);
                            if seen.insert(name.clone()) {
                                self.translations.push(Item {
                                    name,
                                    detail: entry.value,
                                    loc: Some(Loc {
                                        path: file.clone(),
                                        line: entry.line as u32,
                                        column: entry.column_utf16 as u32,
                                    }),
                                    class: None,
                                });
                            }
                        }
                    }
                } else if path.extension().and_then(|e| e.to_str()) == Some("json")
                    && let Some(text) = read(&path)
                {
                    for (row, line) in text.lines().enumerate() {
                        if let Some(caps) = key_re.captures(line) {
                            let name = caps[1].replace("\\\"", "\"");
                            if seen.insert(name.clone()) {
                                self.translations.push(Item {
                                    name,
                                    detail: caps[2].to_owned(),
                                    loc: Some(Loc {
                                        path: path.clone(),
                                        line: row as u32,
                                        column: line.find('"').unwrap_or(0) as u32,
                                    }),
                                    class: None,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    fn read_middleware(&mut self) {
        static ENTRY: OnceLock<Regex> = OnceLock::new();
        let entry = ENTRY.get_or_init(|| {
            Regex::new(r#"['"]([\w.\-]+)['"]\s*=>\s*\\?([\w\\]+)::class"#).expect("valid")
        });
        for (name, class, detail) in BUILTIN_MIDDLEWARE {
            self.middleware.push(Item {
                name: (*name).into(),
                detail: (*detail).into(),
                loc: None,
                class: (!class.is_empty()).then(|| (*class).into()),
            });
        }
        for name in ["web", "api"] {
            self.middleware.push(Item {
                name: name.into(),
                detail: "Middleware group".into(),
                ..Item::default()
            });
        }
        let sources = [
            (self.root.join("bootstrap/app.php"), vec!["alias("]),
            (
                self.root.join("app/Http/Kernel.php"),
                vec!["$middlewareAliases", "$routeMiddleware"],
            ),
        ];
        for (path, markers) in sources {
            let Some(text) = read(&path) else { continue };
            let used = uses(&text);
            for marker in markers {
                let mut from = 0;
                while let Some(found) = text[from..].find(marker) {
                    let start = from + found;
                    from = start + marker.len();
                    let Some(open) = text[start..].find('[').map(|i| start + i) else {
                        break;
                    };
                    let close = close_of(&text, open);
                    for caps in entry.captures_iter(&text[open..close]) {
                        self.has_middleware_source = true;
                        let class = resolve(&caps[2], &used);
                        let name = caps.get(1).expect("group");
                        self.middleware.retain(|m| m.name != name.as_str());
                        self.middleware.push(Item {
                            name: name.as_str().to_owned(),
                            detail: class.clone(),
                            loc: at(&path, &text, open + name.start()),
                            class: Some(class),
                        });
                    }
                    from = close.max(from);
                }
            }
        }
    }

    fn read_providers(&mut self) {
        static BIND: OnceLock<Regex> = OnceLock::new();
        static GATE: OnceLock<Regex> = OnceLock::new();
        let bind = BIND.get_or_init(|| {
            Regex::new(
                r#"->(?:bind|singleton|scoped|instance|alias|bindIf|singletonIf)\(\s*(?:['"]([^'"]+)['"]|\\?([\w\\]+)::class)"#,
            )
            .expect("valid")
        });
        let gate =
            GATE.get_or_init(|| Regex::new(r#"Gate::define\(\s*['"]([^'"]+)['"]"#).expect("valid"));
        for (name, detail) in BUILTIN_BINDINGS {
            self.bindings.push(Item {
                name: (*name).into(),
                detail: (*detail).into(),
                ..Item::default()
            });
        }
        for ability in [
            "viewAny",
            "view",
            "create",
            "update",
            "delete",
            "restore",
            "forceDelete",
        ] {
            self.abilities.push(Item {
                name: ability.into(),
                detail: "Policy ability".into(),
                ..Item::default()
            });
        }
        for path in walk(&self.root.join("app/Providers")) {
            let Some(text) = read(&path) else { continue };
            let used = uses(&text);
            for caps in bind.captures_iter(&text) {
                let (name, whole) = match (caps.get(1), caps.get(2)) {
                    (Some(m), _) => (m.as_str().to_owned(), m),
                    (None, Some(m)) => (resolve(m.as_str(), &used), m),
                    _ => continue,
                };
                self.bindings.push(Item {
                    class: name.contains('\\').then(|| name.clone()),
                    name,
                    detail: "Container binding".into(),
                    loc: at(&path, &text, whole.start()),
                });
            }
            for caps in gate.captures_iter(&text) {
                let name = caps.get(1).expect("group");
                self.abilities.push(Item {
                    name: name.as_str().to_owned(),
                    detail: "Gate".into(),
                    loc: at(&path, &text, name.start()),
                    class: None,
                });
            }
        }
        // Policy methods, which `can('update', $post)` runs.
        static METHOD: OnceLock<Regex> = OnceLock::new();
        let method =
            METHOD.get_or_init(|| Regex::new(r"(?m)^\s*public function (\w+)\(").expect("valid"));
        for path in walk(&self.root.join("app/Policies")) {
            let Some(text) = read(&path) else { continue };
            for caps in method.captures_iter(&text) {
                let name = caps.get(1).expect("group");
                if name.as_str().starts_with("__")
                    || self.abilities.iter().any(|a| a.name == name.as_str())
                {
                    continue;
                }
                self.abilities.push(Item {
                    name: name.as_str().to_owned(),
                    detail: format!("{} policy", stem(&path).trim_end_matches("Policy")),
                    loc: at(&path, &text, name.start()),
                    class: None,
                });
            }
        }
    }

    fn read_classes(&mut self) {
        for (dir, namespace) in [
            ("app/Livewire", "App\\Livewire"),
            ("app/Http/Livewire", "App\\Http\\Livewire"),
        ] {
            let base = self.root.join(dir);
            for path in walk(&base) {
                let Some(rel) = dotted(&base, &path) else {
                    continue;
                };
                let class = format!("{namespace}\\{}", rel.replace('.', "\\"));
                self.livewire.push(Item {
                    name: kebab(&rel),
                    detail: class.clone(),
                    loc: file_loc(&path),
                    class: Some(class),
                });
            }
        }
        let base = self.root.join("app/View/Components");
        for path in walk(&base) {
            let Some(rel) = dotted(&base, &path) else {
                continue;
            };
            let class = format!("App\\View\\Components\\{}", rel.replace('.', "\\"));
            self.components.push(Item {
                name: kebab(&rel),
                detail: class.clone(),
                loc: file_loc(&path),
                class: Some(class),
            });
        }
    }

    fn read_pages(&mut self) {
        for dir in [
            "resources/js/Pages",
            "resources/js/pages",
            "resources/ts/Pages",
        ] {
            let base = self.root.join(dir);
            if !base.is_dir() {
                continue;
            }
            self.has_pages = true;
            for path in walk(&base) {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !["vue", "jsx", "tsx", "js", "ts", "svelte"].contains(&ext) {
                    continue;
                }
                let Ok(rel) = path.strip_prefix(&base) else {
                    continue;
                };
                let name = rel.with_extension("").to_string_lossy().replace('\\', "/");
                // `Pages` and `pages` are one folder on a case-insensitive disk.
                if self.pages.iter().any(|page| page.name == name) {
                    continue;
                }
                self.pages.push(Item {
                    name,
                    detail: path
                        .strip_prefix(&self.root)
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    loc: file_loc(&path),
                    class: None,
                });
            }
        }
    }

    fn read_custom_rules(&mut self) {
        static EXTEND: OnceLock<Regex> = OnceLock::new();
        let re = EXTEND.get_or_init(|| {
            Regex::new(r#"Validator::(?:extend|extendImplicit)\(\s*['"]([\w]+)['"]"#)
                .expect("valid")
        });
        for path in walk(&self.root.join("app/Providers")) {
            let Some(text) = read(&path) else { continue };
            for caps in re.captures_iter(&text) {
                let name = caps.get(1).expect("group");
                self.rules.push(Item {
                    name: name.as_str().to_owned(),
                    detail: "Custom rule".into(),
                    loc: at(&path, &text, name.start()),
                    class: None,
                });
            }
        }
    }

    fn read_migrations(&mut self) {
        static TABLE: OnceLock<Regex> = OnceLock::new();
        static CALL: OnceLock<Regex> = OnceLock::new();
        let table_re = TABLE.get_or_init(|| {
            Regex::new(r#"Schema::(create|table)\(\s*['"](\w+)['"]"#).expect("valid")
        });
        let call_re = CALL
            .get_or_init(|| Regex::new(r#"\$\w+->(\w+)\(\s*(?:['"](\w+)['"])?"#).expect("valid"));
        let mut files: Vec<PathBuf> = walk(&self.root.join("database/migrations"))
            .into_iter()
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("php"))
            .collect();
        files.sort();
        for path in files {
            let Some(text) = read(&path) else { continue };
            for caps in table_re.captures_iter(&text) {
                let name = caps[2].to_owned();
                let after = caps.get(0).expect("match").end();
                let Some(open) = text[after..].find('{').map(|i| after + i) else {
                    continue;
                };
                let close = close_of(&text, open);
                let body = &text[open..close];
                let position = self.tables.iter().position(|t| t.name == name);
                let table = match position {
                    Some(i) => &mut self.tables[i],
                    None => {
                        self.tables.push(Table {
                            name: name.clone(),
                            columns: Vec::new(),
                        });
                        self.tables.last_mut().expect("pushed")
                    }
                };
                for call in call_re.captures_iter(body) {
                    let method = &call[1];
                    let arg = call.get(2).map(|m| m.as_str());
                    let byte = open
                        + call
                            .get(2)
                            .map_or(call.get(0).expect("match").start(), |m| m.start());
                    let mut add = |column: &str, kind: &str| {
                        if !table.columns.iter().any(|c| c.name == column) {
                            table.columns.push(Item {
                                name: column.to_owned(),
                                detail: kind.to_owned(),
                                loc: at(&path, &text, byte),
                                class: None,
                            });
                        }
                    };
                    match (method, arg) {
                        (
                            "id" | "bigIncrements" | "increments" | "tinyIncrements"
                            | "smallIncrements" | "mediumIncrements",
                            arg,
                        ) => {
                            add(arg.unwrap_or("id"), "id");
                        }
                        ("timestamps" | "timestampsTz" | "nullableTimestamps", _) => {
                            add("created_at", "timestamp");
                            add("updated_at", "timestamp");
                        }
                        ("softDeletes" | "softDeletesTz", arg) => {
                            add(arg.unwrap_or("deleted_at"), "timestamp");
                        }
                        ("rememberToken", _) => add("remember_token", "string"),
                        (
                            "morphs" | "nullableMorphs" | "uuidMorphs" | "nullableUuidMorphs"
                            | "ulidMorphs" | "nullableUlidMorphs",
                            Some(arg),
                        ) => {
                            add(&format!("{arg}_id"), "morph");
                            add(&format!("{arg}_type"), "morph");
                        }
                        ("dropColumn", Some(arg)) => {
                            table.columns.retain(|c| c.name != arg);
                        }
                        ("dropTimestamps", _) => {
                            table
                                .columns
                                .retain(|c| c.name != "created_at" && c.name != "updated_at");
                        }
                        ("dropSoftDeletes", _) => {
                            table.columns.retain(|c| c.name != "deleted_at");
                        }
                        ("renameColumn", Some(from)) => {
                            let to = body[call.get(0).expect("match").end()..]
                                .trim_start_matches([' ', ',', '\n'])
                                .trim_start_matches(['\'', '"'])
                                .split(['\'', '"'])
                                .next()
                                .unwrap_or("")
                                .to_owned();
                            if !to.is_empty()
                                && let Some(column) =
                                    table.columns.iter_mut().find(|c| c.name == from)
                            {
                                column.name = to;
                            }
                        }
                        (kind, Some(arg)) if COLUMN_TYPES.contains(&kind) => add(arg, kind),
                        _ => {}
                    }
                }
            }
        }
    }

    /// Columns of `table`, empty for a table no migration creates.
    pub fn columns_of(&self, table: &str) -> Vec<Item> {
        self.tables
            .iter()
            .find(|t| t.name == table)
            .map(|t| t.columns.clone())
            .unwrap_or_default()
    }

    /// Keys one level below `section` (`filesystems.disks`).
    pub fn section(&self, section: &str) -> Vec<Item> {
        let prefix = format!("{section}.");
        self.config
            .iter()
            .filter(|item| {
                item.name
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| !rest.is_empty() && !rest.contains('.'))
            })
            .map(|item| Item {
                name: item.name[prefix.len()..].to_owned(),
                ..item.clone()
            })
            .collect()
    }

    /// Entries of the folder `base` below the project, one level at a time:
    /// `typed` is the part already written, such as `Models/`.
    pub fn paths(&self, base: &str, typed: &str) -> Vec<Item> {
        let dir_part = typed.rsplit_once('/').map_or("", |(dir, _)| dir);
        let dir = self.root.join(base).join(dir_part);
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<Item> = entries
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| {
                let mut name = e.file_name().to_string_lossy().into_owned();
                let is_dir = e.path().is_dir();
                if !dir_part.is_empty() {
                    name = format!("{dir_part}/{name}");
                }
                if is_dir {
                    name.push('/');
                }
                Item {
                    name,
                    detail: if is_dir {
                        "Folder".into()
                    } else {
                        String::new()
                    },
                    loc: file_loc(&e.path()),
                    class: None,
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// The project file `typed` below `base`, if it exists.
    pub fn path_exists(&self, base: &str, typed: &str) -> bool {
        self.root
            .join(base)
            .join(typed.trim_start_matches('/'))
            .exists()
    }

    pub fn find<'a>(items: &'a [Item], name: &str) -> Option<&'a Item> {
        items.iter().find(|item| item.name == name)
    }
}

/// Every named route under `root/routes`.
fn routes_in(root: &Path) -> Vec<Item> {
    let mut routes = Vec::new();
    for path in walk(&root.join("routes")) {
        if path.extension().and_then(|e| e.to_str()) == Some("php")
            && let Some(text) = read(&path)
        {
            scan_routes(&path, &text, &mut routes);
        }
    }
    routes
}

/// Route names with the method and path they answer, from one routes file.
fn scan_routes(path: &Path, text: &str, out: &mut Vec<Item>) {
    static NAME: OnceLock<Regex> = OnceLock::new();
    static ROUTE: OnceLock<Regex> = OnceLock::new();
    static RESOURCE: OnceLock<Regex> = OnceLock::new();
    static GROUP: OnceLock<Regex> = OnceLock::new();
    let name_re = NAME
        .get_or_init(|| Regex::new(r#"(?:->|::)name\(\s*['"]([^'"]*)['"]\s*\)"#).expect("valid"));
    let route_re = ROUTE.get_or_init(|| {
        Regex::new(
            r#"Route::(get|post|put|patch|delete|options|any|match|view|redirect)\(\s*(?:\[[^\]]*\]\s*,\s*)?['"]([^'"]*)['"]"#,
        )
        .expect("valid")
    });
    let resource_re = RESOURCE.get_or_init(|| {
        Regex::new(r#"Route::(resource|apiResource|singleton)\(\s*['"]([^'"]+)['"]"#)
            .expect("valid")
    });
    let group_re = GROUP.get_or_init(|| {
        Regex::new(r"^(?:\s*->\w+\((?:[^()]|\([^()]*\))*\))*\s*->group\(").expect("valid")
    });

    // Names that end a prefix group apply to every route inside it.
    let mut prefixes: Vec<(usize, usize, String)> = Vec::new();
    for caps in name_re.captures_iter(text) {
        let whole = caps.get(0).expect("match");
        if let Some(group) = group_re.find(&text[whole.end()..]) {
            let open = whole.end() + group.end() - 1;
            prefixes.push((open, close_of(text, open), caps[1].to_owned()));
        }
    }
    let prefix_at = |byte: usize, own: usize| -> String {
        let mut applying: Vec<&(usize, usize, String)> = prefixes
            .iter()
            .filter(|(open, close, _)| *open < byte && byte < *close && *open != own)
            .collect();
        applying.sort_by_key(|(open, _, _)| *open);
        applying.iter().map(|(_, _, p)| p.as_str()).collect()
    };
    let statement_start =
        |byte: usize| -> usize { text[..byte].rfind([';', '}']).map_or(0, |i| i + 1) };

    for caps in name_re.captures_iter(text) {
        let whole = caps.get(0).expect("match");
        let own = prefixes
            .iter()
            .find(|(open, _, p)| *open > whole.end() && *p == caps[1] && *open - whole.end() < 400)
            .map_or(usize::MAX, |(open, _, _)| *open);
        if own != usize::MAX {
            continue;
        }
        let from = statement_start(whole.start());
        let detail = route_re
            .captures(&text[from..whole.start()])
            .map(|c| {
                format!(
                    "{} /{}",
                    c[1].to_ascii_uppercase(),
                    c[2].trim_start_matches('/')
                )
            })
            .unwrap_or_default();
        let key = caps.get(1).expect("group");
        out.push(Item {
            name: format!("{}{}", prefix_at(whole.start(), usize::MAX), key.as_str()),
            detail,
            loc: at(path, text, key.start()),
            class: None,
        });
    }

    for caps in resource_re.captures_iter(text) {
        let whole = caps.get(0).expect("match");
        let end = text[whole.end()..]
            .find(';')
            .map_or(text.len(), |i| whole.end() + i);
        let chain = &text[whole.end()..end];
        let api = &caps[1] != "resource";
        let mut actions: Vec<&str> = if &caps[1] == "singleton" {
            vec!["show", "edit", "update"]
        } else if api {
            vec!["index", "store", "show", "update", "destroy"]
        } else {
            vec![
                "index", "create", "store", "show", "edit", "update", "destroy",
            ]
        };
        let list = |word: &str| -> Option<Vec<String>> {
            let at = chain.find(&format!("->{word}("))?;
            let open = at + word.len() + 3;
            let close = close_of(chain, open);
            Some(
                chain[open..close]
                    .split(',')
                    .map(|s| s.trim().trim_matches(['[', ']', '\'', '"', ' ']).to_owned())
                    .filter(|s| !s.is_empty())
                    .collect(),
            )
        };
        if let Some(only) = list("only") {
            actions.retain(|a| only.iter().any(|o| o == a));
        }
        if let Some(except) = list("except") {
            actions.retain(|a| !except.iter().any(|o| o == a));
        }
        let base = caps[2].trim_matches('/').replace('/', ".");
        let uri = caps[2].trim_matches('/');
        for action in actions {
            let method = match action {
                "store" => "POST",
                "update" => "PUT|PATCH",
                "destroy" => "DELETE",
                _ => "GET",
            };
            let suffix = match action {
                "create" => "/create",
                "show" | "update" | "destroy" => "/{id}",
                "edit" => "/{id}/edit",
                _ => "",
            };
            out.push(Item {
                name: format!("{}{base}.{action}", prefix_at(whole.start(), usize::MAX)),
                detail: format!("{method} /{uri}{suffix}"),
                loc: at(path, text, whole.start()),
                class: None,
            });
        }
    }
}

/// Blueprint methods that add one column named by their first argument.
const COLUMN_TYPES: &[&str] = &[
    "string",
    "char",
    "text",
    "tinyText",
    "mediumText",
    "longText",
    "integer",
    "tinyInteger",
    "smallInteger",
    "mediumInteger",
    "bigInteger",
    "unsignedInteger",
    "unsignedTinyInteger",
    "unsignedSmallInteger",
    "unsignedMediumInteger",
    "unsignedBigInteger",
    "float",
    "double",
    "decimal",
    "unsignedDecimal",
    "boolean",
    "date",
    "dateTime",
    "dateTimeTz",
    "time",
    "timeTz",
    "timestamp",
    "timestampTz",
    "year",
    "binary",
    "json",
    "jsonb",
    "uuid",
    "ulid",
    "enum",
    "set",
    "ipAddress",
    "macAddress",
    "geometry",
    "geography",
    "point",
    "lineString",
    "polygon",
    "geometryCollection",
    "multiPoint",
    "multiLineString",
    "multiPolygon",
    "vector",
    "foreignId",
    "foreignUuid",
    "foreignUlid",
    "computed",
];

const BUILTIN_MIDDLEWARE: &[(&str, &str, &str)] = &[
    (
        "auth",
        "Illuminate\\Auth\\Middleware\\Authenticate",
        "Requires a logged in user",
    ),
    (
        "auth.basic",
        "Illuminate\\Auth\\Middleware\\AuthenticateWithBasicAuth",
        "HTTP basic auth",
    ),
    (
        "auth.session",
        "Illuminate\\Session\\Middleware\\AuthenticateSession",
        "Ends other sessions on password change",
    ),
    (
        "bindings",
        "Illuminate\\Routing\\Middleware\\SubstituteBindings",
        "Route model binding",
    ),
    (
        "cache.headers",
        "Illuminate\\Http\\Middleware\\SetCacheHeaders",
        "Sets cache headers",
    ),
    (
        "can",
        "Illuminate\\Auth\\Middleware\\Authorize",
        "Authorizes an ability",
    ),
    (
        "guest",
        "Illuminate\\Auth\\Middleware\\RedirectIfAuthenticated",
        "Only for guests",
    ),
    (
        "password.confirm",
        "Illuminate\\Auth\\Middleware\\RequirePassword",
        "Asks for the password again",
    ),
    (
        "precognitive",
        "Illuminate\\Foundation\\Http\\Middleware\\HandlePrecognitiveRequests",
        "Precognitive requests",
    ),
    (
        "signed",
        "Illuminate\\Routing\\Middleware\\ValidateSignature",
        "Requires a signed URL",
    ),
    (
        "throttle",
        "Illuminate\\Routing\\Middleware\\ThrottleRequests",
        "Rate limit: throttle:60,1",
    ),
    (
        "verified",
        "Illuminate\\Auth\\Middleware\\EnsureEmailIsVerified",
        "Requires a verified email",
    ),
];

const BUILTIN_BINDINGS: &[(&str, &str)] = &[
    ("app", "Illuminate\\Foundation\\Application"),
    ("auth", "Illuminate\\Auth\\AuthManager"),
    ("auth.driver", "Illuminate\\Contracts\\Auth\\Guard"),
    (
        "blade.compiler",
        "Illuminate\\View\\Compilers\\BladeCompiler",
    ),
    ("cache", "Illuminate\\Cache\\CacheManager"),
    ("cache.store", "Illuminate\\Cache\\Repository"),
    ("config", "Illuminate\\Config\\Repository"),
    ("cookie", "Illuminate\\Cookie\\CookieJar"),
    ("db", "Illuminate\\Database\\DatabaseManager"),
    ("db.connection", "Illuminate\\Database\\Connection"),
    ("encrypter", "Illuminate\\Encryption\\Encrypter"),
    ("events", "Illuminate\\Events\\Dispatcher"),
    ("files", "Illuminate\\Filesystem\\Filesystem"),
    ("filesystem", "Illuminate\\Filesystem\\FilesystemManager"),
    (
        "filesystem.disk",
        "Illuminate\\Contracts\\Filesystem\\Filesystem",
    ),
    ("hash", "Illuminate\\Hashing\\HashManager"),
    ("log", "Illuminate\\Log\\LogManager"),
    ("mailer", "Illuminate\\Mail\\Mailer"),
    ("queue", "Illuminate\\Queue\\QueueManager"),
    ("redirect", "Illuminate\\Routing\\Redirector"),
    ("redis", "Illuminate\\Redis\\RedisManager"),
    ("request", "Illuminate\\Http\\Request"),
    ("router", "Illuminate\\Routing\\Router"),
    ("session", "Illuminate\\Session\\SessionManager"),
    ("session.store", "Illuminate\\Session\\Store"),
    ("translator", "Illuminate\\Translation\\Translator"),
    ("url", "Illuminate\\Routing\\UrlGenerator"),
    ("validator", "Illuminate\\Validation\\Factory"),
    ("view", "Illuminate\\View\\Factory"),
];

/// Validation rules Laravel ships with, as `name`, usage and meaning.
const RULES: &[(&str, &str)] = &[
    ("accepted", "Must be yes, on, 1 or true"),
    (
        "accepted_if:anotherfield,value,...",
        "Accepted when another field has a value",
    ),
    ("active_url", "Must have a valid A or AAAA record"),
    ("after:date", "Must be after a date"),
    ("after_or_equal:date", "Must be after or equal to a date"),
    ("alpha", "Only letters"),
    ("alpha_dash", "Letters, numbers, dashes and underscores"),
    ("alpha_num", "Letters and numbers"),
    ("array", "Must be an array"),
    ("ascii", "Only 7-bit ASCII characters"),
    ("bail", "Stop at the first failing rule"),
    ("before:date", "Must be before a date"),
    ("before_or_equal:date", "Must be before or equal to a date"),
    ("between:min,max", "Size between min and max"),
    ("boolean", "Must be true, false, 1, 0, \"1\" or \"0\""),
    ("confirmed", "Needs a matching `_confirmation` field"),
    ("current_password", "Must match the user's password"),
    ("date", "A valid date"),
    ("date_equals:date", "Equal to a date"),
    ("date_format:format", "Matches a date format"),
    ("decimal:min,max", "Number with decimal places"),
    ("declined", "Must be no, off, 0 or false"),
    ("different:field", "Differs from another field"),
    ("digits:value", "Integer with an exact length"),
    (
        "digits_between:min,max",
        "Integer length between min and max",
    ),
    ("dimensions", "Image dimensions"),
    ("distinct", "No duplicates in an array"),
    (
        "doesnt_end_with:foo,bar,...",
        "Must not end with one of the values",
    ),
    (
        "doesnt_start_with:foo,bar,...",
        "Must not start with one of the values",
    ),
    ("email", "A valid email address"),
    ("ends_with:foo,bar,...", "Must end with one of the values"),
    ("enum", "Value of an enum"),
    ("exclude", "Leave the field out of validated data"),
    (
        "exclude_if:anotherfield,value",
        "Exclude when another field has a value",
    ),
    (
        "exclude_unless:anotherfield,value",
        "Exclude unless another field has a value",
    ),
    ("exists:table,column", "Must exist in a database table"),
    ("extensions:foo,bar", "File extension"),
    ("file", "An uploaded file"),
    ("filled", "Not empty when present"),
    ("gt:field", "Greater than"),
    ("gte:field", "Greater than or equal"),
    ("image", "An image"),
    ("in:foo,bar,...", "One of the values"),
    ("in_array:anotherfield.*", "Exists in another array field"),
    ("integer", "An integer"),
    ("ip", "An IP address"),
    ("ipv4", "An IPv4 address"),
    ("ipv6", "An IPv6 address"),
    ("json", "A JSON string"),
    ("lt:field", "Less than"),
    ("lte:field", "Less than or equal"),
    ("lowercase", "Lowercase text"),
    ("mac_address", "A MAC address"),
    ("max:value", "At most value"),
    ("max_digits:value", "At most this many digits"),
    ("mimes:foo,bar,...", "File MIME by extension"),
    ("mimetypes:text/plain,...", "File MIME type"),
    ("min:value", "At least value"),
    ("min_digits:value", "At least this many digits"),
    ("missing", "Must not be present"),
    ("multiple_of:value", "A multiple of value"),
    ("not_in:foo,bar,...", "None of the values"),
    ("not_regex:pattern", "Must not match a pattern"),
    ("nullable", "May be null"),
    ("numeric", "A number"),
    ("present", "Must be present, may be empty"),
    ("prohibited", "Must be absent or empty"),
    ("prohibits:anotherfield,...", "Other fields must be absent"),
    ("regex:pattern", "Must match a pattern"),
    ("required", "Must be present and not empty"),
    (
        "required_array_keys:foo,bar,...",
        "Array with the given keys",
    ),
    (
        "required_if:anotherfield,value,...",
        "Required when another field has a value",
    ),
    (
        "required_unless:anotherfield,value,...",
        "Required unless another field has a value",
    ),
    (
        "required_with:foo,bar,...",
        "Required when any other field is present",
    ),
    (
        "required_with_all:foo,bar,...",
        "Required when all other fields are present",
    ),
    (
        "required_without:foo,bar,...",
        "Required when any other field is missing",
    ),
    (
        "required_without_all:foo,bar,...",
        "Required when all other fields are missing",
    ),
    ("same:field", "Matches another field"),
    ("size:value", "Exact size"),
    ("sometimes", "Validate only when present"),
    (
        "starts_with:foo,bar,...",
        "Must start with one of the values",
    ),
    ("string", "A string"),
    ("timezone", "A time zone identifier"),
    ("ulid", "A ULID"),
    ("unique:table,column", "Must not exist in a database table"),
    ("uppercase", "Uppercase text"),
    ("url", "A URL"),
    ("uuid", "A UUID"),
];

fn builtin_rules() -> Vec<Item> {
    RULES
        .iter()
        .map(|(usage, detail)| Item {
            name: (*usage).into(),
            detail: (*detail).into(),
            ..Item::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "artisan", "<?php");
        write(
            r,
            "config/app.php",
            "<?php\nreturn [\n    'name' => env('APP_NAME', 'Laravel'),\n    'nested' => [\n        'deep' => 1,\n    ],\n];\n",
        );
        write(
            r,
            "config/filesystems.php",
            "<?php\nreturn [\n    'disks' => [\n        'local' => ['driver' => 'local'],\n        's3' => ['driver' => 's3'],\n    ],\n];\n",
        );
        write(
            r,
            ".env",
            "APP_NAME=Galaxy\nDB_PASSWORD=hunter2\n# NOTE=x\nAPP_ENV=local\n",
        );
        write(r, ".env.example", "APP_NAME=Laravel\nEXAMPLE_ONLY=1\n");
        write(
            r,
            "routes/web.php",
            "<?php\nuse Illuminate\\Support\\Facades\\Route;\n\nRoute::get('/', fn () => 1)->name('home');\nRoute::name('admin.')->prefix('admin')->group(function () {\n    Route::get('users', fn () => 1)->name('users.index');\n    Route::post('users', fn () => 1)->name('users.store');\n});\nRoute::resource('photos', PhotoController::class)->only(['index', 'show']);\n",
        );
        write(r, "resources/views/welcome.blade.php", "hi");
        write(r, "resources/views/admin/users/index.blade.php", "hi");
        write(
            r,
            "resources/views/components/forms/input.blade.php",
            "<input>",
        );
        write(
            r,
            "lang/en/messages.php",
            "<?php\nreturn [\n    'hello' => 'Hello',\n    'nested' => ['a' => 'A'],\n];\n",
        );
        write(
            r,
            "lang/en.json",
            "{\n    \"Welcome back\": \"Welcome back!\"\n}\n",
        );
        write(
            r,
            "bootstrap/app.php",
            "<?php\nuse App\\Http\\Middleware\\EnsureOwner;\n\nreturn Application::configure()\n    ->withMiddleware(function ($middleware) {\n        $middleware->alias([\n            'role.owner' => EnsureOwner::class,\n        ]);\n    })->create();\n",
        );
        write(
            r,
            "app/Providers/AppServiceProvider.php",
            "<?php\nnamespace App\\Providers;\nuse App\\Services\\Pay;\nclass P {\n    public function register() {\n        $this->app->singleton(Pay::class, fn () => 1);\n        $this->app->bind('pay.fake', fn () => 1);\n        Gate::define('manage-users', fn () => true);\n    }\n}\n",
        );
        write(
            r,
            "app/Policies/PostPolicy.php",
            "<?php\nclass PostPolicy {\n    public function update() {}\n    public function publish() {}\n}\n",
        );
        write(r, "app/Livewire/Admin/UserTable.php", "<?php");
        write(r, "resources/js/Pages/Users/Index.vue", "<template/>");
        dir
    }

    fn names(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.name.as_str()).collect()
    }

    #[test]
    fn reads_config_env_and_views() {
        let dir = project();
        let index = Index::build(dir.path());
        let config = names(&index.config);
        assert!(config.contains(&"app.name") && config.contains(&"app.nested.deep"));
        assert_eq!(
            Index::find(&index.config, "app.name").map(|i| i.detail.as_str()),
            Some("env('APP_NAME', 'Laravel')")
        );
        assert!(index.config_files.contains("filesystems"));
        assert_eq!(
            names(&index.section("filesystems.disks")),
            vec!["local", "s3"]
        );

        // .env wins over .env.example; secrets are masked; comments skipped.
        assert_eq!(
            Index::find(&index.env, "APP_NAME").unwrap().detail,
            "Galaxy"
        );
        assert_eq!(
            Index::find(&index.env, "DB_PASSWORD").unwrap().detail,
            "••••••"
        );
        assert!(Index::find(&index.env, "EXAMPLE_ONLY").is_some());
        assert!(Index::find(&index.env, "NOTE").is_none());

        let views = names(&index.views);
        assert!(views.contains(&"welcome") && views.contains(&"admin.users.index"));
        assert!(names(&index.components).contains(&"forms.input"));
    }

    #[test]
    fn route_names_follow_prefix_groups_and_resources() {
        let dir = project();
        let index = Index::build(dir.path());
        let mut routes = names(&index.routes);
        routes.sort_unstable();
        assert_eq!(
            routes,
            vec![
                "admin.users.index",
                "admin.users.store",
                "home",
                "photos.index",
                "photos.show"
            ]
        );
        assert_eq!(
            Index::find(&index.routes, "admin.users.store")
                .unwrap()
                .detail,
            "POST /users"
        );
        assert_eq!(Index::find(&index.routes, "home").unwrap().detail, "GET /");
    }

    #[test]
    fn find_route_reads_only_laravel_applications() {
        let dir = project();
        let found = Index::find_route(dir.path(), "admin.users.store").expect("route");
        assert!(
            found
                .loc
                .expect("location")
                .path
                .ends_with("routes/web.php")
        );
        assert!(Index::find_route(dir.path(), "nope").is_none());
        let plain = tempfile::tempdir().unwrap();
        assert!(Index::find_route(plain.path(), "home").is_none());
    }

    #[test]
    fn reads_translations_middleware_and_providers() {
        let dir = project();
        let index = Index::build(dir.path());
        let translations = names(&index.translations);
        assert!(translations.contains(&"messages.hello"));
        assert!(translations.contains(&"messages.nested.a"));
        assert!(translations.contains(&"Welcome back"));
        assert!(index.translation_groups.contains("messages"));

        let owner = Index::find(&index.middleware, "role.owner").expect("alias");
        assert_eq!(
            owner.class.as_deref(),
            Some("App\\Http\\Middleware\\EnsureOwner")
        );
        assert!(Index::find(&index.middleware, "auth").is_some());
        assert!(index.has_middleware_source);

        assert!(Index::find(&index.bindings, "App\\Services\\Pay").is_some());
        assert!(Index::find(&index.bindings, "pay.fake").is_some());
        let abilities = names(&index.abilities);
        assert!(abilities.contains(&"manage-users") && abilities.contains(&"publish"));

        assert_eq!(names(&index.livewire), vec!["admin.user-table"]);
        assert_eq!(names(&index.pages), vec!["Users/Index"]);
    }

    #[test]
    fn migrations_give_tables_their_columns() {
        let dir = project();
        let r = dir.path();
        write(
            r,
            "database/migrations/2024_01_01_000000_create_posts_table.php",
            "<?php\nSchema::create('posts', function (Blueprint $table) {\n    $table->id();\n    $table->string('title')->nullable();\n    $table->foreignId('user_id')->constrained();\n    $table->morphs('taggable');\n    $table->timestamps();\n    $table->softDeletes();\n    $table->index('title');\n});\n",
        );
        write(
            r,
            "database/migrations/2024_02_01_000000_alter_posts_table.php",
            "<?php\nSchema::table('posts', function (Blueprint $table) {\n    $table->text('body');\n    $table->dropColumn('title');\n    $table->renameColumn('body', 'content');\n});\n",
        );
        let index = Index::build(r);
        let columns: Vec<String> = index
            .columns_of("posts")
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            columns,
            vec![
                "id",
                "user_id",
                "taggable_id",
                "taggable_type",
                "created_at",
                "updated_at",
                "deleted_at",
                "content"
            ]
        );
        assert!(index.columns_of("nope").is_empty());
    }

    #[test]
    fn lists_folder_entries() {
        let dir = project();
        let index = Index::build(dir.path());
        let top = index.paths("app", "");
        assert!(names(&top).contains(&"Livewire/") && names(&top).contains(&"Policies/"));
        assert_eq!(
            names(&index.paths("app", "Policies/")),
            vec!["Policies/PostPolicy.php"]
        );
        assert!(index.path_exists("app", "Policies/PostPolicy.php"));
        assert!(!index.path_exists("app", "Nope"));
    }
}
