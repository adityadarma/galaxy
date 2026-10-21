//! Expression evaluation: the [`Ty`] of an expression text.
//!
//! The text is parsed with Tree-sitter and walked node by node. Names come
//! from the local scope first, then the package, its imports and the
//! universe. Whatever cannot be resolved is [`Ty::Unknown`]; the walk stops
//! with the same answer once the node or step budget of the request is used.

use std::sync::Arc;

use tree_sitter::Node;

use super::Ty;
use super::site::{Ctx, Env, Eval, MAX_NODES, MAX_STEPS, kids, text, uni};
use crate::go::engine::{FileSymbol, Package};
use crate::go::parse::FuncDecl;
use crate::go::scope::{Binding, Origin};

#[cfg(test)]
#[path = "eval2_tests.rs"]
mod eval2_tests;
#[cfg(test)]
#[path = "eval_tests.rs"]
mod eval_tests;

/// Node kinds of untyped literals: in `1 + x` the type of `x` wins.
pub(super) fn is_literal(kind: &str) -> bool {
    matches!(
        kind,
        "int_literal"
            | "float_literal"
            | "imaginary_literal"
            | "rune_literal"
            | "interpreted_string_literal"
            | "raw_string_literal"
    )
}

/// Whether the value of `kind` may be read with the comma-ok form.
fn is_comma_ok(node: Node<'_>) -> bool {
    match node.kind() {
        "index_expression" | "type_assertion_expression" => true,
        "unary_expression" => node
            .child_by_field_name("operator")
            .is_some_and(|op| op.kind() == "<-"),
        _ => false,
    }
}

/// Entry `index` of `count` values read from one expression of type `value`.
fn pick(value: Ty, index: usize, count: usize, comma_ok: bool) -> Ty {
    if count <= 1 {
        return value;
    }
    match value {
        Ty::Tuple(items) => items.get(index).cloned().unwrap_or(Ty::Unknown),
        Ty::Unknown => match index {
            1 if comma_ok && count == 2 => uni("bool"),
            _ => Ty::Unknown,
        },
        other => match index {
            0 => other,
            1 if comma_ok && count == 2 => uni("bool"),
            _ => Ty::Unknown,
        },
    }
}

