//! The walk from the root to the cursor, collecting bindings on the way.

use std::collections::HashMap;
use std::rc::Rc;

use tree_sitter::Node;

use super::{Binding, BindingKind, reach};

/// Deepest structural nesting followed; deeper code is left unvisited.
const MAX_DEPTH: usize = 600;

/// What a lookup learns about a tree and can reuse for the next one at
/// another byte: the children of each block, and which function bodies can
/// hold a label. Without it every lookup walks every statement before the
/// cursor, so a pass over all identifiers of a long function is quadratic.
#[derive(Default)]
pub struct ScopeIndex<'t> {
    kids: HashMap<usize, Rc<Kids<'t>>>,
    labels: HashMap<usize, bool>,
}

/// The named children of one block, in order.
struct Kids<'t> {
    all: Vec<Node<'t>>,
    /// Positions in `all` of the children that bring names in or hold
    /// nested statements whether or not the cursor is inside them.
    open: Vec<usize>,
}

/// Kinds [`Walker::stmt`] acts on without needing the cursor inside.
fn always_visited(kind: &str) -> bool {
    matches!(
        kind,
        "statement_list"
            | "ERROR"
            | "var_declaration"
            | "const_declaration"
            | "labeled_statement"
            | "short_var_declaration"
    )
}

/// The block a declaration lives in: an id telling blocks apart (for `:=`
/// redeclaration) and the byte where the block ends.
#[derive(Clone, Copy)]
pub(super) struct Ctx {
    pub id: usize,
    pub end: usize,
}

pub(super) struct Walker<'a, 't> {
    pub src: &'a str,
    pub byte: usize,
    index: Option<&'a mut ScopeIndex<'t>>,
    /// Visible bindings with the id of the block each lives in.
    pub entries: Vec<(Binding, usize)>,
    next_scope: usize,
}

impl<'a, 't> Walker<'a, 't> {
    pub(super) fn new(src: &'a str, byte: usize, index: Option<&'a mut ScopeIndex<'t>>) -> Self {
        Self {
            src,
            byte,
            index,
            entries: Vec::new(),
            next_scope: 0,
        }
    }

    pub(super) fn finish(self) -> Vec<Binding> {
        self.entries.into_iter().map(|(b, _)| b).collect()
    }

