//! Turning declaration nodes into bindings.

use tree_sitter::Node;

use super::walk::{Ctx, Walker};
use super::{Binding, BindingKind, Origin, position, type_text};

/// Named children of `list` other than comments.
fn items<'t>(list: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = list.walk();
    list.named_children(&mut cursor)
        .filter(|n| n.kind() != "comment")
        .collect()
}

/// Whether `node` has a direct `:=` child.
fn has_define(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor).any(|n| n.kind() == ":=")
}

impl<'a, 't> Walker<'a, 't> {
    /// Adds `binding` to the block `scope`. The nearest name wins: an earlier
    /// binding of the same name (and namespace) is dropped.
    fn push(&mut self, binding: Binding, scope: usize) {
        if binding.name == "_" {
            return;
        }
        let label = binding.kind == BindingKind::Label;
        self.entries
            .retain(|(b, _)| !(b.name == binding.name && (b.kind == BindingKind::Label) == label));
        self.entries.push((binding, scope));
    }

    fn add(&mut self, node: Node<'t>, kind: BindingKind, origin: Origin, ctx: Ctx) {
        let name = self.text(node);
        if name.is_empty() || name == "_" {
            return;
        }
        let (line, column, end_column) = position(self.src, node);
        let binding = Binding {
            name: name.to_owned(),
            kind,
            origin,
            line,
            column,
            end_column,
            decl_byte: node.start_byte(),
            scope_end_byte: ctx.end,
        };
        self.push(binding, ctx.id);
    }

    fn defined_in(&self, name: &str, scope: usize) -> bool {
        self.entries
            .iter()
            .any(|(b, s)| *s == scope && b.name == name && b.kind != BindingKind::Label)
    }

    /// Origin of the `index`-th of `count` names given the initializer
    /// expressions: one expression each, or one expression for all.
    fn value_origin(index: usize, count: usize, values: &[String]) -> Origin {
        if values.len() == count {
            Origin::Expr {
                text: values[index].clone(),
                index: 0,
                count: 1,
            }
        } else if values.len() == 1 {
            Origin::Expr {
                text: values[0].clone(),
                index,
                count,
            }
        } else {
            Origin::Unknown
        }
    }

