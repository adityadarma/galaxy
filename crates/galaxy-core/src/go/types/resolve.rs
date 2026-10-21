//! Type text to [`Ty`]: names resolved through the package, its imports and
//! the universe, named types followed to what they are made of, and the type
//! of a field or method with the type arguments of its receiver bound.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Ty;
use super::site::{Ctx, Env, Eval, MAX_DEPTH, uni};
use crate::go::engine::{Member, MemberKind, Package};
use crate::go::parse::{Pos, TypeDecl, TypeKind};
use crate::go::texpr::TypeExpr;

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod resolve_tests;

/// Aliases and `type A B` hops followed by [`Eval::underlying`].
const MAX_HOPS: usize = 8;

/// A type declaration with the package and file it lives in.
struct Decl {
    pkg: Arc<Package>,
    path: PathBuf,
    decl: TypeDecl,
}

/// Type parameters of `decl` bound to `args` by position; missing ones are
/// unknown.
fn bind(decl: &TypeDecl, args: &[Ty]) -> Env {
    let mut env = Env::new();
    for (i, param) in decl.type_params.iter().enumerate() {
        if param.name != "_" {
            env.insert(
                param.name.clone(),
                args.get(i).cloned().unwrap_or(Ty::Unknown),
            );
        }
    }
    env
}

fn is_universe_interface(name: &str) -> bool {
    matches!(name, "error" | "any" | "comparable")
}

fn error_method() -> Ty {
    Ty::Func {
        params: Vec::new(),
        results: vec![uni("string")],
        variadic: false,
    }
}

/// Names a method's receiver gives the type parameters: `(b *Box[U])`.
fn receiver_params(pkg: &Package, type_name: &str, method: &str) -> Option<Vec<String>> {
    let (_, func) = pkg
        .methods_of(type_name)
        .into_iter()
        .find(|(_, f)| f.name == method)?;
    let recv = func.receiver.as_ref()?;
    let TypeExpr::Name { args, .. } = TypeExpr::parse(&recv.ty).deref().clone() else {
        return Some(Vec::new());
    };
    Some(
        args.iter()
            .map(|a| match a {
                TypeExpr::Name {
                    pkg: None, name, ..
                } => name.clone(),
                _ => "_".to_owned(),
            })
            .collect(),
    )
}

