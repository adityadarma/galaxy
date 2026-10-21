//! Completion, navigation, hover, problems and quick fixes for the strings
//! of a Laravel project: `config('app.name')`, `route('home')`,
//! `@include('partials.nav')`, `->middleware('auth')`, `[UserController::class,
//! 'index']`, `User::where('email')`, `<x-forms.input>` and the rest.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use regex::Regex;

use super::{Index, Item, Kind, Loc, Site, site_at, sites};
use crate::php::engine::{
    Action, Completion, Ctx, FileChange, HoverResult, ItemKind, PhpEngine, Problem, Severity,
    Target, byte_offset, member_operator, position_of, receiver_start,
};
use crate::php::framework::common::complete::{
    Found, describe, found, named, pick, relative, target_of,
};
use crate::php::generics;
use crate::php::parse::{ClassInfo, MemberKind, parse_with_tree};

const MODEL: &str = "Illuminate\\Database\\Eloquent\\Model";
const RELATIONS: &str = "Illuminate\\Database\\Eloquent\\Relations\\";

pub(super) fn is_blade(path: &Path) -> bool {
    crate::php::framework::is_template(path)
}

fn label(kind: Kind) -> &'static str {
    match kind {
        Kind::Config => "Config",
        Kind::Env => "Environment variable",
        Kind::Route => "Route",
        Kind::View => "View",
        Kind::Translation => "Translation",
        Kind::Asset => "Asset",
        Kind::Vite => "Vite asset",
        Kind::Middleware => "Middleware",
        Kind::Ability => "Ability",
        Kind::Binding => "Container binding",
        Kind::Page => "Inertia page",
        Kind::Livewire => "Livewire component",
        Kind::Rule => "Validation rule",
        Kind::Action => "Controller action",
        Kind::Column => "Column",
        Kind::Relation => "Relation",
        Kind::Section(_) => "Config entry",
        Kind::Path(_) => "Path",
    }
}

fn item_kind(kind: Kind) -> ItemKind {
    match kind {
        Kind::Config | Kind::Section(_) | Kind::Column => ItemKind::Property,
        Kind::Env => ItemKind::Constant,
        Kind::Relation | Kind::Action => ItemKind::Method,
        Kind::Binding | Kind::Livewire | Kind::Middleware => ItemKind::Class,
        _ => ItemKind::Function,
    }
}

fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn plural(word: &str) -> String {
    let vowel = |c: char| "aeiou".contains(c);
    if let Some(stem) = word.strip_suffix('y')
        && !stem.chars().last().is_some_and(vowel)
    {
        return format!("{stem}ies");
    }
    if ["s", "x", "z", "ch", "sh"]
        .iter()
        .any(|end| word.ends_with(end))
    {
        return format!("{word}es");
    }
    format!("{word}s")
}

/// Rule name of a `required_if:a,b` style piece.
fn rule_name(piece: &str) -> &str {
    piece.split(':').next().unwrap_or(piece)
}

/// The pieces of a `a|b:1|c` rule string with their byte ranges inside it.
fn rule_pieces(contents: &str) -> Vec<(&str, usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for piece in contents.split('|') {
        out.push((piece, start, start + piece.len()));
        start += piece.len() + 1;
    }
    out
}

/// A Blade tag naming a component: `<x-forms.input` or `<livewire:cart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Component,
    Livewire,
}

/// The tag name the byte `at` is in, with the byte range of the name.
fn blade_tag(text: &str, at: usize) -> Option<(Tag, usize, usize)> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    let re = TAG.get_or_init(|| Regex::new(r"<(x-|livewire:)([\w.\-]*)").expect("valid"));
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    for caps in re.captures_iter(&text[line_start..line_end]) {
        let name = caps.get(2)?;
        let (start, end) = (line_start + name.start(), line_start + name.end());
        if start <= at && at <= end {
            let tag = if &caps[1] == "x-" {
                Tag::Component
            } else {
                Tag::Livewire
            };
            return Some((tag, start, end));
        }
    }
    None
}

impl PhpEngine {
    // ---- model helpers -------------------------------------------------

