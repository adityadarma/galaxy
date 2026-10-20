//! What a Symfony project defines, read from its files: route names,
//! Twig templates, service ids, parameters, environment variables and
//! translation keys. PHP never runs, so anything made at run time (routes
//! or services a bundle registers, say) is not in here.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::yaml::{self, Entry};

const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 12;

/// Parameters the kernel defines.
const KERNEL_PARAMETERS: &[&str] = &[
    "kernel.project_dir",
    "kernel.environment",
    "kernel.debug",
    "kernel.cache_dir",
    "kernel.logs_dir",
    "kernel.build_dir",
    "kernel.charset",
    "kernel.container_class",
    "kernel.runtime_environment",
];

/// Where a name is defined. Line and UTF-16 column are 0-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loc {
    pub path: PathBuf,
    pub line: u32,
    pub column_utf16: u32,
}

/// A name the project defines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Item {
    pub name: String,
    /// A value, a signature or a short description. For routes, the
    /// methods and the path: `GET|POST /admin/users`.
    pub detail: String,
    pub loc: Option<Loc>,
}

#[derive(Debug, Default)]
pub struct Index {
    pub root: PathBuf,
    /// Route names, from controller attributes and annotations and from
    /// `config/routes*.yaml`.
    pub routes: Vec<Item>,
    /// `false` when a route import loads routes this index cannot see.
    pub routes_complete: bool,
    /// Template paths below the Twig template directory, `/`-separated.
    pub templates: Vec<Item>,
    /// The directory the templates were read from.
    pub template_dir: PathBuf,
    /// Service ids set in `config/services.yaml`.
    pub services: Vec<Item>,
    /// `false` when `services.yaml` registers whole directories, so any
    /// class may be a service.
    pub services_complete: bool,
    /// Parameters, with the kernel's own.
    pub parameters: Vec<Item>,
    /// Environment variable names from the `.env` files.
    pub env: Vec<Item>,
    /// Translation keys by domain.
    pub translations: BTreeMap<String, Vec<Item>>,
}

impl Index {
    /// `Some` when `root` is a Symfony project.
    pub fn detect(root: &Path) -> Option<Self> {
        is_symfony(root).then(|| Self::build(root))
    }

    /// Reads everything from `root`, whether or not it looks like Symfony.
    pub fn build(root: &Path) -> Self {
        let mut index = Self {
            root: root.to_path_buf(),
            routes_complete: true,
            services_complete: true,
            ..Self::default()
        };
        index.read_routes();
        index.read_templates();
        index.read_services_and_parameters();
        index.read_env();
        index.read_translations();
        index
    }

