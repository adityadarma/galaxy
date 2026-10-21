//! `resolve_at`: what the identifier under the cursor stands for.

use std::path::PathBuf;
use std::sync::Arc;

use tree_sitter::Node;

use super::sig::{self, Subst};
use super::{Resolved, Symbol};
use crate::go::engine::{FileSymbol, Member, MemberKind, Package};
use crate::go::parse::{IdentAt, Pos, exported};
use crate::go::scope::{Binding, BindingKind, Origin};
use crate::go::texpr::TypeExpr;
use crate::go::types::Ty;
use crate::go::types::site::{Ctx, Eval, text, uni};

/// Deepest literal nesting followed to find the type of a composite literal.
const MAX_LITERAL_DEPTH: usize = 16;

struct Out {
    symbol: Symbol,
    ty: Ty,
    signature: String,
    doc: String,
}

pub(super) fn resolve(ev: &mut Eval<'_>, src: &str, id: &IdentAt) -> Option<Resolved> {
    let tree = ev.tree;
    let node = tree
        .root_node()
        .descendant_for_byte_range(id.start_byte, id.end_byte)?;
    if src.get(node.byte_range())? != id.name {
        return None;
    }
    let out = classify(ev, src, node, id)?;
    Some(Resolved {
        symbol: out.symbol,
        ty: out.ty,
        signature: out.signature,
        doc: out.doc,
        start_line: id.line,
        start_column: id.column,
        end_column: id.end_column,
    })
}

fn classify(ev: &mut Eval<'_>, src: &str, node: Node<'_>, id: &IdentAt) -> Option<Out> {
    let parent = node.parent();
    match node.kind() {
        "field_identifier" => {
            let p = parent?;
            match p.kind() {
                "selector_expression" if is_field(p, "field", node) => selector(ev, src, p, id),
                "field_declaration" => field_decl(ev, src, p, id),
                "method_declaration" => method_decl_site(ev, id),
                "method_elem" => iface_method(ev, src, p, id),
                _ => None,
            }
        }
        "package_identifier" => package_ident(ev, parent?, id),
        "label_name" => label(ev, id),
        "type_identifier" => {
            let p = parent?;
            match p.kind() {
                "type_spec" | "type_alias" if is_field(p, "name", node) => type_decl_site(ev, id),
                "qualified_type" if is_field(p, "name", node) => qualified_type(ev, p, src, id),
                _ => bare_name(ev, src, node, id, true),
            }
        }
        _ => value_ident(ev, src, node, parent, id),
    }
}

fn is_field(parent: Node<'_>, field: &str, node: Node<'_>) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|n| n.id() == node.id())
}

fn is_name_of(parent: Node<'_>, node: Node<'_>) -> bool {
    let mut cursor = parent.walk();
    parent
        .children_by_field_name("name", &mut cursor)
        .any(|n| n.id() == node.id())
}

fn value_ident(
    ev: &mut Eval<'_>,
    src: &str,
    node: Node<'_>,
    parent: Option<Node<'_>>,
    id: &IdentAt,
) -> Option<Out> {
    if let Some(p) = parent {
        match p.kind() {
            "function_declaration" if is_field(p, "name", node) => {
                return func_decl_site(ev, id);
            }
            "type_parameter_declaration" if is_name_of(p, node) => {
                let constraint = p
                    .child_by_field_name("type")
                    .map(|t| text(src, t).to_owned())
                    .unwrap_or_default();
                return Some(type_param_out(&id.name, &constraint));
            }
            _ => {}
        }
    }
    if let Some(out) = keyed_field(ev, src, node, id) {
        return Some(out);
    }
    if id.name != "_"
        && let Some(out) = declared_local(ev, src, node, id)
    {
        return Some(out);
    }
    bare_name(ev, src, node, id, false)
}

// ---- packages -----------------------------------------------------------

fn package_ident(ev: &mut Eval<'_>, parent: Node<'_>, id: &IdentAt) -> Option<Out> {
    if !matches!(parent.kind(), "qualified_type" | "import_spec") {
        return None;
    }
    let ctx = ev.site_at(id.start_byte);
    let path = ctx.imports.get(&id.name)?.clone();
    Some(package_out(ev, &id.name, &path))
}

