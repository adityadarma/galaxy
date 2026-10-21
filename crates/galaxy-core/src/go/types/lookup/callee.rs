//! `callee_at`: the function a call under the cursor invokes.

use tree_sitter::{Node, Tree};

use super::Callee;
use super::sig::{self, Subst};
use crate::go::engine::{FileSymbol, Member, MemberKind};
use crate::go::parse::FuncDecl;
use crate::go::scope::Origin;
use crate::go::texpr::TypeExpr;
use crate::go::types::builtins::call_args;
use crate::go::types::site::{Ctx, Env, Eval, kids, text};
use crate::go::types::{Ty, short_pkg};

/// Words that can stand before `(` without calling anything.
const KEYWORDS: &[&str] = &[
    "func",
    "if",
    "for",
    "switch",
    "return",
    "go",
    "defer",
    "select",
    "case",
    "range",
    "else",
    "var",
    "const",
    "type",
    "import",
    "package",
    "interface",
    "struct",
    "map",
    "chan",
];

/// Line starts that end every open bracket before them.
const RESETS: &[&str] = &["func ", "type ", "var ", "const ", "import ", "package "];

/// Longest run walked back over a callee expression.
const MAX_BACK: usize = 4096;

struct Info {
    name: String,
    label: String,
    params: Vec<String>,
    results: Vec<String>,
    doc: String,
    variadic: bool,
}

pub(super) fn callee(ev: &mut Eval<'_>, src: &str, byte: usize) -> Option<Callee> {
    let (open, callee_text) = find_call(ev.tree, src, byte)?;
    let segments = split_args(src.get(open + 1..byte)?);
    let active = segments.len().saturating_sub(1);
    let before: Vec<&str> = segments[..active]
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let expr = format!("{callee_text}({})", before.join(", "));
    let ctx = ev.site_at(byte);
    let info = ev
        .with_expr(&expr, |ev, psrc, node| describe(ev, &ctx, psrc, node))
        .flatten()?;
    let mut active = active as u32;
    if info.variadic && !info.params.is_empty() {
        active = active.min(info.params.len() as u32 - 1);
    }
    Some(Callee {
        name: info.name,
        label: info.label,
        params: info.params,
        results: info.results,
        doc: info.doc,
        active,
    })
}

// ---- finding the call --------------------------------------------------

/// Open paren and callee text of the innermost call around `byte`, from the
/// syntax tree and from the text; the nearer one wins.
fn find_call(tree: &Tree, src: &str, byte: usize) -> Option<(usize, String)> {
    let from_tree = tree_call(tree, src, byte);
    let from_text = text_call(src, byte);
    match (from_tree, from_text) {
        (Some(a), Some(b)) => Some(if b.0 > a.0 { b } else { a }),
        (a, b) => a.or(b),
    }
}

fn tree_call(tree: &Tree, src: &str, byte: usize) -> Option<(usize, String)> {
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    loop {
        if node.kind() == "call_expression"
            && let (Some(args), Some(function)) = (
                node.child_by_field_name("arguments"),
                node.child_by_field_name("function"),
            )
            && args.kind() == "argument_list"
        {
            let open = args.start_byte();
            let close = args
                .child(args.child_count().saturating_sub(1))
                .filter(|c| c.kind() == ")" && !c.is_missing())
                .map(|c| c.start_byte());
            if open < byte
                && function.end_byte() <= open
                && close.is_none_or(|c| byte <= c)
                && !is_decl_head(src, function.start_byte())
            {
                return Some((open, text(src, function).to_owned()));
            }
        }
        node = node.parent()?;
    }
}