    /// The item called `name` in `items`.
    pub fn find<'a>(items: &'a [Item], name: &str) -> Option<&'a Item> {
        items.iter().find(|item| item.name == name)
    }

    /// Keys of `domain`; the default domain is `messages`.
    pub fn translation_keys(&self, domain: Option<&str>) -> &[Item] {
        self.translations
            .get(domain.unwrap_or("messages"))
            .map_or(&[], Vec::as_slice)
    }

    fn read_routes(&mut self) {
        let mut routes = Vec::new();
        for file in walk(&self.root.join("src/Controller")) {
            if file.extension().and_then(|e| e.to_str()) != Some("php") {
                continue;
            }
            let Some(text) = read(&file) else { continue };
            let lines = Lines::new(&text);
            for found in controller_routes(&text) {
                let (line, column_utf16) = lines.at(found.pos);
                routes.push(Item {
                    name: found.name,
                    detail: found.detail,
                    loc: Some(Loc {
                        path: file.clone(),
                        line,
                        column_utf16,
                    }),
                });
            }
        }

        let config = self.root.join("config");
        let mut files = vec![config.join("routes.yaml"), config.join("routes.yml")];
        for file in sorted_entries(&config.join("routes")) {
            match file.extension().and_then(|e| e.to_str()) {
                Some("yaml" | "yml") => files.push(file),
                // Routes written in XML or PHP are not read.
                Some("xml" | "php") => self.routes_complete = false,
                _ => {}
            }
        }
        if config.join("routes.xml").is_file() || config.join("routes.php").is_file() {
            self.routes_complete = false;
        }
        for file in files {
            let Some(text) = read(&file) else { continue };
            let entries = yaml::parse(&text);
            if !yaml_routes(&entries, &file, &mut routes) {
                self.routes_complete = false;
            }
        }
        self.routes = unique(routes);
    }

    fn read_templates(&mut self) {
        let dir = template_dir(&self.root);
        let mut templates = Vec::new();
        for file in walk(&dir) {
            let Ok(rel) = file.strip_prefix(&dir) else {
                continue;
            };
            let Some(rel) = rel.to_str() else { continue };
            templates.push(Item {
                name: rel.replace('\\', "/"),
                detail: String::new(),
                loc: Some(Loc {
                    path: file.clone(),
                    line: 0,
                    column_utf16: 0,
                }),
            });
        }
        self.templates = templates;
        self.template_dir = dir;
    }

    fn read_services_and_parameters(&mut self) {
        let mut parameters: Vec<Item> = KERNEL_PARAMETERS
            .iter()
            .map(|name| Item {
                name: (*name).to_owned(),
                detail: "built in".to_owned(),
                loc: None,
            })
            .collect();
        let config = self.root.join("config");

        let services_file = config.join("services.yaml");
        if let Some(text) = read(&services_file) {
            let entries = yaml::parse(&text);
            let (services, complete) = yaml_services(&entries, &services_file);
            self.services = unique(services);
            self.services_complete = complete;
            parameters.extend(yaml_parameters(&entries, &services_file));
        }
        for file in sorted_entries(&config.join("packages")) {
            if !matches!(
                file.extension().and_then(|e| e.to_str()),
                Some("yaml" | "yml")
            ) {
                continue;
            }
            if let Some(text) = read(&file) {
                parameters.extend(yaml_parameters(&yaml::parse(&text), &file));
            }
        }
        self.parameters = unique(parameters);
    }

    fn read_env(&mut self) {
        let mut files: Vec<PathBuf> = sorted_entries(&self.root)
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| {
                        (name == ".env" || name.starts_with(".env."))
                            && !name.ends_with(".php")
                            && path.is_file()
                    })
            })
            .collect();
        let rank = |path: &PathBuf| match path.file_name().and_then(|n| n.to_str()) {
            Some(".env") => 0,
            Some(".env.local") => 1,
            Some(".env.dist") => 2,
            _ => 3,
        };
        files.sort_by_key(rank);
        let mut env = Vec::new();
        for file in files {
            let Some(text) = read(&file) else { continue };
            let label = file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            for (line, raw) in text.lines().enumerate() {
                let Some((name, column)) = env_name(raw) else {
                    continue;
                };
                env.push(Item {
                    name: name.to_owned(),
                    detail: label.clone(),
                    loc: Some(Loc {
                        path: file.clone(),
                        line: line as u32,
                        column_utf16: column,
                    }),
                });
            }
        }
        self.env = unique(env);
    }

    fn read_translations(&mut self) {
        let mut out: BTreeMap<String, Vec<Item>> = BTreeMap::new();
        for file in walk(&self.root.join("translations")) {
            let Some((domain, format)) = translation_file(&file) else {
                continue;
            };
            let Some(text) = read(&file) else { continue };
            let items = match format {
                Format::Yaml => yaml_translations(&text, &file),
                Format::Xliff => xliff_translations(&text, &file),
            };
            out.entry(domain).or_default().extend(items);
        }
        self.translations = out.into_iter().map(|(k, v)| (k, unique(v))).collect();
    }
}

// ---------------------------------------------------------------------------
// Detection

fn is_symfony(root: &Path) -> bool {
    root.join("symfony.lock").is_file()
        || (root.join("bin/console").is_file() && root.join("config/bundles.php").is_file())
        || composer_requires_symfony(root)
}