    fn is_model(&mut self, class: &Arc<ClassInfo>) -> bool {
        self.lineage(class).iter().any(|c| c.fqn == MODEL)
    }

    fn table_of(&mut self, class: &Arc<ClassInfo>) -> String {
        static TABLE: OnceLock<Regex> = OnceLock::new();
        let re =
            TABLE.get_or_init(|| Regex::new(r#"\$table\s*=\s*['"]([\w.]+)['"]"#).expect("valid"));
        for owner in self.lineage(class).into_iter().take(6) {
            if owner.fqn == MODEL {
                break;
            }
            if let Ok(text) = std::fs::read_to_string(&owner.path)
                && let Some(caps) = re.captures(&text)
            {
                return caps[1].to_owned();
            }
        }
        plural(&snake(&class.name))
    }

    /// Columns a model reads: those migrations create and the attributes it
    /// lists itself.
    fn model_columns(&mut self, idx: &Index, class: &Arc<ClassInfo>) -> Vec<Item> {
        let table = self.table_of(class);
        let mut columns = idx.columns_of(&table);
        for owner in self.lineage(class).into_iter().take(6) {
            if owner.fqn == MODEL {
                break;
            }
            for member in &owner.members {
                let listed = member
                    .doc
                    .as_deref()
                    .is_some_and(|d| d.starts_with("Attribute listed in"))
                    || member.in_docblock;
                if member.kind == MemberKind::Property
                    && !member.is_static
                    && listed
                    && !columns.iter().any(|c| c.name == member.name)
                {
                    columns.push(Item {
                        name: member.name.clone(),
                        detail: member.detail.clone(),
                        loc: Some(Loc {
                            path: owner.path.clone(),
                            line: member.line,
                            column: member.column,
                        }),
                        class: None,
                    });
                }
            }
        }
        columns
    }

    fn model_relations(&mut self, class: &Arc<ClassInfo>) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        for owner in self.lineage(class).into_iter().take(8) {
            if owner.fqn == MODEL {
                break;
            }
            for member in &owner.members {
                if member.kind == MemberKind::Method
                    && member
                        .ty
                        .as_deref()
                        .is_some_and(|t| t.starts_with(RELATIONS))
                    && !out.iter().any(|i| i.name == member.name)
                {
                    out.push(Item {
                        name: member.name.clone(),
                        detail: member
                            .ty
                            .as_deref()
                            .and_then(|t| t.rsplit('\\').next())
                            .unwrap_or("")
                            .to_owned(),
                        loc: Some(Loc {
                            path: owner.path.clone(),
                            line: member.line,
                            column: member.column,
                        }),
                        class: None,
                    });
                }
            }
        }
        out
    }

    /// Runs `run` with a context at `byte` of the PHP in `text`.
    fn with_context<T>(
        &mut self,
        path: &Path,
        text: &str,
        byte: usize,
        run: impl FnOnce(&mut Self, &Ctx<'_>) -> Option<T>,
    ) -> Option<T> {
        let (file, tree) = parse_with_tree(path, text)?;
        let row = position_of(text, byte).0;
        let class = file
            .classes
            .iter()
            .find(|c| c.start_line <= row && row <= c.end_line)
            .cloned();
        let ctx = Ctx {
            src: text,
            tree: &tree,
            file: &file,
            byte,
            class,
        };
        run(self, &ctx)
    }

    /// The model a `where('…')` or `with('…')` call is made on.
    fn model_of_call(&mut self, path: &Path, text: &str, site: &Site) -> Option<Arc<ClassInfo>> {
        static ROOT: OnceLock<Regex> = OnceLock::new();
        let paren = site.paren?;
        self.with_context(path, text, paren, |engine, ctx| {
            let before = text[..paren].trim_end();
            let name_start = before
                .char_indices()
                .rev()
                .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
                .map_or(0, |(i, c)| i + c.len_utf8());
            let head = &before[..name_start];
            let (receiver_end, is_static) = member_operator(head)?;
            let receiver = head[receiver_start(head, receiver_end)..receiver_end].trim();
            if is_static {
                let fqn = engine.class_ref(ctx, receiver)?;
                return engine.class(&fqn);
            }
            if let Some(ty) = engine.eval(ctx, receiver, 0) {
                let (base, args) = generics::split(&ty);
                if let Some(arg) = args.first()
                    && let Some(class) = engine.class(arg)
                {
                    return Some(class);
                }
                if let Some(class) = engine.class(base)
                    && engine.is_model(&class)
                {
                    return Some(class);
                }
            }
            let root = ROOT.get_or_init(|| Regex::new(r"^\\?([\w\\]+)::").expect("valid"));
            let caps = root.captures(receiver)?;
            let fqn = engine.class_ref(ctx, &caps[1])?;
            engine.class(&fqn)
        })
    }

    /// The class named before a controller action.
    fn action_class(&mut self, path: &Path, text: &str, site: &Site) -> Option<Arc<ClassInfo>> {
        let name = site.class.clone()?;
        self.with_context(path, text, site.start, |engine, ctx| {
            let fqn = engine.class_ref(ctx, &name)?;
            engine.class(&fqn)
        })
    }

    // ---- lookup --------------------------------------------------------

    /// What the string of `site` names. `None`: not judged, `Some(None)`:
    /// the project has no such thing.
    fn resolve_site(
        &mut self,
        idx: &Arc<Index>,
        path: &Path,
        text: &str,
        site: &Site,
    ) -> Option<Option<Found>> {
        let name = &text[site.start..site.end];
        if name.is_empty() {
            return None;
        }
        let from_list = |items: &[Item], missing: bool| -> Option<Option<Found>> {
            match pick(items, name) {
                Some(item) => Some(Some(found(item))),
                None => missing.then_some(None),
            }
        };
        match site.kind {
            Kind::Config => {
                if let Some(item) = pick(&idx.config, name) {
                    return Some(Some(found(item)));
                }
                let first = name.split('.').next().unwrap_or(name);
                if !idx.config_files.contains(first) {
                    return None;
                }
                let parent = name.rsplit_once('.').map(|(parent, _)| parent)?;
                (pick(&idx.config, parent).is_some()).then_some(None)
            }
            Kind::Env => {
                if let Some(item) = pick(&idx.env, name) {
                    return Some(Some(found(item)));
                }
                (idx.has_env && !site.has_default).then_some(None)
            }
            Kind::Route => from_list(
                &idx.routes,
                !idx.routes.is_empty() && !name.contains(['{', '$']),
            ),
            Kind::View => {
                if name.contains("::") {
                    return None;
                }
                from_list(&idx.views, idx.has_views)
            }
            Kind::Translation => {
                if let Some(item) = pick(&idx.translations, name) {
                    return Some(Some(found(item)));
                }
                let group = name.split('.').next().unwrap_or(name);
                (name.contains('.') && idx.translation_groups.contains(group)).then_some(None)
            }
            Kind::Asset => {
                if name.contains("://") || name.starts_with("//") {
                    return None;
                }
                let path = idx.root.join("public").join(name.trim_start_matches('/'));
                if path.exists() {
                    return Some(Some(Found {
                        detail: relative(&idx.root, &path),
                        loc: Some(Loc {
                            path,
                            line: 0,
                            column: 0,
                        }),
                        class: None,
                    }));
                }
                idx.root.join("public").is_dir().then_some(None)
            }
            Kind::Vite => {
                let path = idx.root.join(name.trim_start_matches('/'));
                if path.exists() {
                    return Some(Some(Found {
                        detail: relative(&idx.root, &path),
                        loc: Some(Loc {
                            path,
                            line: 0,
                            column: 0,
                        }),
                        class: None,
                    }));
                }
                name.contains('/').then_some(None)
            }
            Kind::Middleware => {
                if name.contains('\\') {
                    return None;
                }
                match pick(&idx.middleware, rule_name(name)) {
                    Some(item) => Some(Some(found(item))),
                    None => idx.has_middleware_source.then_some(None),
                }
            }
            Kind::Ability => from_list(&idx.abilities, false),
            Kind::Binding => {
                if let Some(item) = pick(&idx.bindings, name) {
                    return Some(Some(found(item)));
                }
                let class = self.class(name)?;
                Some(Some(Found {
                    detail: class.fqn.clone(),
                    loc: None,
                    class: Some(class.fqn.clone()),
                }))
            }
            Kind::Page => from_list(&idx.pages, idx.has_pages),
            Kind::Livewire => from_list(&idx.livewire, false),
            Kind::Rule => {
                let rule = idx
                    .rules
                    .iter()
                    .find(|r| rule_name(&r.name) == rule_name(name))?;
                Some(Some(found(rule)))
            }
            Kind::Section(section) => {
                let items = idx.section(section);
                match pick(&items, name) {
                    Some(item) => Some(Some(found(item))),
                    None => (!items.is_empty()).then_some(None),
                }
            }
            Kind::Path(base) => {
                let path = idx.root.join(base).join(name.trim_start_matches('/'));
                path.exists().then(|| {
                    Some(Found {
                        detail: relative(&idx.root, &path),
                        loc: Some(Loc {
                            path,
                            line: 0,
                            column: 0,
                        }),
                        class: None,
                    })
                })
            }
            Kind::Action => {
                let class = self.action_class(path, text, site)?;
                if name.starts_with("__") {
                    return None;
                }
                let methods = self.action_methods(&class, true);
                if let Some(item) = pick(&methods, name) {
                    return Some(Some(found(item)));
                }
                if !self.fully_known(&class) {
                    return None;
                }
                let has_call = self.lineage(&class).iter().any(|c| {
                    c.members.iter().any(|m| {
                        m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case("__call")
                    })
                });
                (!has_call).then_some(None)
            }
            Kind::Column => {
                let class = self.model_of_call(path, text, site)?;
                let columns = self.model_columns(idx, &class);
                pick(&columns, name).map(|item| Some(found(item)))
            }
            Kind::Relation => {
                let class = self.model_of_call(path, text, site)?;
                let relations = self.model_relations(&class);
                let first = name.split('.').next().unwrap_or(name);
                pick(&relations, first).map(|item| Some(found(item)))
            }
        }
    }

    // ---- completion ----------------------------------------------------

    /// Suggestions for the Laravel string the cursor is in. `None` when it
    /// is not in one, so the caller goes on with plain PHP.
    pub(super) fn laravel_complete(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Completion>> {
        let blade = is_blade(path);
        let byte = byte_offset(text, line, column)?;
        if blade && let Some((tag, start, end)) = blade_tag(text, byte) {
            let typed = &text[start..byte.min(end)];
            let from = position_of(text, start).1;
            let items = match tag {
                Tag::Component => &idx.components,
                Tag::Livewire => &idx.livewire,
            };
            return Some(named(typed, items, ItemKind::Class, from));
        }
        let site = site_at(text, byte, blade)?;
        let typed = &text[site.start..byte.min(site.end)];
        let from = position_of(text, site.start).1;
        let kind = item_kind(site.kind);
        let items = match site.kind {
            Kind::Config => named(typed, &idx.config, kind, from),
            Kind::Env => named(typed, &idx.env, kind, from),
            Kind::Route => named(typed, &idx.routes, kind, from),
            Kind::View => named(typed, &idx.views, kind, from),
            Kind::Translation => named(typed, &idx.translations, kind, from),
            Kind::Ability => named(typed, &idx.abilities, kind, from),
            Kind::Binding => named(typed, &idx.bindings, kind, from),
            Kind::Page => named(typed, &idx.pages, kind, from),
            Kind::Livewire => named(typed, &idx.livewire, kind, from),
            Kind::Middleware if typed.contains(':') => Vec::new(),
            Kind::Middleware => named(typed, &idx.middleware, kind, from),
            Kind::Section(section) => named(typed, &idx.section(section), kind, from),
            Kind::Asset => named(typed, &idx.paths("public", typed), kind, from),
            Kind::Vite => named(typed, &idx.paths("", typed), kind, from),
            Kind::Path(base) => named(typed, &idx.paths(base, typed), kind, from),
            Kind::Rule => {
                let tail = typed.rfind('|').map_or(0, |i| i + 1);
                let piece = &typed[tail..];
                if piece.contains(':') {
                    return Some(Vec::new());
                }
                let start = from + typed[..tail].encode_utf16().count() as u32;
                let rules: Vec<Item> = idx
                    .rules
                    .iter()
                    .map(|rule| Item {
                        name: rule_name(&rule.name).to_owned(),
                        detail: format!("{} — {}", rule.name, rule.detail),
                        ..rule.clone()
                    })
                    .collect();
                let mut out = named(piece, &rules, kind, start);
                for item in &mut out {
                    if idx
                        .rules
                        .iter()
                        .any(|r| rule_name(&r.name) == item.label && r.name.contains(':'))
                    {
                        item.insert_text.push(':');
                    }
                }
                out
            }
            Kind::Action => {
                let class = self.action_class(path, text, &site)?;
                let methods = self.action_methods(&class, false);
                named(typed, &methods, kind, from)
            }
            Kind::Column => {
                let class = self.model_of_call(path, text, &site)?;
                let columns = self.model_columns(&idx, &class);
                named(typed, &columns, kind, from)
            }
            Kind::Relation => {
                let class = self.model_of_call(path, text, &site)?;
                // After a dot the relation is the one named before it.
                let relations = self.model_relations(&class);
                let head = typed.rsplit_once('.').map_or("", |(head, _)| head);
                if head.is_empty() {
                    named(typed, &relations, kind, from)
                } else {
                    Vec::new()
                }
            }
        };
        Some(items)
    }

    // ---- definition and hover ------------------------------------------

    /// Where the Laravel string under the cursor is defined.
    pub(super) fn laravel_definition(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<Vec<Target>> {
        let blade = is_blade(path);
        let byte = byte_offset(text, line, column)?;
        if blade && let Some((tag, start, end)) = blade_tag(text, byte) {
            let items = match tag {
                Tag::Component => &idx.components,
                Tag::Livewire => &idx.livewire,
            };
            let item = pick(items, &text[start..end])?;
            return Some(self.targets(item.loc.as_ref(), item.class.as_deref()));
        }
        let site = site_at(text, byte, blade)?;
        if site.kind == Kind::Rule {
            let contents = &text[site.start..site.end];
            let at = byte - site.start;
            let (piece, ..) = rule_pieces(contents)
                .into_iter()
                .find(|(_, start, end)| *start <= at && at <= *end)?;
            let rule = idx
                .rules
                .iter()
                .find(|r| rule_name(&r.name) == rule_name(piece))?;
            return Some(self.targets(rule.loc.as_ref(), None));
        }
        let hit = self.resolve_site(&idx, path, text, &site)??;
        Some(self.targets(hit.loc.as_ref(), hit.class.as_deref()))
    }

    fn targets(&mut self, loc: Option<&Loc>, class: Option<&str>) -> Vec<Target> {
        if let Some(fqn) = class
            && let Some(class) = self.class(fqn)
            && !class.path.as_os_str().is_empty()
        {
            return vec![Target {
                path: class.path.clone(),
                line: class.line,
                column: class.column,
            }];
        }
        loc.map(|loc| vec![target_of(loc)]).unwrap_or_default()
    }

    /// Documentation for the Laravel string under the cursor.
    pub(super) fn laravel_hover(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        let blade = is_blade(path);
        let byte = byte_offset(text, line, column)?;
        let result = |start: usize, end: usize, markdown: String| HoverResult {
            markdown,
            line: position_of(text, start).0,
            start_column: position_of(text, start).1,
            end_column: position_of(text, end).1,
        };
        if blade && let Some((tag, start, end)) = blade_tag(text, byte) {
            let (items, what) = match tag {
                Tag::Component => (&idx.components, "Blade component"),
                Tag::Livewire => (&idx.livewire, "Livewire component"),
            };
            let name = &text[start..end];
            let hit = found(pick(items, name)?);
            let markdown = describe(what, name, &hit, false, &idx.root);
            return Some(result(start, end, markdown));
        }
        let site = site_at(text, byte, blade)?;
        if site.kind == Kind::Rule {
            let contents = &text[site.start..site.end];
            let at = byte - site.start;
            let (piece, from, to) = rule_pieces(contents)
                .into_iter()
                .find(|(_, start, end)| *start <= at && at <= *end)?;
            let rule = idx
                .rules
                .iter()
                .find(|r| rule_name(&r.name) == rule_name(piece))?;
            let markdown = describe("Validation rule", &rule.name, &found(rule), true, &idx.root);
            return Some(result(site.start + from, site.start + to, markdown));
        }
        let hit = self.resolve_site(&idx, path, text, &site)??;
        let name = &text[site.start..site.end];
        let plain = matches!(site.kind, Kind::Translation | Kind::Column | Kind::Relation);
        let markdown = describe(label(site.kind), name, &hit, plain, &idx.root);
        Some(result(site.start, site.end, markdown))
    }

    /// Strings that name something the project does not have.
    pub(super) fn laravel_problems(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
    ) -> Vec<Problem> {
        const MAX: usize = 200;
        let blade = is_blade(path);
        let mut out = Vec::new();
        for site in sites(text, blade) {
            if out.len() >= MAX {
                break;
            }
            let severity = match site.kind {
                Kind::Rule
                | Kind::Column
                | Kind::Relation
                | Kind::Livewire
                | Kind::Ability
                | Kind::Binding => continue,
                // Packages add routes and middleware this scan cannot see.
                Kind::Route | Kind::Middleware => Severity::Hint,
                _ => Severity::Warning,
            };
            let Some(None) = self.resolve_site(&idx, path, text, &site) else {
                continue;
            };
            let name = &text[site.start..site.end];
            let message = match site.kind {
                Kind::Env => format!("Environment variable `{name}` is not set in .env"),
                Kind::Section(_) => format!("`{name}` is not defined in the config"),
                kind => format!("{} `{name}` not found", label(kind)),
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

    /// Fixes for a missing view, environment variable or Inertia page.
    pub(super) fn laravel_actions(
        &mut self,
        idx: Arc<Index>,
        path: &Path,
        text: &str,
        byte: usize,
        out: &mut Vec<Action>,
    ) {
        let Some(site) = site_at(text, byte, is_blade(path)) else {
            return;
        };
        let Some(None) = self.resolve_site(&idx, path, text, &site) else {
            return;
        };
        let name = text[site.start..site.end].to_owned();
        let create = |title: String, file: PathBuf, body: &str| Action {
            title,
            edits: Vec::new(),
            files: vec![FileChange {
                path: file,
                line: 0,
                column: 0,
                text: body.to_owned(),
            }],
        };
        match site.kind {
            Kind::View => {
                let file = idx
                    .root
                    .join("resources/views")
                    .join(format!("{}.blade.php", name.replace('.', "/")));
                out.push(create(
                    format!("Create view `{name}`"),
                    file,
                    "<div>\n    \n</div>\n",
                ));
            }
            Kind::Env => {
                for file in [".env", ".env.example"] {
                    out.push(Action {
                        title: format!("Add `{name}` to {file}"),
                        edits: Vec::new(),
                        files: vec![append_line(idx.root.join(file), &format!("{name}="))],
                    });
                }
            }
            Kind::Page => {
                let extension = idx
                    .pages
                    .iter()
                    .filter_map(|p| p.loc.as_ref())
                    .filter_map(|l| l.path.extension().and_then(|e| e.to_str()))
                    .next()
                    .unwrap_or("vue")
                    .to_owned();
                let base = [
                    "resources/js/Pages",
                    "resources/js/pages",
                    "resources/ts/Pages",
                ]
                .into_iter()
                .find(|dir| idx.root.join(dir).is_dir())
                .unwrap_or("resources/js/Pages");
                let body = if extension == "vue" {
                    "<script setup>\n</script>\n\n<template>\n    <div></div>\n</template>\n"
                } else {
                    "export default function Page() {\n    return <div />;\n}\n"
                };
                out.push(create(
                    format!("Create page `{name}`"),
                    idx.root.join(base).join(format!("{name}.{extension}")),
                    body,
                ));
            }
            _ => {}
        }
    }
}

/// A change that adds `line` at the end of the file `path`.
fn append_line(path: PathBuf, line: &str) -> FileChange {
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let rows = existing.lines().count() as u32;
    if !existing.is_empty() && !existing.ends_with('\n') {
        let last = existing.lines().last().unwrap_or("");
        return FileChange {
            path,
            line: rows - 1,
            column: last.encode_utf16().count() as u32,
            text: format!("\n{line}\n"),
        };
    }
    FileChange {
        path,
        line: rows,
        column: 0,
        text: format!("{line}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "artisan", "<?php");
        write(
            r,
            "composer.json",
            r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
        );
        write(
            r,
            "config/app.php",
            "<?php\nreturn [\n    'name' => 'Galaxy',\n    'debug' => true,\n];\n",
        );
        write(r, ".env", "APP_NAME=Galaxy\nAPP_KEY=secret");
        write(
            r,
            "routes/web.php",
            "<?php\nRoute::get('/', fn () => 1)->name('home');\n",
        );
        write(r, "resources/views/welcome.blade.php", "hi");
        write(r, "resources/views/layouts/app.blade.php", "hi");
        write(
            r,
            "resources/views/components/forms/input.blade.php",
            "<input>",
        );
        write(
            r,
            "app/Http/Controllers/PostController.php",
            "<?php\nnamespace App\\Http\\Controllers;\n\nclass PostController\n{\n    public function index() {}\n\n    public function show() {}\n}\n",
        );
        write(
            r,
            "app/Models/User.php",
            "<?php\nnamespace App\\Models;\n\nclass User\n{\n}\n",
        );
        write(
            r,
            "database/migrations/2024_01_01_000000_create_users_table.php",
            "<?php\nSchema::create('users', function (Blueprint $table) {\n    $table->id();\n    $table->string('email');\n    $table->timestamps();\n});\n",
        );
        dir
    }

    /// `text` with `|` at the cursor: the text and the position.
    fn cursor(text: &str) -> (String, u32, u32) {
        let marker = text.find('|').expect("cursor");
        let before = &text[..marker];
        let line = before.matches('\n').count() as u32;
        let column = before.rsplit('\n').next().unwrap().encode_utf16().count() as u32;
        (text.replacen('|', "", 1), line, column)
    }

    #[test]
    fn completes_names_and_replaces_the_whole_key() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("app/Http/X.php");
        let (text, line, column) = cursor("<?php\nconfig('app.na|');\n");
        let items = engine.complete(&path, &text, line, column);
        let item = items
            .iter()
            .find(|i| i.label == "app.name")
            .expect("app.name");
        assert_eq!(item.replace_from, Some(8));
        assert_eq!(item.detail.as_deref(), Some("Galaxy"));

        let (text, line, column) = cursor("<?php\nroute('|');\n");
        let labels: Vec<String> = engine
            .complete(&path, &text, line, column)
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, vec!["home"]);

        let (text, line, column) = cursor("<?php\nenv('APP_|');\n");
        let labels: Vec<String> = engine
            .complete(&path, &text, line, column)
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, vec!["APP_KEY", "APP_NAME"]);
    }

    #[test]
    fn completes_controller_actions_and_model_columns() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("routes/web.php");
        let (text, line, column) = cursor(
            "<?php\nuse App\\Http\\Controllers\\PostController;\nRoute::get('/p', [PostController::class, '|']);\n",
        );
        let labels: Vec<String> = engine
            .complete(&path, &text, line, column)
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, vec!["index", "show"]);

        let (text, line, column) =
            cursor("<?php\nnamespace App;\nuse App\\Models\\User;\n\nUser::where('em|');\n");
        let labels: Vec<String> = engine
            .complete(&dir.path().join("app/X.php"), &text, line, column)
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, vec!["email"]);
    }

    #[test]
    fn jumps_to_where_a_name_is_defined() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("app/X.php");
        let (text, line, column) = cursor("<?php\nroute('ho|me');\n");
        let targets = engine.definition(&path, &text, line, column);
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with("routes/web.php"));
        assert_eq!(targets[0].line, 1);

        let (text, line, column) = cursor("<?php\nview('layouts.a|pp');\n");
        let targets = engine.definition(&path, &text, line, column);
        assert!(
            targets[0]
                .path
                .ends_with("resources/views/layouts/app.blade.php")
        );

        let (text, line, column) = cursor("<?php\nconfig('app.de|bug');\n");
        let targets = engine.definition(&path, &text, line, column);
        assert!(targets[0].path.ends_with("config/app.php"));
        assert_eq!(targets[0].line, 3);
    }