    /// Declares `names` (identifier nodes, anything else is skipped).
    fn pairs(
        &mut self,
        names: &[Node<'t>],
        values: &[String],
        declared: Option<&str>,
        kind: &BindingKind,
        ctx: Ctx,
        redeclare: bool,
    ) {
        for (index, name) in names.iter().enumerate() {
            if name.kind() != "identifier" {
                continue;
            }
            // `a, err := f(); b, err := g()` assigns the second `err`.
            if redeclare && self.defined_in(self.text(*name), ctx.id) {
                continue;
            }
            let origin = match declared {
                Some(ty) => Origin::Declared(ty.to_owned()),
                None => Self::value_origin(index, names.len(), values),
            };
            self.add(*name, kind.clone(), origin, ctx);
        }
    }

    fn value_texts(&self, list: Option<Node<'t>>) -> Vec<String> {
        list.map(|l| {
            items(l)
                .into_iter()
                .map(|n| self.text(n).to_owned())
                .collect()
        })
        .unwrap_or_default()
    }

    /// `names := values`
    pub(super) fn short(&mut self, stmt: Node<'t>, ctx: Ctx) {
        let (Some(left), right) = (
            stmt.child_by_field_name("left"),
            stmt.child_by_field_name("right"),
        ) else {
            return;
        };
        let values = self.value_texts(right);
        self.pairs(
            &items(left),
            &values,
            None,
            &BindingKind::ShortVar,
            ctx,
            true,
        );
    }

    /// The specs of a `var` or `const` declaration that end before the
    /// cursor.
    pub(super) fn group(&mut self, decl: Node<'t>, ctx: Ctx) {
        let is_const = decl.kind() == "const_declaration";
        let mut specs = Vec::new();
        for kid in items(decl) {
            match kid.kind() {
                "var_spec" | "const_spec" => specs.push(kid),
                "var_spec_list" => specs.extend(items(kid)),
                _ => {}
            }
        }
        // Type and values of the last spec that had either, which a bare
        // `const` spec repeats.
        let mut carried: Option<(Option<String>, Vec<String>)> = None;
        for spec in specs {
            if !self.done(spec.end_byte()) {
                break;
            }
            let mut cursor = spec.walk();
            let names: Vec<Node<'t>> = spec
                .children_by_field_name("name", &mut cursor)
                .filter(|n| n.kind() == "identifier")
                .collect();
            let mut ty = spec
                .child_by_field_name("type")
                .map(|n| type_text(self.src, n));
            let mut values = self.value_texts(spec.child_by_field_name("value"));
            if is_const {
                if ty.is_none() && values.is_empty() {
                    if let Some((carried_ty, carried_values)) = &carried {
                        ty.clone_from(carried_ty);
                        values.clone_from(carried_values);
                    }
                } else {
                    carried = Some((ty.clone(), values.clone()));
                }
            }
            let kind = if is_const {
                BindingKind::Const
            } else {
                BindingKind::Var
            };
            self.pairs(&names, &values, ty.as_deref(), &kind, ctx, false);
        }
    }

    pub(super) fn params(&mut self, list: Node<'t>, kind: BindingKind, ctx: Ctx) {
        for decl in items(list) {
            let variadic = match decl.kind() {
                "parameter_declaration" => false,
                "variadic_parameter_declaration" => true,
                _ => continue,
            };
            let Some(ty_node) = decl.child_by_field_name("type") else {
                continue;
            };
            let mut ty = type_text(self.src, ty_node);
            if variadic {
                ty.insert_str(0, "[]");
            }
            let mut cursor = decl.walk();
            for name in decl.children_by_field_name("name", &mut cursor) {
                if name.kind() == "identifier" {
                    self.add(name, kind.clone(), Origin::Declared(ty.clone()), ctx);
                }
            }
        }
    }

    pub(super) fn type_params(&mut self, list: Node<'t>, ctx: Ctx) {
        for decl in items(list) {
            if decl.kind() != "type_parameter_declaration" {
                continue;
            }
            let constraint = decl
                .child_by_field_name("type")
                .map(|n| type_text(self.src, n))
                .unwrap_or_default();
            let mut cursor = decl.walk();
            for name in decl.children_by_field_name("name", &mut cursor) {
                if name.kind() == "identifier" {
                    let origin = Origin::Declared(constraint.clone());
                    self.add(name, BindingKind::TypeParam, origin, ctx);
                }
            }
        }
    }

    /// The labels of a function body, nested function literals excluded.
    pub(super) fn labels(&mut self, body: Node<'t>, ctx: Ctx) {
        // Walking every node of a long function for each cursor is the most
        // expensive step of a scope lookup, and nearly no function has a
        // label. A scan of the text rules them out first.
        let text = self.text(body).as_bytes();
        let candidate = self.label_candidate(body, || {
            text.iter()
                .enumerate()
                .any(|(i, b)| *b == b':' && may_end_label(text, i))
        });
        if !candidate {
            return;
        }
        let mut found = Vec::new();
        let mut stack = vec![body];
        while let Some(node) = stack.pop() {
            if node.kind() == "labeled_statement"
                && let Some(label) = node.child_by_field_name("label")
            {
                found.push(label);
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() != "func_literal" {
                    stack.push(child);
                }
            }
        }
        found.sort_by_key(|n| n.start_byte());
        for label in found {
            self.add(label, BindingKind::Label, Origin::Unknown, ctx);
        }
    }

    /// `k, v := range expr`
    pub(super) fn range(&mut self, clause: Node<'t>, ctx: Ctx) {
        if !has_define(clause) {
            return;
        }
        let (Some(left), Some(right)) = (
            clause.child_by_field_name("left"),
            clause.child_by_field_name("right"),
        ) else {
            return;
        };
        let source = self.text(right);
        for (index, name) in items(left).into_iter().take(2).enumerate() {
            if name.kind() != "identifier" {
                continue;
            }
            let key = index == 0;
            let kind = if key {
                BindingKind::RangeKey
            } else {
                BindingKind::RangeValue
            };
            let origin = Origin::RangeOver {
                text: source.to_owned(),
                key,
            };
            self.add(name, kind, origin, ctx);
        }
    }

    /// `case v, ok := <-ch:` of a `select`.
    pub(super) fn receive(&mut self, comm: Node<'t>, ctx: Ctx) {
        if comm.kind() != "receive_statement" || !has_define(comm) {
            return;
        }
        let (Some(left), Some(right)) = (
            comm.child_by_field_name("left"),
            comm.child_by_field_name("right"),
        ) else {
            return;
        };
        let values = [self.text(right).to_owned()];
        self.pairs(
            &items(left),
            &values,
            None,
            &BindingKind::ShortVar,
            ctx,
            false,
        );
    }

    /// `v` of `switch v := x.(type)` inside one clause.
    pub(super) fn type_binding(&mut self, switch: Node<'t>, clause: Node<'t>, ctx: Ctx) {
        let Some(alias) = switch
            .child_by_field_name("alias")
            .and_then(|list| items(list).into_iter().next())
            .filter(|n| n.kind() == "identifier")
        else {
            return;
        };
        let subject = switch
            .child_by_field_name("value")
            .map(|n| self.text(n).to_owned())
            .unwrap_or_default();
        let mut case_types = Vec::new();
        if clause.kind() == "type_case" {
            let mut cursor = clause.walk();
            for ty in clause.children_by_field_name("type", &mut cursor) {
                if ty.is_named() && ty.kind() != "comment" {
                    case_types.push(type_text(self.src, ty));
                }
            }
        }
        let origin = Origin::TypeSwitch {
            subject,
            case_types,
        };
        self.add(alias, BindingKind::TypeSwitch, origin, ctx);
    }
}

/// Whether the `:` at `colon` could end a label (`Name:` opening a
/// statement). Every real label passes; `:=`, `x:` after other text and
/// `case x:` do not, so most of the false hits left are keyed literals.
pub(super) fn may_end_label(bytes: &[u8], colon: usize) -> bool {
    if bytes.get(colon + 1) == Some(&b'=') {
        return false;
    }
    let mut i = colon;
    while i > 0
        && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_' || bytes[i - 1] >= 0x80)
    {
        i -= 1;
    }
    if i == colon {
        return false;
    }
    if &bytes[i..colon] == b"default" {
        return false;
    }
    while i > 0 && matches!(bytes[i - 1], b' ' | b'\t' | b'\r') {
        i -= 1;
    }
    // First on its line (a statement before it ended at the newline), or
    // after a brace, `;`, another label or the end of a comment.
    i == 0 || matches!(bytes[i - 1], b'\n' | b'{' | b'}' | b';' | b':' | b'/')
}