fn package_out(ev: &mut Eval<'_>, name: &str, path: &str) -> Out {
    let doc = ev
        .package_for(path)
        .and_then(|p| {
            p.files
                .iter()
                .map(|f| f.package_doc.clone())
                .find(|d| !d.is_empty())
        })
        .unwrap_or_default();
    Out {
        symbol: Symbol::Package {
            import_path: path.to_owned(),
        },
        ty: Ty::Package(path.to_owned()),
        signature: format!("package {name}"),
        doc,
    }
}

fn qualified_type(ev: &mut Eval<'_>, qualified: Node<'_>, src: &str, id: &IdentAt) -> Option<Out> {
    let local = text(src, qualified.child_by_field_name("package")?);
    let ctx = ev.site_at(id.start_byte);
    let path = ctx.imports.get(local)?.clone();
    package_member(ev, &path, &id.name)
}

// ---- declarations in the current file -------------------------------------

fn func_decl_site(ev: &mut Eval<'_>, id: &IdentAt) -> Option<Out> {
    let file = ev.file.clone();
    let decl = file
        .funcs
        .iter()
        .find(|f| f.receiver.is_none() && same_pos(f.pos, id) && f.name == id.name)?;
    let ctx = ev.site_at(id.start_byte);
    Some(Out {
        symbol: Symbol::Func {
            pkg: ev.pkg.import_path.clone(),
            name: decl.name.clone(),
            path: ev.path.to_path_buf(),
            pos: decl.pos,
        },
        ty: ev.func_decl_ty(&ctx, decl),
        signature: sig::func_sig(decl, &Subst::new()),
        doc: decl.doc.clone(),
    })
}

fn type_decl_site(ev: &mut Eval<'_>, id: &IdentAt) -> Option<Out> {
    let file = ev.file.clone();
    let decl = file
        .types
        .iter()
        .find(|t| t.name == id.name && same_pos(t.pos, id))?;
    Some(Out {
        symbol: Symbol::Type {
            pkg: ev.pkg.import_path.clone(),
            name: decl.name.clone(),
            path: ev.path.to_path_buf(),
            pos: decl.pos,
        },
        ty: Ty::Named {
            pkg: ev.pkg.import_path.clone(),
            name: decl.name.clone(),
            args: Vec::new(),
        },
        signature: sig::type_sig(decl),
        doc: decl.doc.clone(),
    })
}

fn method_decl_site(ev: &mut Eval<'_>, id: &IdentAt) -> Option<Out> {
    let file = ev.file.clone();
    let decl = file
        .funcs
        .iter()
        .find(|f| f.receiver.is_some() && f.name == id.name && same_pos(f.pos, id))?;
    let ctx = ev.site_at(id.start_byte);
    Some(Out {
        symbol: Symbol::Method {
            owner: decl.receiver.as_ref().map(|r| r.base.clone())?,
            name: decl.name.clone(),
            path: ev.path.to_path_buf(),
            pos: decl.pos,
        },
        ty: ev.func_decl_ty(&ctx, decl),
        signature: sig::func_sig(decl, &Subst::new()),
        doc: decl.doc.clone(),
    })
}

fn same_pos(pos: Pos, id: &IdentAt) -> bool {
    pos.line == id.line && pos.column == id.column
}

/// The name of the type declaration around `node`.
fn enclosing_type_name(node: Node<'_>, src: &str) -> Option<String> {
    let mut cur = node;
    while let Some(p) = cur.parent() {
        if matches!(p.kind(), "type_spec" | "type_alias") {
            return p
                .child_by_field_name("name")
                .map(|n| text(src, n).to_owned());
        }
        cur = p;
    }
    None
}

