//! Completion, definition and hover for PHP, answered from sources.
//!
//! Nothing here knows a framework. Classes come from Composer's autoload
//! data (or a workspace scan), and types from declarations and docblocks,
//! so facades (`@method`), ide-helper models (`@property`), container
//! helpers (`@template` + `class-string<T>`) and plain PHP all work the
//! same way.

use crate::php::laravel::Index;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use regex::Regex;
use tree_sitter::{Node, Tree};

use super::composer::{Autoload, ClassName};
use super::generics;
use super::parse::{
    ClassInfo, ClassKind, FileSymbols, FunctionInfo, Member, MemberKind, Visibility, parse_file,
    parse_with_tree, resolve_name, resolve_type,
};
use super::stubs::{Stubs, is_stub_path, materialize};

mod assist;
mod diag;
mod laravel;
mod refs;

pub use assist::{Action, Edit, FileChange, OutlineSymbol, Signature, outline};
pub use diag::{Problem, Severity};
pub use refs::{Occurrence, RefQuery, finish, valid_name};

const MAX_ITEMS: usize = 200;
const MAX_DEPTH: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Class,
    Interface,
    Trait,
    Enum,
    Method,
    Property,
    Constant,
    Variable,
    Function,
    Keyword,
}

/// Text to insert at a position of the original document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    pub line: u32,
    pub column: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: String,
    pub is_snippet: bool,
    pub kind: ItemKind,
    pub sort: String,
    /// Extra edits elsewhere in the file, such as a `use` import.
    pub extra: Vec<Insertion>,
    /// UTF-16 column where the text to replace starts, when it is not just
    /// the word before the cursor (`app.na` in `config('app.na')`).
    pub replace_from: Option<u32>,
}

/// Where a symbol is declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverResult {
    pub markdown: String,
    pub line: u32,
    pub start_column: u32,
    pub end_column: u32,
}

#[derive(Debug)]
struct Parsed {
    mtime: Option<SystemTime>,
    file: Arc<FileSymbols>,
}

#[derive(Debug, Clone)]
struct Ctx<'a> {
    src: &'a str,
    tree: &'a Tree,
    file: &'a FileSymbols,
    byte: usize,
    class: Option<Arc<ClassInfo>>,
}

enum Symbol {
    Class(String),
    Member {
        class: String,
        name: String,
        call: bool,
    },
    Function(String),
    Variable(String),
}

enum Lookup {
    Found(Option<String>),
    Missing,
}

/// Autoload data and class names of a project, built off the UI thread.
#[derive(Debug)]
pub struct Built {
    autoload: Autoload,
    names: Vec<ClassName>,
    scanned: HashMap<String, PathBuf>,
    laravel: Option<Index>,
}

/// Reads Composer's data and walks the autoload directories (and, without
/// Composer data, the source tree). Slow on big `vendor/` trees, so callers
/// run it on a worker thread.
pub fn build(root: &Path) -> Built {
    let autoload = Autoload::load(root);
    let mut names = autoload.all_classes();
    names.extend(Stubs::get().class_names().iter().cloned());
    let mut scanned = HashMap::new();
    if autoload.is_empty() {
        for (fqn, path) in scan_sources(root) {
            let short = fqn.rsplit('\\').next().unwrap_or(&fqn).to_owned();
            names.push(ClassName {
                short,
                fqn: fqn.clone(),
                vendor: false,
            });
            scanned.entry(fqn.to_lowercase()).or_insert(path);
        }
    }
    Built {
        autoload,
        names,
        scanned,
        laravel: Index::detect(root),
    }
}