    pub(super) fn text(&self, node: Node<'t>) -> &'a str {
        self.src.get(node.byte_range()).unwrap_or("")
    }

    pub(super) fn fresh(&mut self, end: usize) -> Ctx {
        let id = self.next_scope;
        self.next_scope += 1;
        Ctx { id, end }
    }

    /// Whether a statement ending at `end` is finished for the cursor: a
    /// line break or `;` lies between them. On the same line the user may
    /// still be typing the statement.
    pub(super) fn done(&self, end: usize) -> bool {
        end < self.byte
            && self
                .src
                .get(end..self.byte)
                .is_some_and(|gap| gap.contains(['\n', ';']))
    }

    /// Whether the cursor is in `node` or right at its edges.
    pub(super) fn contains(&self, node: Node<'t>) -> bool {
        node.start_byte() <= self.byte && self.byte <= reach(self.src, node)
    }

    /// Whether the cursor is between the braces of `node`. A `}` that is
    /// missing (code still being typed) leaves the end open.
    fn in_braces(&self, node: Node<'t>) -> bool {
        if self.byte <= node.start_byte() || self.byte > reach(self.src, node) {
            return false;
        }
        let closed = node
            .child(node.child_count().saturating_sub(1))
            .is_some_and(|last| last.kind() == "}" && !last.is_missing());
        !closed || self.byte < node.end_byte()
    }

    /// The child holding the cursor, preferring one that has it strictly
    /// inside over one that only touches it.
    fn child_at(&self, node: Node<'t>) -> Option<Node<'t>> {
        let mut cursor = node.walk();
        let mut touching = None;
        for child in node.children(&mut cursor) {
            if child.start_byte() > self.byte {
                break;
            }
            if self.byte <= reach(self.src, child) {
                if child.start_byte() < self.byte {
                    return Some(child);
                }
                touching.get_or_insert(child);
            }
        }
        touching
    }

    /// Follows `node` down to the cursor, entering every scope on the way.
    /// Expressions are crossed without recursion; only functions, blocks and
    /// statements with their own scope recurse.
    pub(super) fn descend(&mut self, node: Node<'t>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let mut current = node;
        loop {
            match current.kind() {
                "source_file" => return self.source(current, depth),
                // A file Tree-sitter could not make sense of at all has an
                // `ERROR` root holding the same top-level pieces.
                "ERROR" if depth == 0 && current.parent().is_none() => {
                    return self.source(current, depth);
                }
                "function_declaration" | "method_declaration" | "func_literal" => {
                    return self.func(current, depth);
                }
                "block" => return self.block(current, depth),
                "if_statement" => return self.if_stmt(current, depth),
                "for_statement" => return self.for_stmt(current, depth),
                "expression_switch_statement" | "type_switch_statement" | "select_statement" => {
                    return self.switch(current, depth);
                }
                _ => {}
            }
            match self.child_at(current) {
                Some(child) => current = child,
                None => return,
            }
        }
    }

    /// The file level: a function whose body Tree-sitter could not attach
    /// (an unclosed `{`) owns the error nodes that follow it.
    fn source(&mut self, root: Node<'t>, depth: usize) {
        if let Some((func, errors)) = orphan_at(root, self.byte) {
            let ctx = self.fresh(self.src.len());
            self.header(func, ctx, true);
            for error in errors {
                self.stmts(error, 0, ctx, depth + 1);
            }
        } else if let Some(loose) = loose_header_at(root, self.byte) {
            let ctx = self.fresh(self.src.len());
            if let Some(list) = loose.type_params {
                self.type_params(list, ctx);
            }
            if let Some(list) = loose.receiver {
                self.params(list, BindingKind::Receiver, ctx);
            }
            self.params(loose.params, BindingKind::Param, ctx);
            if let Some(list) = loose.result {
                self.params(list, BindingKind::Result, ctx);
            }
        } else if let Some(child) = self.child_at(root) {
            self.descend(child, depth + 1);
        }
    }

    /// Type parameters, receiver, parameters and results of a function.
    /// Without `whole` only the type parameters are declared, which the
    /// signature itself can use.
    fn header(&mut self, node: Node<'t>, ctx: Ctx, whole: bool) {
        if let Some(list) = node.child_by_field_name("type_parameters")
            && self.byte >= list.start_byte()
        {
            self.type_params(list, ctx);
        }
        if !whole {
            return;
        }
        if let Some(list) = node.child_by_field_name("receiver") {
            self.params(list, BindingKind::Receiver, ctx);
        }
        if let Some(list) = node.child_by_field_name("parameters") {
            self.params(list, BindingKind::Param, ctx);
        }
        if let Some(list) = node
            .child_by_field_name("result")
            .filter(|n| n.kind() == "parameter_list")
        {
            self.params(list, BindingKind::Result, ctx);
        }
    }

    fn func(&mut self, node: Node<'t>, depth: usize) {
        if node.kind() == "func_literal" {
            // Labels belong to the function that declares them.
            self.entries.retain(|(b, _)| b.kind != BindingKind::Label);
        }
        let body = node.child_by_field_name("body");
        let end = body.map_or(node.end_byte(), |b| b.end_byte());
        let ctx = self.fresh(end);
        let body = body.filter(|b| self.in_braces(*b));
        if let Some(body) = body {
            self.labels(body, ctx);
        }
        self.header(node, ctx, body.is_some());
        if let Some(body) = body {
            self.stmts(body, 0, ctx, depth + 1);
        }
    }

    fn block(&mut self, node: Node<'t>, depth: usize) {
        if !self.in_braces(node) {
            return;
        }
        let ctx = self.fresh(node.end_byte());
        self.stmts(node, 0, ctx, depth + 1);
    }

    /// Declares what the statements of `parent` before the cursor bring into
    /// `ctx`, and enters the one holding the cursor. Statements starting
    /// before `from` are skipped.
    fn stmts(&mut self, parent: Node<'t>, from: usize, ctx: Ctx, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let Some(kids) = self.kids_of(parent) else {
            let mut cursor = parent.walk();
            for child in parent.named_children(&mut cursor) {
                if child.start_byte() > self.byte {
                    break;
                }
                if child.start_byte() < from || child.kind() == "comment" {
                    continue;
                }
                self.stmt(child, ctx, depth);
            }
            return;
        };
        // A statement that is neither always visited nor holding the cursor
        // does nothing. Only the last child starting before the cursor, and
        // those starting right at it, can hold it: no node reaches past the
        // start of its next sibling.
        let all = &kids.all;
        let upto = all.partition_point(|n| n.start_byte() <= self.byte);
        let near = all
            .partition_point(|n| n.start_byte() < self.byte)
            .saturating_sub(1);
        let split = kids.open.partition_point(|&i| i < near);
        for &i in &kids.open[..split] {
            let child = all[i];
            if child.start_byte() >= from {
                self.stmt(child, ctx, depth);
            }
        }
        for &child in &all[near.min(upto)..upto] {
            if child.start_byte() < from || child.kind() == "comment" {
                continue;
            }
            self.stmt(child, ctx, depth);
        }
    }

    /// The indexed children of `parent`, when there is an index.
    fn kids_of(&mut self, parent: Node<'t>) -> Option<Rc<Kids<'t>>> {
        let index = self.index.as_mut()?;
        let found = index.kids.entry(parent.id()).or_insert_with(|| {
            let mut cursor = parent.walk();
            let all: Vec<Node<'t>> = parent.named_children(&mut cursor).collect();
            let open = (0..all.len())
                .filter(|&i| always_visited(all[i].kind()))
                .collect();
            Rc::new(Kids { all, open })
        });
        Some(found.clone())
    }

    /// Memoized answer of `compute` for the function body `body`.
    pub(super) fn label_candidate(
        &mut self,
        body: Node<'t>,
        compute: impl FnOnce() -> bool,
    ) -> bool {
        match self.index.as_mut() {
            Some(index) => *index.labels.entry(body.id()).or_insert_with(compute),
            None => compute(),
        }
    }

    /// One statement: a declaration takes effect once the cursor is past
    /// it; any other statement is entered if it holds the cursor.
    pub(super) fn stmt(&mut self, stmt: Node<'t>, ctx: Ctx, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match stmt.kind() {
            "statement_list" | "ERROR" => self.stmts(stmt, 0, ctx, depth + 1),
            "var_declaration" | "const_declaration" => {
                self.group(stmt, ctx);
                if self.contains(stmt) {
                    self.descend(stmt, depth + 1);
                }
            }
            "labeled_statement" => {
                let mut cursor = stmt.walk();
                let inner = stmt
                    .named_children(&mut cursor)
                    .find(|n| n.kind() != "label_name" && n.kind() != "comment");
                if let Some(inner) = inner {
                    self.stmt(inner, ctx, depth + 1);
                }
            }
            "short_var_declaration" if self.done(stmt.end_byte()) => self.short(stmt, ctx),
            _ => {
                if self.contains(stmt) {
                    self.descend(stmt, depth + 1);
                }
            }
        }
    }

    /// The header scope of an `if`: its initializer is visible in the
    /// condition and in every branch.
    fn if_stmt(&mut self, node: Node<'t>, depth: usize) {
        let ctx = self.fresh(node.end_byte());
        let init = node.child_by_field_name("initializer").map(|n| n.id());
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.start_byte() > self.byte {
                break;
            }
            if Some(child.id()) == init {
                self.stmt(child, ctx, depth + 1);
            } else if self.contains(child) {
                self.descend(child, depth + 1);
            }
        }
    }

