//! Type argument inference for calls of generic functions: the parameter
//! types are unified with the argument types, and the results are read with
//! the type parameters bound.

use tree_sitter::Node;

use super::Ty;
use super::builtins::call_args;
use super::site::{Ctx, Env, Eval, kids, text};
use crate::go::engine::FileSymbol;
use crate::go::parse::FuncDecl;
use crate::go::texpr::TypeExpr;

/// A generic function found for a call, with the file it is declared in.
struct Generic {
    ctx: Ctx,
    decl: FuncDecl,
}

impl Eval<'_> {
    /// The result of `function(args)` when `function` names a generic
    /// function, written with or without explicit type arguments.
    pub(super) fn generic_call(
        &mut self,
        ctx: &Ctx,
        src: &str,
        call: Node<'_>,
        function: Node<'_>,
    ) -> Option<Ty> {
        let (name_node, explicit) = self.split_instantiation(src, call, function);
        let generic = self.generic_decl(ctx, src, name_node)?;
        let mut env = Env::new();
        for tp in &generic.decl.type_params {
            env.insert(tp.name.clone(), Ty::Unknown);
        }
        for (tp, text) in generic.decl.type_params.iter().zip(&explicit) {
            let bound = self.ty(ctx, text);
            env.insert(tp.name.clone(), bound);
        }
        let names: Vec<String> = generic
            .decl
            .type_params
            .iter()
            .map(|tp| tp.name.clone())
            .collect();
        let args = call_args(call);
        for (i, arg) in args.iter().enumerate() {
            let Some((pattern, spread)) = param_pattern(&generic.decl, i, arg.kind()) else {
                break;
            };
            let value = match arg.kind() {
                "variadic_argument" => match kids(*arg).first() {
                    Some(inner) => self.eval_node(ctx, src, *inner),
                    None => Ty::Unknown,
                },
                _ => self.eval_node(ctx, src, *arg),
            };
            let pattern = if spread {
                TypeExpr::Slice(Box::new(pattern))
            } else {
                pattern
            };
            self.unify(&pattern, &value, &mut env, &names);
        }
        let results: Vec<Ty> = generic
            .decl
            .results
            .iter()
            .map(|r| self.ty_of_text(&generic.ctx, &r.ty, &env))
            .collect();
        Some(match results.as_slice() {
            [one] => one.clone(),
            many => Ty::Tuple(many.to_vec()),
        })
    }

    /// The callee node and the type argument texts of `F[int, string]`.
    fn split_instantiation<'t>(
        &mut self,
        src: &str,
        call: Node<'t>,
        function: Node<'t>,
    ) -> (Node<'t>, Vec<String>) {
        let mut texts = Vec::new();
        if let Some(list) = call.child_by_field_name("type_arguments") {
            texts.extend(kids(list).into_iter().map(|n| text(src, n).to_owned()));
            return (function, texts);
        }
        match function.kind() {
            "index_expression" => {
                let operand = function.child_by_field_name("operand");
                let index = function.child_by_field_name("index");
                match (operand, index) {
                    (Some(operand), Some(index)) => {
                        texts.push(text(src, index).to_owned());
                        (operand, texts)
                    }
                    _ => (function, texts),
                }
            }
            "type_instantiation_expression" => {
                let Some(inner) = function.child_by_field_name("type") else {
                    return (function, texts);
                };
                for child in kids(function) {
                    if child.id() != inner.id() {
                        texts.push(text(src, child).to_owned());
                    }
                }
                (inner, texts)
            }
            _ => (function, texts),
        }
    }

    /// The generic function `node` names: `F` or `pkg.F`.
    fn generic_decl(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Option<Generic> {
        let (pkg, name) = match node.kind() {
            "identifier" => {
                let name = text(src, node);
                if ctx.scope.get(name).is_some() {
                    return None;
                }
                match self
                    .engine
                    .lookup_in_file_scope(&ctx.file, &ctx.pkg, name)?
                {
                    FileSymbol::Func { pkg, name } => (pkg, name),
                    _ => return None,
                }
            }
            "selector_expression" => {
                let operand = node.child_by_field_name("operand")?;
                let field = node.child_by_field_name("field")?;
                if operand.kind() != "identifier" {
                    return None;
                }
                let local = text(src, operand);
                if ctx.scope.get(local).is_some() {
                    return None;
                }
                let path = ctx.imports.get(local)?.clone();
                (self.package_for(&path)?, text(src, field).to_owned())
            }
            _ => return None,
        };
        let (path, decl) = pkg.func(&name).map(|(f, d)| (f.path.clone(), d.clone()))?;
        if decl.type_params.is_empty() {
            return None;
        }
        let ctx = self.ctx_for(&pkg, &path)?;
        Some(Generic { ctx, decl })
    }

    /// Binds the type parameters `names` in `pattern` to what `actual` has
    /// in the same place. A parameter already bound keeps its binding.
    pub(super) fn unify(
        &mut self,
        pattern: &TypeExpr,
        actual: &Ty,
        env: &mut Env,
        names: &[String],
    ) {
        if actual.is_unknown() || self.depth >= super::site::MAX_DEPTH {
            return;
        }
        match pattern {
            TypeExpr::Name {
                pkg: None,
                name,
                args,
            } if args.is_empty() && names.contains(name) => {
                if env.get(name).is_none_or(Ty::is_unknown) {
                    env.insert(name.clone(), actual.clone());
                }
            }
            TypeExpr::Name { name, args, .. } if !args.is_empty() => {
                if let Ty::Named {
                    name: actual_name,
                    args: actual_args,
                    ..
                } = actual.deref()
                    && actual_name == name
                {
                    for (p, a) in args.iter().zip(actual_args) {
                        self.unify(p, a, env, names);
                    }
                }
            }
            TypeExpr::Name { .. } => {}
            TypeExpr::Pointer(p) => {
                if let Ty::Pointer(a) = actual {
                    self.unify(p, a, env, names);
                }
            }
            TypeExpr::Slice(p) | TypeExpr::Ellipsis(p) => {
                if let Ty::Slice(a) = self.under(actual) {
                    self.unify(p, &a, env, names);
                }
            }
            TypeExpr::Array { elem, .. } => match self.under(actual) {
                Ty::Array(a) | Ty::Slice(a) => self.unify(elem, &a, env, names),
                _ => {}
            },
            TypeExpr::Map { key, value } => {
                if let Ty::Map(k, v) = self.under(actual) {
                    self.unify(key, &k, env, names);
                    self.unify(value, &v, env, names);
                }
            }
            TypeExpr::Chan { elem, .. } => {
                if let Ty::Chan(a) = self.under(actual) {
                    self.unify(elem, &a, env, names);
                }
            }
            TypeExpr::Func {
                params, results, ..
            } => {
                if let Ty::Func {
                    params: ap,
                    results: ar,
                    ..
                } = self.under(actual)
                {
                    for (p, a) in params.iter().zip(&ap) {
                        self.unify(p, a, env, names);
                    }
                    for (p, a) in results.iter().zip(&ar) {
                        self.unify(p, a, env, names);
                    }
                }
            }
            TypeExpr::Struct | TypeExpr::Interface | TypeExpr::Union(_) | TypeExpr::Unknown => {}
        }
    }
}

/// The type of the parameter that argument `index` is passed to, and whether
/// the argument spreads a slice into a variadic parameter (`xs...`).
fn param_pattern(decl: &FuncDecl, index: usize, kind: &str) -> Option<(TypeExpr, bool)> {
    let last = decl.params.len().checked_sub(1)?;
    let param = decl.params.get(index.min(last))?;
    if index > last && !is_variadic(param) {
        return None;
    }
    let text = param.ty.strip_prefix("...").unwrap_or(&param.ty);
    let expr = TypeExpr::parse(text);
    Some((expr, kind == "variadic_argument" && is_variadic(param)))
}

fn is_variadic(param: &crate::go::parse::Param) -> bool {
    param.variadic || param.ty.starts_with("...")
}