fn composer_requires_symfony(root: &Path) -> bool {
    let Some(text) = read(&root.join("composer.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    ["require", "require-dev"].iter().any(|section| {
        value
            .get(section)
            .and_then(|v| v.as_object())
            .is_some_and(|deps| {
                deps.keys()
                    .any(|name| name == "symfony/framework-bundle" || name == "symfony/symfony")
            })
    })
}

// ---------------------------------------------------------------------------
// File system

fn read(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    fs::read_to_string(path).ok()
}

/// Files below `dir`, sorted, skipping hidden entries.
fn walk(dir: &Path) -> Vec<PathBuf> {
    fn go(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth == 0 || out.len() >= MAX_FILES {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if out.len() >= MAX_FILES {
                return;
            }
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                go(&path, depth - 1, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    go(dir, MAX_DEPTH, &mut out);
    out
}

/// Entries directly in `dir`, sorted. Capped like a walk.
fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .take(MAX_FILES)
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// Keeps the first item of each name.
fn unique(items: Vec<Item>) -> Vec<Item> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.name.clone()))
        .collect()
}

/// Line starts of a text, to turn byte offsets into positions.
struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, starts }
    }

    /// 0-based line and UTF-16 column of `pos`.
    fn at(&self, pos: usize) -> (u32, u32) {
        let pos = pos.min(self.text.len());
        let line = self.starts.partition_point(|s| *s <= pos) - 1;
        let from = self.starts[line];
        let column = self
            .text
            .get(from..pos)
            .map_or(0, |s| s.encode_utf16().count());
        (line as u32, column as u32)
    }
}

// ---------------------------------------------------------------------------
// Controllers

/// A route found in a controller.
struct Found {
    name: String,
    detail: String,
    pos: usize,
}

/// The arguments of one `Route` attribute or annotation.
#[derive(Debug, Default, Clone)]
struct RouteArgs {
    path: Option<String>,
    name: Option<String>,
    methods: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Decl {
    Class,
    Function,
    Other,
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Index after the string literal that starts at `at`.
fn skip_string(bytes: &[u8], at: usize) -> usize {
    let quote = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// The bracket that closes the one at `open`, or the end of the text.
fn matching(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0i32;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth <= 0 {
                    return i;
                }
            }
            b'\'' | b'"' => {
                i = skip_string(bytes, i);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|c| *c == b'\n')
        .map_or(bytes.len(), |n| from + n)
}

/// Pieces of `text` between top-level commas, with their offsets.
fn split_top(text: &str) -> Vec<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut from = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'\'' | b'"' => {
                i = skip_string(bytes, i);
                continue;
            }
            b',' if depth == 0 => {
                out.push((from, &text[from..i]));
                from = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push((from, &text[from.min(text.len())..]));
    out
}

/// The string literal at the start of `text`: its value and the length of
/// the literal with quotes.
fn literal_at(text: &str) -> Option<(String, usize)> {
    let quote = match text.as_bytes().first() {
        Some(q @ (b'\'' | b'"')) => *q as char,
        _ => return None,
    };
    let mut out = String::new();
    let mut chars = text[1..].char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            if let Some((_, next)) = chars.next() {
                if next != quote && next != '\\' {
                    out.push('\\');
                }
                out.push(next);
            }
            continue;
        }
        if c == quote {
            return Some((out, i + 2));
        }
        out.push(c);
    }
    None
}

/// Every string literal in `text`.
fn literals(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'\'' | b'"') {
            match literal_at(&text[i..]) {
                Some((value, len)) => {
                    out.push(value);
                    i += len;
                }
                None => break,
            }
        } else {
            i += 1;
        }
    }
    out
}

/// `name: value` or `name=value` at the start of an argument.
fn named(piece: &str) -> Option<(&str, &str)> {
    let len = piece.bytes().take_while(|c| is_word(*c)).count();
    if len == 0 || piece.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let rest = piece[len..].trim_start();
    if let Some(value) = rest.strip_prefix(':') {
        return (!value.starts_with(':')).then(|| (&piece[..len], value));
    }
    let value = rest.strip_prefix('=')?;
    (!value.starts_with(['=', '>'])).then(|| (&piece[..len], value))
}

