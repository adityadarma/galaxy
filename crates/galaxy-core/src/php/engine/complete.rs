//! Completion: members, variables, functions, classes and `use` lines.

use super::*;

impl PhpEngine {
    // ---- completion --------------------------------------------------

    /// Makes the declarations of `text` visible under `path` while one
    /// request runs, so classes of the file being edited resolve too.
    pub(super) fn register_current(&mut self, path: &Path, text: &str) -> Option<Arc<FileSymbols>> {
        self.open
            .insert(path.to_path_buf(), Arc::new(parse_file(path, text)))
    }

    pub(super) fn restore_current(&mut self, path: &Path, previous: Option<Arc<FileSymbols>>) {
        match previous {
            Some(file) => {
                self.open.insert(path.to_path_buf(), file);
            }
            None => {
                self.open.remove(path);
            }
        }
    }

    /// Suggestions at `(line, column)` of `text`, a UTF-16 column.
    pub fn complete(&mut self, path: &Path, text: &str, line: u32, column: u32) -> Vec<Completion> {
        let previous = self.register_current(path, text);
        let frameworks = self.frameworks.clone();
        let answer = frameworks
            .iter()
            .find_map(|fw| fw.complete(self, path, text, line, column));
        let out = match answer {
            Some(items) => items,
            // Templates are not PHP: only a framework has anything to say.
            None if self.is_template(path) => Vec::new(),
            // A string a framework handles but has no suggestion for.
            None if byte_offset(text, line, column)
                .is_some_and(|byte| frameworks.iter().any(|fw| fw.owns_string(text, byte))) =>
            {
                Vec::new()
            }
            None => self.complete_inner(path, text, line, column),
        };
        self.restore_current(path, previous);
        out
    }

    pub(super) fn complete_inner(
        &mut self,
        path: &Path,
        text: &str,
        line: u32,
        column: u32,
    ) -> Vec<Completion> {
        let Some(byte) = byte_offset(text, line, column) else {
            return Vec::new();
        };
        let Some((file, tree)) = parse_with_tree(path, text) else {
            return Vec::new();
        };
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
        let word_start = word_start(text, byte);
        let word = &text[word_start..byte];
        let before = &text[..word_start];
        let trimmed = before.trim_end_matches([' ', '\t']);

        if let Some((receiver_end, is_static)) = member_operator(trimmed) {
            let start = receiver_start(trimmed, receiver_end);
            let receiver = &trimmed[start..receiver_end];
            let Some(class_fqn) = self.eval(&ctx, receiver, 0) else {
                return Vec::new();
            };
            let Some(class) = self.class(&class_fqn) else {
                return Vec::new();
            };
            let via_scope = is_static
                && matches!(
                    receiver.trim().to_ascii_lowercase().as_str(),
                    "self" | "static" | "parent"
                );
            return self.member_items(&ctx, &class, is_static, via_scope, word);
        }

        let line_start = text[..byte].rfind('\n').map_or(0, |i| i + 1);
        let line_before = &text[line_start..byte];
        static USE_LINE: OnceLock<Regex> = OnceLock::new();
        let use_line = USE_LINE.get_or_init(|| Regex::new(r"^\s*use\s+([\w\\]*)$").expect("valid"));
        if let Some(caps) = use_line.captures(line_before) {
            return self.import_items(&caps[1]);
        }

        if let Some(name) = word.strip_prefix('$') {
            return self.variable_items(&ctx, name);
        }

        let line_text = line_before.trim_start();
        static NEW: OnceLock<Regex> = OnceLock::new();
        let after_new = NEW
            .get_or_init(|| Regex::new(r"\bnew\s+$").expect("valid"))
            .is_match(before);
        let wants_interface = line_text.contains("implements");
        if word.is_empty() && !after_new {
            return Vec::new();
        }
        let mut items = self.class_items(&ctx, word, after_new, wants_interface);
        if !after_new {
            items.extend(self.function_items(&ctx, word));
            items.extend(keyword_items(word));
        }
        items.sort_by(|a, b| a.sort.cmp(&b.sort));
        items.truncate(MAX_ITEMS);
        items
    }

