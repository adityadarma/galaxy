//! Working out the type of an expression: variables, calls, members,
//! generics and return types read from bodies.

use super::*;

impl PhpEngine {
    // ---- type evaluation ---------------------------------------------

    pub(in crate::php) fn class_ref(&mut self, ctx: &Ctx<'_>, name: &str) -> Option<String> {
        match name.to_ascii_lowercase().as_str() {
            "self" | "static" | "$this" => ctx.class.as_ref().map(|c| c.fqn.clone()),
            "parent" => ctx.class.as_ref()?.extends.first().cloned(),
            _ => Some(resolve_name(name, &ctx.file.namespace, &ctx.file.uses)),
        }
    }

    pub(super) fn class_from_class_const(&mut self, ctx: &Ctx<'_>, arg: &str) -> Option<String> {
        let name = arg.trim().strip_suffix("::class")?;
        if name.is_empty() || !name.bytes().all(is_name_char) {
            return None;
        }
        self.class_ref(ctx, name)
    }

    pub(super) fn function_return(
        &mut self,
        ctx: &Ctx<'_>,
        name: &str,
        args: &str,
    ) -> Option<String> {
        let mut candidates = Vec::new();
        if !name.starts_with('\\') && !ctx.file.namespace.is_empty() && !name.contains('\\') {
            candidates.push(format!("{}\\{}", ctx.file.namespace, name));
        }
        candidates.push(name.trim_start_matches('\\').to_owned());
        let function = candidates.iter().find_map(|fqn| self.function(fqn))?;
        let parts = split_args(args);
        if let Some(index) = function.class_arg {
            return self.class_from_class_const(ctx, parts.get(index)?);
        }
        let mut env = HashMap::new();
        for (template, index) in &function.class_params {
            if let Some(class) = parts
                .get(*index)
                .and_then(|a| self.class_from_class_const(ctx, a))
            {
                env.insert(template.clone(), class);
            }
        }
        let out = generics::subst(function.ty.as_deref()?, &env, "");
        (!out.is_empty()).then_some(out)
    }

    /// The class and what it inherits, each with the types its template
    /// parameters stand for when reached from `ty` (`Coll<int,User>`).
    pub(super) fn lineage_env(
        &mut self,
        ty: &str,
    ) -> Vec<(Arc<ClassInfo>, HashMap<String, String>)> {
        let (base, args) = generics::split(ty);
        let Some(class) = self.class(base) else {
            return Vec::new();
        };
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let env = generics::bind(&class.templates, args);
        let mut seen = HashSet::from([class.fqn.to_lowercase()]);
        let mut out = vec![(class, env)];
        let mut index = 0;
        while index < out.len() && out.len() < 48 {
            let (current, env) = out[index].clone();
            index += 1;
            let parents: Vec<String> = current
                .traits
                .iter()
                .chain(&current.extends)
                .chain(&current.implements)
                .chain(&current.mixins)
                .cloned()
                .collect();
            for parent in parents {
                if !seen.insert(parent.to_lowercase()) {
                    continue;
                }
                let Some(found) = self.class(&parent) else {
                    continue;
                };
                let args: Vec<String> = current
                    .generic_parents
                    .iter()
                    .find(|g| generics::base(g).eq_ignore_ascii_case(&parent))
                    .map(|g| {
                        generics::split(g)
                            .1
                            .iter()
                            .map(|arg| generics::subst(arg, &env, ""))
                            .collect()
                    })
                    .unwrap_or_default();
                let env = generics::bind(&found.templates, args);
                out.push((found, env));
            }
        }
        out
    }

    /// The nearest declaration of `name`, preferring one that says what it
    /// returns over an untyped override (`@inheritDoc`).
    pub(super) fn pick_member(
        chain: &[(Arc<ClassInfo>, HashMap<String, String>)],
        name: &str,
        call: bool,
    ) -> Option<(usize, Member)> {
        let name = name.trim_start_matches('$');
        let mut first = None;
        for (index, (owner, _)) in chain.iter().enumerate() {
            let hit = owner.members.iter().find(|m| {
                if call {
                    m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case(name)
                } else {
                    m.kind != MemberKind::Method && m.name == name
                }
            });
            if let Some(member) = hit {
                if member.ty.is_some() || member.class_arg.is_some() {
                    return Some((index, member.clone()));
                }
                first.get_or_insert((index, member.clone()));
            }
        }
        first
    }

