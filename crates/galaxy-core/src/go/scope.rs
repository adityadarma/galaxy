//! Local names visible at a cursor position in a Go function body.
//!
//! Nothing is type-checked. Each binding keeps the type text it was declared
//! with, or the source expression it was initialised from, so a type
//! evaluator can resolve it later. Struct fields and package-level
//! declarations are not bindings here. Lines are 0-based and columns are
//! UTF-16 code units, as in [`super::parse`].

use tree_sitter::{Node, Tree};

use super::parse::utf16_col;

mod decl;
mod func;
mod walk;

#[cfg(test)]
mod tests;

pub use func::{FuncContext, enclosing_func};
pub use walk::ScopeIndex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingKind {
    Receiver,
    Param,
    Result,
    Var,
    Const,
    ShortVar,
    RangeKey,
    RangeValue,
    TypeSwitch,
    TypeParam,
    Label,
    /// Reserved; the walker never produces it. A closure assigned to a name
    /// is a `ShortVar` or `Var` whose [`Origin::Expr`] text is the literal.
    FuncLit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Explicit type text: parameters, `var x T`, receiver, results, type
    /// parameter constraints, inherited `const` group types.
    Declared(String),
    /// Initializer source. `a, b := f()` gives `a` index 0 and `b` index 1,
    /// both with count 2. `v, ok := x.(T)`, `v, ok := m[k]` and
    /// `v, ok := <-ch` are the same shape.
    Expr {
        text: String,
        index: usize,
        count: usize,
    },
    /// `for k, v := range expr`; `key` is true for `k`.
    RangeOver {
        text: String,
        key: bool,
    },
    /// `switch v := x.(type)`. `case_types` lists the types of the clause the
    /// binding lives in: one for `case A:`, several for `case A, B:`, none
    /// for `default:`.
    TypeSwitch {
        subject: String,
        case_types: Vec<String>,
    },
    /// Reserved for the `ok` of a comma-ok form; the walker uses
    /// [`Origin::Expr`] with count 2 instead.
    AssertOk,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub kind: BindingKind,
    pub origin: Origin,
    pub line: u32,
    pub column: u32,
    pub end_column: u32,
    /// Byte offset of the name where it is declared.
    pub decl_byte: usize,
    /// End of the block the name lives in.
    pub scope_end_byte: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Innermost declarations last. A name shadowed by a nearer one of the
    /// same name is dropped, so each name appears once. Labels have their own
    /// namespace and may repeat a variable name.
    pub bindings: Vec<Binding>,
}

impl Scope {
    /// The nearest binding named `name`; a variable wins over a label.
    pub fn get(&self, name: &str) -> Option<&Binding> {
        let mut found = None;
        for binding in self.bindings.iter().rev().filter(|b| b.name == name) {
            if binding.kind != BindingKind::Label {
                return Some(binding);
            }
            found.get_or_insert(binding);
        }
        found
    }

    /// Every visible name once, in binding order.
    pub fn names(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::with_capacity(self.bindings.len());
        for binding in &self.bindings {
            if !out.contains(&binding.name.as_str()) {
                out.push(&binding.name);
            }
        }
        out
    }
}

/// The names visible at `byte`. A name declared by a statement is visible
/// only once the cursor is past that statement, so inside `x := x + 1` the
/// right-hand `x` is still the outer one. A byte past the end of `src` is
/// clamped.
pub fn scope_at(tree: &Tree, src: &str, byte: usize) -> Scope {
    scope_with(tree, src, byte, None)
}

/// [`scope_at`] reusing what earlier lookups in the same tree learned.
/// Many lookups at different bytes of one tree cost far less this way.
pub fn scope_with<'t>(
    tree: &'t Tree,
    src: &str,
    byte: usize,
    index: Option<&mut ScopeIndex<'t>>,
) -> Scope {
    let mut walker = walk::Walker::new(src, byte.min(src.len()), index);
    walker.descend(tree.root_node(), 0);
    Scope {
        bindings: walker.finish(),
    }
}

/// Source text of a node.
fn text<'a>(src: &'a str, node: Node<'_>) -> &'a str {
    src.get(node.byte_range()).unwrap_or("")
}

/// Whitespace runs to one space, none inside parentheses edges.
fn norm(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace(" )", ")")
}

/// Normalized text of a type node, with the comments inside it removed.
fn type_text(src: &str, node: Node<'_>) -> String {
    let mut cuts = Vec::new();
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if current.kind() == "comment" {
            cuts.push((current.start_byte(), current.end_byte()));
            continue;
        }
        let mut cursor = current.walk();
        stack.extend(current.children(&mut cursor));
    }
    cuts.sort_unstable();
    let mut out = String::new();
    let mut at = node.start_byte();
    for (from, to) in cuts {
        if from < at {
            continue;
        }
        out.push_str(src.get(at..from).unwrap_or(""));
        out.push(' ');
        at = to;
    }
    out.push_str(src.get(at..node.end_byte()).unwrap_or(""));
    norm(&out)
}

/// Line, start column and end column (UTF-16) of a single-line node.
fn position(src: &str, node: Node<'_>) -> (u32, u32, u32) {
    let col = |byte: usize, byte_col: usize| {
        utf16_col(
            src.get(byte.saturating_sub(byte_col)..).unwrap_or(""),
            byte_col,
        )
    };
    let start = node.start_position();
    let end = node.end_position();
    (
        start.row as u32,
        col(node.start_byte(), start.column),
        col(node.end_byte(), end.column),
    )
}

/// Whether the last token of `node` is a real closing brace.
fn closed(node: Node<'_>) -> bool {
    let mut current = node;
    while let Some(last) = current.child(current.child_count().saturating_sub(1)) {
        current = last;
    }
    current.kind() == "}" && !current.is_missing()
}

/// End of `node` as far as a cursor can still be inside it. Code that is
/// still being typed has no closing brace; whitespace after it up to the next
/// sibling (or the end of the file) belongs to the open node.
fn reach(src: &str, node: Node<'_>) -> usize {
    let end = node.end_byte();
    if !node.has_error() || closed(node) {
        return end;
    }
    let limit = node.next_sibling().map_or(src.len(), |n| n.start_byte());
    match src.get(end..limit) {
        Some(gap) if gap.trim().is_empty() => limit,
        _ => end,
    }
}
