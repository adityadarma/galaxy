//! Definition and hover for the symbol under the cursor.

use super::*;

impl PhpEngine {
    // ---- definition and hover ------------------------------------------

    /// Where the symbol under the cursor is declared.
    pub fn definition(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Target> {
        let previous = self.register_current(path, text);
        let frameworks = self.frameworks.clone();
        let named = frameworks.iter().find_map(|fw| {
            fw.definition(self, path, text, line, column)
                .filter(|targets| !targets.is_empty())
        });
        self.restore_current(path, previous);
        if let Some(targets) = named {
            return targets;
        }
        if self.is_template(path) {
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
        let frameworks = self.frameworks.clone();
        let named = frameworks
            .iter()
            .find_map(|fw| fw.hover(self, path, text, line, column));
        self.restore_current(path, previous);
        if named.is_some() {
            return named;
        }
        if self.is_template(path) {
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

    pub(super) fn with_symbol<T>(
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

    pub(super) fn with_symbol_inner<T>(
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

    pub(super) fn classify(&mut self, ctx: &Ctx<'_>, node: Node<'_>) -> Option<Symbol> {
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

    pub(super) fn symbol_target(&mut self, symbol: Symbol) -> Option<Vec<Target>> {
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

    pub(super) fn symbol_markdown(&mut self, ctx: &Ctx<'_>, symbol: Symbol) -> Option<String> {
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