/// HTTP methods in `['GET', 'POST']`, `{"GET"}` or `Request::METHOD_GET`.
fn methods_of(value: &str) -> Vec<String> {
    let mut out: Vec<String> = literals(value)
        .into_iter()
        .map(|m| m.trim().to_ascii_uppercase())
        .filter(|m| !m.is_empty())
        .collect();
    for (at, _) in value.match_indices("METHOD_") {
        let word: String = value[at + 7..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !word.is_empty() {
            out.push(word.to_ascii_uppercase());
        }
    }
    let mut seen = HashSet::new();
    out.retain(|m| seen.insert(m.clone()));
    out
}

/// Reads the argument list of a `Route` attribute or annotation.
fn route_args(args: &str) -> RouteArgs {
    let mut route = RouteArgs::default();
    let mut positional = 0;
    for (_, piece) in split_top(args) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let (key, value) = match named(piece) {
            Some((key, value)) => (key, value.trim()),
            None => {
                let key = match positional {
                    0 => "path",
                    1 => "name",
                    _ => "",
                };
                positional += 1;
                (key, piece)
            }
        };
        match key {
            "path" | "value" => route.path = literals(value).into_iter().next(),
            "name" => {
                route.name = literal_at(value)
                    .filter(|(_, len)| *len == value.len())
                    .map(|(name, _)| name);
            }
            "methods" => route.methods = methods_of(value),
            _ => {}
        }
    }
    route
}

/// `Route(...)` in one item of an attribute group: the argument text and
/// the offset of the name in the item.
fn route_item(item: &str) -> Option<(&str, usize)> {
    let lead = item.len() - item.trim_start().len();
    let open = item.find('(')?;
    let name = item[..open].trim();
    if name.rsplit('\\').next() != Some("Route") {
        return None;
    }
    let close = matching(item.as_bytes(), open).min(item.len());
    Some((&item[open + 1..close], lead))
}

/// What the declaration that follows `from` is, past attributes, comments
/// and modifiers.
fn next_decl(text: &str, from: usize) -> Decl {
    let bytes = text.as_bytes();
    let mut i = from.min(bytes.len());
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let rest = &text[i..];
        if rest.starts_with("#[") {
            i = (matching(bytes, i + 1) + 1).min(bytes.len());
        } else if rest.starts_with("/*") {
            i = rest.find("*/").map_or(bytes.len(), |n| i + n + 2);
        } else if rest.starts_with("//") || rest.starts_with('#') {
            i = line_end(bytes, i);
        } else {
            break;
        }
    }
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let len = bytes[i.min(bytes.len())..]
            .iter()
            .take_while(|c| is_word(**c))
            .count();
        if len == 0 {
            return Decl::Other;
        }
        match &text[i..i + len] {
            "final" | "abstract" | "readonly" | "public" | "protected" | "private" | "static"
            | "var" => i += len,
            "class" => return Decl::Class,
            "function" => return Decl::Function,
            _ => return Decl::Other,
        }
    }
}

/// `class` at `at` (ending at `end`) starts a class declaration, rather
/// than being `Foo::class`, `new class`, `$class` or `class:`.
fn is_class_declaration(text: &str, at: usize, end: usize) -> bool {
    let before = text[..at].trim_end();
    if before.ends_with("::") || before.ends_with("->") || text[..at].ends_with('$') {
        return false;
    }
    if let Some(head) = before.strip_suffix("new")
        && !head
            .as_bytes()
            .last()
            .is_some_and(|c| is_word(*c) || *c == b'$')
    {
        return false;
    }
    !text[end..].trim_start().starts_with(':')
}

#[derive(Default)]
struct Scan {
    class: Option<RouteArgs>,
    pending: Option<RouteArgs>,
    out: Vec<Found>,
}