fn text_call(src: &str, byte: usize) -> Option<(usize, String)> {
    let frames = scan_frames(src, byte);
    for (ch, pos) in frames.into_iter().rev() {
        if ch != b'(' {
            continue;
        }
        if let Some((start, expr)) = callee_before(src, pos)
            && valid_callee(&expr)
            && !is_decl_head(src, start)
        {
            return Some((pos, expr));
        }
    }
    None
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn valid_callee(expr: &str) -> bool {
    let Some(first) = expr.chars().next() else {
        return false;
    };
    (first.is_alphabetic() || first == '_' || first == '(') && !KEYWORDS.contains(&expr)
}

/// Start and text of the expression that ends right before `open`.
fn callee_before(src: &str, open: usize) -> Option<(usize, String)> {
    let b = src.as_bytes();
    let mut i = open;
    let mut steps = 0;
    while i > 0 {
        let c = b[i - 1];
        if is_ident_byte(c) || c == b'.' {
            i -= 1;
        } else if c == b')' || c == b']' {
            i = matching_open(b, i - 1)?;
        } else {
            break;
        }
        steps += 1;
        if steps > MAX_BACK {
            return None;
        }
    }
    if i >= open || !src.is_char_boundary(i) {
        return None;
    }
    Some((i, src[i..open].to_owned()))
}

fn matching_open(b: &[u8], close: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = close + 1;
    let mut steps = 0;
    while i > 0 {
        i -= 1;
        steps += 1;
        if steps > MAX_BACK {
            return None;
        }
        match b[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether the name starting at `start` is declared by `func` (a function
/// or method declaration) rather than called.
fn is_decl_head(src: &str, start: usize) -> bool {
    let head = src.get(..start).unwrap_or("").trim_end_matches([' ', '\t']);
    if let Some(before) = head.strip_suffix("func") {
        return !before.as_bytes().last().is_some_and(|b| is_ident_byte(*b));
    }
    if head.ends_with(')') {
        let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
        return src[line_start..start].trim_start().starts_with("func (");
    }
    false
}

// ---- text scanning -------------------------------------------------------

fn skip_quoted(b: &[u8], i: usize, quote: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j] == quote {
            return j + 1;
        }
        if quote == b'"' && b[j] == b'\n' {
            return j;
        }
        j += 1;
    }
    b.len()
}

fn skip_raw(b: &[u8], i: usize) -> usize {
    b[i + 1..]
        .iter()
        .position(|c| *c == b'`')
        .map_or(b.len(), |p| i + 1 + p + 1)
}

fn skip_line(b: &[u8], i: usize) -> usize {
    b[i..]
        .iter()
        .position(|c| *c == b'\n')
        .map_or(b.len(), |p| i + p)
}

fn skip_block(b: &[u8], i: usize) -> usize {
    b[i + 2..]
        .windows(2)
        .position(|w| w == b"*/")
        .map_or(b.len(), |p| i + 2 + p + 2)
}

/// Where a string, comment or rune literal starting at `i` ends; `None`
/// when `i` starts none.
fn skip_literal(b: &[u8], i: usize) -> Option<usize> {
    match b[i] {
        b'"' => Some(skip_quoted(b, i, b'"')),
        b'\'' => Some(skip_quoted(b, i, b'\'')),
        b'`' => Some(skip_raw(b, i)),
        b'/' if b.get(i + 1) == Some(&b'/') => Some(skip_line(b, i)),
        b'/' if b.get(i + 1) == Some(&b'*') => Some(skip_block(b, i)),
        _ => None,
    }
}

/// Brackets still open at `end`, outermost first, as (bracket, position).
fn scan_frames(src: &str, end: usize) -> Vec<(u8, usize)> {
    let b = src.as_bytes();
    let end = end.min(b.len());
    let mut frames: Vec<(u8, usize)> = Vec::new();
    let mut i = 0;
    while i < end {
        if let Some(next) = skip_literal(b, i) {
            i = next.max(i + 1);
            continue;
        }
        match b[i] {
            b'(' | b'[' | b'{' => frames.push((b[i], i)),
            b')' | b']' | b'}' => {
                frames.pop();
            }
            b'\n' => {
                let rest = &src[i + 1..];
                if RESETS.iter().any(|k| rest.starts_with(k)) {
                    frames.clear();
                }
            }
            _ => {}
        }
        i += 1;
    }
    frames
}

/// The arguments of `text` split at commas outside brackets, strings and
/// comments. The last entry is the one being typed.
fn split_args(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        if let Some(next) = skip_literal(b, i) {
            i = next.max(i + 1);
            continue;
        }
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth <= 0 => {
                out.push(text[start..i].to_owned());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(text.get(start..).unwrap_or("").to_owned());
    out
}

// ---- describing the callee -----------------------------------------------

fn describe(ev: &mut Eval<'_>, ctx: &Ctx, src: &str, call: Node<'_>) -> Option<Info> {
    if call.kind() == "type_conversion_expression" {
        return instantiated_info(ev, ctx, src, call);
    }
    if call.kind() != "call_expression" {
        return None;
    }
    let function = call.child_by_field_name("function")?;
    if let Some(done) = builtin_info(ev, ctx, src, function) {
        return done;
    }
    let (target, explicit) = split_instantiation(src, call, function);
    if target.kind() != "qualified_type" && ev.as_type(ctx, src, target).is_some() {
        return None;
    }
    match target.kind() {
        "qualified_type" => qualified_info(ev, ctx, src, call, target, &explicit),
        "identifier" | "type_identifier" => ident_info(ev, ctx, src, call, target, &explicit),
        "selector_expression" => selector_info(ev, ctx, src, call, target, &explicit),
        _ => ty_info(ev, ctx, src, target),
    }
}

/// `Some(None)` for a predeclared name that has no help, `Some(Some(_))`
/// for one that has, `None` when `function` is not a predeclared function.
fn builtin_info(
    ev: &mut Eval<'_>,
    ctx: &Ctx,
    src: &str,
    function: Node<'_>,
) -> Option<Option<Info>> {
    if function.kind() != "identifier" {
        return None;
    }
    let name = text(src, function);
    if ctx.scope.get(name).is_some() {
        return None;
    }
    if !matches!(
        ev.engine.lookup_in_file_scope(&ctx.file, &ctx.pkg, name),
        Some(FileSymbol::Builtin)
    ) {
        return None;
    }
    let Some(label) = sig::builtin_label(name) else {
        return Some(None);
    };
    let rest = &label[5 + name.len()..];
    let (params, results) = sig::split_signature(rest);
    Some(Some(Info {
        name: name.to_owned(),
        label: label.to_owned(),
        variadic: params.last().is_some_and(|p| p.contains("...")),
        params,
        results,
        doc: String::new(),
    }))
}

/// The callee node and the type argument texts of `F[int, string]`.
fn split_instantiation<'t>(
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
            if let (Some(operand), Some(index)) = (
                function.child_by_field_name("operand"),
                function.child_by_field_name("index"),
            ) {
                texts.push(text(src, index).to_owned());
                return (operand, texts);
            }
            (function, texts)
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

fn ident_info(
    ev: &mut Eval<'_>,
    ctx: &Ctx,
    src: &str,
    call: Node<'_>,
    target: Node<'_>,
    explicit: &[String],
) -> Option<Info> {
    let name = text(src, target);
    if ctx.scope.get(name).is_none()
        && let Some(FileSymbol::Func { pkg, name }) =
            ev.engine.lookup_in_file_scope(&ctx.file, &ctx.pkg, name)
    {
        let decl = pkg.func(&name)?.1.clone();
        return Some(decl_info(ev, ctx, src, call, &decl, explicit, Subst::new()));
    }
    ty_info(ev, ctx, src, target)
}

/// `F[int, string](x)`, which the grammar reads as a conversion to the
/// generic type `F[int, string]`.
fn instantiated_info(ev: &mut Eval<'_>, ctx: &Ctx, src: &str, node: Node<'_>) -> Option<Info> {
    let generic = node.child_by_field_name("type")?;
    if generic.kind() != "generic_type" {
        return None;
    }
    let inner = generic.child_by_field_name("type")?;
    let explicit: Vec<String> = generic
        .child_by_field_name("type_arguments")
        .map(|list| {
            kids(list)
                .into_iter()
                .map(|n| text(src, n).to_owned())
                .collect()
        })
        .unwrap_or_default();
    match inner.kind() {
        "qualified_type" => qualified_info(ev, ctx, src, node, inner, &explicit),
        _ => {
            let name = text(src, inner);
            if ctx.scope.get(name).is_some() {
                return None;
            }
            let Some(FileSymbol::Func { pkg, name }) =
                ev.engine.lookup_in_file_scope(&ctx.file, &ctx.pkg, name)
            else {
                return None;
            };
            let decl = pkg.func(&name)?.1.clone();
            Some(decl_info(
                ev,
                ctx,
                src,
                node,
                &decl,
                &explicit,
                Subst::new(),
            ))
        }
    }
}

/// `pkg.F[int]`, which the grammar reads as a qualified type.
fn qualified_info(
    ev: &mut Eval<'_>,
    ctx: &Ctx,
    src: &str,
    call: Node<'_>,
    target: Node<'_>,
    explicit: &[String],
) -> Option<Info> {
    let local = text(src, target.child_by_field_name("package")?);
    let name = text(src, target.child_by_field_name("name")?);
    if ctx.scope.get(local).is_some() {
        return None;
    }
    let path = ctx.imports.get(local)?.clone();
    let pkg = ev.package_for(&path)?;
    let decl = pkg.func(name)?.1.clone();
    Some(decl_info(ev, ctx, src, call, &decl, explicit, Subst::new()))
}

fn selector_info(
    ev: &mut Eval<'_>,
    ctx: &Ctx,
    src: &str,
    call: Node<'_>,
    target: Node<'_>,
    explicit: &[String],
) -> Option<Info> {
    let operand = target.child_by_field_name("operand")?;
    let field = target.child_by_field_name("field")?;
    let name = text(src, field);
    if ev.as_type(ctx, src, operand).is_some() {
        return None;
    }
    let recv = ev.eval_node(ctx, src, operand);
    match &recv {
        Ty::Unknown => None,
        Ty::Package(path) => {
            let pkg = ev.package_for(path)?;
            if let Some((_, decl)) = pkg.func(name) {
                let decl = decl.clone();
                return Some(decl_info(ev, ctx, src, call, &decl, explicit, Subst::new()));
            }
            ty_info(ev, ctx, src, target)
        }
        _ => {
            let member = ev.members(&recv).into_iter().find(|m| m.name == name)?;
            match member.kind {
                MemberKind::Method => method_info(ev, ctx, src, call, &recv, &member),
                MemberKind::Field => field_info(ev, ctx, src, target, &member),
            }
        }
    }
}

fn method_info(
    ev: &mut Eval<'_>,
    ctx: &Ctx,
    src: &str,
    call: Node<'_>,
    recv: &Ty,
    member: &Member,
) -> Option<Info> {
    let owner = member.owner.rsplit('.').next().unwrap_or(&member.owner);
    let decl = ev.pkg_of_path(&member.path).and_then(|pkg| {
        pkg.methods_of(owner)
            .into_iter()
            .find(|(_, f)| f.name == member.name)
            .map(|(_, f)| f.clone())
    });
    let Some(decl) = decl else {
        // An interface method: only its signature text is known.
        let (params, results) = sig::split_signature(&member.ty);
        return Some(Info {
            name: member.name.clone(),
            label: format!("func ({}) {}{}", member.owner, member.name, member.ty),
            variadic: params.last().is_some_and(|p| p.contains("...")),
            params,
            results,
            doc: member.doc.clone(),
        });
    };
    let mut map = Subst::new();
    if member.embedded_via.is_empty()
        && let Ty::Named { args, .. } = recv.deref()
        && let Some(r) = &decl.receiver
        && let TypeExpr::Name { args: names, .. } = TypeExpr::parse(&r.ty).deref()
    {
        let cur = short_pkg(&ctx.pkg.import_path);
        for (n, a) in names.iter().zip(args) {
            if let TypeExpr::Name {
                pkg: None, name, ..
            } = n
                && !a.is_unknown()
            {
                map.insert(name.clone(), strip_local(&a.display(), &cur));
            }
        }
    }
    Some(decl_info(ev, ctx, src, call, &decl, &[], map))
}

/// A field of function type: the declared type text keeps parameter names.
fn field_info(
    ev: &mut Eval<'_>,
    ctx: &Ctx,
    src: &str,
    target: Node<'_>,
    member: &Member,
) -> Option<Info> {
    if let Some(rest) = member.ty.strip_prefix("func")
        && rest.starts_with('(')
    {
        return Some(text_func_info(&member.name, rest, &member.doc));
    }
    let mut info = ty_info(ev, ctx, src, target)?;
    info.doc = member.doc.clone();
    Some(info)
}

fn text_func_info(name: &str, rest: &str, doc: &str) -> Info {
    let (params, results) = sig::split_signature(rest);
    Info {
        name: name.to_owned(),
        label: format!("func {name}{rest}"),
        variadic: params.last().is_some_and(|p| p.contains("...")),
        params,
        results,
        doc: doc.to_owned(),
    }
}

/// A callee known only by its type: a local, a package-level variable, a
/// call result.
fn ty_info(ev: &mut Eval<'_>, ctx: &Ctx, src: &str, target: Node<'_>) -> Option<Info> {
    let expr = text(src, target);
    let name = expr
        .rsplit('.')
        .next()
        .unwrap_or(expr)
        .trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'))
        .to_owned();
    if target.kind() == "identifier"
        && let Some(binding) = ctx.scope.get(&name)
        && let Origin::Declared(declared) = &binding.origin
        && let Some(rest) = declared.strip_prefix("func")
        && rest.starts_with('(')
    {
        return Some(text_func_info(&name, rest, ""));
    }
    let ty = ev.eval_node(ctx, src, target);
    let ty = match ty {
        Ty::Func { .. } => ty,
        other => ev.under(&other),
    };
    let Ty::Func {
        params,
        results,
        variadic,
    } = ty
    else {
        return None;
    };
    let cur = short_pkg(&ctx.pkg.import_path);
    let show = |t: &Ty| strip_local(&t.display(), &cur);
    let mut shown: Vec<String> = params.iter().map(show).collect();
    if variadic && let Some(last) = shown.last_mut() {
        last.insert_str(0, "...");
    }
    let results: Vec<String> = results.iter().map(show).collect();
    let suffix = sig::results_suffix(&results, false);
    Some(Info {
        label: format!("func {name}({}){suffix}", shown.join(", ")),
        name,
        params: shown,
        results,
        doc: String::new(),
        variadic,
    })
}

/// The declaration `decl`, with the type parameters that are given
/// explicitly or can be inferred from the arguments typed so far replaced.
fn decl_info(
    ev: &mut Eval<'_>,
    ctx: &Ctx,
    src: &str,
    call: Node<'_>,
    decl: &FuncDecl,
    explicit: &[String],
    mut map: Subst,
) -> Info {
    if !decl.type_params.is_empty() {
        let names: Vec<String> = decl.type_params.iter().map(|t| t.name.clone()).collect();
        let mut env = Env::new();
        for n in &names {
            env.insert(n.clone(), Ty::Unknown);
        }
        for (n, written) in names.iter().zip(explicit) {
            let bound = ev.ty(ctx, written);
            env.insert(n.clone(), bound);
            map.insert(n.clone(), written.trim().to_owned());
        }
        let last = decl.params.len().saturating_sub(1);
        for (i, arg) in call_args(call).iter().enumerate() {
            let Some(param) = decl.params.get(i.min(last)) else {
                break;
            };
            let variadic = param.variadic || param.ty.starts_with("...");
            if i > last && !variadic || arg.kind() == "variadic_argument" {
                break;
            }
            let pattern = TypeExpr::parse(param.ty.strip_prefix("...").unwrap_or(&param.ty));
            let value = ev.eval_node(ctx, src, *arg);
            ev.unify(&pattern, &value, &mut env, &names);
        }
        let cur = short_pkg(&ctx.pkg.import_path);
        for n in &names {
            if map.contains_key(n) {
                continue;
            }
            if let Some(bound) = env.get(n)
                && !bound.is_unknown()
            {
                map.insert(n.clone(), strip_local(&bound.display(), &cur));
            }
        }
    }
    let params = sig::params_of(decl, &map);
    Info {
        name: decl.name.clone(),
        label: sig::func_sig(decl, &map),
        results: sig::results_of(decl, &map),
        doc: decl.doc.clone(),
        variadic: decl
            .params
            .last()
            .is_some_and(|p| p.variadic || p.ty.starts_with("...")),
        params,
    }
}

/// `shown` without the `pkg.` qualifier of the package being edited.
fn strip_local(shown: &str, pkg: &str) -> String {
    let needle = format!("{pkg}.");
    let mut out = String::with_capacity(shown.len());
    let mut rest = shown;
    let mut prev: Option<char> = None;
    while let Some(at) = rest.find(&needle) {
        let before = rest[..at].chars().next_back().or(prev);
        let boundary = before.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'));
        out.push_str(&rest[..at]);
        if !boundary {
            out.push_str(&needle);
        }
        prev = needle.chars().next_back();
        rest = &rest[at + needle.len()..];
    }
    out.push_str(rest);
    out
}