    pub(super) fn accessible(
        &mut self,
        ctx: &Ctx<'_>,
        owner: &Arc<ClassInfo>,
        visibility: Visibility,
    ) -> bool {
        match visibility {
            Visibility::Public => true,
            Visibility::Private => ctx.class.as_ref().is_some_and(|c| c.fqn == owner.fqn),
            Visibility::Protected => {
                let Some(current) = ctx.class.clone() else {
                    return false;
                };
                if current.fqn == owner.fqn {
                    return true;
                }
                self.lineage(&current).iter().any(|c| c.fqn == owner.fqn)
                    || self.lineage(owner).iter().any(|c| c.fqn == current.fqn)
            }
        }
    }

    pub(super) fn member_items(
        &mut self,
        ctx: &Ctx<'_>,
        class: &Arc<ClassInfo>,
        is_static: bool,
        via_scope: bool,
        word: &str,
    ) -> Vec<Completion> {
        let typed_dollar = word.starts_with('$');
        let prefix = word.trim_start_matches('$');
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for owner in self.lineage(class) {
            for member in &owner.members {
                let wanted = match member.kind {
                    MemberKind::Method => {
                        if is_static {
                            member.is_static || via_scope
                        } else {
                            true
                        }
                    }
                    MemberKind::Property => member.is_static == is_static,
                    MemberKind::Constant | MemberKind::Case => is_static,
                };
                if !wanted {
                    continue;
                }
                if member.name.starts_with("__") && !via_scope {
                    continue;
                }
                if !fuzzy(prefix, &member.name) {
                    continue;
                }
                let key = (
                    member.kind == MemberKind::Method,
                    member.name.to_lowercase(),
                );
                if !seen.insert(key) {
                    continue;
                }
                if !self.accessible(ctx, &owner, member.visibility) {
                    continue;
                }
                let own = if owner.fqn == class.fqn { '0' } else { '1' };
                out.push(member_completion(member, &owner, own, typed_dollar));
            }
        }
        if is_static && fuzzy(prefix, "class") {
            out.push(Completion {
                label: "class".into(),
                detail: Some("class name".into()),
                insert_text: "class".into(),
                is_snippet: false,
                kind: ItemKind::Constant,
                sort: "9class".into(),
                extra: Vec::new(),
                replace_from: None,
            });
        }
        out.sort_by(|a, b| a.sort.cmp(&b.sort));
        out.truncate(MAX_ITEMS);
        out
    }

    pub(super) fn variable_items(&mut self, ctx: &Ctx<'_>, prefix: &str) -> Vec<Completion> {
        let mut names: Vec<String> = Vec::new();
        if let Some(mut node) = ctx
            .tree
            .root_node()
            .descendant_for_byte_range(ctx.byte, ctx.byte)
        {
            while !is_scope(node.kind()) {
                match node.parent() {
                    Some(parent) => node = parent,
                    None => break,
                }
            }
            let mut stack = vec![node];
            while let Some(current) = stack.pop() {
                let mut cursor = current.walk();
                for child in current.named_children(&mut cursor) {
                    if child.kind() == "variable_name" && child.start_byte() < ctx.byte {
                        let name = ctx.src[child.byte_range()].trim_start_matches('$');
                        if !names.iter().any(|n| n == name) {
                            names.push(name.to_owned());
                        }
                    }
                    stack.push(child);
                }
            }
        }
        if ctx.class.is_some() {
            names.push("this".into());
        }
        names.retain(|name| !name.is_empty() && name != prefix && fuzzy(prefix, name));
        names.sort();
        names
            .into_iter()
            .map(|name| Completion {
                label: format!("${name}"),
                detail: None,
                insert_text: name.clone(),
                is_snippet: false,
                kind: ItemKind::Variable,
                sort: format!("0{}", name.to_lowercase()),
                extra: Vec::new(),
                replace_from: None,
            })
            .collect()
    }