fn field_decl(ev: &mut Eval<'_>, src: &str, decl: Node<'_>, id: &IdentAt) -> Option<Out> {
    let owner = enclosing_type_name(decl, src).unwrap_or_default();
    let file = ev.file.clone();
    let found = file.types.iter().find(|t| t.name == owner).and_then(|t| {
        t.fields
            .iter()
            .find(|f| f.name == id.name && same_pos(f.pos, id))
    });
    let ty_text = match found {
        Some(f) => f.ty.clone(),
        None => decl
            .child_by_field_name("type")
            .map(|t| {
                text(src, t)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default(),
    };
    let ctx = ev.site_at(id.start_byte);
    let ty = ev.ty(&ctx, &ty_text);
    Some(Out {
        symbol: Symbol::Field {
            owner,
            name: id.name.clone(),
            path: ev.path.to_path_buf(),
            pos: Pos {
                line: id.line,
                column: id.column,
                end_column: id.end_column,
            },
        },
        ty,
        signature: format!("field {} {ty_text}", id.name),
        doc: found.map(|f| f.doc.clone()).unwrap_or_default(),
    })
}

fn iface_method(ev: &mut Eval<'_>, src: &str, elem: Node<'_>, id: &IdentAt) -> Option<Out> {
    let owner = enclosing_type_name(elem, src)?;
    let file = ev.file.clone();
    let decl = file.types.iter().find(|t| t.name == owner)?;
    let m = decl
        .methods
        .iter()
        .find(|m| m.name == id.name && same_pos(m.pos, id))?;
    let ctx = ev.site_at(id.start_byte);
    Some(Out {
        symbol: Symbol::Method {
            owner: owner.clone(),
            name: m.name.clone(),
            path: ev.path.to_path_buf(),
            pos: m.pos,
        },
        ty: ev.ty(&ctx, &format!("func{}", m.signature)),
        signature: format!("func ({owner}) {}{}", m.name, m.signature),
        doc: m.doc.clone(),
    })
}

// ---- locals -------------------------------------------------------------

/// What kind of local an identifier declares, when it is in a declaring
/// position.
fn decl_kind(node: Node<'_>) -> Option<&'static str> {
    let p = node.parent()?;
    match p.kind() {
        "parameter_declaration" | "variadic_parameter_declaration" if is_name_of(p, node) => {
            Some("param")
        }
        "var_spec" if is_name_of(p, node) => Some("var"),
        "const_spec" if is_name_of(p, node) => Some("const"),
        "expression_list" => {
            let gp = p.parent()?;
            match gp.kind() {
                "short_var_declaration" if is_field(gp, "left", p) => Some("var"),
                "range_clause" if is_field(gp, "left", p) => Some("range var"),
                "type_switch_statement" if is_field(gp, "alias", p) => Some("type switch var"),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Bytes at which the local declared by `node` is already in scope.
fn decl_candidates(node: Node<'_>, src: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut cur = node;
    while let Some(p) = cur.parent() {
        let end = p.end_byte();
        if src.as_bytes().get(end) == Some(&b'\n') {
            out.push(end + 1);
        }
        for field in ["body", "consequence"] {
            if let Some(block) = p.child_by_field_name(field) {
                out.push(block.start_byte() + 1);
            }
        }
        if p.kind() == "type_switch_statement" {
            let mut c = p.walk();
            for case in p.named_children(&mut c) {
                if matches!(case.kind(), "type_case" | "default_case") {
                    out.push(case.end_byte());
                }
            }
        }
        if matches!(
            p.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        ) {
            if let Some(body) = p.child_by_field_name("body") {
                out.push(body.start_byte() + 1);
                out.push(body.end_byte().saturating_sub(1));
            }
            break;
        }
        cur = p;
    }
    out.dedup();
    out
}

fn declared_local(ev: &mut Eval<'_>, src: &str, node: Node<'_>, id: &IdentAt) -> Option<Out> {
    let kind = decl_kind(node)?;
    for candidate in decl_candidates(node, src) {
        let site = ev.site_at(candidate);
        let found = site
            .scope
            .bindings
            .iter()
            .find(|b| b.name == id.name && b.decl_byte == id.start_byte)
            .cloned();
        if let Some(binding) = found {
            return Some(local_out(ev, &site, &binding));
        }
    }
    Some(Out {
        symbol: Symbol::Local {
            name: id.name.clone(),
            path: ev.path.to_path_buf(),
            line: id.line,
            column: id.column,
            end_column: id.end_column,
            kind: kind.to_owned(),
        },
        ty: Ty::Unknown,
        signature: id.name.clone(),
        doc: String::new(),
    })
}

fn label(ev: &mut Eval<'_>, id: &IdentAt) -> Option<Out> {
    let site = ev.site_at(id.start_byte);
    let b = site
        .scope
        .bindings
        .iter()
        .find(|b| b.name == id.name && b.kind == BindingKind::Label)?
        .clone();
    Some(local_out(ev, &site, &b))
}

fn local_out(ev: &mut Eval<'_>, ctx: &Ctx, b: &Binding) -> Out {
    let kind = match b.kind {
        BindingKind::Receiver => "receiver",
        BindingKind::Param => "param",
        BindingKind::Result => "result",
        BindingKind::Const => "const",
        BindingKind::RangeKey | BindingKind::RangeValue => "range var",
        BindingKind::TypeSwitch => "type switch var",
        BindingKind::Label => "label",
        BindingKind::TypeParam => {
            let constraint = match &b.origin {
                Origin::Declared(t) => t.clone(),
                _ => String::new(),
            };
            return type_param_out(&b.name, &constraint);
        }
        BindingKind::Var | BindingKind::ShortVar | BindingKind::FuncLit => "var",
    };
    let ty = if b.kind == BindingKind::Label {
        Ty::Unknown
    } else {
        ev.binding_ty(ctx, b)
    };
    let signature = if b.kind == BindingKind::Label {
        format!("label {}", b.name)
    } else if ty.is_unknown() {
        b.name.clone()
    } else {
        format!("{} {}", b.name, ty.display())
    };
    Out {
        symbol: Symbol::Local {
            name: b.name.clone(),
            path: ev.path.to_path_buf(),
            line: b.line,
            column: b.column,
            end_column: b.end_column,
            kind: kind.to_owned(),
        },
        ty,
        signature,
        doc: String::new(),
    }
}

fn type_param_out(name: &str, constraint: &str) -> Out {
    Out {
        symbol: Symbol::TypeParam {
            name: name.to_owned(),
        },
        ty: Ty::Unknown,
        signature: if constraint.is_empty() {
            name.to_owned()
        } else {
            format!("{name} {constraint}")
        },
        doc: String::new(),
    }
}

/// The constraint of a type parameter `name` declared by a type, function or
/// receiver around `node`.
fn type_param_in_ancestors(node: Node<'_>, src: &str, name: &str) -> Option<String> {
    let mut cur = node;
    while let Some(p) = cur.parent() {
        if let Some(list) = p.child_by_field_name("type_parameters") {
            let mut c = list.walk();
            for decl in list.named_children(&mut c) {
                if decl.kind() != "type_parameter_declaration" {
                    continue;
                }
                let mut c2 = decl.walk();
                let named = decl
                    .children_by_field_name("name", &mut c2)
                    .any(|n| text(src, n) == name);
                if named {
                    return Some(
                        decl.child_by_field_name("type")
                            .map(|t| text(src, t).to_owned())
                            .unwrap_or_default(),
                    );
                }
            }
        }
        if p.kind() == "method_declaration"
            && let Some(receiver) = p.child_by_field_name("receiver")
            && receiver_declares(receiver, src, name)
        {
            return Some("any".to_owned());
        }
        cur = p;
    }
    None
}

/// Whether a receiver such as `(b *Box[U])` names `name` as a type argument.
fn receiver_declares(receiver: Node<'_>, src: &str, name: &str) -> bool {
    let mut stack = vec![(receiver, false)];
    let mut steps = 0;
    while let Some((node, inside)) = stack.pop() {
        steps += 1;
        if steps > 256 {
            return false;
        }
        if inside && node.kind() == "type_identifier" && text(src, node) == name {
            return true;
        }
        let inside = inside || node.kind() == "type_arguments";
        let mut c = node.walk();
        for child in node.children(&mut c) {
            stack.push((child, inside));
        }
    }
    false
}

// ---- names ----------------------------------------------------------------

fn type_ty(pkg: &Package, name: &str) -> Ty {
    Ty::Named {
        pkg: pkg.import_path.clone(),
        name: name.to_owned(),
        args: Vec::new(),
    }
}

fn type_symbol(pkg: &Arc<Package>, name: &str) -> Option<Out> {
    let (file, decl) = pkg.type_decl(name)?;
    Some(Out {
        symbol: Symbol::Type {
            pkg: pkg.import_path.clone(),
            name: name.to_owned(),
            path: file.path.clone(),
            pos: decl.pos,
        },
        ty: type_ty(pkg, name),
        signature: sig::type_sig(decl),
        doc: decl.doc.clone(),
    })
}

fn func_symbol(ev: &mut Eval<'_>, pkg: &Arc<Package>, name: &str) -> Option<Out> {
    let (file, decl) = pkg.func(name)?;
    let (path, decl) = (file.path.clone(), decl.clone());
    Some(Out {
        symbol: Symbol::Func {
            pkg: pkg.import_path.clone(),
            name: name.to_owned(),
            path,
            pos: decl.pos,
        },
        ty: ev.func_ty(pkg, name),
        signature: sig::func_sig(&decl, &Subst::new()),
        doc: decl.doc.clone(),
    })
}

fn value_symbol(ev: &mut Eval<'_>, pkg: &Arc<Package>, name: &str) -> Option<Out> {
    let (file, decl) = pkg.value(name)?;
    let (path, decl) = (file.path.clone(), decl.clone());
    let ty = ev.value_ty(pkg, name);
    Some(Out {
        symbol: Symbol::Value {
            pkg: pkg.import_path.clone(),
            name: name.to_owned(),
            path,
            pos: decl.pos,
            is_const: decl.is_const,
        },
        signature: sig::value_sig(&decl, &ty),
        ty,
        doc: decl.doc.clone(),
    })
}

fn builtin_out(name: &str) -> Out {
    let (signature, ty) = if TypeExpr::is_builtin(name) {
        (format!("type {name}"), uni(name))
    } else {
        match name {
            "true" | "false" => (format!("const {name} bool"), uni("bool")),
            "iota" => ("const iota int".to_owned(), uni("int")),
            "nil" => ("var nil".to_owned(), Ty::Unknown),
            _ => (
                sig::builtin_label(name)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("func {name}")),
                Ty::Unknown,
            ),
        }
    };
    Out {
        symbol: Symbol::Builtin {
            name: name.to_owned(),
        },
        ty,
        signature,
        doc: String::new(),
    }
}

/// A bare name: local, type parameter, package level declaration, import or
/// predeclared identifier. In a type position locals other than type
/// parameters do not count.
fn bare_name(
    ev: &mut Eval<'_>,
    src: &str,
    node: Node<'_>,
    id: &IdentAt,
    as_type: bool,
) -> Option<Out> {
    let name = id.name.as_str();
    let ctx = ev.site_at(id.start_byte);
    let local = if as_type {
        ctx.scope
            .bindings
            .iter()
            .rev()
            .find(|b| b.name == name && b.kind == BindingKind::TypeParam)
            .cloned()
    } else {
        ctx.scope.get(name).cloned()
    };
    if let Some(b) = local {
        return Some(local_out(ev, &ctx, &b));
    }
    if let Some(constraint) = type_param_in_ancestors(node, src, name) {
        return Some(type_param_out(name, &constraint));
    }
    let file = ctx.file.clone();
    let pkg = ctx.pkg.clone();
    match ev.engine.lookup_in_file_scope(&file, &pkg, name)? {
        FileSymbol::Type { pkg, name } => type_symbol(&pkg, &name),
        FileSymbol::Func { pkg, name } => func_symbol(ev, &pkg, &name),
        FileSymbol::Value { pkg, name } => value_symbol(ev, &pkg, &name),
        FileSymbol::Import { path } => Some(package_out(ev, name, &path)),
        FileSymbol::Builtin => Some(builtin_out(name)),
    }
}

// ---- selectors ----------------------------------------------------------

fn selector(ev: &mut Eval<'_>, src: &str, sel: Node<'_>, id: &IdentAt) -> Option<Out> {
    let operand = sel.child_by_field_name("operand")?;
    let ctx = ev.site_at(id.start_byte);
    let recv = ev.eval_node(&ctx, src, operand);
    match &recv {
        Ty::Package(path) => package_member(ev, path, &id.name),
        Ty::Unknown => {
            let ty = ev.as_type(&ctx, src, operand)?;
            member_of(ev, &ty, &id.name)
        }
        _ => member_of(ev, &recv, &id.name),
    }
}

fn package_member(ev: &mut Eval<'_>, path: &str, name: &str) -> Option<Out> {
    if !exported(name) {
        return None;
    }
    let pkg = ev.package_for(path)?;
    type_symbol(&pkg, name)
        .or_else(|| func_symbol(ev, &pkg, name))
        .or_else(|| value_symbol(ev, &pkg, name))
}

fn find_member(ev: &mut Eval<'_>, recv: &Ty, name: &str) -> Option<Member> {
    ev.members(recv).into_iter().find(|m| m.name == name)
}

fn member_of(ev: &mut Eval<'_>, recv: &Ty, name: &str) -> Option<Out> {
    let m = find_member(ev, recv, name)?;
    Some(member_out(ev, recv, &m))
}

/// The declaration of the method `m`, when it is a concrete method.
fn method_decl(ev: &mut Eval<'_>, m: &Member) -> Option<crate::go::parse::FuncDecl> {
    let pkg = ev.pkg_of_path(&m.path)?;
    let owner = m.owner.rsplit('.').next()?;
    pkg.methods_of(owner)
        .into_iter()
        .find(|(_, f)| f.name == m.name)
        .map(|(_, f)| f.clone())
}

fn member_out(ev: &mut Eval<'_>, recv: &Ty, m: &Member) -> Out {
    let ty = ev.member_ty(recv, &m.name).unwrap_or(Ty::Unknown);
    let path: PathBuf = m.path.clone();
    match m.kind {
        MemberKind::Field => Out {
            symbol: Symbol::Field {
                owner: m.owner.clone(),
                name: m.name.clone(),
                path,
                pos: m.pos,
            },
            ty,
            signature: format!("field {} {}", m.name, m.ty),
            doc: m.doc.clone(),
        },
        MemberKind::Method => {
            let (signature, doc) = match method_decl(ev, m) {
                Some(d) => (
                    sig::func_sig(&d, &Subst::new()),
                    if d.doc.is_empty() {
                        m.doc.clone()
                    } else {
                        d.doc.clone()
                    },
                ),
                None => (
                    format!("func ({}) {}{}", m.owner, m.name, m.ty),
                    m.doc.clone(),
                ),
            };
            Out {
                symbol: Symbol::Method {
                    owner: m.owner.clone(),
                    name: m.name.clone(),
                    path,
                    pos: m.pos,
                },
                ty,
                signature,
                doc,
            }
        }
    }
}

// ---- composite literal keys ---------------------------------------------

/// `Server{Addr: 1}`: the field a key names, when the literal is a struct.
fn keyed_field(ev: &mut Eval<'_>, src: &str, node: Node<'_>, id: &IdentAt) -> Option<Out> {
    let element = node.parent().filter(|p| p.kind() == "literal_element")?;
    let keyed = element.parent().filter(|p| p.kind() == "keyed_element")?;
    if !is_field(keyed, "key", element) {
        return None;
    }
    let literal = keyed.parent().filter(|p| p.kind() == "literal_value")?;
    let ctx = ev.site_at(id.start_byte);
    let ty = literal_ty(ev, &ctx, src, literal, 0)?;
    let base = ty.deref().clone();
    if !matches!(ev.under(&base), Ty::Struct) {
        return None;
    }
    let m = find_member(ev, &base, &id.name).filter(|m| m.kind == MemberKind::Field)?;
    Some(member_out(ev, &base, &m))
}

/// The type of the value `{...}` of a composite literal, also when its type
/// is elided inside an outer literal.
fn literal_ty(
    ev: &mut Eval<'_>,
    ctx: &Ctx,
    src: &str,
    literal: Node<'_>,
    depth: usize,
) -> Option<Ty> {
    if depth > MAX_LITERAL_DEPTH {
        return None;
    }
    let parent = literal.parent()?;
    match parent.kind() {
        "composite_literal" => {
            let t = parent.child_by_field_name("type")?;
            Some(ev.ty(ctx, text(src, t)))
        }
        "literal_element" => {
            let holder = parent.parent()?;
            match holder.kind() {
                "literal_value" => {
                    let outer = literal_ty(ev, ctx, src, holder, depth + 1)?;
                    element_ty(ev, &outer, false)
                }
                "keyed_element" => {
                    let outer_literal = holder.parent()?;
                    let outer = literal_ty(ev, ctx, src, outer_literal, depth + 1)?;
                    element_ty(ev, &outer, true)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn element_ty(ev: &mut Eval<'_>, outer: &Ty, keyed: bool) -> Option<Ty> {
    let base = outer.deref().clone();
    match ev.under(&base) {
        Ty::Slice(elem) | Ty::Array(elem) => Some(*elem),
        Ty::Map(_, value) if keyed => Some(*value),
        _ => None,
    }
}