    #[test]
    fn hover_shows_the_value() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("app/X.php");
        let (text, line, column) = cursor("<?php\nconfig('app.na|me');\n");
        let hover = engine.hover(&path, &text, line, column).expect("hover");
        assert!(hover.markdown.contains("`Galaxy`"), "{}", hover.markdown);
        assert!(
            hover.markdown.contains("config/app.php:3"),
            "{}",
            hover.markdown
        );
        assert_eq!((hover.start_column, hover.end_column), (8, 16));

        let (text, line, column) = cursor("<?php\nenv('APP_K|EY');\n");
        let hover = engine.hover(&path, &text, line, column).expect("hover");
        assert!(hover.markdown.contains("••••••"), "{}", hover.markdown);
        assert!(!hover.markdown.contains("secret"));
    }

    #[test]
    fn flags_names_the_project_lacks() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("app/X.php");
        let text = "<?php\nview('welcome');\nview('gone');\nenv('NOPE');\nenv('NOPE', 'fallback');\nconfig('app.name');\nconfig('app.gone');\nroute('nowhere');\nconfig('vendor.pkg');\n";
        let problems = engine.diagnostics(&path, text);
        let found: Vec<(u32, &str)> = problems
            .iter()
            .map(|p| (p.start_line, p.message.as_str()))
            .collect();
        assert_eq!(
            found,
            vec![
                (2, "View `gone` not found"),
                (3, "Environment variable `NOPE` is not set in .env"),
                (6, "Config `app.gone` not found"),
                (7, "Route `nowhere` not found"),
            ]
        );
        assert_eq!(problems[3].severity, Severity::Hint);
    }

    #[test]
    fn offers_to_create_what_is_missing() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("app/X.php");
        let (text, line, column) = cursor("<?php\nview('admin.da|sh');\n");
        let actions = engine.actions(&path, &text, line, column);
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].title, "Create view `admin.dash`");
        assert!(
            actions[0].files[0]
                .path
                .ends_with("resources/views/admin/dash.blade.php")
        );

        let (text, line, column) = cursor("<?php\nenv('NE|W_KEY');\n");
        let actions = engine.actions(&path, &text, line, column);
        assert_eq!(actions.len(), 2);
        // `.env` has no final newline, so the new line starts after APP_KEY.
        let change = &actions[0].files[0];
        assert_eq!((change.line, change.column), (1, 14));
        assert_eq!(change.text, "\nNEW_KEY=\n");
    }

    #[test]
    fn blade_files_complete_views_and_components() {
        let dir = project();
        let mut engine = PhpEngine::new(dir.path());
        let path = dir.path().join("resources/views/page.blade.php");
        let (text, line, column) = cursor("<div>\n@extends('layouts.a|')\n</div>\n");
        let labels: Vec<String> = engine
            .complete(&path, &text, line, column)
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, vec!["layouts.app"]);

        let (text, line, column) = cursor("<div>\n<x-forms.in|\n</div>\n");
        let items = engine.complete(&path, &text, line, column);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "forms.input");
        assert_eq!(items[0].replace_from, Some(3));

        // Plain Blade text gets no PHP suggestions.
        let (text, line, column) = cursor("<div>Wel|come</div>\n");
        assert!(engine.complete(&path, &text, line, column).is_empty());

        let problems = engine.diagnostics(
            &path,
            "@include('missing.view')\n<p>{{ config('app.name') }}</p>\n",
        );
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].message, "View `missing.view` not found");
    }
}