impl Scan {
    fn route(&mut self, decl: Decl, args: RouteArgs, pos: usize) {
        match decl {
            Decl::Class => {
                self.pending.get_or_insert(args);
            }
            Decl::Function => {
                let Some(name) = args.name else { return };
                let class = self.class.as_ref();
                let prefix_name = class.and_then(|c| c.name.as_deref()).unwrap_or("");
                let prefix_path = class.and_then(|c| c.path.as_deref()).unwrap_or("");
                let methods = if args.methods.is_empty() {
                    class.map(|c| c.methods.clone()).unwrap_or_default()
                } else {
                    args.methods
                };
                let methods = if methods.is_empty() {
                    "ANY".to_owned()
                } else {
                    methods.join("|")
                };
                let path = format!("{prefix_path}{}", args.path.unwrap_or_default());
                self.out.push(Found {
                    name: format!("{prefix_name}{name}"),
                    detail: format!("{methods} {path}"),
                    pos,
                });
            }
            Decl::Other => {}
        }
    }
}

/// Named routes of a controller file. Class-level routes give the prefix
/// of the routes of the methods after them.
fn controller_routes(text: &str) -> Vec<Found> {
    let bytes = text.as_bytes();
    let mut scan = Scan::default();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' | b'"' => i = skip_string(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => i = line_end(bytes, i),
            b'#' if bytes.get(i + 1) == Some(&b'[') => {
                let close = matching(bytes, i + 1);
                let inner = &text[i + 2..close.max(i + 2).min(bytes.len())];
                let mut decl = None;
                for (offset, item) in split_top(inner) {
                    if let Some((args, lead)) = route_item(item) {
                        let decl = *decl.get_or_insert_with(|| next_decl(text, close + 1));
                        scan.route(decl, route_args(args), i + 2 + offset + lead);
                    }
                }
                i = close + 1;
            }
            b'#' => i = line_end(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = text[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |n| i + 2 + n + 2);
                if text[i..].starts_with("/**") {
                    let mut decl = None;
                    for (at, _) in text[i..end].match_indices("@Route(") {
                        let open = i + at + 6;
                        let close = matching(bytes, open).min(end);
                        let args = &text[open + 1..close.max(open + 1)];
                        let decl = *decl.get_or_insert_with(|| next_decl(text, end));
                        scan.route(decl, route_args(args), i + at);
                    }
                }
                i = end;
            }
            c if is_word(c) => {
                let len = bytes[i..].iter().take_while(|c| is_word(**c)).count();
                if &text[i..i + len] == "class" && is_class_declaration(text, i, i + len) {
                    scan.class = scan.pending.take();
                }
                i += len;
            }
            _ => i += 1,
        }
    }
    scan.out
}

// ---------------------------------------------------------------------------
// YAML

fn yaml_loc(path: &Path, entry: &Entry) -> Loc {
    Loc {
        path: path.to_path_buf(),
        line: entry.line,
        column_utf16: entry.column,
    }
}

/// Methods in `[GET, POST]` or `GET`.
fn yaml_methods(value: &str) -> String {
    let inner = value.trim().trim_start_matches('[').trim_end_matches(']');
    let methods: Vec<String> = inner
        .split(',')
        .map(|m| m.trim().trim_matches(['\'', '"']).to_ascii_uppercase())
        .filter(|m| !m.is_empty())
        .collect();
    if methods.is_empty() {
        "ANY".to_owned()
    } else {
        methods.join("|")
    }
}

/// Reads the routes of one routing file into `out`. `false` when it
/// imports routes this index does not read.
fn yaml_routes(entries: &[Entry], file: &Path, out: &mut Vec<Item>) -> bool {
    let mut complete = true;
    for top in entries.iter().filter(|e| e.path.len() == 1) {
        let name = top.key();
        if name.starts_with("when@") {
            continue;
        }
        if yaml::find(entries, &[name, "resource"]).is_some() {
            if !plain_controller_import(entries, name) {
                complete = false;
            }
            continue;
        }
        let Some(path) = yaml::find(entries, &[name, "path"]) else {
            continue;
        };
        // A path per locale is a mapping: show the first.
        let path_value = path.value.clone().or_else(|| {
            yaml::children(entries, &[name, "path"]).find_map(|child| child.value.clone())
        });
        let methods =
            yaml::value(entries, &[name, "methods"]).map_or_else(|| "ANY".to_owned(), yaml_methods);
        out.push(Item {
            name: name.to_owned(),
            detail: format!("{methods} {}", path_value.unwrap_or_default()),
            loc: Some(yaml_loc(file, top)),
        });
    }
    complete
}

