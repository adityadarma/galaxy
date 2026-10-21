//! Walks the Tree-sitter tree of a Go file into [`GoFile`] declarations.

use std::path::Path;

use tree_sitter::Node;

use super::{
    Field, FuncDecl, GoFile, IfaceMethod, Import, MAX_DEPTH, Param, Pos, Receiver, TypeDecl,
    TypeKind, TypeParam, ValueDecl, utf16_col,
};

/// Reads the declarations below `root`.
pub(super) fn read(path: &Path, src: &str, root: Node<'_>) -> GoFile {
    let mut reader = Reader {
        src,
        out: GoFile {
            path: path.to_path_buf(),
            ..GoFile::default()
        },
    };
    reader.top(root, 0);
    reader.out
}

/// Whitespace runs to one space, none inside parentheses edges.
fn norm(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace(" )", ")")
}

/// Value of a Go string literal (raw or interpreted).
fn unquote(literal: &str) -> String {
    if let Some(raw) = literal.strip_prefix('`') {
        return raw.strip_suffix('`').unwrap_or(raw).to_owned();
    }
    let inner = literal.strip_prefix('"').unwrap_or(literal);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// A `//go:generate` style line, which is not part of a doc comment.
fn is_directive(line: &str) -> bool {
    let Some((word, rest)) = line.split_once(':') else {
        return false;
    };
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && rest
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// Lines of a `/* ... */` comment with the decoration removed.
fn block_lines(text: &str) -> Vec<String> {
    let inner = text.strip_prefix("/*").unwrap_or(text);
    let inner = inner.strip_suffix("*/").unwrap_or(inner);
    let mut lines: Vec<String> = inner
        .lines()
        .map(|line| {
            let line = line.trim();
            if line == "*" {
                String::new()
            } else {
                line.strip_prefix("* ").unwrap_or(line).to_owned()
            }
        })
        .collect();
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

struct Reader<'a> {
    src: &'a str,
    out: GoFile,
}

impl Reader<'_> {
    fn text(&self, node: Node<'_>) -> &str {
        self.src.get(node.byte_range()).unwrap_or("")
    }

    /// Normalized text of a type node, comments inside it removed.
    fn ty(&self, node: Node<'_>) -> String {
        self.clean(node, node.start_byte(), node.end_byte())
    }

    /// Normalized source text of `start..end` inside `node`, without the
    /// comments `node` holds (a `//` comment would swallow what follows
    /// once the lines are joined).
    fn clean(&self, node: Node<'_>, start: usize, end: usize) -> String {
        let mut cuts = Vec::new();
        comment_ranges(node, &mut cuts, 0);
        let mut out = String::new();
        let mut at = start;
        for (from, to) in cuts {
            if from < at || to > end {
                continue;
            }
            out.push_str(self.src.get(at..from).unwrap_or(""));
            out.push(' ');
            at = to;
        }
        out.push_str(self.src.get(at..end).unwrap_or(""));
        norm(&out)
    }

    fn col(&self, byte: usize, byte_col: usize) -> u32 {
        let line_start = byte.saturating_sub(byte_col);
        utf16_col(self.src.get(line_start..).unwrap_or(""), byte_col)
    }

    fn pos(&self, node: Node<'_>) -> Pos {
        let start = node.start_position();
        let end = node.end_position();
        Pos {
            line: start.row as u32,
            column: self.col(node.start_byte(), start.column),
            end_column: self.col(node.end_byte(), end.column),
        }
    }

    /// The `//` comment lines directly above `node`, joined with newlines.
    fn doc(&self, node: Node<'_>) -> String {
        let mut comments = Vec::new();
        let mut next_row = node.start_position().row;
        let mut current = node.prev_sibling();
        while let Some(comment) = current {
            if comment.kind() != "comment" || comment.end_position().row + 1 < next_row {
                break;
            }
            // A comment at the end of the previous declaration's line.
            let before = comment.prev_sibling();
            if before.is_some_and(|prev| {
                prev.kind() != "comment" && prev.end_position().row == comment.start_position().row
            }) {
                break;
            }
            comments.push(comment);
            next_row = comment.start_position().row;
            current = before;
        }
        let mut lines: Vec<String> = Vec::new();
        for comment in comments.iter().rev() {
            let text = self.text(*comment);
            if let Some(line) = text.strip_prefix("//") {
                if !is_directive(line) {
                    lines.push(line.strip_prefix(' ').unwrap_or(line).trim_end().to_owned());
                }
            } else {
                lines.extend(block_lines(text));
            }
        }
        lines.join("\n")
    }

    fn top(&mut self, parent: Node<'_>, depth: usize) {
        let mut cursor = parent.walk();
        let kids: Vec<Node<'_>> = parent.named_children(&mut cursor).collect();
        for node in kids {
            match node.kind() {
                "package_clause" => self.package(node),
                "import_declaration" => self.imports(node),
                "type_declaration" => self.types(node),
                "function_declaration" => self.func(node, false),
                "method_declaration" => self.func(node, true),
                "const_declaration" => self.values(node, true),
                "var_declaration" => self.values(node, false),
                "ERROR" if depth < MAX_DEPTH => self.top(node, depth + 1),
                _ => {}
            }
        }
    }

    fn package(&mut self, node: Node<'_>) {
        if !self.out.package.is_empty() {
            return;
        }
        let mut cursor = node.walk();
        let name = node
            .named_children(&mut cursor)
            .find(|n| n.kind() == "package_identifier")
            .map(|n| self.text(n).to_owned());
        let Some(name) = name else { return };
        self.out.package = name;
        self.out.package_doc = self.doc(node);
        let mut current = node.prev_sibling();
        while let Some(comment) = current {
            if comment.kind() != "comment" {
                break;
            }
            if let Some(rest) = self.text(comment).strip_prefix("//go:build") {
                self.out.build_constraint = Some(rest.trim().to_owned());
            }
            current = comment.prev_sibling();
        }
    }

    fn imports(&mut self, decl: Node<'_>) {
        let mut cursor = decl.walk();
        let kids: Vec<Node<'_>> = decl.named_children(&mut cursor).collect();
        for kid in kids {
            match kid.kind() {
                "import_spec" => self.import_spec(kid),
                "import_spec_list" => {
                    let mut inner = kid.walk();
                    let specs: Vec<Node<'_>> = kid.named_children(&mut inner).collect();
                    for spec in specs.into_iter().filter(|s| s.kind() == "import_spec") {
                        self.import_spec(spec);
                    }
                }
                _ => {}
            }
        }
    }

    fn import_spec(&mut self, spec: Node<'_>) {
        let Some(path) = spec.child_by_field_name("path") else {
            return;
        };
        let alias = spec.child_by_field_name("name").map(|n| match n.kind() {
            "dot" => ".".to_owned(),
            "blank_identifier" => "_".to_owned(),
            _ => self.text(n).to_owned(),
        });
        let whole = self.pos(spec);
        let literal = self.pos(path);
        self.out.imports.push(Import {
            alias,
            path: unquote(self.text(path)),
            line: whole.line,
            column: whole.column,
            end_column: whole.end_column,
            path_column: literal.column,
            path_end_column: literal.end_column,
        });
    }

    fn types(&mut self, decl: Node<'_>) {
        let group_doc = self.doc(decl);
        let mut cursor = decl.walk();
        let specs: Vec<Node<'_>> = decl
            .named_children(&mut cursor)
            .filter(|n| matches!(n.kind(), "type_spec" | "type_alias"))
            .collect();
        for spec in specs {
            let Some(name) = spec.child_by_field_name("name") else {
                continue;
            };
            let text = self.text(name);
            if text.is_empty() || text == "_" {
                continue;
            }
            let mut doc = self.doc(spec);
            if doc.is_empty() {
                doc.clone_from(&group_doc);
            }
            let mut item = TypeDecl {
                name: text.to_owned(),
                kind: TypeKind::Named,
                type_params: spec
                    .child_by_field_name("type_parameters")
                    .map(|l| self.type_params(l))
                    .unwrap_or_default(),
                fields: Vec::new(),
                methods: Vec::new(),
                embeds: Vec::new(),
                underlying: String::new(),
                doc,
                pos: self.pos(name),
                end_line: spec.end_position().row as u32,
            };
            if let Some(body) = spec.child_by_field_name("type") {
                if spec.kind() == "type_alias" {
                    item.kind = TypeKind::Alias;
                    item.underlying = self.ty(body);
                } else {
                    match body.kind() {
                        "struct_type" => {
                            item.kind = TypeKind::Struct;
                            "struct".clone_into(&mut item.underlying);
                            item.fields = self.fields(body);
                        }
                        "interface_type" => {
                            item.kind = TypeKind::Interface;
                            "interface".clone_into(&mut item.underlying);
                            self.interface(body, &mut item);
                        }
                        _ => item.underlying = self.ty(body),
                    }
                }
            } else if spec.kind() == "type_alias" {
                item.kind = TypeKind::Alias;
            }
            self.out.types.push(item);
        }
    }

    fn type_params(&self, list: Node<'_>) -> Vec<TypeParam> {
        let mut out = Vec::new();
        let mut cursor = list.walk();
        for decl in list.named_children(&mut cursor) {
            if decl.kind() != "type_parameter_declaration" {
                continue;
            }
            let constraint = decl
                .child_by_field_name("type")
                .map(|n| self.ty(n))
                .unwrap_or_default();
            let mut names = decl.walk();
            for name in decl.children_by_field_name("name", &mut names) {
                if name.kind() == "identifier" && !self.text(name).is_empty() {
                    out.push(TypeParam {
                        name: self.text(name).to_owned(),
                        constraint: constraint.clone(),
                    });
                }
            }
        }
        out
    }

    fn fields(&self, body: Node<'_>) -> Vec<Field> {
        let mut out = Vec::new();
        let mut cursor = body.walk();
        let Some(list) = body
            .named_children(&mut cursor)
            .find(|n| n.kind() == "field_declaration_list")
        else {
            return out;
        };
        let mut inner = list.walk();
        for decl in list.named_children(&mut inner) {
            if decl.kind() != "field_declaration" {
                continue;
            }
            let Some(ty_node) = decl.child_by_field_name("type") else {
                continue;
            };
            let doc = self.doc(decl);
            let tag = decl
                .child_by_field_name("tag")
                .map(|n| unquote(self.text(n)));
            let mut names = decl.walk();
            let named: Vec<Node<'_>> = decl
                .children_by_field_name("name", &mut names)
                .filter(|n| n.kind() == "field_identifier")
                .collect();
            if named.is_empty() {
                // Embedded: `Base`, `*Base`, `pkg.Base`, `Base[T]`. The
                // `*` sits beside the type node, so read from the start.
                let Some(ident) = base_ident(ty_node) else {
                    continue;
                };
                let start = decl.start_byte();
                let ty = self.clean(decl, start, ty_node.end_byte());
                out.push(Field {
                    name: self.text(ident).to_owned(),
                    ty,
                    tag,
                    embedded: true,
                    doc,
                    pos: self.pos(ident),
                });
                continue;
            }
            let ty = self.ty(ty_node);
            for name in named {
                let text = self.text(name);
                if text.is_empty() || text == "_" {
                    continue;
                }
                out.push(Field {
                    name: text.to_owned(),
                    ty: ty.clone(),
                    tag: tag.clone(),
                    embedded: false,
                    doc: doc.clone(),
                    pos: self.pos(name),
                });
            }
        }
        out
    }

    fn interface(&self, body: Node<'_>, item: &mut TypeDecl) {
        let mut cursor = body.walk();
        for elem in body.named_children(&mut cursor) {
            match elem.kind() {
                "type_elem" => item.embeds.push(self.ty(elem)),
                "method_elem" => {
                    let Some(name) = elem.child_by_field_name("name") else {
                        continue;
                    };
                    let mut signature = elem
                        .child_by_field_name("parameters")
                        .map(|n| self.ty(n))
                        .unwrap_or_default();
                    if let Some(result) = elem.child_by_field_name("result") {
                        signature.push(' ');
                        signature.push_str(&self.ty(result));
                    }
                    item.methods.push(IfaceMethod {
                        name: self.text(name).to_owned(),
                        signature,
                        doc: self.doc(elem),
                        pos: self.pos(name),
                    });
                }
                _ => {}
            }
        }
    }

    fn func(&mut self, node: Node<'_>, is_method: bool) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let text = self.text(name);
        if text.is_empty() {
            return;
        }
        let results = match node.child_by_field_name("result") {
            Some(n) if n.kind() == "parameter_list" => self.params(n),
            Some(n) => vec![Param {
                name: String::new(),
                ty: self.ty(n),
                variadic: false,
            }],
            None => Vec::new(),
        };
        let item = FuncDecl {
            name: text.to_owned(),
            receiver: if is_method {
                node.child_by_field_name("receiver")
                    .and_then(|l| self.receiver(l))
            } else {
                None
            },
            type_params: node
                .child_by_field_name("type_parameters")
                .map(|l| self.type_params(l))
                .unwrap_or_default(),
            params: node
                .child_by_field_name("parameters")
                .map(|l| self.params(l))
                .unwrap_or_default(),
            results,
            doc: self.doc(node),
            pos: self.pos(name),
            body: node
                .child_by_field_name("body")
                .map(|b| (b.start_position().row as u32, b.end_position().row as u32)),
            end_line: node.end_position().row as u32,
        };
        self.out.funcs.push(item);
    }

    fn params(&self, list: Node<'_>) -> Vec<Param> {
        let mut out = Vec::new();
        let mut cursor = list.walk();
        for child in list.named_children(&mut cursor) {
            let variadic = match child.kind() {
                "parameter_declaration" => false,
                "variadic_parameter_declaration" => true,
                _ => continue,
            };
            let ty = child
                .child_by_field_name("type")
                .map(|n| self.ty(n))
                .unwrap_or_default();
            let mut names = child.walk();
            let named: Vec<&str> = child
                .children_by_field_name("name", &mut names)
                .filter(|n| n.kind() == "identifier")
                .map(|n| self.text(n))
                .collect();
            if named.is_empty() {
                out.push(Param {
                    name: String::new(),
                    ty: ty.clone(),
                    variadic,
                });
            }
            for name in named {
                out.push(Param {
                    name: name.to_owned(),
                    ty: ty.clone(),
                    variadic,
                });
            }
        }
        out
    }

    fn receiver(&self, list: Node<'_>) -> Option<Receiver> {
        let mut cursor = list.walk();
        let decl = list
            .named_children(&mut cursor)
            .find(|n| n.kind() == "parameter_declaration")?;
        let ty_node = decl.child_by_field_name("type")?;
        let mut names = decl.walk();
        let name = decl
            .children_by_field_name("name", &mut names)
            .find(|n| n.kind() == "identifier")
            .map(|n| self.text(n).to_owned())
            .unwrap_or_default();
        let base = base_ident(ty_node).map(|n| self.text(n).to_owned())?;
        Some(Receiver {
            name,
            ty: self.ty(ty_node),
            base,
            pointer: is_pointer(ty_node),
        })
    }

    fn values(&mut self, decl: Node<'_>, is_const: bool) {
        let group_doc = self.doc(decl);
        let mut specs = Vec::new();
        let mut cursor = decl.walk();
        for kid in decl.named_children(&mut cursor) {
            match kid.kind() {
                "const_spec" | "var_spec" => specs.push(kid),
                "var_spec_list" => {
                    let mut inner = kid.walk();
                    specs.extend(
                        kid.named_children(&mut inner)
                            .filter(|n| n.kind() == "var_spec"),
                    );
                }
                _ => {}
            }
        }
        // Type of the last spec that had a value, for `iota` repetition.
        let mut carried: Option<String> = None;
        for spec in specs {
            let ty = spec.child_by_field_name("type").map(|n| self.ty(n));
            let values: Vec<Node<'_>> = spec
                .child_by_field_name("value")
                .map(|list| {
                    let mut c = list.walk();
                    list.named_children(&mut c)
                        .filter(|n| n.kind() != "comment")
                        .collect()
                })
                .unwrap_or_default();
            let whole = spec
                .child_by_field_name("value")
                .map(|list| self.text(list).to_owned());
            let group_type = if is_const && ty.is_none() && whole.is_none() {
                carried.clone()
            } else {
                None
            };
            if whole.is_some() || ty.is_some() {
                carried.clone_from(&ty);
            }
            let mut doc = self.doc(spec);
            if doc.is_empty() {
                doc.clone_from(&group_doc);
            }
            let mut names = spec.walk();
            let idents: Vec<Node<'_>> = spec
                .children_by_field_name("name", &mut names)
                .filter(|n| n.kind() == "identifier")
                .collect();
            let single = idents.len() == values.len();
            for (index, ident) in idents.iter().enumerate() {
                let name = self.text(*ident);
                if name.is_empty() || name == "_" {
                    continue;
                }
                let value = if single {
                    Some(self.text(values[index]).to_owned())
                } else {
                    whole.clone()
                };
                let item = ValueDecl {
                    name: name.to_owned(),
                    ty: ty.clone(),
                    value,
                    doc: doc.clone(),
                    pos: self.pos(*ident),
                    group_type: group_type.clone(),
                    is_const,
                };
                if is_const {
                    self.out.consts.push(item);
                } else {
                    self.out.vars.push(item);
                }
            }
        }
    }
}

/// Byte ranges of the comments below `node`, in source order.
fn comment_ranges(node: Node<'_>, out: &mut Vec<(usize, usize)>, depth: usize) {
    if node.kind() == "comment" {
        out.push((node.start_byte(), node.end_byte()));
        return;
    }
    if depth >= MAX_DEPTH {
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        comment_ranges(child, out, depth + 1);
    }
}

/// The identifier naming a (possibly pointer, generic or qualified) type.
fn base_ident(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node;
    for _ in 0..MAX_DEPTH {
        match current.kind() {
            "type_identifier" => return Some(current),
            "qualified_type" => return current.child_by_field_name("name"),
            "generic_type" => current = current.child_by_field_name("type")?,
            "pointer_type" | "parenthesized_type" => current = current.named_child(0)?,
            _ => return None,
        }
    }
    None
}

fn is_pointer(node: Node<'_>) -> bool {
    let mut current = node;
    for _ in 0..MAX_DEPTH {
        match current.kind() {
            "pointer_type" => return true,
            "parenthesized_type" => match current.named_child(0) {
                Some(inner) => current = inner,
                None => return false,
            },
            _ => return false,
        }
    }
    false
}