    /// The type of `receiver->name` (a read) or `receiver->name(args)`,
    /// with template parameters replaced by what the receiver binds them to.
    pub(super) fn member_type(
        &mut self,
        ctx: &Ctx<'_>,
        receiver: &str,
        name: &str,
        args: Option<&str>,
    ) -> Option<String> {
        let chain = self.lineage_env(receiver);
        if chain.is_empty() {
            return None;
        }
        let call = args.is_some();
        let Some((index, member)) = Self::pick_member(&chain, name, call) else {
            if call {
                // `$model->where()` and `$relation->create()` reach the
                // query builder through `__call`.
                let frameworks = self.frameworks.clone();
                let builder = frameworks
                    .iter()
                    .find_map(|fw| fw.forwarding_builder(self, &chain))?;
                return self.member_type(ctx, &builder, name, args);
            }
            // A read of something that is not a property, on a class with
            // `__get`: Eloquent exposes a relation method that way, and
            // what the read gives is what the relation resolves to.
            let has_getter = chain.iter().any(|(owner, _)| {
                owner
                    .members
                    .iter()
                    .any(|m| m.kind == MemberKind::Method && m.name.eq_ignore_ascii_case("__get"))
            });
            if !has_getter {
                return None;
            }
            let relation = self.member_type(ctx, receiver, name, Some(""))?;
            return self.member_type(ctx, &relation, "getResults", Some(""));
        };
        let parts = args.map(split_args).unwrap_or_default();
        if let Some(index) = member.class_arg {
            return self.class_from_class_const(ctx, parts.get(index)?);
        }
        let (owner, owner_env) = &chain[index];
        let mut env = owner_env.clone();
        for (template, position) in &member.class_params {
            if let Some(class) = parts
                .get(*position)
                .and_then(|arg| self.class_from_class_const(ctx, arg))
            {
                env.insert(template.clone(), class);
            }
        }
        let mut declared = member.ty.clone();
        // `: HasOne` says little; the body (`$this->hasOne(X::class)`) says which.
        if call && let Some(body) = &member.body_return {
            let generic_class = declared
                .as_deref()
                .and_then(|ty| self.class(generics::base(ty)))
                .is_some_and(|class| !class.templates.is_empty());
            let bare = declared.as_deref().is_none_or(|ty| !ty.contains('<'));
            if bare
                && (declared.is_none() || generic_class)
                && let Some(inferred) = self.infer_body(&chain[0].0, owner, body)
                && declared.as_deref().is_none_or(|ty| {
                    generics::base(&inferred).eq_ignore_ascii_case(generics::base(ty))
                })
            {
                return Some(inferred);
            }
        }
        let out = generics::subst(&declared.take()?, &env, receiver);
        (!out.is_empty()).then_some(out)
    }

    /// Evaluates the `return` expression of a method as written in its own
    /// file, with `$this` standing for `this`.
    pub(super) fn infer_body(
        &mut self,
        this: &Arc<ClassInfo>,
        owner: &Arc<ClassInfo>,
        expr: &str,
    ) -> Option<String> {
        if self.infer_depth >= 3 {
            return None;
        }
        let file = self.load(&owner.path)?;
        let ctx = Ctx {
            src: "",
            tree: empty_tree(),
            file: &file,
            byte: 0,
            class: Some(this.clone()),
        };
        self.infer_depth += 1;
        let out = self.eval(&ctx, expr, 0);
        self.infer_depth -= 1;
        out
    }

    /// Key or value type when iterating `ty` with `foreach`.
    pub(super) fn element_type(&mut self, ty: &str, key: bool) -> Option<String> {
        let chain = self.lineage_env(ty);
        let (_, env) = chain.iter().find(|(class, _)| {
            matches!(
                class.fqn.to_ascii_lowercase().as_str(),
                "traversable" | "iterator" | "iteratoraggregate"
            )
        })?;
        let found = env.get(if key { "TKey" } else { "TValue" })?;
        (!found.is_empty()).then(|| found.clone())
    }

    /// The class an expression evaluates to, when it can be told.
    pub(in crate::php) fn eval(&mut self, ctx: &Ctx<'_>, expr: &str, depth: u8) -> Option<String> {
        if depth > MAX_DEPTH {
            return None;
        }
        let s = expr.trim();
        let bytes = s.as_bytes();
        let first = *bytes.first()?;
        let mut pos;
        let mut current: Option<String>;
        if first == b'(' {
            let end = skip_balanced(s, 0)?;
            current = self.eval(ctx, &s[1..end - 1], depth + 1);
            pos = end;
        } else if let Some(rest) = strip_new(s) {
            let start = s.len() - rest.len();
            let name_end = start + read_name(rest);
            current = self.class_ref(ctx, &s[start..name_end]);
            pos = name_end;
            if s[pos..].starts_with('(') {
                pos = skip_balanced(s, pos)?;
            }
        } else if first == b'$' {
            let end = 1 + ident_len(&s[1..]);
            let name = &s[1..end];
            current = if name == "this" {
                ctx.class.as_ref().map(|c| c.fqn.clone())
            } else {
                self.variable_type(ctx, name, depth)
            };
            pos = end;
        } else {
            let end = read_name(s);
            if end == 0 {
                return None;
            }
            let name = &s[..end];
            if s[end..].starts_with('(') {
                let close = skip_balanced(s, end)?;
                current = self.function_return(ctx, name, &s[end + 1..close - 1]);
                pos = close;
            } else {
                current = self.class_ref(ctx, name);
                pos = end;
            }
        }
        loop {
            let rest = s[pos..].trim_start();
            pos = s.len() - rest.len();
            let op_len = if rest.starts_with("?->") {
                3
            } else if rest.starts_with("->") || rest.starts_with("::") {
                2
            } else {
                break;
            };
            pos += op_len;
            pos = s.len() - s[pos..].trim_start().len();
            let start = pos;
            if s[pos..].starts_with('$') {
                pos += 1;
            }
            pos += ident_len(&s[pos..]);
            let name = &s[start..pos];
            if name.is_empty() {
                return None;
            }
            let mut args = None;
            if s[pos..].starts_with('(') {
                let close = skip_balanced(s, pos)?;
                args = Some(&s[pos + 1..close - 1]);
                pos = close;
            }
            let class = current.take()?;
            current = self.member_type(ctx, &class, name, args);
        }
        if !s[pos..].trim().is_empty() {
            return None;
        }
        current
    }