/// The route import `name` loads controllers by attributes or annotations
/// (or the kernel class, which the Flex recipe imports the same way).
fn plain_controller_import(entries: &[Entry], name: &str) -> bool {
    let kind = yaml::value(entries, &[name, "type"]);
    if !matches!(kind, Some("attribute" | "annotation")) {
        return false;
    }
    let resource = yaml::value(entries, &[name, "resource"])
        .or_else(|| yaml::value(entries, &[name, "resource", "path"]));
    resource.is_some_and(|r| {
        let r = r.trim();
        r.starts_with("../src/Controller")
            || r.starts_with("../../src/Controller")
            || r == "../src/Kernel.php"
            || r == "Kernel"
            || r.ends_with("\\Kernel")
    })
}

/// Service ids of `services.yaml`, and whether it lists them all.
fn yaml_services(entries: &[Entry], file: &Path) -> (Vec<Item>, bool) {
    let mut complete = !entries
        .iter()
        .any(|e| e.path.len() == 1 && e.key() == "imports");
    let mut out = Vec::new();
    for entry in entries
        .iter()
        .filter(|e| e.path.len() == 2 && e.path[0] == "services")
    {
        let id = entry.key();
        if id == "_defaults" || id == "_instanceof" {
            continue;
        }
        if yaml::find(entries, &["services", id, "resource"]).is_some() {
            complete = false;
            continue;
        }
        let class = yaml::value(entries, &["services", id, "class"])
            .or_else(|| yaml::value(entries, &["services", id, "alias"]))
            .unwrap_or("");
        out.push(Item {
            name: id.to_owned(),
            detail: class.to_owned(),
            loc: Some(yaml_loc(file, entry)),
        });
    }
    (out, complete)
}

fn yaml_parameters(entries: &[Entry], file: &Path) -> Vec<Item> {
    yaml::children(entries, &["parameters"])
        .map(|entry| Item {
            name: entry.key().to_owned(),
            detail: entry.value.clone().unwrap_or_default(),
            loc: Some(yaml_loc(file, entry)),
        })
        .collect()
}