    pub(super) fn function_items(&mut self, ctx: &Ctx<'_>, prefix: &str) -> Vec<Completion> {
        let mut found: Vec<Arc<FunctionInfo>> = Vec::new();
        for file in self.open.values() {
            found.extend(file.functions.iter().cloned());
        }
        found.extend(self.function_index().values().cloned());
        found.extend(self.builtin().functions().cloned());
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for function in found {
            if !fuzzy(prefix, &function.name) || !seen.insert(function.fqn.to_lowercase()) {
                continue;
            }
            let empty = function.detail.starts_with("()");
            out.push(Completion {
                label: function.name.clone(),
                detail: Some(function.detail.clone()),
                insert_text: if empty {
                    format!("{}()", function.name)
                } else {
                    format!("{}($0)", function.name)
                },
                is_snippet: !empty,
                kind: ItemKind::Function,
                sort: rank(prefix, &function.name, '2'),
                extra: Vec::new(),
                replace_from: None,
            });
        }
        let _ = ctx;
        out
    }

    pub(super) fn class_items(
        &mut self,
        ctx: &Ctx<'_>,
        prefix: &str,
        concrete_only: bool,
        interfaces_only: bool,
    ) -> Vec<Completion> {
        let names = self.class_names();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut consider = |short: &str,
                            fqn: &str,
                            vendor: bool,
                            kind: Option<ClassKind>,
                            out: &mut Vec<Completion>| {
            if !fuzzy(prefix, short) || !seen.insert(fqn.to_lowercase()) {
                return;
            }
            if let Some(kind) = kind {
                if concrete_only && matches!(kind, ClassKind::Interface | ClassKind::Trait) {
                    return;
                }
                if interfaces_only && kind != ClassKind::Interface {
                    return;
                }
            }
            let namespace = fqn.rsplit_once('\\').map(|(ns, _)| ns).unwrap_or("");
            out.push(Completion {
                label: short.to_owned(),
                detail: (!namespace.is_empty()).then(|| namespace.to_owned()),
                insert_text: short.to_owned(),
                is_snippet: false,
                kind: match kind {
                    Some(ClassKind::Interface) => ItemKind::Interface,
                    Some(ClassKind::Trait) => ItemKind::Trait,
                    Some(ClassKind::Enum) => ItemKind::Enum,
                    _ => ItemKind::Class,
                },
                sort: rank(prefix, short, if vendor { '1' } else { '0' }),
                extra: import_edit(ctx.file, fqn).into_iter().collect(),
                replace_from: None,
            });
        };
        let open: Vec<Arc<ClassInfo>> = self
            .open
            .values()
            .flat_map(|file| file.classes.iter().cloned())
            .collect();
        for class in &open {
            consider(&class.name, &class.fqn, false, Some(class.kind), &mut out);
        }
        for name in names.iter() {
            consider(&name.short, &name.fqn, name.vendor, None, &mut out);
            if out.len() > MAX_ITEMS * 8 {
                break;
            }
        }
        out.sort_by(|a, b| a.sort.cmp(&b.sort));
        out.truncate(MAX_ITEMS);
        out
    }

    /// Classes for a `use` statement: the text after what is typed so far
    /// up to the namespace separator.
    pub(super) fn import_items(&mut self, typed: &str) -> Vec<Completion> {
        let dir_len = typed.rfind('\\').map_or(0, |i| i + 1);
        let (dir, prefix) = typed.split_at(dir_len);
        let names = self.class_names();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for name in names.iter() {
            let Some(head) = name.fqn.get(..dir.len()) else {
                continue;
            };
            if !head.eq_ignore_ascii_case(dir) {
                continue;
            }
            let rest = &name.fqn[dir.len()..];
            // Offer the next namespace segment once, classes by name.
            let (label, is_namespace) = match rest.split_once('\\') {
                Some((segment, _)) => (segment, true),
                None => (rest, false),
            };
            if !fuzzy(prefix, label) || !seen.insert((label.to_lowercase(), is_namespace)) {
                continue;
            }
            out.push(Completion {
                label: label.to_owned(),
                detail: Some(name.fqn.clone()),
                insert_text: if is_namespace {
                    format!("{label}\\")
                } else {
                    label.to_owned()
                },
                is_snippet: false,
                kind: if is_namespace {
                    ItemKind::Keyword
                } else {
                    ItemKind::Class
                },
                sort: rank(prefix, label, if name.vendor { '1' } else { '0' }),
                extra: Vec::new(),
                replace_from: None,
            });
            if out.len() > MAX_ITEMS * 4 {
                break;
            }
        }
        out.sort_by(|a, b| a.sort.cmp(&b.sort));
        out.truncate(MAX_ITEMS);
        out
    }
}
