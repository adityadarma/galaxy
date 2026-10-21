//! Fields and methods of a type, promoted ones included.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use super::{GoEngine, Package};
use crate::go::parse::{FuncDecl, GoFile, Param, Pos, TypeKind};
use crate::go::texpr::TypeExpr;

/// Deepest embedding followed.
const MAX_EMBED_DEPTH: usize = 6;
/// Types visited for one lookup.
const MAX_VISITED: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Field,
    Method,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    /// Field type text, or a method signature such as `(a int) (string, error)`.
    pub ty: String,
    pub doc: String,
    /// The type that declares it, qualified (`http.Header`) when it lives in
    /// another package than the looked-up type.
    pub owner: String,
    pub pointer_receiver: bool,
    /// Names of the embedded fields leading to it; empty when direct.
    pub embedded_via: Vec<String>,
    pub path: PathBuf,
    pub pos: Pos,
}

#[derive(Debug, Clone, Default)]
pub struct Members {
    pub items: Vec<Member>,
}

impl Members {
    pub fn find(&self, name: &str) -> Option<&Member> {
        self.items.iter().find(|m| m.name == name)
    }
}

/// A type whose members are collected.
struct Entry {
    pkg: Arc<Package>,
    name: String,
    /// Package name to qualify the owner with; `None` in the starting package.
    qual: Option<String>,
    via: Vec<String>,
    /// Whether methods declared on this very type count. They do not for the
    /// underlying type of `type A B`.
    methods: bool,
}