impl Eval<'_> {
    /// Parses `text` as a type and resolves it in `ctx`, with the type
    /// parameters in `env` replaced by their binding.
    pub(super) fn ty_of_text(&mut self, ctx: &Ctx, text: &str, env: &Env) -> Ty {
        if self.depth >= MAX_DEPTH {
            return Ty::Unknown;
        }
        let expr = TypeExpr::parse(text);
        self.ty_of_expr(ctx, &expr, env)
    }

    /// What a named type is made of, following aliases and `type A B` up to
    /// 8 hops. `string` stays a named type, `error` and `any` are interfaces.
    /// Types other than named ones are returned as they are.
    pub(super) fn underlying(&mut self, ty: &Ty) -> Ty {
        if self.depth >= MAX_DEPTH {
            return Ty::Unknown;
        }
        self.depth += 1;
        let out = self.underlying_inner(ty);
        self.depth -= 1;
        out
    }

    /// The type of the field or method `name` of `recv`, through one level
    /// of pointer and through embedded types.
    pub(super) fn member_ty(&mut self, recv: &Ty, name: &str) -> Option<Ty> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let out = self.member_ty_inner(recv, name);
        self.depth -= 1;
        out
    }

    /// Every field and method of `recv`, promoted ones included.
    pub(super) fn members(&mut self, recv: &Ty) -> Vec<Member> {
        let Ty::Named { pkg, name, .. } = recv.deref() else {
            return Vec::new();
        };
        if pkg.is_empty() {
            if name == "error" {
                return vec![Member {
                    name: "Error".to_owned(),
                    kind: MemberKind::Method,
                    ty: "() string".to_owned(),
                    doc: String::new(),
                    owner: "error".to_owned(),
                    pointer_receiver: false,
                    embedded_via: Vec::new(),
                    path: PathBuf::new(),
                    pos: Pos::default(),
                }];
            }
            return Vec::new();
        }
        let Some(package) = self.package_for(pkg) else {
            return Vec::new();
        };
        self.engine.members_of(&package, name).items
    }

    /// The results of a call of `f` as one type: none give an empty tuple,
    /// one gives that type, several a tuple.
    pub(super) fn call_result(&self, f: &Ty) -> Ty {
        match f {
            Ty::Func { results, .. } => match results.as_slice() {
                [one] => one.clone(),
                many => Ty::Tuple(many.to_vec()),
            },
            _ => Ty::Unknown,
        }
    }

    fn ty_of_expr(&mut self, ctx: &Ctx, expr: &TypeExpr, env: &Env) -> Ty {
        let boxed = |this: &mut Self, t: &TypeExpr| Box::new(this.ty_of_expr(ctx, t, env));
        match expr {
            TypeExpr::Name { pkg, name, args } => {
                let args: Vec<Ty> = args.iter().map(|a| self.ty_of_expr(ctx, a, env)).collect();
                match pkg {
                    Some(local) => match ctx.imports.get(local) {
                        Some(path) => Ty::Named {
                            pkg: path.clone(),
                            name: name.clone(),
                            args,
                        },
                        None => Ty::Unknown,
                    },
                    None => {
                        if args.is_empty()
                            && let Some(bound) = env.get(name)
                        {
                            return bound.clone();
                        }
                        if ctx.pkg.type_decl(name).is_some() {
                            Ty::Named {
                                pkg: ctx.pkg.import_path.clone(),
                                name: name.clone(),
                                args,
                            }
                        } else if TypeExpr::is_builtin(name) {
                            Ty::Named {
                                pkg: String::new(),
                                name: name.clone(),
                                args,
                            }
                        } else {
                            Ty::Unknown
                        }
                    }
                }
            }
            TypeExpr::Pointer(t) => Ty::Pointer(boxed(self, t)),
            TypeExpr::Slice(t) | TypeExpr::Ellipsis(t) => Ty::Slice(boxed(self, t)),
            TypeExpr::Array { elem, .. } => Ty::Array(boxed(self, elem)),
            TypeExpr::Map { key, value } => {
                let k = boxed(self, key);
                Ty::Map(k, boxed(self, value))
            }
            TypeExpr::Chan { elem, .. } => Ty::Chan(boxed(self, elem)),
            TypeExpr::Func {
                params,
                results,
                variadic,
            } => Ty::Func {
                params: params
                    .iter()
                    .map(|p| self.ty_of_expr(ctx, p, env))
                    .collect(),
                results: results
                    .iter()
                    .map(|r| self.ty_of_expr(ctx, r, env))
                    .collect(),
                variadic: *variadic,
            },
            TypeExpr::Struct => Ty::Struct,
            TypeExpr::Interface => Ty::Interface,
            TypeExpr::Union(_) | TypeExpr::Unknown => Ty::Unknown,
        }
    }

    fn underlying_inner(&mut self, ty: &Ty) -> Ty {
        let mut cur = ty.clone();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for _ in 0..MAX_HOPS {
            let Ty::Named { pkg, name, args } = &cur else {
                return cur;
            };
            if pkg.is_empty() {
                return if is_universe_interface(name) {
                    Ty::Interface
                } else {
                    cur
                };
            }
            if !seen.insert((pkg.clone(), name.clone())) {
                return Ty::Unknown;
            }
            let Some(d) = self.decl_of(pkg, name) else {
                return Ty::Unknown;
            };
            cur = match d.decl.kind {
                TypeKind::Struct => return Ty::Struct,
                TypeKind::Interface => return Ty::Interface,
                TypeKind::Alias | TypeKind::Named => {
                    let text = d.decl.underlying.clone();
                    self.decl_text(&d, args, &text)
                }
            };
        }
        Ty::Unknown
    }

    fn member_ty_inner(&mut self, recv: &Ty, name: &str) -> Option<Ty> {
        let Ty::Named {
            pkg, name: tname, ..
        } = recv.deref()
        else {
            return None;
        };
        if pkg.is_empty() {
            return (tname == "error" && name == "Error").then(error_method);
        }
        let package = self.package_for(pkg)?;
        let members = self.engine.members_of(&package, tname);
        let member = members.find(name)?.clone();
        let (decl_pkg, owner, args) = self.declarer(recv.deref(), &member)?;
        let d = self.decl_of(&decl_pkg, &owner)?;
        let ctx = self.ctx_for(&d.pkg, &member.path)?;
        match member.kind {
            MemberKind::Field => {
                let env = bind(&d.decl, &args);
                Some(self.ty_of_text(&ctx, &member.ty, &env))
            }
            MemberKind::Method => {
                let env = match receiver_params(&d.pkg, &owner, name) {
                    Some(names) => names
                        .into_iter()
                        .enumerate()
                        .filter(|(_, n)| n != "_")
                        .map(|(i, n)| (n, args.get(i).cloned().unwrap_or(Ty::Unknown)))
                        .collect(),
                    None => bind(&d.decl, &args),
                };
                let expr = TypeExpr::parse(&format!("func{}", member.ty));
                Some(self.ty_of_expr(&ctx, &expr, &env))
            }
        }
    }

    /// Import path, name and type arguments of the type that declares
    /// `member` of `base`, following the embedded fields it was promoted by.
    fn declarer(&mut self, base: &Ty, member: &Member) -> Option<(String, String, Vec<Ty>)> {
        let decl_pkg = self.pkg_of_path(&member.path)?;
        let owner = member
            .owner
            .rsplit('.')
            .next()
            .unwrap_or(&member.owner)
            .to_owned();
        let mut cur = base.clone();
        for via in &member.embedded_via {
            cur = self.embedded_step(&cur, via);
            if cur.is_unknown() {
                break;
            }
        }
        let args = match cur.deref() {
            Ty::Named { pkg, name, args } if *name == owner && *pkg == decl_pkg.import_path => {
                args.clone()
            }
            _ => Vec::new(),
        };
        Some((decl_pkg.import_path.clone(), owner, args))
    }

    /// The type embedded in `cur` under the name `via`.
    fn embedded_step(&mut self, cur: &Ty, via: &str) -> Ty {
        let mut cur = cur.deref().clone();
        for _ in 0..MAX_HOPS {
            let Ty::Named { pkg, name, args } = &cur else {
                return Ty::Unknown;
            };
            let Some(d) = self.decl_of(pkg, name) else {
                return Ty::Unknown;
            };
            let text = match d.decl.kind {
                TypeKind::Struct => d
                    .decl
                    .fields
                    .iter()
                    .find(|f| f.embedded && f.name == via)
                    .map(|f| f.ty.clone()),
                TypeKind::Interface => d
                    .decl
                    .embeds
                    .iter()
                    .find(|e| TypeExpr::parse(e).base_name() == Some(via))
                    .cloned(),
                TypeKind::Alias | TypeKind::Named => {
                    let text = d.decl.underlying.clone();
                    let next = self.decl_text(&d, args, &text);
                    cur = next.deref().clone();
                    continue;
                }
            };
            return match text {
                Some(text) => self.decl_text(&d, args, &text),
                None => Ty::Unknown,
            };
        }
        Ty::Unknown
    }

    /// The declaration of `name` in the package `pkg`.
    fn decl_of(&mut self, pkg: &str, name: &str) -> Option<Decl> {
        let package = self.package_for(pkg)?;
        let (path, decl) = {
            let (file, decl) = package.type_decl(name)?;
            (file.path.clone(), decl.clone())
        };
        Some(Decl {
            pkg: package,
            path,
            decl,
        })
    }

    /// `text` as written in the file of `d`, with its type parameters bound
    /// to `args`.
    fn decl_text(&mut self, d: &Decl, args: &[Ty], text: &str) -> Ty {
        let Some(ctx) = self.ctx_for(&d.pkg, &d.path) else {
            return Ty::Unknown;
        };
        let env = bind(&d.decl, args);
        self.ty_of_text(&ctx, text, &env)
    }

    /// The package the file `path` belongs to.
    pub(super) fn pkg_of_path(&mut self, path: &Path) -> Option<Arc<Package>> {
        if self.pkg.files.iter().any(|f| f.path == path) {
            Some(self.pkg.clone())
        } else {
            self.engine.package_of_file(path)
        }
    }
}