/// The directory Twig reads templates from.
fn template_dir(root: &Path) -> PathBuf {
    let default = root.join("templates");
    let Some(text) = read(&root.join("config/packages/twig.yaml")) else {
        return default;
    };
    let entries = yaml::parse(&text);
    let Some(value) = yaml::value(&entries, &["twig", "default_path"]) else {
        return default;
    };
    let value = value
        .replace("%kernel.project_dir%", &root.to_string_lossy())
        .trim_end_matches('/')
        .to_owned();
    if value.is_empty() || value.contains('%') {
        return default;
    }
    let path = Path::new(&value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

// ---------------------------------------------------------------------------
// Environment

/// The variable a `.env` line sets, with its UTF-16 column.
fn env_name(line: &str) -> Option<(&str, u32)> {
    let trimmed = line.trim_start();
    let body = trimmed
        .strip_prefix("export ")
        .map_or(trimmed, str::trim_start);
    let len = body.bytes().take_while(|c| is_word(*c)).count();
    if len == 0 || body.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    body[len..].trim_start().starts_with('=').then(|| {
        let at = line.len() - body.len();
        (&body[..len], line[..at].encode_utf16().count() as u32)
    })
}

// ---------------------------------------------------------------------------
// Translations

enum Format {
    Yaml,
    Xliff,
}

/// The domain and format of `translations/<domain>.<locale>.<ext>`.
fn translation_file(path: &Path) -> Option<(String, Format)> {
    let name = path.file_name()?.to_str()?;
    let (stem, ext) = name.rsplit_once('.')?;
    let format = match ext {
        "yaml" | "yml" => Format::Yaml,
        "xlf" | "xliff" => Format::Xliff,
        _ => return None,
    };
    let (domain, locale) = stem.rsplit_once('.')?;
    // `messages+intl-icu` is the `messages` domain in ICU format.
    let domain = domain.split('+').next().unwrap_or("");
    (!domain.is_empty() && !locale.is_empty()).then(|| (domain.to_owned(), format))
}

fn yaml_translations(text: &str, file: &Path) -> Vec<Item> {
    yaml::parse(text)
        .iter()
        .filter(|entry| entry.value.is_some())
        .map(|entry| Item {
            name: entry.dotted(),
            detail: entry.value.clone().unwrap_or_default(),
            loc: Some(yaml_loc(file, entry)),
        })
        .collect()
}

fn xliff_translations(text: &str, file: &Path) -> Vec<Item> {
    let lines = Lines::new(text);
    xliff_units(text)
        .into_iter()
        .map(|unit| {
            let (line, column_utf16) = lines.at(unit.pos);
            Item {
                name: unit.key,
                detail: unit.target,
                loc: Some(Loc {
                    path: file.to_path_buf(),
                    line,
                    column_utf16,
                }),
            }
        })
        .collect()
}

struct Unit {
    key: String,
    target: String,
    pos: usize,
}

/// `<trans-unit>` (XLIFF 1) and `<unit>` (XLIFF 2) elements. The key is the
/// `resname`, else the `<source>`, else the `id`, as Symfony's loader does.
fn xliff_units(text: &str) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find('<') {
        let open = at + found;
        let rest = &text[open + 1..];
        let tag_len = ["trans-unit", "unit"]
            .iter()
            .find(|tag| {
                rest.strip_prefix(**tag)
                    .and_then(|r| r.chars().next())
                    .is_some_and(|c| c.is_whitespace() || c == '>')
            })
            .map(|tag| tag.len());
        let Some(tag_len) = tag_len else {
            at = open + 1;
            continue;
        };
        let Some(tag_end) = text[open..].find('>').map(|n| open + n) else {
            break;
        };
        let attrs = &text[open + 1 + tag_len..tag_end];
        let body_end = ["</trans-unit>", "</unit>"]
            .iter()
            .filter_map(|close| text[tag_end..].find(close).map(|n| tag_end + n))
            .min()
            .unwrap_or(text.len());
        let body = &text[tag_end..body_end];
        let key = attribute(attrs, "resname")
            .or_else(|| element_text(body, "source"))
            .or_else(|| attribute(attrs, "id"))
            .filter(|key| !key.is_empty());
        if let Some(key) = key {
            out.push(Unit {
                key,
                target: element_text(body, "target").unwrap_or_default(),
                pos: open,
            });
        }
        at = body_end.max(open + 1);
    }
    out
}

/// The value of attribute `name` in the text of a tag.
fn attribute(attrs: &str, name: &str) -> Option<String> {
    for (at, _) in attrs.match_indices(&format!("{name}=")) {
        if !attrs[..at].ends_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let rest = &attrs[at + name.len() + 1..];
        let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let end = rest[1..].find(quote)?;
        return Some(unescape(&rest[1..1 + end]));
    }
    None
}

/// Text of the first `<tag>…</tag>` in `body`.
fn element_text(body: &str, tag: &str) -> Option<String> {
    let mut from = 0;
    while let Some(found) = body[from..].find(&format!("<{tag}")) {
        let open = from + found + tag.len() + 1;
        let next = body[open..].chars().next()?;
        if !(next == '>' || next.is_whitespace()) {
            from = open;
            continue;
        }
        let start = open + body[open..].find('>')? + 1;
        let end = start + body[start..].find(&format!("</{tag}>"))?;
        return Some(unescape(body[start..end].trim()));
    }
    None
}

fn unescape(text: &str) -> String {
    match text
        .strip_prefix("<![CDATA[")
        .and_then(|t| t.strip_suffix("]]>"))
    {
        Some(raw) => raw.to_owned(),
        None => text
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    }
}