    pub(super) fn variable_type(&mut self, ctx: &Ctx<'_>, name: &str, depth: u8) -> Option<String> {
        let var = format!("${name}");
        let mut node = ctx
            .tree
            .root_node()
            .descendant_for_byte_range(ctx.byte, ctx.byte)?;
        let mut limit = ctx.byte;
        loop {
            while !is_scope(node.kind()) {
                node = node.parent()?;
            }
            let scope = node;
            match self.scope_variable(ctx, scope, &var, limit, depth) {
                Lookup::Found(ty) => return ty,
                Lookup::Missing => {}
            }
            let inherits = match scope.kind() {
                "arrow_function" => true,
                "anonymous_function" => uses_variable(scope, &var, ctx.src),
                _ => false,
            };
            if !inherits {
                return None;
            }
            limit = scope.start_byte();
            node = scope.parent()?;
        }
    }

    pub(super) fn scope_variable(
        &mut self,
        ctx: &Ctx<'_>,
        scope: Node<'_>,
        var: &str,
        limit: usize,
        depth: u8,
    ) -> Lookup {
        enum Source {
            Resolved(Option<String>),
            Expr(String),
            /// Key or value of `foreach (<expr> as ...)`.
            Element {
                expr: String,
                key: bool,
            },
        }
        let self_fqn = ctx.class.as_ref().map_or("", |c| c.fqn.as_str());
        let mut candidates: Vec<(usize, Source)> = Vec::new();
        let mut offer = |pos: usize, source: Source| candidates.push((pos, source));

        if let Some(params) = scope.child_by_field_name("parameters") {
            let mut cursor = params.walk();
            for param in params.named_children(&mut cursor) {
                let Some(name) = param.child_by_field_name("name") else {
                    continue;
                };
                if &ctx.src[name.byte_range()] != var {
                    continue;
                }
                let declared = param
                    .child_by_field_name("type")
                    .and_then(|t| {
                        resolve_type(
                            &ctx.src[t.byte_range()],
                            &ctx.file.namespace,
                            &ctx.file.uses,
                            self_fqn,
                        )
                    })
                    .or_else(|| {
                        let doc = preceding_comment(scope, ctx.src)?;
                        let re = param_doc_regex(var);
                        let caps = re.captures(doc)?;
                        resolve_type(&caps[1], &ctx.file.namespace, &ctx.file.uses, self_fqn)
                    })
                    .or_else(|| self.builder_callback_parameter_type(ctx, scope, var, depth));
                offer(scope.start_byte(), Source::Resolved(declared));
            }
        }

        let mut stack = vec![scope];
        while let Some(node) = stack.pop() {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.id() != scope.id() && is_scope(child.kind()) {
                    continue;
                }
                match child.kind() {
                    "assignment_expression" => {
                        if let (Some(left), Some(right)) = (
                            child.child_by_field_name("left"),
                            child.child_by_field_name("right"),
                        ) && left.kind() == "variable_name"
                            && &ctx.src[left.byte_range()] == var
                            && child.end_byte() <= limit
                        {
                            offer(
                                child.start_byte(),
                                Source::Expr(ctx.src[right.byte_range()].to_owned()),
                            );
                        }
                    }
                    "foreach_statement" if child.start_byte() <= limit => {
                        let mut inner = child.walk();
                        let kids: Vec<Node<'_>> = child
                            .named_children(&mut inner)
                            .filter(|n| {
                                Some(n.id()) != child.child_by_field_name("body").map(|b| b.id())
                            })
                            .collect();
                        if let [iterated, target, ..] = kids.as_slice() {
                            let (key, value) = if target.kind() == "pair" {
                                let mut pair = target.walk();
                                let parts: Vec<Node<'_>> =
                                    target.named_children(&mut pair).collect();
                                (parts.first().copied(), parts.get(1).copied())
                            } else {
                                (None, Some(*target))
                            };
                            let is_var = |node: Option<Node<'_>>| {
                                node.is_some_and(|n| {
                                    let n = if n.kind() == "by_ref" {
                                        n.named_child(0).unwrap_or(n)
                                    } else {
                                        n
                                    };
                                    n.kind() == "variable_name" && &ctx.src[n.byte_range()] == var
                                })
                            };
                            let expr = ctx.src[iterated.byte_range()].to_owned();
                            if is_var(value) {
                                offer(
                                    child.start_byte(),
                                    Source::Element {
                                        expr: expr.clone(),
                                        key: false,
                                    },
                                );
                            }
                            if is_var(key) {
                                offer(child.start_byte(), Source::Element { expr, key: true });
                            }
                        }
                    }
                    "catch_clause" => {
                        if let (Some(name), Some(ty)) = (
                            child.child_by_field_name("name"),
                            child.child_by_field_name("type"),
                        ) && &ctx.src[name.byte_range()] == var
                            && child.start_byte() <= limit
                        {
                            let resolved = resolve_type(
                                &ctx.src[ty.byte_range()],
                                &ctx.file.namespace,
                                &ctx.file.uses,
                                self_fqn,
                            );
                            offer(child.start_byte(), Source::Resolved(resolved));
                        }
                    }
                    _ => {}
                }
                stack.push(child);
            }
        }

        // `/** @var Foo $x */` and `$x instanceof Foo` before the cursor.
        let end = limit.min(ctx.src.len());
        let start = scope.start_byte().min(end);
        let window = &ctx.src[start..end];
        let escaped = regex::escape(var);
        if let Ok(re) = Regex::new(&format!(r"@var\s+(\S+)\s+{escaped}\b")) {
            for caps in re.captures_iter(window) {
                let at = start + caps.get(0).map_or(0, |m| m.start());
                let ty = resolve_type(&caps[1], &ctx.file.namespace, &ctx.file.uses, self_fqn);
                offer(at, Source::Resolved(ty));
            }
        }
        if let Ok(re) = Regex::new(&format!(r"{escaped}\s+instanceof\s+([\w\\]+)")) {
            for caps in re.captures_iter(window) {
                let at = start + caps.get(0).map_or(0, |m| m.start());
                let ty = self.class_ref(ctx, &caps[1]);
                offer(at, Source::Resolved(ty));
            }
        }

        if candidates.is_empty() {
            return Lookup::Missing;
        }
        // Latest first; one whose type cannot be told (a call to something
        // unknown) defers to an earlier hint such as `@var` or a parameter.
        candidates.sort_by(|a, b| b.0.cmp(&a.0));
        for (pos, source) in candidates {
            let found = match source {
                Source::Resolved(ty) => ty,
                Source::Expr(expr) => {
                    let inner = Ctx {
                        byte: pos,
                        ..ctx.clone()
                    };
                    self.eval(&inner, &expr, depth + 1)
                }
                Source::Element { expr, key } => {
                    let inner = Ctx {
                        byte: pos,
                        ..ctx.clone()
                    };
                    let iterated = self.eval(&inner, &expr, depth + 1);
                    iterated.and_then(|ty| self.element_type(&ty, key))
                }
            };
            if found.is_some() {
                return Lookup::Found(found);
            }
        }
        Lookup::Found(None)
    }

    /// The type of the sole closure parameter `var` when a framework knows
    /// what the called method passes to its callback. The engine only finds
    /// the call and the receiver type; frameworks decide the answer.
    fn builder_callback_parameter_type(
        &mut self,
        ctx: &Ctx<'_>,
        scope: Node<'_>,
        var: &str,
        depth: u8,
    ) -> Option<String> {
        let parameters = scope.child_by_field_name("parameters")?;
        let mut cursor = parameters.walk();
        let first = parameters
            .named_children(&mut cursor)
            .find_map(|parameter| parameter.child_by_field_name("name"))?;
        if &ctx.src[first.byte_range()] != var {
            return None;
        }
        let call = std::iter::successors(scope.parent(), |node| node.parent())
            .find(|node| node.kind() == "member_call_expression")?;
        let method = &ctx.src[call.child_by_field_name("name")?.byte_range()];
        let receiver = call.child_by_field_name("object")?;
        let ty = self.eval(ctx, &ctx.src[receiver.byte_range()], depth + 1)?;
        let chain = self.lineage_env(&ty);
        let frameworks = self.frameworks.clone();
        frameworks
            .iter()
            .find_map(|fw| fw.callback_parameter_type(self, method, &ty, &chain))
    }
}