impl Entry {
    fn owner(&self) -> String {
        match &self.qual {
            Some(q) => format!("{q}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Walk state: what was seen and what comes next.
struct Walk {
    visited: HashSet<(PathBuf, String, bool)>,
    /// Names found on shallower levels; they hide deeper ones.
    seen: HashSet<String>,
    /// Names found on this level.
    level: HashSet<String>,
    items: Vec<Member>,
    same: VecDeque<Entry>,
    next: Vec<Entry>,
}

impl Walk {
    fn add(&mut self, member: Member) {
        if self.seen.contains(&member.name) || !self.level.insert(member.name.clone()) {
            return;
        }
        self.items.push(member);
    }
}

impl GoEngine {
    /// Fields and methods of `type_name` in `pkg`, with those promoted
    /// through embedded structs and interfaces (up to 6 levels deep). Names
    /// on a shallower level hide deeper ones. Nothing is filtered by
    /// exportedness.
    pub fn members_of(&mut self, pkg: &Package, type_name: &str) -> Members {
        let mut walk = Walk {
            visited: HashSet::new(),
            seen: HashSet::new(),
            level: HashSet::new(),
            items: Vec::new(),
            same: VecDeque::new(),
            next: Vec::new(),
        };
        walk.next.push(Entry {
            pkg: Arc::new(pkg.clone()),
            name: type_name.to_owned(),
            qual: None,
            via: Vec::new(),
            methods: true,
        });
        for _ in 0..=MAX_EMBED_DEPTH {
            if walk.next.is_empty() {
                break;
            }
            walk.same = std::mem::take(&mut walk.next).into();
            while let Some(entry) = walk.same.pop_front() {
                if walk.visited.len() > MAX_VISITED {
                    break;
                }
                self.collect(entry, &mut walk);
            }
            walk.seen.extend(walk.level.drain());
        }
        Members { items: walk.items }
    }

    fn collect(&mut self, entry: Entry, walk: &mut Walk) {
        let key = (entry.pkg.dir.clone(), entry.name.clone(), entry.methods);
        if !walk.visited.insert(key) {
            return;
        }
        let pkg = entry.pkg.clone();
        let Some((file, decl)) = pkg.type_decl(&entry.name) else {
            return;
        };
        let owner = entry.owner();
        let member = |name: &str, kind, ty: String, doc: &str, ptr: bool, pos: Pos| Member {
            name: name.to_owned(),
            kind,
            ty,
            doc: doc.to_owned(),
            owner: owner.clone(),
            pointer_receiver: ptr,
            embedded_via: entry.via.clone(),
            path: file.path.clone(),
            pos,
        };

        if decl.kind == TypeKind::Alias {
            let target = TypeExpr::parse(&decl.underlying);
            if let Some(next) = self.resolve(&pkg, file, &target, &entry.qual) {
                walk.same.push_back(Entry {
                    pkg: next.0,
                    name: next.1,
                    qual: next.2,
                    via: entry.via.clone(),
                    methods: entry.methods,
                });
            }
            return;
        }

        if entry.methods {
            for (decl_file, func) in pkg.methods_of(&entry.name) {
                let ptr = func.receiver.as_ref().is_some_and(|r| r.pointer);
                let mut m = member(
                    &func.name,
                    MemberKind::Method,
                    signature(func),
                    &func.doc,
                    ptr,
                    func.pos,
                );
                m.path = decl_file.path.clone();
                walk.add(m);
            }
        }

        match decl.kind {
            TypeKind::Struct => {
                for field in &decl.fields {
                    walk.add(member(
                        &field.name,
                        MemberKind::Field,
                        field.ty.clone(),
                        &field.doc,
                        false,
                        field.pos,
                    ));
                    if field.embedded {
                        let te = TypeExpr::parse(&field.ty);
                        if let Some(next) = self.resolve(&pkg, file, &te, &entry.qual) {
                            let mut via = entry.via.clone();
                            via.push(field.name.clone());
                            walk.next.push(Entry {
                                pkg: next.0,
                                name: next.1,
                                qual: next.2,
                                via,
                                methods: true,
                            });
                        }
                    }
                }
            }
            TypeKind::Interface => {
                for method in &decl.methods {
                    walk.add(member(
                        &method.name,
                        MemberKind::Method,
                        method.signature.clone(),
                        &method.doc,
                        false,
                        method.pos,
                    ));
                }
                for embed in &decl.embeds {
                    let te = TypeExpr::parse(embed);
                    if matches!(&te, TypeExpr::Name { pkg: None, name, .. } if name == "error") {
                        walk.add(member(
                            "Error",
                            MemberKind::Method,
                            "() string".to_owned(),
                            "",
                            false,
                            decl.pos,
                        ));
                        continue;
                    }
                    if let Some(next) = self.resolve(&pkg, file, &te, &entry.qual) {
                        let mut via = entry.via.clone();
                        via.push(next.1.clone());
                        walk.next.push(Entry {
                            pkg: next.0,
                            name: next.1,
                            qual: next.2,
                            via,
                            methods: true,
                        });
                    }
                }
            }
            TypeKind::Named => {
                // `type A B`: A keeps the fields (or interface methods) of B's
                // underlying type, not the methods declared on B.
                let target = TypeExpr::parse(&decl.underlying);
                if let TypeExpr::Name { .. } = target
                    && let Some(next) = self.resolve(&pkg, file, &target, &entry.qual)
                {
                    walk.same.push_back(Entry {
                        pkg: next.0,
                        name: next.1,
                        qual: next.2,
                        via: entry.via.clone(),
                        methods: false,
                    });
                }
            }
            TypeKind::Alias => {}
        }
    }

    /// The declaration a type reference in `file` (of `pkg`) points at, the
    /// package it lives in and the qualifier to show for it.
    fn resolve(
        &mut self,
        pkg: &Arc<Package>,
        file: &GoFile,
        te: &TypeExpr,
        qual: &Option<String>,
    ) -> Option<(Arc<Package>, String, Option<String>)> {
        let TypeExpr::Name { pkg: q, name, .. } = te.deref() else {
            return None;
        };
        match q {
            None => {
                pkg.type_decl(name)?;
                Some((pkg.clone(), name.clone(), qual.clone()))
            }
            Some(local) => {
                let path = self.import_names(file).remove(local)?;
                let target = self.package_by_import(&path)?;
                target.type_decl(name)?;
                let shown = (!target.name.is_empty()).then(|| target.name.clone());
                Some((target, name.clone(), shown))
            }
        }
    }
}

/// `(a int, b ...string) (string, error)`.
fn signature(func: &FuncDecl) -> String {
    let param = |p: &Param| {
        let ty = if p.variadic {
            format!("...{}", p.ty)
        } else {
            p.ty.clone()
        };
        if p.name.is_empty() {
            ty
        } else {
            format!("{} {ty}", p.name)
        }
    };
    let params: Vec<String> = func.params.iter().map(param).collect();
    let mut out = format!("({})", params.join(", "));
    match func.results.as_slice() {
        [] => {}
        [one] if one.name.is_empty() => {
            out.push(' ');
            out.push_str(&param(one));
        }
        many => {
            let list: Vec<String> = many.iter().map(param).collect();
            out.push_str(&format!(" ({})", list.join(", ")));
        }
    }
    out
}