impl Eval<'_> {
    pub(super) fn eval_text_in(&mut self, ctx: &Ctx, text: &str) -> Ty {
        self.with_expr(text, |ev, src, node| ev.eval_node(ctx, src, node))
            .unwrap_or(Ty::Unknown)
    }

    pub(super) fn ty(&mut self, ctx: &Ctx, text: &str) -> Ty {
        self.ty_of_text(ctx, text, &Env::new())
    }

    pub(super) fn eval_node(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Ty {
        if self.nodes >= MAX_NODES || self.steps >= MAX_STEPS {
            return Ty::Unknown;
        }
        self.nodes += 1;
        self.steps += 1;
        self.eval_kind(ctx, src, node)
    }

    fn eval_field(&mut self, ctx: &Ctx, src: &str, node: Node<'_>, field: &str) -> Ty {
        match node.child_by_field_name(field) {
            Some(child) => self.eval_node(ctx, src, child),
            None => Ty::Unknown,
        }
    }

    /// The type written in the `type` field of `node`.
    fn type_field(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Ty {
        match node.child_by_field_name("type") {
            Some(t) => self.ty(ctx, text(src, t)),
            None => Ty::Unknown,
        }
    }

    fn eval_kind(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Ty {
        match node.kind() {
            "identifier" => self.eval_ident(ctx, text(src, node)),
            "int_literal" => uni("int"),
            "float_literal" => uni("float64"),
            "imaginary_literal" => uni("complex128"),
            "rune_literal" => uni("rune"),
            "interpreted_string_literal" | "raw_string_literal" => uni("string"),
            "true" | "false" => uni("bool"),
            "iota" => uni("int"),
            "composite_literal" | "type_conversion_expression" | "type_assertion_expression" => {
                self.type_field(ctx, src, node)
            }
            "parenthesized_expression" => match kids(node).first() {
                Some(inner) => self.eval_node(ctx, src, *inner),
                None => Ty::Unknown,
            },
            "func_literal" => self.eval_func_literal(ctx, src, node),
            "unary_expression" => self.eval_unary(ctx, src, node),
            "binary_expression" => self.eval_binary(ctx, src, node),
            "selector_expression" => self.eval_selector(ctx, src, node),
            "call_expression" => self.eval_call(ctx, src, node),
            "index_expression" => {
                let operand = self.eval_field(ctx, src, node, "operand");
                self.index_ty(&operand)
            }
            "slice_expression" => {
                let operand = self.eval_field(ctx, src, node, "operand");
                self.slice_ty(&operand)
            }
            _ => Ty::Unknown,
        }
    }

    fn eval_ident(&mut self, ctx: &Ctx, name: &str) -> Ty {
        if let Some(binding) = ctx.scope.get(name) {
            let binding = binding.clone();
            return self.binding_ty(ctx, &binding);
        }
        match self.engine.lookup_in_file_scope(&ctx.file, &ctx.pkg, name) {
            Some(FileSymbol::Func { pkg, name }) => self.func_ty(&pkg, &name),
            Some(FileSymbol::Value { pkg, name }) => self.value_ty(&pkg, &name),
            Some(FileSymbol::Import { path }) => Ty::Package(path),
            Some(FileSymbol::Builtin) => match name {
                "true" | "false" => uni("bool"),
                "iota" => uni("int"),
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        }
    }

    /// The type of a local name, from what it was declared with.
    pub(super) fn binding_ty(&mut self, ctx: &Ctx, binding: &Binding) -> Ty {
        match &binding.origin {
            Origin::Declared(text) => self.ty(ctx, text),
            Origin::Expr { text, index, count } => self.cached(binding, |ev, site| {
                let (value, comma_ok) = ev
                    .with_expr(text, |ev, src, node| {
                        (ev.eval_node(site, src, node), is_comma_ok(node))
                    })
                    .unwrap_or((Ty::Unknown, false));
                pick(value, *index, *count, comma_ok)
            }),
            Origin::RangeOver { text, key } => {
                self.cached(binding, |ev, site| ev.range_ty(site, text, *key))
            }
            Origin::TypeSwitch {
                subject,
                case_types,
            } => {
                if let [one] = case_types.as_slice()
                    && one != "nil"
                {
                    return self.ty(ctx, one);
                }
                self.cached(binding, |ev, site| ev.eval_text_in(site, subject))
            }
            Origin::AssertOk | Origin::Unknown => Ty::Unknown,
        }
    }

    /// Runs `f` at the declaration of `binding` once; a name that depends
    /// on itself stays unknown.
    fn cached(&mut self, binding: &Binding, f: impl FnOnce(&mut Self, &Ctx) -> Ty) -> Ty {
        if let Some(known) = self.bindings.get(&binding.decl_byte) {
            return known.clone();
        }
        self.bindings.insert(binding.decl_byte, Ty::Unknown);
        let site = self.site_at(binding.decl_byte);
        let out = f(self, &site);
        self.bindings.insert(binding.decl_byte, out.clone());
        out
    }

    /// The function type of the package-level function `name`.
    pub(super) fn func_ty(&mut self, pkg: &Arc<Package>, name: &str) -> Ty {
        let Some((path, decl)) = pkg.func(name).map(|(f, d)| (f.path.clone(), d.clone())) else {
            return Ty::Unknown;
        };
        let Some(ctx) = self.ctx_for(pkg, &path) else {
            return Ty::Unknown;
        };
        self.func_decl_ty(&ctx, &decl)
    }

    pub(super) fn func_decl_ty(&mut self, ctx: &Ctx, decl: &FuncDecl) -> Ty {
        let mut variadic = false;
        let mut params = Vec::with_capacity(decl.params.len());
        for p in &decl.params {
            let (elem, rest) = match p.ty.strip_prefix("...") {
                Some(elem) => (elem, true),
                None => (p.ty.as_str(), p.variadic),
            };
            variadic |= rest;
            params.push(self.ty(ctx, elem));
        }
        let mut results = Vec::with_capacity(decl.results.len());
        for r in &decl.results {
            results.push(self.ty(ctx, &r.ty));
        }
        Ty::Func {
            params,
            results,
            variadic,
        }
    }

    /// The type of the package-level variable or constant `name`.
    pub(super) fn value_ty(&mut self, pkg: &Arc<Package>, name: &str) -> Ty {
        let key = (pkg.import_path.clone(), name.to_owned());
        if !self.visiting.insert(key.clone()) {
            return Ty::Unknown;
        }
        let out = self.value_ty_inner(pkg, name);
        self.visiting.remove(&key);
        out
    }

    fn value_ty_inner(&mut self, pkg: &Arc<Package>, name: &str) -> Ty {
        let Some((path, decl)) = pkg.value(name).map(|(f, d)| (f.path.clone(), d.clone())) else {
            return Ty::Unknown;
        };
        let Some(ctx) = self.ctx_for(pkg, &path) else {
            return Ty::Unknown;
        };
        if let Some(declared) = decl.ty.as_ref().or(decl.group_type.as_ref()) {
            return self.ty(&ctx, declared);
        }
        match decl.value {
            Some(value) => match self.eval_text_in(&ctx, &value) {
                Ty::Tuple(_) => Ty::Unknown,
                other => other,
            },
            None => Ty::Unknown,
        }
    }

    /// The member `name` of the imported package `path`. Types are not
    /// values.
    fn package_member(&mut self, path: &str, name: &str) -> Ty {
        let Some(pkg) = self.package_for(path) else {
            return Ty::Unknown;
        };
        if pkg.func(name).is_some() {
            return self.func_ty(&pkg, name);
        }
        if pkg.value(name).is_some() {
            return self.value_ty(&pkg, name);
        }
        Ty::Unknown
    }

    /// `ty`, or what it is made of when it is a declared type.
    pub(super) fn under(&mut self, ty: &Ty) -> Ty {
        match ty {
            Ty::Named { .. } => self.underlying(ty),
            other => other.clone(),
        }
    }

    /// The type an expression node names, when it is one: `T`, `pkg.T`,
    /// `*T`, `[]T`, `Repo[int]`, `(T)`. A local name or a value is not.
    pub(super) fn as_type(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Option<Ty> {
        match node.kind() {
            "identifier" => {
                let name = text(src, node);
                if ctx.scope.get(name).is_some() {
                    return None;
                }
                match self
                    .engine
                    .lookup_in_file_scope(&ctx.file, &ctx.pkg, name)?
                {
                    FileSymbol::Type { pkg, name } => Some(Ty::Named {
                        pkg: pkg.import_path.clone(),
                        name,
                        args: Vec::new(),
                    }),
                    FileSymbol::Builtin if crate::go::texpr::TypeExpr::is_builtin(name) => {
                        Some(uni(name))
                    }
                    _ => None,
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
                let pkg = self.package_for(&path)?;
                let name = text(src, field);
                pkg.type_decl(name)?;
                Some(Ty::Named {
                    pkg: path,
                    name: name.to_owned(),
                    args: Vec::new(),
                })
            }
            "parenthesized_expression" => {
                let inner = *kids(node).first()?;
                self.as_type(ctx, src, inner)
            }
            "unary_expression" => {
                let op = node.child_by_field_name("operator")?;
                if op.kind() != "*" {
                    return None;
                }
                let operand = node.child_by_field_name("operand")?;
                let inner = self.as_type(ctx, src, operand)?;
                Some(Ty::Pointer(Box::new(inner)))
            }
            "index_expression" | "type_instance_expression" | "generic_type" => {
                let operand = node
                    .child_by_field_name("operand")
                    .or_else(|| node.child_by_field_name("type"))?;
                self.as_type(ctx, src, operand)?;
                Some(self.ty(ctx, text(src, node)))
            }
            "array_type" | "slice_type" | "map_type" | "channel_type" | "func_type"
            | "pointer_type" | "qualified_type" | "type_identifier" | "interface_type"
            | "struct_type" => Some(self.ty(ctx, text(src, node))),
            _ => None,
        }
    }

    fn eval_func_literal(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Ty {
        let params = node
            .child_by_field_name("parameters")
            .map_or("()", |n| text(src, n));
        let mut sig = format!("func{params}");
        if let Some(result) = node.child_by_field_name("result") {
            sig.push(' ');
            sig.push_str(text(src, result));
        }
        self.ty(ctx, &sig)
    }

    fn eval_unary(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Ty {
        let op = node.child_by_field_name("operator").map(|n| n.kind());
        if op == Some("!") {
            return uni("bool");
        }
        let operand = self.eval_field(ctx, src, node, "operand");
        match op {
            Some("&") if !operand.is_unknown() => Ty::Pointer(Box::new(operand)),
            Some("*") => match operand {
                Ty::Pointer(inner) => *inner,
                _ => Ty::Unknown,
            },
            Some("<-") => match self.under(&operand) {
                Ty::Chan(elem) => *elem,
                _ => Ty::Unknown,
            },
            Some("-" | "+" | "^") => operand,
            _ => Ty::Unknown,
        }
    }

    fn eval_binary(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Ty {
        let (Some(left), Some(right), Some(op)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
            node.child_by_field_name("operator"),
        ) else {
            return Ty::Unknown;
        };
        match op.kind() {
            "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||" => return uni("bool"),
            _ => {}
        }
        let lt = self.eval_node(ctx, src, left);
        if matches!(op.kind(), "<<" | ">>") {
            return lt;
        }
        if is_literal(left.kind()) {
            let rt = self.eval_node(ctx, src, right);
            if !rt.is_unknown() {
                return rt;
            }
        }
        lt
    }

    fn eval_selector(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Ty {
        let (Some(operand), Some(field)) = (
            node.child_by_field_name("operand"),
            node.child_by_field_name("field"),
        ) else {
            return Ty::Unknown;
        };
        // `T.Method` is a method expression.
        if self.as_type(ctx, src, operand).is_some() {
            return Ty::Unknown;
        }
        let recv = self.eval_node(ctx, src, operand);
        let name = text(src, field);
        match &recv {
            Ty::Package(path) => self.package_member(path, name),
            Ty::Unknown => Ty::Unknown,
            _ => self.member_ty(&recv, name).unwrap_or(Ty::Unknown),
        }
    }

    fn eval_call(&mut self, ctx: &Ctx, src: &str, node: Node<'_>) -> Ty {
        let Some(function) = node.child_by_field_name("function") else {
            return Ty::Unknown;
        };
        if let Some(result) = self.builtin_call(ctx, src, node, function) {
            return result;
        }
        if let Some(converted) = self.as_type(ctx, src, function) {
            return converted;
        }
        if let Some(result) = self.generic_call(ctx, src, node, function) {
            return result;
        }
        let callee = self.eval_node(ctx, src, function);
        let callee = match callee {
            Ty::Func { .. } => callee,
            other => self.under(&other),
        };
        self.call_result(&callee)
    }

    /// The element of an indexed value of type `operand`.
    fn index_ty(&mut self, operand: &Ty) -> Ty {
        match self.under(operand) {
            Ty::Slice(elem) | Ty::Array(elem) => *elem,
            Ty::Map(_, value) => *value,
            Ty::Pointer(inner) => match self.under(&inner) {
                Ty::Array(elem) => *elem,
                _ => Ty::Unknown,
            },
            Ty::Named { pkg, name, .. } if pkg.is_empty() && name == "string" => uni("byte"),
            _ => Ty::Unknown,
        }
    }

    /// The type of `operand[a:b]`.
    fn slice_ty(&mut self, operand: &Ty) -> Ty {
        match self.under(operand) {
            Ty::Slice(_) => operand.clone(),
            Ty::Array(elem) => Ty::Slice(elem),
            Ty::Pointer(inner) => match self.under(&inner) {
                Ty::Array(elem) => Ty::Slice(elem),
                _ => Ty::Unknown,
            },
            Ty::Named { pkg, name, .. } if pkg.is_empty() && name == "string" => operand.clone(),
            _ => Ty::Unknown,
        }
    }
}