/// The PHP project containing `file`: the nearest ancestor with a
/// `composer.json`, else one with `.git`, else the file's own folder.
pub fn project_root(file: &Path) -> PathBuf {
    let marker = |name: &str| {
        file.ancestors()
            .skip(1)
            .find(|dir| dir.join(name).exists())
            .map(Path::to_path_buf)
    };
    marker("composer.json")
        .or_else(|| marker(".git"))
        .or_else(|| file.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[derive(Debug)]
pub struct PhpEngine {
    root: PathBuf,
    autoload: Autoload,
    parsed: HashMap<PathBuf, Parsed>,
    open: HashMap<PathBuf, Arc<FileSymbols>>,
    names: Option<Arc<Vec<ClassName>>>,
    scanned: Option<HashMap<String, PathBuf>>,
    deferred: bool,
    building: bool,
    built_at: Option<Instant>,
    functions: Option<HashMap<String, Arc<FunctionInfo>>>,
    /// What the project defines, when it is a Laravel application.
    laravel: Option<Arc<Index>>,
    /// Nesting of return types worked out from method bodies.
    infer_depth: u8,
}

impl PhpEngine {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            autoload: Autoload::load(root),
            parsed: HashMap::new(),
            open: HashMap::new(),
            names: None,
            scanned: None,
            deferred: false,
            building: false,
            built_at: None,
            functions: None,
            laravel: Index::detect(root).map(Arc::new),
            infer_depth: 0,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// An engine that reads nothing yet: Composer data and class names
    /// arrive through [`Self::install`] from a worker thread. Until then
    /// it answers only from open buffers.
    pub fn pending(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            autoload: Autoload::default(),
            parsed: HashMap::new(),
            open: HashMap::new(),
            names: None,
            scanned: None,
            deferred: true,
            building: true,
            built_at: None,
            functions: None,
            laravel: None,
            infer_depth: 0,
        }
    }

    /// The index was installed at least once.
    pub fn is_ready(&self) -> bool {
        self.built_at.is_some()
    }

    /// Until [`Self::install`] runs, class names are not offered and
    /// nothing walks the file system on the calling thread.
    pub fn defer_index(&mut self) {
        self.deferred = true;
    }

    /// `true` when the caller should run [`build`] and [`Self::install`]:
    /// no index yet, or one older than `max_age`. Asks once until installed.
    pub fn wants_index(&mut self, max_age: Duration) -> bool {
        if self.building {
            return false;
        }
        let stale = self.built_at.is_none_or(|at| at.elapsed() > max_age);
        self.building = stale;
        stale
    }

    pub fn install(&mut self, built: Built) {
        self.autoload = built.autoload;
        self.scanned = Some(built.scanned);
        self.names = Some(Arc::new(built.names));
        self.functions = None;
        self.laravel = built.laravel.map(Arc::new);
        self.building = false;
        self.built_at = Some(Instant::now());
    }

    /// Registers the text of an open buffer; it wins over the file on disk.
    pub fn open_document(&mut self, path: &Path, text: &str) {
        let file = Arc::new(parse_file(path, text));
        self.open.insert(path.to_path_buf(), file);
    }

    pub fn close_document(&mut self, path: &Path) {
        self.open.remove(path);
    }

    /// Forgets what was read from disk, e.g. after files were added or
    /// `composer dump-autoload` ran.
    pub fn reset_index(&mut self) {
        self.autoload = Autoload::load(&self.root);
        self.parsed.clear();
        self.names = None;
        self.scanned = None;
        self.functions = None;
    }

    // ---- lookup -----------------------------------------------------

    fn load(&mut self, path: &Path) -> Option<Arc<FileSymbols>> {
        if let Some(file) = self.open.get(path) {
            return Some(file.clone());
        }
        let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if let Some(cached) = self.parsed.get(path)
            && cached.mtime == mtime
        {
            return Some(cached.file.clone());
        }
        let text = std::fs::read_to_string(path).ok()?;
        let file = Arc::new(parse_file(path, &text));
        self.parsed.insert(
            path.to_path_buf(),
            Parsed {
                mtime,
                file: file.clone(),
            },
        );
        Some(file)
    }

    fn scanned(&mut self) -> &HashMap<String, PathBuf> {
        if self.scanned.is_none() {
            if self.deferred {
                static EMPTY: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
                return EMPTY.get_or_init(HashMap::new);
            }
            self.scanned = Some(if self.autoload.is_empty() {
                scan_sources(&self.root)
                    .into_iter()
                    .map(|(fqn, path)| (fqn.to_lowercase(), path))
                    .collect()
            } else {
                HashMap::new()
            });
        }
        self.scanned.as_ref().expect("set above")
    }

    pub fn class(&mut self, fqn: &str) -> Option<Arc<ClassInfo>> {
        let fqn = generics::base(fqn).trim_start_matches('\\');
        for file in self.open.values() {
            if let Some(found) = file
                .classes
                .iter()
                .find(|c| c.fqn.eq_ignore_ascii_case(fqn))
            {
                return Some(found.clone());
            }
        }
        let path = match self.autoload.path_for(fqn) {
            Some(path) => Some(path),
            None => self.scanned().get(&fqn.to_lowercase()).cloned(),
        };
        if let Some(path) = path
            && let Some(file) = self.load(&path)
            && let Some(found) = file
                .classes
                .iter()
                .find(|c| c.fqn.eq_ignore_ascii_case(fqn))
        {
            return Some(found.clone());
        }
        self.builtin().class(fqn)
    }

    /// PHP's own declarations. Before the index is installed they are not
    /// parsed on the calling thread, so lookups see none.
    fn builtin(&self) -> &'static Stubs {
        static NONE: OnceLock<Stubs> = OnceLock::new();
        if self.deferred && self.built_at.is_none() {
            NONE.get_or_init(Stubs::default)
        } else {
            Stubs::get()
        }
    }

    fn function(&mut self, fqn: &str) -> Option<Arc<FunctionInfo>> {
        let key = fqn.trim_start_matches('\\').to_lowercase();
        for file in self.open.values() {
            if let Some(found) = file.functions.iter().find(|f| f.fqn.to_lowercase() == key) {
                return Some(found.clone());
            }
        }
        if let Some(found) = self.function_index().get(&key) {
            return Some(found.clone());
        }
        self.builtin().function(&key)
    }

    fn function_index(&mut self) -> &HashMap<String, Arc<FunctionInfo>> {
        if self.functions.is_none() {
            let mut map = HashMap::new();
            let files: Vec<PathBuf> = self.autoload.function_files().to_vec();
            for path in files {
                if let Some(file) = self.load(&path) {
                    for function in &file.functions {
                        map.entry(function.fqn.to_lowercase())
                            .or_insert_with(|| function.clone());
                    }
                }
            }
            self.functions = Some(map);
        }
        self.functions.as_ref().expect("set above")
    }

    fn class_names(&mut self) -> Arc<Vec<ClassName>> {
        if self.names.is_none() {
            if self.deferred {
                return Arc::new(Vec::new());
            }
            let built = build(&self.root);
            self.install(built);
        }
        self.names.clone().expect("set above")
    }

    /// The class and what it inherits from or mixes in, nearest first.
    fn lineage(&mut self, class: &Arc<ClassInfo>) -> Vec<Arc<ClassInfo>> {
        let mut out = vec![class.clone()];
        let mut seen = HashSet::from([class.fqn.to_lowercase()]);
        let mut index = 0;
        while index < out.len() && out.len() < 48 {
            let current = out[index].clone();
            index += 1;
            let parents: Vec<String> = current
                .traits
                .iter()
                .chain(&current.extends)
                .chain(&current.implements)
                .chain(&current.mixins)
                .cloned()
                .collect();
            for parent in parents {
                if seen.insert(parent.to_lowercase())
                    && let Some(found) = self.class(&parent)
                {
                    out.push(found);
                }
            }
        }
        out
    }

    fn find_member(
        &mut self,
        class: &Arc<ClassInfo>,
        name: &str,
        call: bool,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        let name = name.trim_start_matches('$');
        for owner in self.lineage(class) {
            let hit = owner.members.iter().find(|m| {
                if call {
                    m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case(name)
                } else {
                    m.kind != MemberKind::Method && m.name == name
                }
            });
            if let Some(member) = hit {
                return Some((owner.clone(), member.clone()));
            }
        }
        None
    }

    /// `$object->name` where no property `name` exists but the class has
    /// `__get`: PHP routes the read through it, and libraries commonly
    /// back such reads with a method of that name (Eloquent relations).
    /// Used for navigation and hover only; the method's return type is not
    /// the type of the property.
    fn magic_property_method(
        &mut self,
        class: &Arc<ClassInfo>,
        name: &str,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        let has_getter = self.lineage(class).iter().any(|owner| {
            owner
                .members
                .iter()
                .any(|m| m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case("__get"))
        });
        if has_getter {
            self.find_member(class, name, true)
        } else {
            None
        }
    }

    /// The declaration a member access points at, with the `__get` fallback
    /// for reads of properties that are really methods.
    fn find_member_or_magic(
        &mut self,
        class: &Arc<ClassInfo>,
        name: &str,
        call: bool,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        self.find_member(class, name, call).or_else(|| {
            if call {
                self.forwarded_method(class, name)
            } else {
                self.magic_property_method(class, name)
            }
        })
    }

    /// `Model::create()` and friends: Eloquent models forward calls they do
    /// not declare to a query builder through `__call` and `__callStatic`.
    fn forwarded_method(
        &mut self,
        class: &Arc<ClassInfo>,
        name: &str,
    ) -> Option<(Arc<ClassInfo>, Member)> {
        const MODEL: &str = "Illuminate\\Database\\Eloquent\\Model";
        const BUILDER: &str = "Illuminate\\Database\\Eloquent\\Builder";
        if !self.lineage(class).iter().any(|c| c.fqn == MODEL) {
            return None;
        }
        let builder = self.class(BUILDER)?;
        self.find_member(&builder, name, true)
    }

    /// The `Builder<Model>` type that calls a model or relation does not
    /// declare are forwarded to, when `chain` is one of those.
    fn forwarding_builder(
        &mut self,
        chain: &[(Arc<ClassInfo>, HashMap<String, String>)],
    ) -> Option<String> {
        const MODEL: &str = "Illuminate\\Database\\Eloquent\\Model";
        const RELATION: &str = "Illuminate\\Database\\Eloquent\\Relations\\Relation";
        const BUILDER: &str = "Illuminate\\Database\\Eloquent\\Builder";
        self.class(BUILDER)?;
        let model = if chain.iter().any(|(c, _)| c.fqn == MODEL) {
            chain[0].0.fqn.clone()
        } else {
            let (_, env) = chain.iter().find(|(c, _)| c.fqn == RELATION)?;
            env.get("TRelatedModel").filter(|m| !m.is_empty())?.clone()
        };
        Some(format!("{BUILDER}<{model}>"))
    }

    // ---- type evaluation ---------------------------------------------

    fn class_ref(&mut self, ctx: &Ctx<'_>, name: &str) -> Option<String> {
        match name.to_ascii_lowercase().as_str() {
            "self" | "static" | "$this" => ctx.class.as_ref().map(|c| c.fqn.clone()),
            "parent" => ctx.class.as_ref()?.extends.first().cloned(),
            _ => Some(resolve_name(name, &ctx.file.namespace, &ctx.file.uses)),
        }
    }

    fn class_from_class_const(&mut self, ctx: &Ctx<'_>, arg: &str) -> Option<String> {
        let name = arg.trim().strip_suffix("::class")?;
        if name.is_empty() || !name.bytes().all(is_name_char) {
            return None;
        }
        self.class_ref(ctx, name)
    }

    fn function_return(&mut self, ctx: &Ctx<'_>, name: &str, args: &str) -> Option<String> {
        let mut candidates = Vec::new();
        if !name.starts_with('\\') && !ctx.file.namespace.is_empty() && !name.contains('\\') {
            candidates.push(format!("{}\\{}", ctx.file.namespace, name));
        }
        candidates.push(name.trim_start_matches('\\').to_owned());
        let function = candidates.iter().find_map(|fqn| self.function(fqn))?;
        let parts = split_args(args);
        if let Some(index) = function.class_arg {
            return self.class_from_class_const(ctx, parts.get(index)?);
        }
        let mut env = HashMap::new();
        for (template, index) in &function.class_params {
            if let Some(class) = parts
                .get(*index)
                .and_then(|a| self.class_from_class_const(ctx, a))
            {
                env.insert(template.clone(), class);
            }
        }
        let out = generics::subst(function.ty.as_deref()?, &env, "");
        (!out.is_empty()).then_some(out)
    }

    /// The class and what it inherits, each with the types its template
    /// parameters stand for when reached from `ty` (`Coll<int,User>`).
    fn lineage_env(&mut self, ty: &str) -> Vec<(Arc<ClassInfo>, HashMap<String, String>)> {
        let (base, args) = generics::split(ty);
        let Some(class) = self.class(base) else {
            return Vec::new();
        };
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let env = generics::bind(&class.templates, args);
        let mut seen = HashSet::from([class.fqn.to_lowercase()]);
        let mut out = vec![(class, env)];
        let mut index = 0;
        while index < out.len() && out.len() < 48 {
            let (current, env) = out[index].clone();
            index += 1;
            let parents: Vec<String> = current
                .traits
                .iter()
                .chain(&current.extends)
                .chain(&current.implements)
                .chain(&current.mixins)
                .cloned()
                .collect();
            for parent in parents {
                if !seen.insert(parent.to_lowercase()) {
                    continue;
                }
                let Some(found) = self.class(&parent) else {
                    continue;
                };
                let args: Vec<String> = current
                    .generic_parents
                    .iter()
                    .find(|g| generics::base(g).eq_ignore_ascii_case(&parent))
                    .map(|g| {
                        generics::split(g)
                            .1
                            .iter()
                            .map(|arg| generics::subst(arg, &env, ""))
                            .collect()
                    })
                    .unwrap_or_default();
                let env = generics::bind(&found.templates, args);
                out.push((found, env));
            }
        }
        out
    }

    /// The nearest declaration of `name`, preferring one that says what it
    /// returns over an untyped override (`@inheritDoc`).
    fn pick_member(
        chain: &[(Arc<ClassInfo>, HashMap<String, String>)],
        name: &str,
        call: bool,
    ) -> Option<(usize, Member)> {
        let name = name.trim_start_matches('$');
        let mut first = None;
        for (index, (owner, _)) in chain.iter().enumerate() {
            let hit = owner.members.iter().find(|m| {
                if call {
                    m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case(name)
                } else {
                    m.kind != MemberKind::Method && m.name == name
                }
            });
            if let Some(member) = hit {
                if member.ty.is_some() || member.class_arg.is_some() {
                    return Some((index, member.clone()));
                }
                first.get_or_insert((index, member.clone()));
            }
        }
        first
    }

    /// The type of `receiver->name` (a read) or `receiver->name(args)`,
    /// with template parameters replaced by what the receiver binds them to.
    fn member_type(
        &mut self,
        ctx: &Ctx<'_>,
        receiver: &str,
        name: &str,
        args: Option<&str>,
    ) -> Option<String> {
        let chain = self.lineage_env(receiver);
        if chain.is_empty() {
            return None;
        }
        let call = args.is_some();
        let Some((index, member)) = Self::pick_member(&chain, name, call) else {
            if call {
                // `$model->where()` and `$relation->create()` reach the
                // query builder through `__call`.
                let builder = self.forwarding_builder(&chain)?;
                return self.member_type(ctx, &builder, name, args);
            }
            // A read of something that is not a property, on a class with
            // `__get`: Eloquent exposes a relation method that way, and
            // what the read gives is what the relation resolves to.
            let has_getter = chain.iter().any(|(owner, _)| {
                owner
                    .members
                    .iter()
                    .any(|m| m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case("__get"))
            });
            if !has_getter {
                return None;
            }
            let relation = self.member_type(ctx, receiver, name, Some(""))?;
            return self.member_type(ctx, &relation, "getResults", Some(""));
        };
        let parts = args.map(split_args).unwrap_or_default();
        if let Some(index) = member.class_arg {
            return self.class_from_class_const(ctx, parts.get(index)?);
        }
        let (owner, owner_env) = &chain[index];
        let mut env = owner_env.clone();
        for (template, position) in &member.class_params {
            if let Some(class) = parts
                .get(*position)
                .and_then(|arg| self.class_from_class_const(ctx, arg))
            {
                env.insert(template.clone(), class);
            }
        }
        let mut declared = member.ty.clone();
        // `: HasOne` says little; the body (`$this->hasOne(X::class)`) says which.
        if call && let Some(body) = &member.body_return {
            let generic_class = declared
                .as_deref()
                .and_then(|ty| self.class(generics::base(ty)))
                .is_some_and(|class| !class.templates.is_empty());
            let bare = declared.as_deref().is_none_or(|ty| !ty.contains('<'));
            if bare
                && (declared.is_none() || generic_class)
                && let Some(inferred) = self.infer_body(&chain[0].0, owner, body)
                && declared.as_deref().is_none_or(|ty| {
                    generics::base(&inferred).eq_ignore_ascii_case(generics::base(ty))
                })
            {
                return Some(inferred);
            }
        }
        let out = generics::subst(&declared.take()?, &env, receiver);
        (!out.is_empty()).then_some(out)
    }

    /// Evaluates the `return` expression of a method as written in its own
    /// file, with `$this` standing for `this`.
    fn infer_body(
        &mut self,
        this: &Arc<ClassInfo>,
        owner: &Arc<ClassInfo>,
        expr: &str,
    ) -> Option<String> {
        if self.infer_depth >= 3 {
            return None;
        }
        let file = self.load(&owner.path)?;
        let ctx = Ctx {
            src: "",
            tree: empty_tree(),
            file: &file,
            byte: 0,
            class: Some(this.clone()),
        };
        self.infer_depth += 1;
        let out = self.eval(&ctx, expr, 0);
        self.infer_depth -= 1;
        out
    }

    /// Key or value type when iterating `ty` with `foreach`.
    fn element_type(&mut self, ty: &str, key: bool) -> Option<String> {
        let chain = self.lineage_env(ty);
        let (_, env) = chain.iter().find(|(class, _)| {
            matches!(
                class.fqn.to_ascii_lowercase().as_str(),
                "traversable" | "iterator" | "iteratoraggregate"
            )
        })?;
        let found = env.get(if key { "TKey" } else { "TValue" })?;
        (!found.is_empty()).then(|| found.clone())
    }

    /// The class an expression evaluates to, when it can be told.
    fn eval(&mut self, ctx: &Ctx<'_>, expr: &str, depth: u8) -> Option<String> {
        if depth > MAX_DEPTH {
            return None;
        }
        let s = expr.trim();
        let bytes = s.as_bytes();
        let first = *bytes.first()?;
        let mut pos;
        let mut current: Option<String>;
        if first == b'(' {
            let end = skip_balanced(s, 0)?;
            current = self.eval(ctx, &s[1..end - 1], depth + 1);
            pos = end;
        } else if let Some(rest) = strip_new(s) {
            let start = s.len() - rest.len();
            let name_end = start + read_name(rest);
            current = self.class_ref(ctx, &s[start..name_end]);
            pos = name_end;
            if s[pos..].starts_with('(') {
                pos = skip_balanced(s, pos)?;
            }
        } else if first == b'$' {
            let end = 1 + ident_len(&s[1..]);
            let name = &s[1..end];
            current = if name == "this" {
                ctx.class.as_ref().map(|c| c.fqn.clone())
            } else {
                self.variable_type(ctx, name, depth)
            };
            pos = end;
        } else {
            let end = read_name(s);
            if end == 0 {
                return None;
            }
            let name = &s[..end];
            if s[end..].starts_with('(') {
                let close = skip_balanced(s, end)?;
                current = self.function_return(ctx, name, &s[end + 1..close - 1]);
                pos = close;
            } else {
                current = self.class_ref(ctx, name);
                pos = end;
            }
        }
        loop {
            let rest = s[pos..].trim_start();
            pos = s.len() - rest.len();
            let op_len = if rest.starts_with("?->") {
                3
            } else if rest.starts_with("->") || rest.starts_with("::") {
                2
            } else {
                break;
            };
            pos += op_len;
            pos = s.len() - s[pos..].trim_start().len();
            let start = pos;
            if s[pos..].starts_with('$') {
                pos += 1;
            }
            pos += ident_len(&s[pos..]);
            let name = &s[start..pos];
            if name.is_empty() {
                return None;
            }
            let mut args = None;
            if s[pos..].starts_with('(') {
                let close = skip_balanced(s, pos)?;
                args = Some(&s[pos + 1..close - 1]);
                pos = close;
            }
            let class = current.take()?;
            current = self.member_type(ctx, &class, name, args);
        }
        if !s[pos..].trim().is_empty() {
            return None;
        }
        current
    }

    fn variable_type(&mut self, ctx: &Ctx<'_>, name: &str, depth: u8) -> Option<String> {
        let var = format!("${name}");
        let mut node = ctx
            .tree
            .root_node()
            .descendant_for_byte_range(ctx.byte, ctx.byte)?;
        let mut limit = ctx.byte;
        loop {
            while !is_scope(node.kind()) {
                node = node.parent()?;
            }
            let scope = node;
            match self.scope_variable(ctx, scope, &var, limit, depth) {
                Lookup::Found(ty) => return ty,
                Lookup::Missing => {}
            }
            let inherits = match scope.kind() {
                "arrow_function" => true,
                "anonymous_function" => uses_variable(scope, &var, ctx.src),
                _ => false,
            };
            if !inherits {
                return None;
            }
            limit = scope.start_byte();
            node = scope.parent()?;
        }
    }

    fn scope_variable(
        &mut self,
        ctx: &Ctx<'_>,
        scope: Node<'_>,
        var: &str,
        limit: usize,
        depth: u8,
    ) -> Lookup {
        enum Source {
            Resolved(Option<String>),
            Expr(String),
            /// Key or value of `foreach (<expr> as ...)`.
            Element {
                expr: String,
                key: bool,
            },
        }
        let self_fqn = ctx.class.as_ref().map_or("", |c| c.fqn.as_str());
        let mut candidates: Vec<(usize, Source)> = Vec::new();
        let mut offer = |pos: usize, source: Source| candidates.push((pos, source));

        if let Some(params) = scope.child_by_field_name("parameters") {
            let mut cursor = params.walk();
            for param in params.named_children(&mut cursor) {
                let Some(name) = param.child_by_field_name("name") else {
                    continue;
                };
                if &ctx.src[name.byte_range()] != var {
                    continue;
                }
                let declared = param
                    .child_by_field_name("type")
                    .and_then(|t| {
                        resolve_type(
                            &ctx.src[t.byte_range()],
                            &ctx.file.namespace,
                            &ctx.file.uses,
                            self_fqn,
                        )
                    })
                    .or_else(|| {
                        let doc = preceding_comment(scope, ctx.src)?;
                        let re = param_doc_regex(var);
                        let caps = re.captures(doc)?;
                        resolve_type(&caps[1], &ctx.file.namespace, &ctx.file.uses, self_fqn)
                    });
                offer(scope.start_byte(), Source::Resolved(declared));
            }
        }

        let mut stack = vec![scope];
        while let Some(node) = stack.pop() {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.id() != scope.id() && is_scope(child.kind()) {
                    continue;
                }
                match child.kind() {
                    "assignment_expression" => {
                        if let (Some(left), Some(right)) = (
                            child.child_by_field_name("left"),
                            child.child_by_field_name("right"),
                        ) && left.kind() == "variable_name"
                            && &ctx.src[left.byte_range()] == var
                            && child.end_byte() <= limit
                        {
                            offer(
                                child.start_byte(),
                                Source::Expr(ctx.src[right.byte_range()].to_owned()),
                            );
                        }
                    }
                    "foreach_statement" if child.start_byte() <= limit => {
                        let mut inner = child.walk();
                        let kids: Vec<Node<'_>> = child
                            .named_children(&mut inner)
                            .filter(|n| {
                                Some(n.id()) != child.child_by_field_name("body").map(|b| b.id())
                            })
                            .collect();
                        if let [iterated, target, ..] = kids.as_slice() {
                            let (key, value) = if target.kind() == "pair" {
                                let mut pair = target.walk();
                                let parts: Vec<Node<'_>> =
                                    target.named_children(&mut pair).collect();
                                (parts.first().copied(), parts.get(1).copied())
                            } else {
                                (None, Some(*target))
                            };
                            let is_var = |node: Option<Node<'_>>| {
                                node.is_some_and(|n| {
                                    let n = if n.kind() == "by_ref" {
                                        n.named_child(0).unwrap_or(n)
                                    } else {
                                        n
                                    };
                                    n.kind() == "variable_name" && &ctx.src[n.byte_range()] == var
                                })
                            };
                            let expr = ctx.src[iterated.byte_range()].to_owned();
                            if is_var(value) {
                                offer(
                                    child.start_byte(),
                                    Source::Element {
                                        expr: expr.clone(),
                                        key: false,
                                    },
                                );
                            }
                            if is_var(key) {
                                offer(child.start_byte(), Source::Element { expr, key: true });
                            }
                        }
                    }
                    "catch_clause" => {
                        if let (Some(name), Some(ty)) = (
                            child.child_by_field_name("name"),
                            child.child_by_field_name("type"),
                        ) && &ctx.src[name.byte_range()] == var
                            && child.start_byte() <= limit
                        {
                            let resolved = resolve_type(
                                &ctx.src[ty.byte_range()],
                                &ctx.file.namespace,
                                &ctx.file.uses,
                                self_fqn,
                            );
                            offer(child.start_byte(), Source::Resolved(resolved));
                        }
                    }
                    _ => {}
                }
                stack.push(child);
            }
        }

        // `/** @var Foo $x */` and `$x instanceof Foo` before the cursor.
        let end = limit.min(ctx.src.len());
        let start = scope.start_byte().min(end);
        let window = &ctx.src[start..end];
        let escaped = regex::escape(var);
        if let Ok(re) = Regex::new(&format!(r"@var\s+(\S+)\s+{escaped}\b")) {
            for caps in re.captures_iter(window) {
                let at = start + caps.get(0).map_or(0, |m| m.start());
                let ty = resolve_type(&caps[1], &ctx.file.namespace, &ctx.file.uses, self_fqn);
                offer(at, Source::Resolved(ty));
            }
        }
        if let Ok(re) = Regex::new(&format!(r"{escaped}\s+instanceof\s+([\w\\]+)")) {
            for caps in re.captures_iter(window) {
                let at = start + caps.get(0).map_or(0, |m| m.start());
                let ty = self.class_ref(ctx, &caps[1]);
                offer(at, Source::Resolved(ty));
            }
        }

        if candidates.is_empty() {
            return Lookup::Missing;
        }
        // Latest first; one whose type cannot be told (a call to something
        // unknown) defers to an earlier hint such as `@var` or a parameter.
        candidates.sort_by(|a, b| b.0.cmp(&a.0));
        for (pos, source) in candidates {
            let found = match source {
                Source::Resolved(ty) => ty,
                Source::Expr(expr) => {
                    let inner = Ctx {
                        byte: pos,
                        ..ctx.clone()
                    };
                    self.eval(&inner, &expr, depth + 1)
                }
                Source::Element { expr, key } => {
                    let inner = Ctx {
                        byte: pos,
                        ..ctx.clone()
                    };
                    let iterated = self.eval(&inner, &expr, depth + 1);
                    iterated.and_then(|ty| self.element_type(&ty, key))
                }
            };
            if found.is_some() {
                return Lookup::Found(found);
            }
        }
        Lookup::Found(None)
    }

    // ---- completion --------------------------------------------------

    /// Makes the declarations of `text` visible under `path` while one
    /// request runs, so classes of the file being edited resolve too.
    fn register_current(&mut self, path: &Path, text: &str) -> Option<Arc<FileSymbols>> {
        self.open
            .insert(path.to_path_buf(), Arc::new(parse_file(path, text)))
    }

    fn restore_current(&mut self, path: &Path, previous: Option<Arc<FileSymbols>>) {
        match previous {
            Some(file) => {
                self.open.insert(path.to_path_buf(), file);
            }
            None => {
                self.open.remove(path);
            }
        }
    }

    /// Suggestions at `(line, column)` of `text`, a UTF-16 column.
    pub fn complete(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Completion> {
        let previous = self.register_current(path, text);
        let out = match self.laravel_complete(path, text, line, column) {
            Some(items) => items,
            None if laravel::is_blade(path) => Vec::new(),
            // A string that is not one of Laravel's has nothing to suggest.
            None if self.laravel.is_some()
                && byte_offset(text, line, column)
                    .is_some_and(|byte| crate::php::laravel::in_string(text, byte, false)) =>
            {
                Vec::new()
            }
            None => self.complete_inner(path, text, line, column),
        };
        self.restore_current(path, previous);
        out
    }

    fn complete_inner(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Vec<Completion> {
        let Some(byte) = byte_offset(text, line, column) else {
            return Vec::new();
        };
        let Some((file, tree)) = parse_with_tree(path, text) else {
            return Vec::new();
        };
        let class = file
            .classes
            .iter()
            .find(|c| c.start_line <= line && line <= c.end_line)
            .cloned();
        let ctx = Ctx {
            src: text,
            tree: &tree,
            file: &file,
            byte,
            class,
        };
        let word_start = word_start(text, byte);
        let word = &text[word_start..byte];
        let before = &text[..word_start];
        let trimmed = before.trim_end_matches([' ', '\t']);

        if let Some((receiver_end, is_static)) = member_operator(trimmed) {
            let start = receiver_start(trimmed, receiver_end);
            let receiver = &trimmed[start..receiver_end];
            let Some(class_fqn) = self.eval(&ctx, receiver, 0) else {
                return Vec::new();
            };
            let Some(class) = self.class(&class_fqn) else {
                return Vec::new();
            };
            let via_scope = is_static
                && matches!(
                    receiver.trim().to_ascii_lowercase().as_str(),
                    "self" | "static" | "parent"
                );
            return self.member_items(&ctx, &class, is_static, via_scope, word);
        }

        let line_start = text[..byte].rfind('\n').map_or(0, |i| i + 1);
        let line_before = &text[line_start..byte];
        static USE_LINE: OnceLock<Regex> = OnceLock::new();
        let use_line = USE_LINE.get_or_init(|| Regex::new(r"^\s*use\s+([\w\\]*)$").expect("valid"));
        if let Some(caps) = use_line.captures(line_before) {
            return self.import_items(&caps[1]);
        }

        if let Some(name) = word.strip_prefix('$') {
            return self.variable_items(&ctx, name);
        }

        let line_text = line_before.trim_start();
        static NEW: OnceLock<Regex> = OnceLock::new();
        let after_new = NEW
            .get_or_init(|| Regex::new(r"\bnew\s+$").expect("valid"))
            .is_match(before);
        let wants_interface = line_text.contains("implements");
        if word.is_empty() && !after_new {
            return Vec::new();
        }
        let mut items = self.class_items(&ctx, word, after_new, wants_interface);
        if !after_new {
            items.extend(self.function_items(&ctx, word));
            items.extend(keyword_items(word));
        }
        items.sort_by(|a, b| a.sort.cmp(&b.sort));
        items.truncate(MAX_ITEMS);
        items
    }

    fn accessible(
        &mut self,
        ctx: &Ctx<'_>,
        owner: &Arc<ClassInfo>,
        visibility: Visibility,
    ) -> bool {
        match visibility {
            Visibility::Public => true,
            Visibility::Private => ctx.class.as_ref().is_some_and(|c| c.fqn == owner.fqn),
            Visibility::Protected => {
                let Some(current) = ctx.class.clone() else {
                    return false;
                };
                if current.fqn == owner.fqn {
                    return true;
                }
                self.lineage(&current).iter().any(|c| c.fqn == owner.fqn)
                    || self.lineage(owner).iter().any(|c| c.fqn == current.fqn)
            }
        }
    }

    fn member_items(
        &mut self,
        ctx: &Ctx<'_>,
        class: &Arc<ClassInfo>,
        is_static: bool,
        via_scope: bool,
        word: &str,
    ) -> Vec<Completion> {
        let typed_dollar = word.starts_with('$');
        let prefix = word.trim_start_matches('$');
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for owner in self.lineage(class) {
            for member in &owner.members {
                let wanted = match member.kind {
                    MemberKind::Method => {
                        if is_static {
                            member.is_static || via_scope
                        } else {
                            true
                        }
                    }
                    MemberKind::Property => member.is_static == is_static,
                    MemberKind::Constant | MemberKind::Case => is_static,
                };
                if !wanted {
                    continue;
                }
                if member.name.starts_with("__") && !via_scope {
                    continue;
                }
                if !fuzzy(prefix, &member.name) {
                    continue;
                }
                let key = (
                    member.kind == MemberKind::Method,
                    member.name.to_lowercase(),
                );
                if !seen.insert(key) {
                    continue;
                }
                if !self.accessible(ctx, &owner, member.visibility) {
                    continue;
                }
                let own = if owner.fqn == class.fqn { '0' } else { '1' };
                out.push(member_completion(member, &owner, own, typed_dollar));
            }
        }
        if is_static && fuzzy(prefix, "class") {
            out.push(Completion {
                label: "class".into(),
                detail: Some("class name".into()),
                insert_text: "class".into(),
                is_snippet: false,
                kind: ItemKind::Constant,
                sort: "9class".into(),
                extra: Vec::new(),
                replace_from: None,
            });
        }
        out.sort_by(|a, b| a.sort.cmp(&b.sort));
        out.truncate(MAX_ITEMS);
        out
    }

    fn variable_items(&mut self, ctx: &Ctx<'_>, prefix: &str) -> Vec<Completion> {
        let mut names: Vec<String> = Vec::new();
        if let Some(mut node) = ctx
            .tree
            .root_node()
            .descendant_for_byte_range(ctx.byte, ctx.byte)
        {
            while !is_scope(node.kind()) {
                match node.parent() {
                    Some(parent) => node = parent,
                    None => break,
                }
            }
            let mut stack = vec![node];
            while let Some(current) = stack.pop() {
                let mut cursor = current.walk();
                for child in current.named_children(&mut cursor) {
                    if child.kind() == "variable_name" && child.start_byte() < ctx.byte {
                        let name = ctx.src[child.byte_range()].trim_start_matches('$');
                        if !names.iter().any(|n| n == name) {
                            names.push(name.to_owned());
                        }
                    }
                    stack.push(child);
                }
            }
        }
        if ctx.class.is_some() {
            names.push("this".into());
        }
        names.retain(|name| !name.is_empty() && name != prefix && fuzzy(prefix, name));
        names.sort();
        names
            .into_iter()
            .map(|name| Completion {
                label: format!("${name}"),
                detail: None,
                insert_text: name.clone(),
                is_snippet: false,
                kind: ItemKind::Variable,
                sort: format!("0{}", name.to_lowercase()),
                extra: Vec::new(),
                replace_from: None,
            })
            .collect()
    }

    fn function_items(&mut self, ctx: &Ctx<'_>, prefix: &str) -> Vec<Completion> {
        let mut found: Vec<Arc<FunctionInfo>> = Vec::new();
        for file in self.open.values() {
            found.extend(file.functions.iter().cloned());
        }
        found.extend(self.function_index().values().cloned());
        found.extend(self.builtin().functions().cloned());
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for function in found {
            if !fuzzy(prefix, &function.name) || !seen.insert(function.fqn.to_lowercase()) {
                continue;
            }
            let empty = function.detail.starts_with("()");
            out.push(Completion {
                label: function.name.clone(),
                detail: Some(function.detail.clone()),
                insert_text: if empty {
                    format!("{}()", function.name)
                } else {
                    format!("{}($0)", function.name)
                },
                is_snippet: !empty,
                kind: ItemKind::Function,
                sort: rank(prefix, &function.name, '2'),
                extra: Vec::new(),
                replace_from: None,
            });
        }
        let _ = ctx;
        out
    }

    fn class_items(
        &mut self,
        ctx: &Ctx<'_>,
        prefix: &str,
        concrete_only: bool,
        interfaces_only: bool,
    ) -> Vec<Completion> {
        let names = self.class_names();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut consider = |short: &str,
                            fqn: &str,
                            vendor: bool,
                            kind: Option<ClassKind>,
                            out: &mut Vec<Completion>| {
            if !fuzzy(prefix, short) || !seen.insert(fqn.to_lowercase()) {
                return;
            }
            if let Some(kind) = kind {
                if concrete_only && matches!(kind, ClassKind::Interface | ClassKind::Trait) {
                    return;
                }
                if interfaces_only && kind != ClassKind::Interface {
                    return;
                }
            }
            let namespace = fqn.rsplit_once('\\').map(|(ns, _)| ns).unwrap_or("");
            out.push(Completion {
                label: short.to_owned(),
                detail: (!namespace.is_empty()).then(|| namespace.to_owned()),
                insert_text: short.to_owned(),
                is_snippet: false,
                kind: match kind {
                    Some(ClassKind::Interface) => ItemKind::Interface,
                    Some(ClassKind::Trait) => ItemKind::Trait,
                    Some(ClassKind::Enum) => ItemKind::Enum,
                    _ => ItemKind::Class,
                },
                sort: rank(prefix, short, if vendor { '1' } else { '0' }),
                extra: import_edit(ctx.file, fqn).into_iter().collect(),
                replace_from: None,
            });
        };
        let open: Vec<Arc<ClassInfo>> = self
            .open
            .values()
            .flat_map(|file| file.classes.iter().cloned())
            .collect();
        for class in &open {
            consider(&class.name, &class.fqn, false, Some(class.kind), &mut out);
        }
        for name in names.iter() {
            consider(&name.short, &name.fqn, name.vendor, None, &mut out);
            if out.len() > MAX_ITEMS * 8 {
                break;
            }
        }
        out.sort_by(|a, b| a.sort.cmp(&b.sort));
        out.truncate(MAX_ITEMS);
        out
    }

    /// Classes for a `use` statement: the text after what is typed so far
    /// up to the namespace separator.
    fn import_items(&mut self, typed: &str) -> Vec<Completion> {
        let dir_len = typed.rfind('\\').map_or(0, |i| i + 1);
        let (dir, prefix) = typed.split_at(dir_len);
        let names = self.class_names();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for name in names.iter() {
            let Some(head) = name.fqn.get(..dir.len()) else {
                continue;
            };
            if !head.eq_ignore_ascii_case(dir) {
                continue;
            }
            let rest = &name.fqn[dir.len()..];
            // Offer the next namespace segment once, classes by name.
            let (label, is_namespace) = match rest.split_once('\\') {
                Some((segment, _)) => (segment, true),
                None => (rest, false),
            };
            if !fuzzy(prefix, label) || !seen.insert((label.to_lowercase(), is_namespace)) {
                continue;
            }
            out.push(Completion {
                label: label.to_owned(),
                detail: Some(name.fqn.clone()),
                insert_text: if is_namespace {
                    format!("{label}\\")
                } else {
                    label.to_owned()
                },
                is_snippet: false,
                kind: if is_namespace {
                    ItemKind::Keyword
                } else {
                    ItemKind::Class
                },
                sort: rank(prefix, label, if name.vendor { '1' } else { '0' }),
                extra: Vec::new(),
                replace_from: None,
            });
            if out.len() > MAX_ITEMS * 4 {
                break;
            }
        }
        out.sort_by(|a, b| a.sort.cmp(&b.sort));
        out.truncate(MAX_ITEMS);
        out
    }

    // ---- definition and hover ------------------------------------------

    /// Where the symbol under the cursor is declared.
    pub fn definition(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Target> {
        let previous = self.register_current(path, text);
        let named = self.laravel_definition(path, text, line, column);
        self.restore_current(path, previous);
        if let Some(targets) = named
            && !targets.is_empty()
        {
            return targets;
        }
        if laravel::is_blade(path) {
            return Vec::new();
        }
        self.with_symbol(path, text, line, column, |engine, _ctx, symbol, _range| {
            engine.symbol_target(symbol)
        })
        .flatten()
        .unwrap_or_default()
        .into_iter()
        // Built-in declarations live in the binary: write them out so the
        // editor has a file to open.
        .filter_map(|target| {
            if !is_stub_path(&target.path) {
                return Some(target);
            }
            Some(Target {
                path: materialize(&target.path)?,
                ..target
            })
        })
        .collect()
    }

    /// Documentation for the symbol under the cursor.
    pub fn hover(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Option<HoverResult> {
        let previous = self.register_current(path, text);
        let named = self.laravel_hover(path, text, line, column);
        self.restore_current(path, previous);
        if named.is_some() {
            return named;
        }
        if laravel::is_blade(path) {
            return None;
        }
        self.with_symbol(path, text, line, column, |engine, ctx, symbol, range| {
            let markdown = engine.symbol_markdown(ctx, symbol)?;
            Some(HoverResult {
                markdown,
                line: range.0,
                start_column: range.1,
                end_column: range.2,
            })
        })
        .flatten()
    }

    fn with_symbol<T>(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
        run: impl FnOnce(&mut Self, &Ctx<'_>, Symbol, (u32, u32, u32)) -> T,
    ) -> Option<T> {
        let previous = self.register_current(path, text);
        let out = self.with_symbol_inner(path, text, line, column, run);
        self.restore_current(path, previous);
        out
    }

    fn with_symbol_inner<T>(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
        run: impl FnOnce(&mut Self, &Ctx<'_>, Symbol, (u32, u32, u32)) -> T,
    ) -> Option<T> {
        let byte = byte_offset(text, line, column)?;
        let (file, tree) = parse_with_tree(path, text)?;
        let class = file
            .classes
            .iter()
            .find(|c| c.start_line <= line && line <= c.end_line)
            .cloned();
        let ctx = Ctx {
            src: text,
            tree: &tree,
            file: &file,
            byte,
            class,
        };
        let root = tree.root_node();
        // `unset` and `exit` are keywords of their own statements, not
        // names, yet read like calls: they point at their declaration.
        let is_token = |n: Node<'_>| {
            matches!(n.kind(), "name" | "variable_name") || construct_keyword(n).is_some()
        };
        let mut node = root.descendant_for_byte_range(byte, byte)?;
        if !is_token(node) && byte > 0 {
            node = root.descendant_for_byte_range(byte - 1, byte - 1)?;
        }
        if !is_token(node) {
            return None;
        }
        if let Some(keyword) = construct_keyword(node) {
            let start = position_of(text, node.start_byte());
            let end = position_of(text, node.end_byte());
            let range = (start.0, start.1, end.1);
            return Some(run(self, &ctx, Symbol::Function(keyword.to_owned()), range));
        }
        // The `name` inside `$item` is part of the variable, not a class name.
        if node.kind() == "name"
            && let Some(parent) = node.parent()
            && parent.kind() == "variable_name"
        {
            node = parent;
        }
        let range = {
            let start = position_of(text, node.start_byte());
            let end = position_of(text, node.end_byte());
            (start.0, start.1, end.1)
        };
        let symbol = self.classify(&ctx, node)?;
        Some(run(self, &ctx, symbol, range))
    }

    fn classify(&mut self, ctx: &Ctx<'_>, node: Node<'_>) -> Option<Symbol> {
        let src = ctx.src;
        let text = |n: Node<'_>| src[n.byte_range()].to_owned();
        if node.kind() == "variable_name" {
            let parent = node.parent()?;
            if parent.kind() == "scoped_property_access_expression"
                && parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id())
            {
                let scope = text(parent.child_by_field_name("scope")?);
                let class = self.eval(ctx, &scope, 0)?;
                return Some(Symbol::Member {
                    class,
                    name: text(node),
                    call: false,
                });
            }
            return Some(Symbol::Variable(text(node)));
        }
        let mut target = node;
        let mut parent = node.parent()?;
        if parent.kind() == "qualified_name" || parent.kind() == "namespace_name" {
            target = parent;
            parent = parent.parent()?;
        }
        let is_name_field = parent.child_by_field_name("name").map(|n| n.id()) == Some(target.id());
        match parent.kind() {
            "member_call_expression" | "nullsafe_member_call_expression" if is_name_field => {
                let object = text(parent.child_by_field_name("object")?);
                Some(Symbol::Member {
                    class: self.eval(ctx, &object, 0)?,
                    name: text(target),
                    call: true,
                })
            }
            "member_access_expression" | "nullsafe_member_access_expression" if is_name_field => {
                let object = text(parent.child_by_field_name("object")?);
                Some(Symbol::Member {
                    class: self.eval(ctx, &object, 0)?,
                    name: text(target),
                    call: false,
                })
            }
            "scoped_call_expression" if is_name_field => {
                let scope = text(parent.child_by_field_name("scope")?);
                Some(Symbol::Member {
                    class: self.eval(ctx, &scope, 0)?,
                    name: text(target),
                    call: true,
                })
            }
            "class_constant_access_expression" => {
                let mut cursor = parent.walk();
                let kids: Vec<Node<'_>> = parent.named_children(&mut cursor).collect();
                if kids.len() >= 2 && kids.last().map(|n| n.id()) == Some(target.id()) {
                    let class = self.eval(ctx, &text(kids[0]), 0)?;
                    Some(Symbol::Member {
                        class,
                        name: text(target),
                        call: false,
                    })
                } else {
                    Some(Symbol::Class(self.class_ref(ctx, &text(target))?))
                }
            }
            "function_call_expression" => {
                let function = parent.child_by_field_name("function")?;
                if function.id() == target.id() {
                    Some(Symbol::Function(text(target)))
                } else {
                    Some(Symbol::Class(self.class_ref(ctx, &text(target))?))
                }
            }
            "namespace_use_clause" => Some(Symbol::Class(
                text(target).trim_start_matches('\\').to_owned(),
            )),
            _ => Some(Symbol::Class(self.class_ref(ctx, &text(target))?)),
        }
    }

    fn symbol_target(&mut self, symbol: Symbol) -> Option<Vec<Target>> {
        match symbol {
            Symbol::Class(fqn) => {
                let class = self.class(&fqn)?;
                Some(vec![Target {
                    path: class.path.clone(),
                    line: class.line,
                    column: class.column,
                }])
            }
            Symbol::Member { class, name, call } => {
                let class = self.class(&class)?;
                let (owner, member) = self.find_member_or_magic(&class, &name, call)?;
                Some(vec![Target {
                    path: owner.path.clone(),
                    line: member.line,
                    column: member.column,
                }])
            }
            Symbol::Function(name) => {
                let function = self.function(&name)?;
                Some(vec![Target {
                    path: function.path.clone(),
                    line: function.line,
                    column: function.column,
                }])
            }
            Symbol::Variable(_) => None,
        }
    }

    fn symbol_markdown(&mut self, ctx: &Ctx<'_>, symbol: Symbol) -> Option<String> {
        match symbol {
            Symbol::Class(fqn) => {
                let class = self.class(&fqn)?;
                let mut head = format!("{} {}", class.kind.keyword(), class.fqn);
                if !class.extends.is_empty() {
                    head += &format!(" extends {}", class.extends.join(", "));
                }
                if !class.implements.is_empty() {
                    head += &format!(" implements {}", class.implements.join(", "));
                }
                Some(with_doc(&head, class.doc.as_deref()))
            }
            Symbol::Member { class, name, call } => {
                let class = self.class(&class)?;
                let (owner, member) = self.find_member_or_magic(&class, &name, call)?;
                let head = match member.kind {
                    MemberKind::Method => format!(
                        "{}function {}::{}{}",
                        if member.is_static { "static " } else { "" },
                        owner.name,
                        member.name,
                        member.detail
                    ),
                    MemberKind::Property => format!(
                        "{}{}${}",
                        if member.detail.is_empty() {
                            String::new()
                        } else {
                            format!("{} ", member.detail)
                        },
                        if member.is_static {
                            format!("{}::", owner.name)
                        } else {
                            String::new()
                        },
                        member.name
                    ),
                    MemberKind::Constant => {
                        format!("const {}::{} = {}", owner.name, member.name, member.detail)
                    }
                    MemberKind::Case => format!("case {}::{}", owner.name, member.name),
                };
                Some(with_doc(&head, member.doc.as_deref()))
            }
            Symbol::Function(name) => {
                let function = self.function(&name)?;
                Some(with_doc(
                    &format!("function {}{}", function.fqn, function.detail),
                    function.doc.as_deref(),
                ))
            }
            Symbol::Variable(name) => {
                let ty = self.variable_type(ctx, name.trim_start_matches('$'), 0)?;
                Some(format!("```php\n{name}: {ty}\n```"))
            }
        }
    }
}

// ---- helpers ------------------------------------------------------------

fn with_doc(head: &str, doc: Option<&str>) -> String {
    let mut out = format!("```php\n{head}\n```");
    if let Some(doc) = doc {
        let cleaned = clean_doc(doc);
        if !cleaned.is_empty() {
            out.push_str("\n\n");
            out.push_str(&cleaned);
        }
    }
    out
}

fn clean_doc(doc: &str) -> String {
    doc.lines()
        .map(|line| {
            line.trim()
                .trim_start_matches("/**")
                .trim_end_matches("*/")
                .trim_start_matches('*')
                .trim()
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn member_completion(
    member: &Member,
    owner: &Arc<ClassInfo>,
    own: char,
    typed_dollar: bool,
) -> Completion {
    let (kind, label, insert, snippet) = match member.kind {
        MemberKind::Method => {
            let empty = member.detail.starts_with("()");
            (
                ItemKind::Method,
                member.name.clone(),
                if empty {
                    format!("{}()", member.name)
                } else {
                    format!("{}($0)", member.name)
                },
                !empty,
            )
        }
        MemberKind::Property => (
            ItemKind::Property,
            if member.is_static {
                format!("${}", member.name)
            } else {
                member.name.clone()
            },
            // The query the editor replaces never includes a typed `$`.
            if member.is_static && !typed_dollar {
                format!("${}", member.name)
            } else {
                member.name.clone()
            },
            false,
        ),
        MemberKind::Constant | MemberKind::Case => (
            ItemKind::Constant,
            member.name.clone(),
            member.name.clone(),
            false,
        ),
    };
    let detail = if member.detail.is_empty() {
        owner.name.clone()
    } else {
        member.detail.clone()
    };
    Completion {
        label,
        detail: Some(detail),
        insert_text: insert,
        is_snippet: snippet,
        kind,
        sort: format!("{own}{}", member.name.to_lowercase()),
        extra: Vec::new(),
        replace_from: None,
    }
}

fn keyword_items(prefix: &str) -> Vec<Completion> {
    const WORDS: &[&str] = &[
        "abstract",
        "array",
        "case",
        "catch",
        "class",
        "const",
        "echo",
        "else",
        "elseif",
        "enum",
        "extends",
        "false",
        "final",
        "finally",
        "fn",
        "foreach",
        "function",
        "if",
        "implements",
        "instanceof",
        "interface",
        "isset",
        "match",
        "namespace",
        "new",
        "null",
        "parent",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "self",
        "static",
        "switch",
        "throw",
        "trait",
        "true",
        "try",
        "unset",
        "use",
        "while",
    ];
    if prefix.chars().count() < 2 {
        return Vec::new();
    }
    WORDS
        .iter()
        .filter(|w| fuzzy(prefix, w))
        .map(|w| Completion {
            label: (*w).to_owned(),
            detail: None,
            insert_text: (*w).to_owned(),
            is_snippet: false,
            kind: ItemKind::Keyword,
            sort: rank(prefix, w, '3'),
            extra: Vec::new(),
            replace_from: None,
        })
        .collect()
}

/// `0` exact prefix, `1` other match, behind the caller's group digit.
fn rank(prefix: &str, name: &str, group: char) -> String {
    let lower = name.to_lowercase();
    let near = if lower.starts_with(&prefix.to_lowercase()) {
        '0'
    } else {
        '1'
    };
    format!("{near}{group}{lower}")
}

/// Letters of `needle` appear in `haystack` in order, ignoring case.
fn fuzzy(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|n| hay.any(|h| h == n))
}

/// `use` statement that makes `fqn` usable by its short name, or `None`
/// when the file already reaches it.
fn import_edit(file: &FileSymbols, fqn: &str) -> Option<Insertion> {
    let (namespace, short) = fqn.rsplit_once('\\').map_or(("", fqn), |(n, s)| (n, s));
    if file.namespace.is_empty() && namespace.is_empty() {
        return None;
    }
    if namespace.eq_ignore_ascii_case(&file.namespace) {
        return None;
    }
    if file.uses.contains_key(&short.to_lowercase()) {
        return None;
    }
    let statement = format!("use {fqn};\n");
    let edit = if let Some(line) = file.last_use_line {
        Insertion {
            line: line + 1,
            column: 0,
            text: statement,
        }
    } else if let Some(line) = file.namespace_line {
        Insertion {
            line: line + 1,
            column: 0,
            text: format!("\n{statement}"),
        }
    } else {
        Insertion {
            line: file.open_tag_line.map_or(0, |l| l + 1),
            column: 0,
            text: format!("\n{statement}"),
        }
    };
    Some(edit)
}

/// A tree for code that is evaluated from a string, not from a position.
fn empty_tree() -> &'static Tree {
    static TREE: OnceLock<Tree> = OnceLock::new();
    TREE.get_or_init(|| {
        parse_with_tree(Path::new(""), "")
            .map(|(_, tree)| tree)
            .expect("the PHP grammar loads")
    })
}

/// `unset` or `exit` when `node` is the keyword of such a statement.
fn construct_keyword(node: Node<'_>) -> Option<&'static str> {
    let parent = node.parent()?.kind();
    match (node.kind(), parent) {
        ("unset", "unset_statement") => Some("unset"),
        ("exit", "exit_statement") => Some("exit"),
        _ => None,
    }
}

fn is_scope(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration"
            | "function_definition"
            | "anonymous_function"
            | "arrow_function"
            | "program"
    )
}

fn uses_variable(scope: Node<'_>, var: &str, src: &str) -> bool {
    let mut cursor = scope.walk();
    scope
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "anonymous_function_use_clause")
        .any(|clause| src[clause.byte_range()].contains(var))
}

fn preceding_comment<'s>(scope: Node<'_>, src: &'s str) -> Option<&'s str> {
    let prev = scope.prev_named_sibling()?;
    (prev.kind() == "comment").then(|| &src[prev.byte_range()])
}

fn param_doc_regex(var: &str) -> Regex {
    Regex::new(&format!(r"@param\s+(\S+)\s+{}\b", regex::escape(var)))
        .unwrap_or_else(|_| Regex::new("$^").expect("valid"))
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn is_name_char(c: u8) -> bool {
    is_ident(c) || c == b'\\'
}

fn ident_len(s: &str) -> usize {
    s.bytes().take_while(|&c| is_ident(c)).count()
}

fn read_name(s: &str) -> usize {
    s.bytes().take_while(|&c| is_name_char(c)).count()
}

fn strip_new(s: &str) -> Option<&str> {
    let rest = s.strip_prefix("new")?;
    let trimmed = rest.trim_start();
    (trimmed.len() < rest.len()).then_some(trimmed)
}

/// Index just past the bracket closing the one at `start`.
fn skip_balanced(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = Some(c),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn split_args(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut start = 0;
    let bytes = args.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = Some(c),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => {
                    out.push(args[start..i].trim());
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    if !args[start..].trim().is_empty() {
        out.push(args[start..].trim());
    }
    out
}

/// Byte offset of a line and UTF-16 column.
fn byte_offset(text: &str, line: u32, column: u32) -> Option<usize> {
    let mut start = 0usize;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let mut units = 0u32;
    for (i, ch) in text[start..].char_indices() {
        if units >= column || ch == '\n' {
            return Some(start + i);
        }
        units += ch.len_utf16() as u32;
    }
    Some(text.len())
}

/// Line and UTF-16 column of a byte offset.
fn position_of(text: &str, byte: usize) -> (u32, u32, u32) {
    let before = &text[..byte.min(text.len())];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = text[line_start..before.len()].encode_utf16().count() as u32;
    (line, column, 0)
}

fn word_start(text: &str, byte: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = byte;
    while i > 0 && (is_ident(bytes[i - 1]) || bytes[i - 1] == b'$') {
        i -= 1;
    }
    i
}

/// If `before` ends with `->`, `?->` or `::`, the byte where the receiver
/// ends and whether the operator is `::`.
fn member_operator(before: &str) -> Option<(usize, bool)> {
    let text = before.trim_end();
    if let Some(rest) = text.strip_suffix("?->") {
        return Some((rest.len(), false));
    }
    if let Some(rest) = text.strip_suffix("->") {
        return Some((rest.len(), false));
    }
    if let Some(rest) = text.strip_suffix("::") {
        return Some((rest.len(), true));
    }
    None
}

/// Start of the chain expression that ends at `end`, scanning backwards
/// over calls, indexes and `->`/`::` links (also across lines).
fn receiver_start(text: &str, end: usize) -> usize {
    let bytes = text.as_bytes();
    let skip_ws = |mut i: usize| {
        while i > 0 && bytes[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        i
    };
    let floor = end.saturating_sub(4000);
    let mut i = skip_ws(end);
    loop {
        i = skip_ws(i);
        if i <= floor {
            return floor.max(i);
        }
        let c = bytes[i - 1];
        if c == b')' || c == b']' {
            let mut depth = 0i32;
            let mut j = i;
            let mut opened = None;
            while j > floor {
                j -= 1;
                match bytes[j] {
                    b')' | b']' => depth += 1,
                    b'(' | b'[' => {
                        depth -= 1;
                        if depth == 0 {
                            opened = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(open) = opened else { return i };
            i = open;
            let k = skip_ws(i);
            if k > 0 && (is_name_char(bytes[k - 1]) || bytes[k - 1] == b'$') {
                i = name_start(bytes, k);
            } else {
                return i;
            }
        } else if is_name_char(c) || c == b'$' {
            i = name_start(bytes, i);
        } else {
            return i;
        }
        let k = skip_ws(i);
        if k >= 3 && &bytes[k - 3..k] == b"?->" {
            i = k - 3;
            continue;
        }
        if k >= 2 && (&bytes[k - 2..k] == b"->" || &bytes[k - 2..k] == b"::") {
            i = k - 2;
            continue;
        }
        if k >= 3 && &bytes[k - 3..k] == b"new" && (k == 3 || !is_ident(bytes[k - 4])) {
            return k - 3;
        }
        return i;
    }
}

fn name_start(bytes: &[u8], mut i: usize) -> usize {
    while i > 0 && (is_name_char(bytes[i - 1]) || bytes[i - 1] == b'$') {
        i -= 1;
    }
    i
}

/// Class declarations of a project that has no Composer autoload data.
fn scan_sources(root: &Path) -> Vec<(String, PathBuf)> {
    static NAMESPACE: OnceLock<Regex> = OnceLock::new();
    static CLASS: OnceLock<Regex> = OnceLock::new();
    let namespace = NAMESPACE
        .get_or_init(|| Regex::new(r"(?m)^\s*namespace\s+([\w\\]+)\s*[;{]").expect("valid"));
    let class = CLASS.get_or_init(|| {
        Regex::new(
            r"(?m)^\s*(?:(?:abstract|final|readonly)\s+)*(?:class|interface|trait|enum)\s+(\w+)",
        )
        .expect("valid")
    });
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut budget = 20_000usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < 12
                    && !name.starts_with('.')
                    && !matches!(name, "vendor" | "node_modules" | "storage" | "cache")
                {
                    stack.push((entry.path(), depth + 1));
                }
            } else if name.ends_with(".php") && !name.ends_with(".blade.php") {
                budget = budget.saturating_sub(1);
                if budget == 0 {
                    return out;
                }
                let Ok(text) = std::fs::read_to_string(entry.path()) else {
                    continue;
                };
                let ns = namespace
                    .captures(&text)
                    .map(|c| c[1].to_owned())
                    .unwrap_or_default();
                for caps in class.captures_iter(&text) {
                    let fqn = if ns.is_empty() {
                        caps[1].to_owned()
                    } else {
                        format!("{ns}\\{}", &caps[1])
                    };
                    out.push((fqn, entry.path()));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
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
                .any(|c| c.label == "name" && c.insert_text == "name()")
        );
    }

    #[test]
    fn filters_by_typed_prefix() {
        let project = Project::new();
        let items = project.complete(&format!("{HEAD}        $user->na|\n    }}\n}}\n"));
        assert_eq!(labels(&items), ["name"]);
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

        // Hover shows the stub; definition opens a copy of it on disk.
        let text = "<?php\nstrlen('a');";
        let hover = engine.hover(&file, text, 1, 2).expect("hover on strlen");
        assert!(hover.markdown.contains("strlen"), "{}", hover.markdown);
        let found = engine.definition(&file, text, 1, 2);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.is_file());
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
                "<?php\nnamespace Illuminate\\Database\\Eloquent;\n\nclass Builder\n{\n    public function create(array $attributes = []) {}\n}\n",
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
        let mut engine = PhpEngine::new(root);
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
        let mut engine = PhpEngine::new(root);
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
        let text =
            "<?php\nuse App\\Item;\nfunction show(Item $item) {\n    return $item->price;\n}\n";
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
}