    fn for_stmt(&mut self, node: Node<'t>, depth: usize) {
        let ctx = self.fresh(node.end_byte());
        let in_body = node
            .child_by_field_name("body")
            .is_some_and(|b| self.in_braces(b));
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.start_byte() > self.byte {
                break;
            }
            match child.kind() {
                "for_clause" => {
                    let init = child.child_by_field_name("initializer").map(|n| n.id());
                    let mut inner = child.walk();
                    for part in child.children(&mut inner) {
                        if part.start_byte() > self.byte {
                            break;
                        }
                        if Some(part.id()) == init {
                            self.stmt(part, ctx, depth + 1);
                        } else if self.contains(part) {
                            self.descend(part, depth + 1);
                        }
                    }
                }
                // The names of a range clause are visible in the body only.
                "range_clause" if in_body => self.range(child, ctx),
                _ => {
                    if self.contains(child) {
                        self.descend(child, depth + 1);
                    }
                }
            }
        }
    }

    /// `switch` and `select`: header parts, then the clause holding the
    /// cursor, which is a block of its own.
    fn switch(&mut self, node: Node<'t>, depth: usize) {
        let ctx = self.fresh(node.end_byte());
        let init = node.child_by_field_name("initializer").map(|n| n.id());
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.start_byte() > self.byte {
                break;
            }
            if is_clause(child) {
                continue;
            }
            if Some(child.id()) == init {
                self.stmt(child, ctx, depth + 1);
            } else if self.contains(child) {
                self.descend(child, depth + 1);
            }
        }
        if let Some((clause, limit)) = self.clause_at(node) {
            self.clause(node, clause, limit, depth);
        }
    }

    /// The clause of `switch` that holds the cursor, with the byte where its
    /// region ends: the next clause, or the closing brace. Whitespace after
    /// the last statement of a clause still belongs to it.
    fn clause_at(&self, switch: Node<'t>) -> Option<(Node<'t>, usize)> {
        let mut cursor = switch.walk();
        let kids: Vec<Node<'t>> = switch.children(&mut cursor).collect();
        let close = kids
            .last()
            .filter(|n| n.kind() == "}" && !n.is_missing())
            .map_or(switch.end_byte(), |n| n.start_byte());
        let clauses: Vec<Node<'t>> = kids.into_iter().filter(|n| is_clause(*n)).collect();
        for (index, clause) in clauses.iter().enumerate() {
            let last = index + 1 == clauses.len();
            let limit = clauses.get(index + 1).map_or(close, |n| n.start_byte());
            let before = if last {
                self.byte <= limit
            } else {
                self.byte < limit
            };
            if clause.start_byte() <= self.byte && before {
                return Some((*clause, limit));
            }
        }
        None
    }

    fn clause(&mut self, switch: Node<'t>, clause: Node<'t>, limit: usize, depth: usize) {
        let ctx = self.fresh(limit);
        let mut cursor = clause.walk();
        let colon = clause
            .children(&mut cursor)
            .find(|n| n.kind() == ":")
            .map_or(clause.end_byte(), |n| n.end_byte());
        if self.byte < colon {
            let mut inner = clause.walk();
            for child in clause.children(&mut inner) {
                if child.start_byte() <= self.byte && self.contains(child) {
                    self.descend(child, depth + 1);
                }
            }
            return;
        }
        match clause.kind() {
            "communication_case" => {
                if let Some(comm) = clause.child_by_field_name("communication") {
                    self.receive(comm, ctx);
                }
            }
            "type_case" | "default_case" if switch.kind() == "type_switch_statement" => {
                self.type_binding(switch, clause, ctx);
            }
            _ => {}
        }
        self.stmts(clause, colon, ctx, depth + 1);
    }
}

fn is_clause(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "expression_case" | "type_case" | "default_case" | "communication_case"
    )
}

/// A function without a body and the error nodes after it, when the cursor
/// is in those errors: the shape of a function whose `{` is never closed.
pub(super) fn orphan_at<'t>(root: Node<'t>, byte: usize) -> Option<(Node<'t>, Vec<Node<'t>>)> {
    let mut cursor = root.walk();
    let kids: Vec<Node<'t>> = root
        .children(&mut cursor)
        .filter(|n| n.start_byte() <= byte)
        .collect();
    let mut errors = Vec::new();
    for child in kids.into_iter().rev() {
        match child.kind() {
            "ERROR" => errors.push(child),
            "comment" => {}
            "function_declaration" | "method_declaration"
                if child.child_by_field_name("body").is_none() =>
            {
                if errors.is_empty() || byte < child.end_byte() {
                    return None;
                }
                errors.reverse();
                return Some((child, errors));
            }
            _ => return None,
        }
    }
    None
}

/// The pieces of a function header that Tree-sitter left as loose tokens of
/// the file.
pub(super) struct Loose<'t> {
    receiver: Option<Node<'t>>,
    type_params: Option<Node<'t>>,
    params: Node<'t>,
    result: Option<Node<'t>>,
}

/// A function whose whole declaration fell apart into top-level tokens (an
/// unfinished call in its body does that) and whose `{` lies before the
/// cursor. Its parameters stay visible. Only a `func` at the start of a line
/// counts, so a closure in the body is not taken for the declaration.
pub(super) fn loose_header_at<'t>(root: Node<'t>, byte: usize) -> Option<Loose<'t>> {
    let mut cursor = root.walk();
    let kids: Vec<Node<'t>> = root
        .children(&mut cursor)
        .filter(|n| n.start_byte() <= byte)
        .collect();
    let at = kids.iter().rposition(|n| {
        matches!(
            n.kind(),
            "function_declaration" | "method_declaration" | "package_clause" | "import_declaration"
        ) || (n.kind() == "func" && n.start_position().column == 0)
    })?;
    if kids[at].kind() != "func" {
        return None;
    }
    let is_name = |n: &Node<'_>| matches!(n.kind(), "identifier" | "field_identifier");
    let mut i = at + 1;
    let mut receiver = None;
    if kids.get(i)?.kind() == "parameter_list" && kids.get(i + 1).is_some_and(is_name) {
        receiver = Some(kids[i]);
        i += 1;
    }
    if !is_name(kids.get(i)?) {
        return None;
    }
    i += 1;
    let mut type_params = None;
    if kids.get(i)?.kind() == "type_parameter_list" {
        type_params = Some(kids[i]);
        i += 1;
    }
    if kids.get(i)?.kind() != "parameter_list" {
        return None;
    }
    let params = kids[i];
    i += 1;
    let mut result = None;
    if kids.get(i)?.kind() != "{" {
        result = Some(kids[i]).filter(|n| n.kind() == "parameter_list");
        i += 1;
    }
    let open = kids.get(i).filter(|n| n.kind() == "{")?;
    (open.start_byte() < byte).then_some(Loose {
        receiver,
        type_params,
        params,
        result,
    })
}
