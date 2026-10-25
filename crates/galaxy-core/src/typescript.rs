//! Per-file TypeScript and JavaScript syntax indexing.
//!
//! This is deliberately syntactic: it does not resolve modules, infer types, or
//! attempt reference finding. All locations use LSP-compatible UTF-16 columns.

use std::path::{Path, PathBuf};

use tree_sitter::{Language, Node, Parser};

use crate::position::Utf16Position;

/// The grammar selected for a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentLanguage {
    TypeScript,
    Tsx,
    JavaScript,
}

/// A half-open source range in UTF-16 line and column coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Utf16Range {
    pub start: Utf16Position,
    pub end: Utf16Position,
}

/// A top-level declaration kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    Function,
    Class,
    Interface,
    Type,
    Enum,
    Namespace,
    Variable,
    Parameter,
}

/// All exact same-file occurrences of one lexically resolved symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOccurrences {
    pub name: String,
    pub kind: DeclarationKind,
    pub declaration: Utf16Range,
    pub occurrences: Vec<Utf16Range>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedDeclaration {
    name: String,
    kind: DeclarationKind,
    range: Utf16Range,
    scope: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    parent: Option<usize>,
    range: Utf16Range,
}

/// A named top-level declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub kind: DeclarationKind,
    pub range: Utf16Range,
    pub exported: bool,
    pub default: bool,
}

/// The form of an import binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportKind {
    Default,
    Named,
    Namespace,
}

/// One local name introduced by an import statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBinding {
    pub local_name: String,
    pub imported_name: Option<String>,
    pub kind: ImportKind,
    pub range: Utf16Range,
    pub type_only: bool,
}

/// One static ES module import statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub specifier: String,
    /// The exact string literal range, including quotes.
    pub specifier_range: Utf16Range,
    pub range: Utf16Range,
    pub bindings: Vec<ImportBinding>,
    pub type_only: bool,
    syntax_valid: bool,
}

/// One exported name, including names re-exported from another module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// The local name, when this export has one.
    pub name: Option<String>,
    /// The exact local-name range for a named export specifier.
    pub name_range: Option<Utf16Range>,
    /// The public name. `None` represents `export *`.
    pub exported_name: Option<String>,
    /// The exact public-name range for a named export specifier.
    pub exported_name_range: Option<Utf16Range>,
    pub specifier: Option<String>,
    /// The exact re-export string literal range, including quotes.
    pub specifier_range: Option<Utf16Range>,
    pub range: Utf16Range,
    pub default: bool,
    pub type_only: bool,
    syntax_valid: bool,
}

/// A direct member use of a namespace import, such as `api.value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceMemberUse {
    pub namespace_range: Utf16Range,
    pub namespace_name: String,
    pub member_name: String,
    pub member_range: Utf16Range,
}

/// A direct property or method access with an explicitly named receiver type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeMemberUse {
    pub target: TypeMemberTarget,
    pub member_name: String,
    pub member_range: Utf16Range,
}

/// The declaration target of a direct type member use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeMemberTarget {
    Named(Utf16Range),
    Imported(Utf16Range),
    /// A direct member of `this` in a class extending this value import.
    SuperImported(Utf16Range),
    /// A member reached through a typed property of an imported superclass.
    SuperMember {
        binding: Utf16Range,
        member_name: String,
    },
    /// A callback parameter passed to a method on a typed imported superclass property.
    SuperMemberCallback {
        binding: Utf16Range,
        member_name: String,
        callback_name: String,
        chain: Vec<String>,
    },
    Inline(Vec<TypeMember>),
    Union(Vec<TypeMemberTarget>),
}

/// A property or method declared directly by an interface, object type, or class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeMember {
    pub name: String,
    pub range: Utf16Range,
    /// The imported namespace and type named by a direct property or getter.
    pub type_reference: Option<(String, String)>,
    /// The named type of the sole callback parameter, when unambiguous.
    pub callback_parameter_type: Option<String>,
    /// The unqualified named return type of a method, when direct and unique.
    pub return_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TypeDeclaration {
    name: String,
    range: Utf16Range,
    members: Vec<TypeMember>,
    kind: DeclarationKind,
    extends: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QualifiedTypeImport {
    namespace_range: Utf16Range,
    member_range: Utf16Range,
    binding_range: Utf16Range,
    member_name: String,
}

/// A recoverable syntax error reported by Tree-sitter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    pub message: String,
    pub range: Utf16Range,
}

/// A conservative, same-file TypeScript semantic diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDiagnostic {
    pub code: String,
    pub message: String,
    pub range: Utf16Range,
}

/// Static syntax facts for one source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentIndex {
    pub path: PathBuf,
    pub language: DocumentLanguage,
    pub declarations: Vec<Declaration>,
    pub imports: Vec<Import>,
    /// Literal dynamic `import()` module specifiers.
    pub dynamic_imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub diagnostics: Vec<ParseDiagnostic>,
    /// Diagnostics produced by the deliberately small TypeScript type subset.
    pub type_diagnostics: Vec<TypeDiagnostic>,
    export_assignment: bool,
    namespace_members: Vec<NamespaceMemberUse>,
    symbols: Vec<SymbolOccurrences>,
    import_symbols: Vec<SymbolOccurrences>,
    type_import_references: Vec<(Utf16Range, Utf16Range)>,
    qualified_type_imports: Vec<QualifiedTypeImport>,
    type_member_uses: Vec<TypeMemberUse>,
    type_declarations: Vec<TypeDeclaration>,
}

impl DocumentIndex {
    /// Parses `text` with the grammar chosen from `path` and indexes top-level syntax.
    pub fn parse(path: impl AsRef<Path>, text: &str) -> Self {
        let path = path.as_ref().to_path_buf();
        let language = language_for_path(&path);
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_language(language))
            .expect("bundled TypeScript grammar is compatible with Tree-sitter");
        let tree = parser
            .parse(text, None)
            .expect("Tree-sitter parser returns a tree for string input");
        let locations = Locations::new(text);
        let root = tree.root_node();
        let mut declarations = Vec::new();
        let mut imports = Vec::new();
        let mut dynamic_imports = Vec::new();
        let mut exports = Vec::new();
        let mut namespace_members = Vec::new();
        let mut export_assignment = false;

        let mut cursor = root.walk();
        for node in root.named_children(&mut cursor) {
            match node.kind() {
                "import_statement" => imports.push(import_from_node(text, node, &locations)),
                "export_statement" => {
                    export_assignment |= is_export_assignment(text, node);
                    index_export(text, node, &locations, &mut declarations, &mut exports);
                }
                _ => declarations
                    .extend(declarations_from_node(node, false, false, text, &locations)),
            }
        }
        collect_dynamic_imports(text, root, &locations, &mut dynamic_imports);
        collect_namespace_members(text, root, &locations, &mut namespace_members);

        let mut type_declarations = Vec::new();
        collect_type_navigation_declarations(root, text, &locations, &mut type_declarations);
        let symbols = resolve_symbols(text, root, &locations, false);
        let import_symbols = resolve_symbols(text, root, &locations, true);
        let type_import_references =
            collect_type_import_references(root, text, &locations, &imports, &symbols);
        let qualified_type_imports =
            collect_qualified_type_imports(root, text, &locations, &imports);
        let type_member_uses = collect_type_member_uses(
            root,
            text,
            &locations,
            &imports,
            &type_declarations,
            &symbols,
            &import_symbols,
        );
        let inherited_member_uses = collect_inherited_member_uses(
            root,
            text,
            &locations,
            &imports,
            &symbols,
            &import_symbols,
        );
        let mut type_member_uses = type_member_uses
            .into_iter()
            .filter(|use_| {
                !inherited_member_uses
                    .iter()
                    .any(|inherited| inherited.member_range == use_.member_range)
            })
            .collect::<Vec<_>>();
        type_member_uses.extend(inherited_member_uses);

        let type_diagnostics = type_diagnostics(text, root, &locations);
        Self {
            path,
            language,
            declarations,
            imports,
            dynamic_imports,
            exports,
            diagnostics: diagnostics(root, &locations),
            type_diagnostics,
            export_assignment,
            namespace_members,
            symbols,
            import_symbols,
            type_import_references,
            qualified_type_imports,
            type_member_uses,
            type_declarations,
        }
    }

    /// Returns a literal dynamic import whose module string contains the cursor.
    pub fn dynamic_import_at(&self, line: usize, col: usize) -> Option<&Import> {
        let position = Utf16Position::new(line, col);
        self.dynamic_imports
            .iter()
            .find(|import| contains(import.range, position))
    }

    /// Resolves the exact identifier at an UTF-16 position within this file.
    ///
    /// Ambiguous syntax, properties, labels, and import/export specifiers are excluded.
    pub fn symbol_at(&self, line: usize, col: usize) -> Option<SymbolOccurrences> {
        let position = Utf16Position::new(line, col);
        self.symbols
            .iter()
            .find(|symbol| {
                symbol
                    .occurrences
                    .iter()
                    .any(|range| contains(*range, position))
            })
            .cloned()
    }

    /// Lexically resolved local symbols, excluding imports.
    pub fn symbols(&self) -> &[SymbolOccurrences] {
        &self.symbols
    }

    /// Returns a direct named or default import binding at the cursor.
    pub fn import_binding_at(&self, line: usize, col: usize) -> Option<(&Import, &ImportBinding)> {
        let position = Utf16Position::new(line, col);
        self.imports.iter().find_map(|import| {
            import
                .bindings
                .iter()
                .find(|binding| contains(binding.range, position))
                .map(|binding| (import, binding))
        })
    }

    /// Returns a type-only named/default import whose lexical reference is at
    /// the cursor, including a use in a property or parameter annotation.
    pub fn type_import_binding_at(
        &self,
        line: usize,
        col: usize,
    ) -> Option<(&Import, &ImportBinding)> {
        let position = Utf16Position::new(line, col);
        let import_from_range = |binding_range| {
            self.imports.iter().find_map(|import| {
                import
                    .bindings
                    .iter()
                    .find(|binding| {
                        binding.range == binding_range
                            && binding.type_only
                            && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
                    })
                    .map(|binding| (import, binding))
            })
        };
        self.import_symbols
            .iter()
            .find(|symbol| {
                symbol
                    .occurrences
                    .iter()
                    .any(|range| contains(*range, position))
            })
            .and_then(|symbol| import_from_range(symbol.declaration))
            .or_else(|| {
                self.type_import_references
                    .iter()
                    .find(|(range, _)| contains(*range, position))
                    .and_then(|(_, binding_range)| import_from_range(*binding_range))
            })
            .or_else(|| {
                self.qualified_type_imports
                    .iter()
                    .find(|reference| contains(reference.namespace_range, position))
                    .and_then(|reference| import_from_range(reference.binding_range))
            })
    }

    /// Returns whether parsing recovered this import without a syntax error in its range.
    pub fn import_is_syntax_valid(&self, import: &Import) -> bool {
        import.syntax_valid
    }

    /// Returns occurrences only when `binding` is the lexically resolved import.
    pub fn import_binding_occurrences(&self, binding: &ImportBinding) -> Option<SymbolOccurrences> {
        self.import_symbols
            .iter()
            .find(|symbol| symbol.name == binding.local_name && symbol.declaration == binding.range)
            .cloned()
    }

    /// Returns a direct `namespace.member` use at the cursor and its namespace binding.
    pub fn namespace_member_at(
        &self,
        line: usize,
        col: usize,
    ) -> Option<(&Import, &ImportBinding, &NamespaceMemberUse)> {
        let position = Utf16Position::new(line, col);
        let member = self
            .namespace_members
            .iter()
            .find(|member| contains(member.member_range, position))?;
        self.imports.iter().find_map(|import| {
            import.bindings.iter().find_map(|binding| {
                (binding.kind == ImportKind::Namespace
                    && binding.local_name == member.namespace_name)
                    .then_some((import, binding, member))
            })
        })
    }

    /// Returns a static module string literal at the cursor.
    pub fn static_module_at(&self, line: usize, col: usize) -> Option<&str> {
        let position = Utf16Position::new(line, col);
        self.imports
            .iter()
            .find(|import| contains(import.specifier_range, position))
            .map(|import| import.specifier.as_str())
            .or_else(|| {
                self.exports.iter().find_map(|export| {
                    export
                        .specifier_range
                        .filter(|range| contains(*range, position))
                        .and(export.specifier.as_deref())
                })
            })
    }

    /// Returns a valid named re-export whose local or public identifier contains the cursor.
    /// The local name is always the name to resolve in the target module.
    pub fn named_reexport_at(&self, line: usize, col: usize) -> Option<&Export> {
        let position = Utf16Position::new(line, col);
        self.exports.iter().find(|export| {
            export.syntax_valid
                && export.specifier.is_some()
                && export.name.is_some()
                && export.exported_name.is_some()
                && (export
                    .name_range
                    .is_some_and(|range| contains(range, position))
                    || export
                        .exported_name_range
                        .is_some_and(|range| contains(range, position)))
        })
    }

    /// Resolves a direct local export, including `export { local as public }`.
    pub fn exported_declaration(&self, exported_name: &str) -> Option<&Declaration> {
        if !self.diagnostics.is_empty() {
            return None;
        }
        let exports = self
            .exports
            .iter()
            .filter(|item| {
                item.exported_name.as_deref() == Some(exported_name)
                    && item.specifier.is_none()
                    && !item.type_only
            })
            .collect::<Vec<_>>();
        if exports.len() != 1 {
            return None;
        }
        let name = exports[0].name.as_deref()?;
        let declarations = self
            .declarations
            .iter()
            .filter(|item| {
                item.name == name
                    && !matches!(
                        item.kind,
                        DeclarationKind::Interface | DeclarationKind::Type
                    )
            })
            .collect::<Vec<_>>();
        (declarations.len() == 1).then(|| declarations[0])
    }

    /// Returns one exported interface or object type, following a local export alias.
    pub fn exported_type_declaration(&self, exported_name: &str) -> Option<&Declaration> {
        let export = self
            .exports
            .iter()
            .filter(|item| {
                item.exported_name.as_deref() == Some(exported_name) && item.specifier.is_none()
            })
            .collect::<Vec<_>>();
        if export.len() != 1 {
            return None;
        }
        let name = export[0].name.as_deref()?;
        let declarations = self
            .declarations
            .iter()
            .filter(|item| {
                item.name == name
                    && matches!(
                        item.kind,
                        DeclarationKind::Interface | DeclarationKind::Type
                    )
            })
            .collect::<Vec<_>>();
        if declarations.len() == 1 {
            Some(declarations[0])
        } else {
            None
        }
    }

    /// Resolves the ambient namespace exposed by a CommonJS `export =` module.
    /// This covers declaration-merging packages such as `knex`, whose public
    /// `Knex` type is not represented by a named `export` statement.
    pub fn export_assignment_namespace(&self, name: &str) -> Option<&Declaration> {
        if !self.diagnostics.is_empty() || !self.export_assignment {
            return None;
        }
        let declarations = self
            .declarations
            .iter()
            .filter(|item| item.name == name && item.kind == DeclarationKind::Namespace)
            .collect::<Vec<_>>();
        (declarations.len() == 1).then(|| declarations[0])
    }

    /// Resolves the imported namespace and member in `Namespace.Type`.
    pub fn qualified_type_import_at(
        &self,
        line: usize,
        col: usize,
    ) -> Option<(&Import, &ImportBinding, &str)> {
        let position = Utf16Position::new(line, col);
        let reference = self
            .qualified_type_imports
            .iter()
            .find(|reference| contains(reference.member_range, position))?;
        let (import, binding) = self.import_binding_by_range(reference.binding_range)?;
        Some((import, binding, &reference.member_name))
    }

    /// Returns one uniquely named type declaration in this declaration file.
    pub fn unique_type_declaration_range(&self, name: &str) -> Option<Utf16Range> {
        let declarations = self
            .type_declarations
            .iter()
            .filter(|declaration| declaration.name == name)
            .collect::<Vec<_>>();
        if declarations.len() == 1 {
            return Some(declarations[0].range);
        }
        // TypeScript declaration merging commonly pairs an exported class with
        // one interface of the same name. A type reference should land on the
        // interface contract, but unrelated duplicate interfaces stay ambiguous.
        let interfaces = declarations
            .iter()
            .filter(|declaration| declaration.kind == DeclarationKind::Interface)
            .collect::<Vec<_>>();
        (interfaces.len() == 1).then(|| interfaces[0].range)
    }

    /// Returns a direct member access backed by a simple, explicit annotation.
    pub fn type_member_at(&self, line: usize, col: usize) -> Option<&TypeMemberUse> {
        let position = Utf16Position::new(line, col);
        self.type_member_uses
            .iter()
            .find(|item| contains(item.member_range, position))
    }

    /// Returns a local interface, object type alias, or class with direct members.
    pub fn type_declaration(&self, name: &str) -> Option<&[TypeMember]> {
        let declarations = self
            .type_declarations
            .iter()
            .filter(|item| item.name == name)
            .collect::<Vec<_>>();
        if declarations.len() == 1 {
            Some(declarations[0].members.as_slice())
        } else {
            None
        }
    }

    pub fn type_declaration_at(&self, range: Utf16Range) -> Option<&[TypeMember]> {
        self.type_declarations
            .iter()
            .find(|item| item.range == range)
            .map(|item| item.members.as_slice())
    }

    pub fn import_binding_by_range(&self, range: Utf16Range) -> Option<(&Import, &ImportBinding)> {
        self.imports.iter().find_map(|import| {
            import
                .bindings
                .iter()
                .find(|binding| binding.range == range)
                .map(|binding| (import, binding))
        })
    }

    pub fn type_member(&self, type_name: &str, member_name: &str) -> Option<&TypeMember> {
        self.type_member_inner(type_name, member_name, &mut Vec::new())
    }

    fn type_member_inner(
        &self,
        type_name: &str,
        member_name: &str,
        visited: &mut Vec<String>,
    ) -> Option<&TypeMember> {
        if visited.iter().any(|name| name == type_name) {
            return None;
        }
        visited.push(type_name.to_owned());
        let declarations = self
            .type_declarations
            .iter()
            .filter(|item| item.name == type_name)
            .collect::<Vec<_>>();
        let declaration = match declarations.as_slice() {
            [declaration] => *declaration,
            _ => {
                let interfaces = declarations
                    .iter()
                    .filter(|declaration| declaration.kind == DeclarationKind::Interface)
                    .collect::<Vec<_>>();
                match interfaces.as_slice() {
                    [interface] => *interface,
                    _ => return None,
                }
            }
        };
        let matches = declaration
            .members
            .iter()
            .filter(|member| member.name == member_name)
            .collect::<Vec<_>>();
        if let [member] = matches.as_slice() {
            return Some(member);
        }
        let inherited = declaration
            .extends
            .iter()
            .filter_map(|parent| self.type_member_inner(parent, member_name, visited))
            .collect::<Vec<_>>();
        (inherited.len() == 1).then(|| inherited[0])
    }

    /// Resolves a member against the direct type target recorded for an access.
    pub fn type_member_target(&self, use_: &TypeMemberUse) -> Option<TypeMember> {
        match &use_.target {
            TypeMemberTarget::Named(range) => self
                .type_declaration_at(*range)?
                .iter()
                .filter(|member| member.name == use_.member_name)
                .collect::<Vec<_>>()
                .as_slice()
                .try_into()
                .ok()
                .map(|[member]: [&TypeMember; 1]| member.clone()),
            TypeMemberTarget::Inline(members) => {
                let matches = members
                    .iter()
                    .filter(|member| member.name == use_.member_name)
                    .collect::<Vec<_>>();
                (matches.len() == 1).then_some(matches[0].clone())
            }
            TypeMemberTarget::Imported(_)
            | TypeMemberTarget::SuperImported(_)
            | TypeMemberTarget::SuperMember { .. }
            | TypeMemberTarget::SuperMemberCallback { .. }
            | TypeMemberTarget::Union(_) => None,
        }
    }
}

fn contains(range: Utf16Range, position: Utf16Position) -> bool {
    range.start <= position && position < range.end
}

/// Type identifiers in declaration fields are not consistently included in
/// Tree-sitter's lexical reference set. Index only an unshadowed, unique
/// `import type` binding so Definition remains conservative.
fn collect_type_import_references(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    symbols: &[SymbolOccurrences],
) -> Vec<(Utf16Range, Utf16Range)> {
    let mut references = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "type_identifier"
            && node
                .parent()
                .is_none_or(|parent| parent.kind() != "import_statement")
            && let Some(name) = node_text(text, node)
            && !symbols.iter().any(|symbol| symbol.name == name)
        {
            let bindings = imports
                .iter()
                .flat_map(|import| import.bindings.iter())
                .filter(|binding| {
                    binding.type_only
                        && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
                        && binding.local_name == name
                })
                .collect::<Vec<_>>();
            if let [binding] = bindings.as_slice() {
                references.push((locations.range(node), binding.range));
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    references
}

fn collect_qualified_type_imports(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
) -> Vec<QualifiedTypeImport> {
    let mut references = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "type_annotation" {
            let mut identifiers = Vec::new();
            let mut descendants = vec![node];
            while let Some(descendant) = descendants.pop() {
                if matches!(descendant.kind(), "identifier" | "type_identifier") {
                    identifiers.push(descendant);
                }
                let mut cursor = descendant.walk();
                let children = descendant.named_children(&mut cursor).collect::<Vec<_>>();
                descendants.extend(children.into_iter().rev());
            }
            if let [namespace, member] = identifiers.as_slice()
                && let Some(namespace_name) = node_text(text, *namespace)
                && let Some(member_name) = node_text(text, *member)
            {
                let bindings = imports
                    .iter()
                    .flat_map(|import| import.bindings.iter())
                    .filter(|binding| {
                        binding.type_only
                            && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
                            && binding.local_name == namespace_name
                    })
                    .collect::<Vec<_>>();
                if let [binding] = bindings.as_slice() {
                    references.push(QualifiedTypeImport {
                        namespace_range: locations.range(*namespace),
                        member_range: locations.range(*member),
                        binding_range: binding.range,
                        member_name: member_name.to_owned(),
                    });
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    references
}

fn collect_type_navigation_declarations(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
    declarations: &mut Vec<TypeDeclaration>,
) {
    if matches!(
        node.kind(),
        "interface_declaration" | "type_alias_declaration" | "class_declaration" | "class"
    ) && let Some(name_node) = node.child_by_field_name("name")
        && let Some(name) = node_text(text, name_node)
        && let Some(members) = direct_type_members(node, text, locations)
    {
        declarations.push(TypeDeclaration {
            name: name.to_owned(),
            range: locations.range(name_node),
            members,
            kind: declaration_kind(node.kind()).unwrap_or(DeclarationKind::Class),
            extends: type_declaration_extends(node, text),
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_navigation_declarations(child, text, locations, declarations);
    }
}

fn type_declaration_extends(node: Node<'_>, text: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| matches!(child.kind(), "extends_type_clause" | "class_heritage"))
        .flat_map(|clause| {
            let mut descendants = vec![clause];
            let mut names = Vec::new();
            while let Some(descendant) = descendants.pop() {
                if descendant.kind() == "type_identifier"
                    && let Some(name) = node_text(text, descendant)
                {
                    names.push(name.to_owned());
                }
                let mut cursor = descendant.walk();
                descendants.extend(descendant.named_children(&mut cursor));
            }
            names
        })
        .collect()
}

fn direct_type_members(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
) -> Option<Vec<TypeMember>> {
    let body = if node.kind() == "type_alias_declaration" {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| child.kind() == "object_type")?
    } else {
        node.child_by_field_name("body")?
    };
    type_members_from_body(body, text, locations)
}

fn collect_inherited_member_uses(
    root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Vec<TypeMemberUse> {
    let mut uses = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "arrow_function" {
            let callback = (|| {
                let parameter = sole_callback_parameter(node)?;
                let call = enclosing_call(node)?;
                let function = call.child_by_field_name("function")?;
                (function.kind() == "member_expression").then_some(())?;
                let object = function.child_by_field_name("object")?;
                let method = function.child_by_field_name("property")?;
                (object.kind() == "member_expression").then_some(())?;
                let receiver = object.child_by_field_name("object")?;
                let intermediate = object.child_by_field_name("property")?;
                (receiver.kind() == "this").then_some(())?;
                let callback_name = direct_member_name(text, method)?;
                let intermediate_name = direct_member_name(text, intermediate)?;
                let class = enclosing_class(node)?;
                let binding = imported_superclass_binding(
                    class,
                    text,
                    locations,
                    imports,
                    symbols,
                    import_symbols,
                )?;
                Some((parameter, binding, intermediate_name, callback_name))
            })();
            if let Some((parameter, binding, intermediate_name, callback_name)) = callback {
                let parameter_name = node_text(text, parameter).unwrap_or_default();
                let mut cursor = node.walk();
                for descendant in node.named_children(&mut cursor) {
                    collect_callback_member_uses(
                        descendant,
                        text,
                        locations,
                        parameter_name,
                        binding,
                        &intermediate_name,
                        &callback_name,
                        &mut uses,
                    );
                }
            }
        }
        if matches!(node.kind(), "member_expression" | "subscript_expression")
            && let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name(if node.kind() == "member_expression" {
                    "property"
                } else {
                    "index"
                }),
            )
            && object.kind() == "this"
            && let Some(member_name) = direct_member_name(text, property)
            && let Some(class) = enclosing_class(node)
            && let Some(binding_range) = imported_superclass_binding(
                class,
                text,
                locations,
                imports,
                symbols,
                import_symbols,
            )
        {
            uses.push(TypeMemberUse {
                target: TypeMemberTarget::SuperImported(binding_range),
                member_name,
                member_range: locations.range(property),
            });
        }
        if node.kind() == "member_expression"
            && let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("property"),
            )
            && object.kind() == "member_expression"
            && let (Some(receiver), Some(intermediate)) = (
                object.child_by_field_name("object"),
                object.child_by_field_name("property"),
            )
            && receiver.kind() == "this"
            && let (Some(member_name), Some(intermediate_name), Some(class)) = (
                direct_member_name(text, property),
                direct_member_name(text, intermediate),
                enclosing_class(node),
            )
            && let Some(binding) = imported_superclass_binding(
                class,
                text,
                locations,
                imports,
                symbols,
                import_symbols,
            )
        {
            uses.push(TypeMemberUse {
                target: TypeMemberTarget::SuperMember {
                    binding,
                    member_name: intermediate_name,
                },
                member_name,
                member_range: locations.range(property),
            });
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    uses
}

fn sole_callback_parameter(node: Node<'_>) -> Option<Node<'_>> {
    let parameters = node
        .child_by_field_name("parameter")
        .or_else(|| node.child_by_field_name("parameters"))?;
    let mut descendants = vec![parameters];
    let mut identifiers = Vec::new();
    while let Some(descendant) = descendants.pop() {
        if descendant.kind() == "identifier" {
            identifiers.push(descendant);
        }
        let mut cursor = descendant.walk();
        descendants.extend(descendant.named_children(&mut cursor));
    }
    let [parameter] = identifiers.as_slice() else {
        return None;
    };
    Some(*parameter)
}

fn enclosing_call(mut node: Node<'_>) -> Option<Node<'_>> {
    while let Some(parent) = node.parent() {
        if parent.kind() == "call_expression"
            && parent
                .child_by_field_name("arguments")
                .is_some_and(|arguments| arguments == node)
        {
            return Some(parent);
        }
        node = parent;
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn collect_callback_member_uses(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
    parameter_name: &str,
    binding: Utf16Range,
    intermediate_name: &str,
    member_name: &str,
    uses: &mut Vec<TypeMemberUse>,
) {
    if node.kind() == "member_expression"
        && let (Some(object), Some(property)) = (
            node.child_by_field_name("object"),
            node.child_by_field_name("property"),
        )
        && let Some(name) = direct_member_name(text, property)
    {
        let chain = if node_text(text, object) == Some(parameter_name) {
            Some(Vec::new())
        } else {
            callback_method_chain(object, text, parameter_name)
        };
        if let Some(chain) = chain {
            uses.push(TypeMemberUse {
                target: TypeMemberTarget::SuperMemberCallback {
                    binding,
                    member_name: intermediate_name.to_owned(),
                    callback_name: member_name.to_owned(),
                    chain,
                },
                member_name: name,
                member_range: locations.range(property),
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_callback_member_uses(
            child,
            text,
            locations,
            parameter_name,
            binding,
            intermediate_name,
            member_name,
            uses,
        );
    }
}

fn callback_method_chain(node: Node<'_>, text: &str, parameter_name: &str) -> Option<Vec<String>> {
    let call = (node.kind() == "call_expression").then_some(node)?;
    let function = call.child_by_field_name("function")?;
    let object = function.child_by_field_name("object")?;
    let method = direct_member_name(text, function.child_by_field_name("property")?)?;
    let mut chain = if node_text(text, object) == Some(parameter_name) {
        Vec::new()
    } else {
        callback_method_chain(object, text, parameter_name)?
    };
    chain.push(method);
    Some(chain)
}

fn enclosing_class(mut node: Node<'_>) -> Option<Node<'_>> {
    while let Some(parent) = node.parent() {
        if matches!(parent.kind(), "class_declaration" | "class") {
            return Some(parent);
        }
        node = parent;
    }
    None
}

fn imported_superclass_binding(
    class: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Option<Utf16Range> {
    let mut cursor = class.walk();
    let heritage = class
        .named_children(&mut cursor)
        .find(|child| child.kind() == "class_heritage")?;
    let mut cursor = heritage.walk();
    let clauses = heritage
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "extends_clause")
        .collect::<Vec<_>>();
    let [clause] = clauses.as_slice() else {
        return None;
    };
    let mut cursor = clause.walk();
    let bases = clause
        .named_children(&mut cursor)
        .filter(|child| matches!(child.kind(), "identifier" | "type_identifier"))
        .collect::<Vec<_>>();
    let [base] = bases.as_slice() else {
        return None;
    };
    let base_range = locations.range(*base);
    let binding_range = import_symbols
        .iter()
        .find(|symbol| symbol.occurrences.contains(&base_range))
        .map(|symbol| symbol.declaration)
        .or_else(|| {
            let name = node_text(text, *base)?;
            (!symbols.iter().any(|symbol| symbol.name == name))
                .then(|| {
                    imports
                        .iter()
                        .flat_map(|import| import.bindings.iter())
                        .filter(|binding| binding.local_name == name)
                        .map(|binding| binding.range)
                        .collect::<Vec<_>>()
                })
                .and_then(|matches| (matches.len() == 1).then(|| matches[0]))
        })?;
    let (_, binding) = imports.iter().find_map(|import| {
        import
            .bindings
            .iter()
            .find(|binding| binding.range == binding_range)
            .map(|binding| (import, binding))
    })?;
    (matches!(binding.kind, ImportKind::Named | ImportKind::Default) && !binding.type_only)
        .then_some(binding.range)
}

fn type_members_from_body(
    body: Node<'_>,
    text: &str,
    locations: &Locations,
) -> Option<Vec<TypeMember>> {
    let mut cursor = body.walk();
    let mut members = Vec::new();
    for member in body.named_children(&mut cursor) {
        if !matches!(
            member.kind(),
            "property_signature"
                | "method_signature"
                | "public_field_definition"
                | "method_definition"
        ) {
            continue;
        }
        let Some(name) = member.child_by_field_name("name") else {
            continue;
        };
        if name.kind() != "property_identifier" && name.kind() != "identifier" {
            continue;
        }
        if let Some(member_name) = node_text(text, name) {
            members.push(TypeMember {
                name: member_name.to_owned(),
                range: locations.range(name),
                type_reference: member_type_reference(member, text),
                callback_parameter_type: member_callback_parameter_type(member, text),
                return_type: member_return_type(member, text),
            });
        }
    }
    Some(members)
}

fn member_return_type(member: Node<'_>, text: &str) -> Option<String> {
    let annotation = member.child_by_field_name("return_type")?;
    let mut names = Vec::new();
    let mut stack = vec![annotation];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier") {
            names.push(node_text(text, node)?.to_owned());
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    matches!(names.as_slice(), [_] | [_, _]).then(|| names.pop().unwrap())
}

fn member_callback_parameter_type(member: Node<'_>, text: &str) -> Option<String> {
    let mut candidates = Vec::new();
    let mut stack = vec![member];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_type"
            && let Some(parameters) = node.child_by_field_name("parameters")
            && let Some(parameter) = sole_type_parameter(parameters)
            && let Some(annotation) = parameter.child_by_field_name("type")
            && let Some(type_name) = first_type_identifier(annotation, text)
        {
            candidates.push(type_name);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    (candidates.len() == 1).then(|| candidates.remove(0))
}

fn sole_type_parameter(parameters: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = parameters.walk();
    let parameters = parameters.named_children(&mut cursor).collect::<Vec<_>>();
    let [parameter] = parameters.as_slice() else {
        return None;
    };
    matches!(
        parameter.kind(),
        "required_parameter" | "optional_parameter"
    )
    .then_some(*parameter)
}

fn first_type_identifier(node: Node<'_>, text: &str) -> Option<String> {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "type_identifier" {
            return node_text(text, node).map(ToOwned::to_owned);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

fn member_type_reference(member: Node<'_>, text: &str) -> Option<(String, String)> {
    let annotation = member
        .child_by_field_name("type")
        .or_else(|| member.child_by_field_name("return_type"))?;
    let mut names = Vec::new();
    let mut stack = vec![annotation];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier") {
            names.push(node_text(text, node)?.to_owned());
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
    let [namespace, name] = names.as_slice() else {
        return None;
    };
    Some((namespace.clone(), name.clone()))
}

fn collect_type_member_uses(
    text_root: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    type_declarations: &[TypeDeclaration],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Vec<TypeMemberUse> {
    let mut bindings = Vec::new();
    collect_explicit_type_bindings(
        text_root,
        text,
        locations,
        imports,
        type_declarations,
        symbols,
        import_symbols,
        &mut bindings,
    );
    let mut uses = Vec::new();
    let mut stack = vec![text_root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "member_expression" | "subscript_expression")
            && let (Some(object), Some(property)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name(if node.kind() == "member_expression" {
                    "property"
                } else {
                    "index"
                }),
            )
            && let Some(member_name) = direct_member_name(text, property)
        {
            let is_this_field = object.kind() == "member_expression"
                && object
                    .child_by_field_name("object")
                    .is_some_and(|item| item.kind() == "this");
            let receiver = if object.kind() == "identifier" {
                node_text(text, object)
            } else if is_this_field
                && let Some(field) = object.child_by_field_name("property")
                && field.kind() == "property_identifier"
            {
                node_text(text, field)
            } else {
                None
            };
            if let Some(receiver) = receiver {
                let object_range = locations.range(object);
                let declaration = symbols
                    .iter()
                    .chain(import_symbols)
                    .find(|symbol| {
                        symbol.name == receiver && symbol.occurrences.contains(&object_range)
                    })
                    .map(|symbol| symbol.declaration);
                let lexical_targets = bindings
                    .iter()
                    .filter(|(name, _, scope, _)| {
                        *name == receiver && contains(*scope, object_range.start)
                    })
                    .map(|(_, _, scope, target)| (*scope, target))
                    .collect::<Vec<_>>();
                let lexical_targets = lexical_targets
                    .iter()
                    .map(|(scope, _)| scope)
                    .max_by_key(|scope| (scope.start.line, scope.start.col))
                    .map(|scope| {
                        lexical_targets
                            .iter()
                            .filter(|(candidate, _)| candidate == scope)
                            .map(|(_, target)| *target)
                            .collect::<Vec<_>>()
                    });
                let targets = lexical_targets.or_else(|| {
                    declaration
                        .map(|declaration| {
                            bindings
                                .iter()
                                .filter(|(_, range, _, _)| *range == declaration)
                                .map(|(_, _, _, target)| target)
                                .collect::<Vec<_>>()
                        })
                        .or_else(|| {
                            is_this_field
                                .then(|| {
                                    let targets = bindings
                                        .iter()
                                        .filter(|(name, _, scope, _)| {
                                            *name == receiver
                                                && contains(*scope, object_range.start)
                                        })
                                        .map(|(_, _, _, target)| target)
                                        .collect::<Vec<_>>();
                                    (targets.len() == 1).then_some(targets)
                                })
                                .flatten()
                        })
                });
                if let Some(targets) = targets
                    && let Some(target) = targets.first()
                    && targets.iter().all(|candidate| candidate == target)
                    && (node.kind() == "member_expression"
                        || matches!(target, TypeMemberTarget::Inline(_)))
                {
                    uses.push(TypeMemberUse {
                        target: (*target).clone(),
                        member_name,
                        member_range: locations.range(property),
                    });
                }
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    uses
}

#[allow(clippy::too_many_arguments)]
fn collect_explicit_type_bindings<'a>(
    node: Node<'a>,
    text: &'a str,
    locations: &Locations,
    imports: &[Import],
    type_declarations: &[TypeDeclaration],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
    bindings: &mut Vec<(&'a str, Utf16Range, Utf16Range, TypeMemberTarget)>,
) {
    if matches!(
        node.kind(),
        "variable_declarator"
            | "required_parameter"
            | "optional_parameter"
            | "property_definition"
            | "field_definition"
            | "public_field_definition"
    ) && let Some(name) = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("pattern"))
        && (name.kind() == "identifier" || name.kind() == "property_identifier")
        && let Some(target) = direct_type_annotation(
            node,
            text,
            locations,
            imports,
            type_declarations,
            symbols,
            import_symbols,
        )
    {
        let range = locations.range(name);
        if let Some(name) = node_text(text, name) {
            bindings.push((name, range, binding_scope(node, locations), target));
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_explicit_type_bindings(
            child,
            text,
            locations,
            imports,
            type_declarations,
            symbols,
            import_symbols,
            bindings,
        );
    }
}

fn binding_scope(node: Node<'_>, locations: &Locations) -> Utf16Range {
    let mut current = Some(node);
    while let Some(item) = current {
        if matches!(item.kind(), "statement_block" | "class_body" | "program") {
            return locations.range(item);
        }
        current = item.parent();
    }
    locations.range(node)
}

fn direct_type_annotation(
    node: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    type_declarations: &[TypeDeclaration],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Option<TypeMemberTarget> {
    let annotation = node.child_by_field_name("type")?;
    if annotation.kind() != "type_annotation" {
        return None;
    }
    let ty = annotation.named_child(0)?;
    type_target(
        ty,
        text,
        locations,
        imports,
        type_declarations,
        symbols,
        import_symbols,
    )
}

fn type_target(
    ty: Node<'_>,
    text: &str,
    locations: &Locations,
    imports: &[Import],
    type_declarations: &[TypeDeclaration],
    symbols: &[SymbolOccurrences],
    import_symbols: &[SymbolOccurrences],
) -> Option<TypeMemberTarget> {
    match ty.kind() {
        "object_type" => type_members_from_body(ty, text, locations).map(TypeMemberTarget::Inline),
        "type_identifier" => {
            let range = locations.range(ty);
            if let Some(symbol) = symbols
                .iter()
                .find(|symbol| symbol.occurrences.contains(&range))
            {
                return type_declarations
                    .iter()
                    .any(|item| item.range == symbol.declaration)
                    .then_some(TypeMemberTarget::Named(symbol.declaration));
            }
            let symbol = import_symbols
                .iter()
                .find(|symbol| symbol.occurrences.contains(&range))?;
            let binding = imports
                .iter()
                .flat_map(|import| import.bindings.iter())
                .find(|binding| binding.range == symbol.declaration && binding.type_only)?;
            Some(TypeMemberTarget::Imported(binding.range))
        }
        "union_type" => {
            let mut cursor = ty.walk();
            let arms = ty
                .named_children(&mut cursor)
                .map(|arm| {
                    type_target(
                        arm,
                        text,
                        locations,
                        imports,
                        type_declarations,
                        symbols,
                        import_symbols,
                    )
                })
                .collect::<Option<Vec<_>>>()?;
            (!arms.is_empty()).then_some(TypeMemberTarget::Union(arms))
        }
        _ => None,
    }
}

fn direct_member_name(text: &str, property: Node<'_>) -> Option<String> {
    match property.kind() {
        "property_identifier" => node_text(text, property).map(str::to_owned),
        "string" => node_text(text, property).map(|name| name.trim_matches(['\'', '"']).to_owned()),
        _ => None,
    }
}

fn resolve_symbols(
    text: &str,
    root: Node<'_>,
    locations: &Locations,
    include_imports: bool,
) -> Vec<SymbolOccurrences> {
    let mut scopes = vec![Scope {
        parent: None,
        range: locations.range(root),
    }];
    let mut declarations = Vec::new();
    collect_scoped_declarations(
        root,
        0,
        text,
        locations,
        &mut scopes,
        &mut declarations,
        include_imports,
    );
    let mut occurrences = declarations
        .iter()
        .map(|item| vec![item.range])
        .collect::<Vec<_>>();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier") {
            let range = locations.range(node);
            if !declarations
                .iter()
                .any(|declaration| declaration.range == range)
                && is_local_reference(node)
            {
                let name = node_text(text, node).unwrap_or_default();
                let scope = innermost_scope(range.start, &scopes);
                if let Some(index) = resolve_declaration(name, scope, &declarations, &scopes) {
                    occurrences[index].push(range);
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    declarations
        .into_iter()
        .zip(occurrences)
        .map(|(declaration, mut occurrences)| {
            occurrences.sort_by_key(|range| (range.start.line, range.start.col));
            occurrences.dedup();
            SymbolOccurrences {
                name: declaration.name,
                kind: declaration.kind,
                declaration: declaration.range,
                occurrences,
            }
        })
        .collect()
}

fn collect_scoped_declarations(
    node: Node<'_>,
    scope: usize,
    text: &str,
    locations: &Locations,
    scopes: &mut Vec<Scope>,
    declarations: &mut Vec<ResolvedDeclaration>,
    include_imports: bool,
) {
    if include_imports && node.kind() == "import_statement" {
        let statement_type_only = has_keyword(node, text, "type");
        let mut bindings = Vec::new();
        collect_import_bindings(text, node, statement_type_only, locations, &mut bindings);
        for binding in bindings {
            declarations.push(ResolvedDeclaration {
                name: binding.local_name,
                kind: DeclarationKind::Variable,
                range: binding.range,
                scope,
            });
        }
    }
    let is_function = matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "arrow_function"
            | "function_expression"
            | "method_definition"
    );
    if matches!(
        node.kind(),
        "function_declaration"
            | "generator_function_declaration"
            | "class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
            | "internal_module"
            | "module"
            | "namespace_declaration"
    ) && let Some(name) = node.child_by_field_name("name")
    {
        declare(
            name,
            DeclarationKind::Function,
            scope,
            text,
            locations,
            declarations,
        );
        if node.kind() != "function_declaration" && node.kind() != "generator_function_declaration"
        {
            declarations.last_mut().unwrap().kind = declaration_kind(node.kind()).unwrap();
        }
    }
    let scope = if is_function || node.kind() == "statement_block" {
        let index = scopes.len();
        scopes.push(Scope {
            parent: Some(scope),
            range: locations.range(node),
        });
        if is_function {
            if let Some(parameters) = node.child_by_field_name("parameters") {
                collect_parameter_bindings(parameters, index, text, locations, declarations);
            } else if let Some(parameter) = node.child_by_field_name("parameter") {
                collect_pattern_declarations(
                    parameter,
                    DeclarationKind::Parameter,
                    index,
                    text,
                    locations,
                    declarations,
                );
            }
        }
        index
    } else {
        scope
    };
    if matches!(node.kind(), "lexical_declaration" | "variable_declaration") {
        let mut cursor = node.walk();
        for declarator in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "variable_declarator")
        {
            if let Some(name) = declarator.child_by_field_name("name") {
                collect_pattern_declarations(
                    name,
                    DeclarationKind::Variable,
                    scope,
                    text,
                    locations,
                    declarations,
                );
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_scoped_declarations(
            child,
            scope,
            text,
            locations,
            scopes,
            declarations,
            include_imports,
        );
    }
}

fn declaration_kind(kind: &str) -> Option<DeclarationKind> {
    Some(match kind {
        "class_declaration" | "class" => DeclarationKind::Class,
        "interface_declaration" => DeclarationKind::Interface,
        "type_alias_declaration" => DeclarationKind::Type,
        "enum_declaration" => DeclarationKind::Enum,
        "internal_module" | "module" | "namespace_declaration" => DeclarationKind::Namespace,
        _ => return None,
    })
}

fn collect_parameter_bindings(
    node: Node<'_>,
    scope: usize,
    text: &str,
    locations: &Locations,
    declarations: &mut Vec<ResolvedDeclaration>,
) {
    let mut cursor = node.walk();
    for parameter in node.named_children(&mut cursor) {
        if let Some(pattern) = parameter.child_by_field_name("pattern") {
            collect_pattern_declarations(
                pattern,
                DeclarationKind::Parameter,
                scope,
                text,
                locations,
                declarations,
            );
        } else if let Some(name) = parameter.child_by_field_name("name") {
            collect_pattern_declarations(
                name,
                DeclarationKind::Parameter,
                scope,
                text,
                locations,
                declarations,
            );
        } else if parameter.kind() == "identifier" {
            declare(
                parameter,
                DeclarationKind::Parameter,
                scope,
                text,
                locations,
                declarations,
            );
        }
    }
}

fn collect_pattern_declarations(
    node: Node<'_>,
    kind: DeclarationKind,
    scope: usize,
    text: &str,
    locations: &Locations,
    declarations: &mut Vec<ResolvedDeclaration>,
) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            declare(node, kind, scope, text, locations, declarations)
        }
        "pair_pattern" => {
            if let Some(value) = node.child_by_field_name("value") {
                collect_pattern_declarations(value, kind, scope, text, locations, declarations);
            }
        }
        "assignment_pattern" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_pattern_declarations(left, kind, scope, text, locations, declarations);
            }
        }
        "rest_pattern" => {
            if let Some(argument) = node.child_by_field_name("argument") {
                collect_pattern_declarations(argument, kind, scope, text, locations, declarations);
            }
        }
        "array_pattern" | "object_pattern" | "required_parameter" | "optional_parameter" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_pattern_declarations(child, kind, scope, text, locations, declarations);
            }
        }
        _ => {}
    }
}

fn declare(
    node: Node<'_>,
    kind: DeclarationKind,
    scope: usize,
    text: &str,
    locations: &Locations,
    declarations: &mut Vec<ResolvedDeclaration>,
) {
    if let Some(name) = node_text(text, node) {
        declarations.push(ResolvedDeclaration {
            name: name.to_owned(),
            kind,
            range: locations.range(node),
            scope,
        });
    }
}

fn innermost_scope(position: Utf16Position, scopes: &[Scope]) -> usize {
    scopes
        .iter()
        .enumerate()
        .filter(|(_, scope)| contains(scope.range, position))
        .max_by_key(|(_, scope)| (scope.range.start.line, scope.range.start.col))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn resolve_declaration(
    name: &str,
    mut scope: usize,
    declarations: &[ResolvedDeclaration],
    scopes: &[Scope],
) -> Option<usize> {
    loop {
        if let Some((index, _)) = declarations
            .iter()
            .enumerate()
            .rev()
            .find(|(_, declaration)| declaration.scope == scope && declaration.name == name)
        {
            return Some(index);
        }
        scope = scopes[scope].parent?;
    }
}

fn is_local_reference(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(item) = current {
        if matches!(item.kind(), "import_statement" | "labeled_statement")
            || item.kind() == "export_statement"
                && !item.named_children(&mut item.walk()).any(|child| {
                    matches!(
                        child.kind(),
                        "function_declaration"
                            | "generator_function_declaration"
                            | "class_declaration"
                            | "class"
                            | "interface_declaration"
                            | "type_alias_declaration"
                            | "enum_declaration"
                            | "lexical_declaration"
                            | "variable_declaration"
                    )
                })
        {
            return false;
        }
        if item.kind() == "member_expression"
            && item
                .child_by_field_name("property")
                .is_some_and(|property| property.id() == node.id())
        {
            return false;
        }
        if item.kind() == "pair"
            && item
                .child_by_field_name("key")
                .is_some_and(|key| key.id() == node.id())
        {
            return false;
        }
        current = item.parent();
    }
    true
}

fn language_for_path(path: &Path) -> DocumentLanguage {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".d.ts"))
    {
        return DocumentLanguage::TypeScript;
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("tsx") => DocumentLanguage::Tsx,
        Some("ts" | "mts" | "cts") => DocumentLanguage::TypeScript,
        _ => DocumentLanguage::JavaScript,
    }
}

fn tree_sitter_language(language: DocumentLanguage) -> Language {
    match language {
        DocumentLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        DocumentLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        DocumentLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
    }
}

fn declarations_from_node(
    node: Node<'_>,
    exported: bool,
    default: bool,
    text: &str,
    locations: &Locations,
) -> Vec<Declaration> {
    if node.kind() == "ambient_declaration" {
        let mut cursor = node.walk();
        return node
            .named_children(&mut cursor)
            .flat_map(|child| {
                let name_range = child
                    .child_by_field_name("name")
                    .map(|name| locations.range(name));
                declarations_from_node(child, exported, default, text, locations)
                    .into_iter()
                    .map(move |mut declaration| {
                        if let Some(range) = name_range {
                            declaration.range = range;
                        }
                        declaration
                    })
            })
            .collect();
    }
    let kind = match node.kind() {
        "function_declaration" | "function_signature" | "generator_function_declaration" => {
            DeclarationKind::Function
        }
        "class_declaration" | "class" => DeclarationKind::Class,
        "interface_declaration" => DeclarationKind::Interface,
        "type_alias_declaration" => DeclarationKind::Type,
        "enum_declaration" => DeclarationKind::Enum,
        "internal_module" | "module" | "namespace_declaration" => DeclarationKind::Namespace,
        "lexical_declaration" | "variable_declaration" => {
            return variable_declarations(node, exported, default, text, locations);
        }
        _ => return Vec::new(),
    };
    node.child_by_field_name("name")
        .and_then(|name| node_text(text, name).map(str::to_owned))
        .map(|name| Declaration {
            name,
            kind,
            range: locations.range(node),
            exported,
            default,
        })
        .into_iter()
        .collect()
}

fn variable_declarations(
    node: Node<'_>,
    exported: bool,
    default: bool,
    text: &str,
    locations: &Locations,
) -> Vec<Declaration> {
    let mut declarations = Vec::new();
    let mut cursor = node.walk();
    for declarator in node.named_children(&mut cursor) {
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(name) = declarator.child_by_field_name("name") else {
            continue;
        };
        let mut bindings = Vec::new();
        collect_bindings(name, text, &mut bindings);
        declarations.extend(bindings.into_iter().map(|name| Declaration {
            name,
            kind: DeclarationKind::Variable,
            range: locations.range(declarator),
            exported,
            default,
        }));
    }
    declarations
}

fn collect_bindings(node: Node<'_>, text: &str, bindings: &mut Vec<String>) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            if let Some(name) = node_text(text, node) {
                bindings.push(name.to_owned());
            }
        }
        "pair_pattern" => {
            if let Some(value) = node.child_by_field_name("value") {
                collect_bindings(value, text, bindings);
            }
        }
        "assignment_pattern" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_bindings(left, text, bindings);
            }
        }
        "rest_pattern" => {
            if let Some(argument) = node.child_by_field_name("argument") {
                collect_bindings(argument, text, bindings);
            }
        }
        "array_pattern" | "object_pattern" | "parenthesized_expression" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_bindings(child, text, bindings);
            }
        }
        _ => {}
    }
}

fn import_from_node(text: &str, node: Node<'_>, locations: &Locations) -> Import {
    let specifier = source_specifier(text, node).unwrap_or_default();
    let specifier_range = source_specifier_node(node)
        .map(|source| locations.range(source))
        .unwrap_or_default();
    let type_only = has_keyword(node, text, "type");
    let mut bindings = Vec::new();
    collect_import_bindings(text, node, type_only, locations, &mut bindings);
    Import {
        specifier,
        specifier_range,
        range: locations.range(node),
        bindings,
        type_only,
        syntax_valid: !node.has_error() && !node.is_error() && !node.is_missing(),
    }
}

fn collect_dynamic_imports(
    text: &str,
    node: Node<'_>,
    locations: &Locations,
    imports: &mut Vec<Import>,
) {
    if node.kind() == "call_expression"
        && node
            .child_by_field_name("function")
            .is_some_and(|function| function.kind() == "import")
        && let Some(arguments) = node.child_by_field_name("arguments")
    {
        let mut cursor = arguments.walk();
        let mut arguments = arguments.named_children(&mut cursor);
        if let (Some(source), None) = (arguments.next(), arguments.next())
            && source.kind() == "string"
            && let Some(specifier) = node_text(text, source)
        {
            imports.push(Import {
                specifier: specifier.trim_matches(['\'', '"']).to_owned(),
                specifier_range: locations.range(source),
                range: locations.range(source),
                bindings: Vec::new(),
                type_only: false,
                syntax_valid: true,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_dynamic_imports(text, child, locations, imports);
    }
}

fn collect_import_bindings(
    text: &str,
    node: Node<'_>,
    statement_type_only: bool,
    locations: &Locations,
    bindings: &mut Vec<ImportBinding>,
) {
    match node.kind() {
        "import_specifier" => {
            bindings.push(import_binding(text, node, statement_type_only, locations))
        }
        "namespace_import" => {
            if let Some(name) = last_identifier_node(node, text) {
                bindings.push(ImportBinding {
                    local_name: node_text(text, name).unwrap_or_default().to_owned(),
                    imported_name: None,
                    kind: ImportKind::Namespace,
                    range: locations.range(name),
                    type_only: statement_type_only,
                });
            }
        }
        "import_clause" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "identifier" {
                    bindings.push(ImportBinding {
                        local_name: node_text(text, child).unwrap_or_default().to_owned(),
                        imported_name: Some("default".to_owned()),
                        kind: ImportKind::Default,
                        range: locations.range(child),
                        type_only: statement_type_only,
                    });
                } else {
                    collect_import_bindings(text, child, statement_type_only, locations, bindings);
                }
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_import_bindings(text, child, statement_type_only, locations, bindings);
            }
        }
    }
}

fn import_binding(
    text: &str,
    node: Node<'_>,
    statement_type_only: bool,
    locations: &Locations,
) -> ImportBinding {
    let identifiers = identifiers(node, text);
    let imported_name = identifiers.first().cloned();
    let local = last_identifier_node(node, text);
    let local_name = local
        .and_then(|node| node_text(text, node))
        .unwrap_or_default()
        .to_owned();
    ImportBinding {
        local_name,
        imported_name,
        kind: ImportKind::Named,
        range: local.map(|node| locations.range(node)).unwrap_or_default(),
        type_only: statement_type_only || has_keyword(node, text, "type"),
    }
}

fn index_export(
    text: &str,
    node: Node<'_>,
    locations: &Locations,
    declarations: &mut Vec<Declaration>,
    exports: &mut Vec<Export>,
) {
    let default = has_keyword(node, text, "default");
    let type_only = has_keyword(node, text, "type");
    let specifier = source_specifier(text, node);
    let specifier_range = source_specifier_node(node).map(|source| locations.range(source));
    let mut declaration = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !declarations_from_node(child, true, default, text, locations).is_empty() {
            declaration = Some(child);
            break;
        }
    }
    if let Some(declaration) = declaration {
        let declared = declarations_from_node(declaration, true, default, text, locations);
        for item in &declared {
            exports.push(Export {
                name: Some(item.name.clone()),
                name_range: Some(item.range),
                exported_name: Some(if default {
                    "default".to_owned()
                } else {
                    item.name.clone()
                }),
                exported_name_range: Some(item.range),
                specifier: specifier.clone(),
                specifier_range,
                range: locations.range(node),
                default,
                type_only,
                syntax_valid: !node.has_error() && !node.is_error() && !node.is_missing(),
            });
        }
        declarations.extend(declared);
        return;
    }

    let mut found = false;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() != "export_clause" {
            continue;
        }
        let mut specifier_cursor = child.walk();
        for export_specifier in child.named_children(&mut specifier_cursor) {
            if export_specifier.kind() != "export_specifier" {
                continue;
            }
            let identifier_nodes = identifier_nodes(export_specifier, text);
            let name = identifier_nodes
                .first()
                .and_then(|identifier| node_text(text, *identifier))
                .map(str::to_owned);
            let exported_name = identifier_nodes
                .last()
                .and_then(|identifier| node_text(text, *identifier))
                .map(str::to_owned)
                .or_else(|| name.clone());
            exports.push(Export {
                name,
                name_range: identifier_nodes
                    .first()
                    .map(|identifier| locations.range(*identifier)),
                exported_name,
                exported_name_range: identifier_nodes
                    .last()
                    .map(|identifier| locations.range(*identifier)),
                specifier: specifier.clone(),
                specifier_range,
                range: locations.range(export_specifier),
                default: false,
                type_only: type_only || has_keyword(export_specifier, text, "type"),
                syntax_valid: !node.has_error() && !node.is_error() && !node.is_missing(),
            });
            found = true;
        }
    }
    if !found {
        exports.push(Export {
            name: None,
            name_range: None,
            exported_name: if default {
                Some("default".to_owned())
            } else {
                None
            },
            exported_name_range: None,
            specifier,
            specifier_range,
            range: locations.range(node),
            default,
            type_only,
            syntax_valid: !node.has_error() && !node.is_error() && !node.is_missing(),
        });
    }
}

fn is_export_assignment(text: &str, node: Node<'_>) -> bool {
    node_text(text, node).is_some_and(|source| source.trim_start().starts_with("export ="))
}

fn diagnostics(root: Node<'_>, locations: &Locations) -> Vec<ParseDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            diagnostics.push(ParseDiagnostic {
                message: if node.is_missing() {
                    "Missing syntax"
                } else {
                    "Syntax error"
                }
                .to_owned(),
                range: locations.range(node),
            });
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start.line, diagnostic.range.start.col));
    diagnostics
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Type {
    Unknown,
    Primitive(&'static str),
    Null,
    Undefined,
    Object(std::collections::BTreeMap<String, Type>),
    Function(Vec<Type>, Box<Type>, Vec<String>),
    Named(String, Vec<Type>),
    Union(Vec<Type>),
}

#[derive(Default, Clone)]
struct TypeState {
    values: std::collections::HashMap<String, Type>,
    aliases: std::collections::HashMap<String, (Vec<String>, Type)>,
    diagnostics: Vec<TypeDiagnostic>,
}

fn type_diagnostics(text: &str, root: Node<'_>, locations: &Locations) -> Vec<TypeDiagnostic> {
    let mut state = TypeState::default();
    // First collect named declarations so forward references and aliases are stable.
    collect_type_declarations(text, root, &mut state);
    check_type_node(text, root, locations, &mut state);
    state.diagnostics
}

fn collect_type_declarations(text: &str, node: Node<'_>, state: &mut TypeState) {
    match node.kind() {
        "type_alias_declaration" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| node_text(text, n))
            {
                let parameters = type_parameters(text, node);
                if let Some(value) = node.child_by_field_name("value") {
                    state
                        .aliases
                        .insert(name.to_owned(), (parameters, parse_type(text, value)));
                }
            }
        }
        "interface_declaration" | "class_declaration" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| node_text(text, n))
            {
                let mut properties = std::collections::BTreeMap::new();
                let mut stack = vec![node];
                while let Some(item) = stack.pop() {
                    if matches!(
                        item.kind(),
                        "property_signature" | "public_field_definition"
                    ) && let Some(field) = item
                        .child_by_field_name("name")
                        .and_then(|n| node_text(text, n))
                    {
                        let ty = item
                            .child_by_field_name("type")
                            .map(|n| parse_type(text, n))
                            .unwrap_or(Type::Unknown);
                        properties.insert(field.to_owned(), ty);
                    }
                    let mut cursor = item.walk();
                    stack.extend(item.named_children(&mut cursor));
                }
                let parameters = type_parameters(text, node);
                let ty = if node.kind() == "class_declaration" {
                    Type::Function(
                        Vec::new(),
                        Box::new(Type::Object(properties)),
                        parameters.clone(),
                    )
                } else {
                    Type::Object(properties)
                };
                state.aliases.insert(name.to_owned(), (parameters, ty));
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_type_declarations(text, child, state);
    }
}

fn check_type_node(text: &str, node: Node<'_>, locations: &Locations, state: &mut TypeState) {
    match node.kind() {
        "lexical_declaration" | "variable_declaration" => {
            let mut cursor = node.walk();
            for declarator in node
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "variable_declarator")
            {
                let Some(name) = declarator
                    .child_by_field_name("name")
                    .and_then(|n| node_text(text, n))
                else {
                    continue;
                };
                let declared = declarator
                    .child_by_field_name("type")
                    .map(|n| parse_type(text, n));
                let value = declarator
                    .child_by_field_name("value")
                    .map(|n| infer_type(text, n, state));
                if let (Some(expected), Some(actual)) = (&declared, &value) {
                    report_mismatch(
                        expected,
                        actual,
                        locations.range(declarator.child_by_field_name("value").unwrap()),
                        state,
                    );
                }
                state
                    .values
                    .insert(name.to_owned(), declared.or(value).unwrap_or(Type::Unknown));
            }
            return;
        }
        "function_declaration" | "method_definition" => {
            let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| node_text(text, n))
            else {
                return;
            };
            let mut parameters = Vec::new();
            if let Some(params) = node.child_by_field_name("parameters") {
                let mut cursor = params.walk();
                for parameter in params.named_children(&mut cursor) {
                    parameters.push(
                        parameter
                            .child_by_field_name("type")
                            .map(|n| parse_type(text, n))
                            .unwrap_or(Type::Unknown),
                    );
                }
            }
            let result = node
                .child_by_field_name("return_type")
                .map(|n| parse_type(text, n))
                .unwrap_or(Type::Unknown);
            state.values.insert(
                name.to_owned(),
                Type::Function(parameters, Box::new(result), type_parameters(text, node)),
            );
        }
        "assignment_expression" => {
            let right = node.child_by_field_name("right");
            let left = node.child_by_field_name("left");
            if let (Some(left), Some(right)) = (left, right) {
                let actual = infer_type(text, right, state);
                if left.kind() == "identifier"
                    && let Some(expected) = node_text(text, left)
                        .and_then(|name| state.values.get(name))
                        .cloned()
                {
                    report_mismatch(&expected, &actual, locations.range(right), state);
                }
            }
        }
        "call_expression" | "new_expression" => check_call(text, node, locations, state),
        "if_statement" => {
            let condition = node.child_by_field_name("condition");
            let consequence = node.child_by_field_name("consequence");
            if let (Some(condition), Some(consequence)) = (condition, consequence) {
                let mut narrowed = state.clone();
                let diagnostic_count = narrowed.diagnostics.len();
                narrow_condition(text, condition, &mut narrowed);
                check_type_node(text, consequence, locations, &mut narrowed);
                state
                    .diagnostics
                    .extend(narrowed.diagnostics.into_iter().skip(diagnostic_count));
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                check_type_node(text, alternative, locations, state);
            }
            return;
        }
        "member_expression" => {
            let object = node
                .child_by_field_name("object")
                .map(|n| infer_type(text, n, state))
                .unwrap_or(Type::Unknown);
            let property = node.child_by_field_name("property");
            if let (Type::Object(fields), Some(property)) = (resolve(&object, state), property) {
                let name = node_text(text, property).unwrap_or("");
                if !fields.contains_key(name) {
                    state.diagnostics.push(TypeDiagnostic {
                        code: "TS2339".into(),
                        message: format!("Property '{}' does not exist on this type", name),
                        range: locations.range(property),
                    });
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        check_type_node(text, child, locations, state);
    }
}

fn infer_type(text: &str, node: Node<'_>, state: &mut TypeState) -> Type {
    match node.kind() {
        "number" => Type::Primitive("number"),
        "string" | "template_string" => Type::Primitive("string"),
        "true" | "false" => Type::Primitive("boolean"),
        "null" => Type::Null,
        "undefined" => Type::Undefined,
        "identifier" => node_text(text, node)
            .map(|name| {
                state.values.get(name).cloned().unwrap_or_else(|| {
                    if state.aliases.contains_key(name) {
                        Type::Named(name.to_owned(), Vec::new())
                    } else {
                        Type::Unknown
                    }
                })
            })
            .unwrap_or(Type::Unknown),
        "object" => {
            let mut fields = std::collections::BTreeMap::new();
            let mut cursor = node.walk();
            for pair in node
                .named_children(&mut cursor)
                .filter(|n| n.kind() == "pair")
            {
                if let (Some(key), Some(value)) = (
                    pair.child_by_field_name("key")
                        .and_then(|n| node_text(text, n)),
                    pair.child_by_field_name("value"),
                ) {
                    fields.insert(
                        key.trim_matches(['\'', '"']).to_owned(),
                        infer_type(text, value, state),
                    );
                }
            }
            Type::Object(fields)
        }
        "arrow_function" | "function_expression" => {
            Type::Function(Vec::new(), Box::new(Type::Unknown), Vec::new())
        }
        "member_expression" => {
            let object = node
                .child_by_field_name("object")
                .map(|n| infer_type(text, n, state))
                .unwrap_or(Type::Unknown);
            let property = node
                .child_by_field_name("property")
                .and_then(|n| node_text(text, n))
                .unwrap_or("");
            match resolve(&object, state) {
                Type::Object(fields) => fields.get(property).cloned().unwrap_or(Type::Unknown),
                _ => Type::Unknown,
            }
        }
        "parenthesized_expression" => node
            .named_child(0)
            .map(|n| infer_type(text, n, state))
            .unwrap_or(Type::Unknown),
        "call_expression" | "new_expression" => {
            let callee = node
                .child_by_field_name("function")
                .or_else(|| node.child_by_field_name("constructor"));
            let Some(callee) = callee else {
                return Type::Unknown;
            };
            let Type::Function(_, result, parameters) =
                resolve(&infer_type(text, callee, state), state)
            else {
                return Type::Unknown;
            };
            let args = node
                .child_by_field_name("type_arguments")
                .map(|arguments| {
                    node_text(text, arguments)
                        .unwrap_or("")
                        .trim_matches(['<', '>'])
                        .split(',')
                        .map(parse_type_text)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            substitute(&result, &parameters, &args)
        }
        _ => Type::Unknown,
    }
}

fn check_call(text: &str, node: Node<'_>, locations: &Locations, state: &mut TypeState) {
    let function = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("constructor"));
    let Some(function) = function else { return };
    let mut callee = infer_type(text, function, state);
    if matches!(node.kind(), "new_expression") {
        callee = resolve(&callee, state);
    }
    let Type::Function(parameters, _, generic_parameters) = callee else {
        return;
    };
    let explicit_arguments = node
        .child_by_field_name("type_arguments")
        .map(|arguments| {
            node_text(text, arguments)
                .unwrap_or("")
                .trim_matches(['<', '>'])
                .split(',')
                .map(parse_type_text)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let parameters: Vec<_> = parameters
        .iter()
        .map(|ty| substitute(ty, &generic_parameters, &explicit_arguments))
        .collect();
    let arguments = node.child_by_field_name("arguments");
    let args: Vec<_> = arguments
        .into_iter()
        .flat_map(|list| {
            let mut cursor = list.walk();
            list.named_children(&mut cursor).collect::<Vec<_>>()
        })
        .collect();
    if args.len() != parameters.len() {
        state.diagnostics.push(TypeDiagnostic {
            code: "TS2554".into(),
            message: format!(
                "Expected {} arguments, but got {}",
                parameters.len(),
                args.len()
            ),
            range: locations.range(node),
        });
    }
    for (argument, expected) in args.into_iter().zip(parameters) {
        let actual = infer_type(text, argument, state);
        report_mismatch(&expected, &actual, locations.range(argument), state);
    }
}

fn report_mismatch(expected: &Type, actual: &Type, range: Utf16Range, state: &mut TypeState) {
    if !assignable(expected, actual, state)
        && !matches!(expected, Type::Unknown)
        && !matches!(actual, Type::Unknown)
    {
        state.diagnostics.push(TypeDiagnostic {
            code: "TS2322".into(),
            message: "Type is not assignable to the declared type".into(),
            range,
        });
    }
}

fn assignable(expected: &Type, actual: &Type, state: &TypeState) -> bool {
    match (resolve(expected, state), resolve(actual, state)) {
        (Type::Unknown, _) | (_, Type::Unknown) => true,
        (Type::Union(types), actual) => types.iter().any(|ty| assignable(ty, &actual, state)),
        (Type::Primitive(a), Type::Primitive(b)) => a == b,
        (Type::Null, Type::Null) | (Type::Undefined, Type::Undefined) => true,
        (Type::Object(expected), Type::Object(actual)) => expected.iter().all(|(name, ty)| {
            actual
                .get(name)
                .is_some_and(|actual| assignable(ty, actual, state))
        }),
        _ => false,
    }
}

fn resolve(ty: &Type, state: &TypeState) -> Type {
    match ty {
        Type::Named(name, args) => state
            .aliases
            .get(name)
            .map_or(Type::Unknown, |(parameters, value)| {
                substitute(value, parameters, args)
            }),
        _ => ty.clone(),
    }
}

fn substitute(ty: &Type, parameters: &[String], args: &[Type]) -> Type {
    match ty {
        Type::Named(name, nested) if nested.is_empty() => parameters
            .iter()
            .position(|parameter| parameter == name)
            .and_then(|index| args.get(index))
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Type::Union(items) => Type::Union(
            items
                .iter()
                .map(|item| substitute(item, parameters, args))
                .collect(),
        ),
        Type::Object(items) => Type::Object(
            items
                .iter()
                .map(|(name, item)| (name.clone(), substitute(item, parameters, args)))
                .collect(),
        ),
        Type::Function(items, result, generic) => Type::Function(
            items
                .iter()
                .map(|item| substitute(item, parameters, args))
                .collect(),
            Box::new(substitute(result, parameters, args)),
            generic.clone(),
        ),
        _ => ty.clone(),
    }
}

fn parse_type(text: &str, node: Node<'_>) -> Type {
    parse_type_text(node_text(text, node).unwrap_or(""))
}

fn narrow_condition(text: &str, condition: Node<'_>, state: &mut TypeState) {
    let condition = node_text(text, condition).unwrap_or("").trim();
    let (name, narrowed) = if let Some(rest) = condition.strip_prefix("typeof ") {
        let Some((name, value)) = rest.split_once("===") else {
            return;
        };
        let value = value.trim().trim_matches(['\'', '"']);
        let ty = match value {
            "string" => Type::Primitive("string"),
            "number" => Type::Primitive("number"),
            "boolean" => Type::Primitive("boolean"),
            _ => return,
        };
        (name.trim(), ty)
    } else {
        let Some((name, value)) = condition
            .split_once("!==")
            .or_else(|| condition.split_once("!="))
        else {
            return;
        };
        let ty = match value.trim() {
            "null" => Type::Null,
            "undefined" => Type::Undefined,
            _ => return,
        };
        let Some(Type::Union(items)) = state.values.get(name.trim()) else {
            return;
        };
        let items: Vec<_> = items.iter().filter(|item| **item != ty).cloned().collect();
        if items.len() != 1 {
            return;
        }
        (name.trim(), items[0].clone())
    };
    if let Some(Type::Union(items)) = state.values.get(name)
        && items.contains(&narrowed)
    {
        state.values.insert(name.to_owned(), narrowed);
    }
}

fn parse_type_text(text: &str) -> Type {
    let text = text.trim().trim_start_matches(':').trim();
    let union: Vec<_> = text.split('|').map(str::trim).collect();
    if union.len() > 1 {
        return Type::Union(union.into_iter().map(parse_type_text).collect());
    }
    match text {
        "string" => Type::Primitive("string"),
        "number" => Type::Primitive("number"),
        "boolean" => Type::Primitive("boolean"),
        "void" => Type::Primitive("void"),
        "null" => Type::Null,
        "undefined" => Type::Undefined,
        _ if text.starts_with('{') && text.ends_with('}') => Type::Object(
            text[1..text.len() - 1]
                .split(';')
                .filter_map(|field| {
                    field.split_once(':').map(|(name, ty)| {
                        (
                            name.trim().trim_end_matches('?').to_owned(),
                            parse_type_text(ty),
                        )
                    })
                })
                .collect(),
        ),
        _ => {
            let (name, args) = text
                .split_once('<')
                .map_or((text, Vec::new()), |(name, args)| {
                    (
                        name.trim(),
                        args.trim_end_matches('>')
                            .split(',')
                            .map(parse_type_text)
                            .collect(),
                    )
                });
            Type::Named(name.to_owned(), args)
        }
    }
}

fn type_parameters(text: &str, node: Node<'_>) -> Vec<String> {
    node.child_by_field_name("type_parameters")
        .map(|params| {
            node_text(text, params)
                .unwrap_or("")
                .trim_matches(['<', '>'])
                .split(',')
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn source_specifier(text: &str, node: Node<'_>) -> Option<String> {
    source_specifier_node(node)
        .and_then(|source| node_text(text, source))
        .map(|source| source.trim_matches(['\'', '"']).to_owned())
}

fn source_specifier_node(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("source").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| child.kind() == "string")
    })
}

fn collect_namespace_members(
    text: &str,
    node: Node<'_>,
    locations: &Locations,
    members: &mut Vec<NamespaceMemberUse>,
) {
    if matches!(node.kind(), "member_expression" | "jsx_member_expression")
        && let (Some(object), Some(property)) = (node.named_child(0), node.named_child(1))
        && matches!(object.kind(), "identifier" | "jsx_identifier")
        && matches!(
            property.kind(),
            "identifier" | "property_identifier" | "jsx_identifier"
        )
        && !node.parent().is_some_and(|parent| {
            matches!(parent.kind(), "member_expression" | "jsx_member_expression")
                && parent
                    .named_child(0)
                    .is_some_and(|object| object.id() == node.id())
        })
        && let Some(member_name) = node_text(text, property)
    {
        members.push(NamespaceMemberUse {
            namespace_range: locations.range(object),
            namespace_name: node_text(text, object).unwrap_or_default().to_owned(),
            member_name: member_name.to_owned(),
            member_range: locations.range(property),
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_namespace_members(text, child, locations, members);
    }
}

fn has_keyword(node: Node<'_>, text: &str, keyword: &str) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| node_text(text, child) == Some(keyword))
}

fn identifiers(node: Node<'_>, text: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            if let Some(name) = node_text(text, node) {
                identifiers.push(name.to_owned());
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    identifiers.reverse();
    identifiers
}

fn identifier_nodes<'a>(node: Node<'a>, text: &str) -> Vec<Node<'a>> {
    let mut identifiers = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            if node_text(text, node).is_some() {
                identifiers.push(node);
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    identifiers.reverse();
    identifiers
}

fn last_identifier_node<'a>(node: Node<'a>, text: &str) -> Option<Node<'a>> {
    let mut identifiers = Vec::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if node.kind() == "identifier" {
            if node_text(text, node).is_some() {
                identifiers.push(node);
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    identifiers.reverse();
    identifiers.pop()
}

fn node_text<'a>(text: &'a str, node: Node<'_>) -> Option<&'a str> {
    text.get(node.start_byte()..node.end_byte())
}

struct Locations<'a> {
    text: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> Locations<'a> {
    fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(offset, _)| offset + 1));
        Self { text, line_starts }
    }

    fn position(&self, text: &str, byte: usize) -> Utf16Position {
        let byte = byte.min(text.len());
        let line = self.line_starts.partition_point(|start| *start <= byte) - 1;
        let start = self.line_starts[line];
        Utf16Position::new(
            line,
            text.get(start..byte).unwrap_or("").encode_utf16().count(),
        )
    }

    fn range(&self, node: Node<'_>) -> Utf16Range {
        Utf16Range {
            start: self.position(self.text, node.start_byte()),
            end: self.position(self.text, node.end_byte()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_typescript_declarations_imports_and_exports() {
        let index = DocumentIndex::parse(
            "src/example.ts",
            "import Main, { type User as Person, run } from './dep';\n\
             import * as api from './api';\n\
             export function greet() {}\n\
             export default class App {}\n\
             export interface Shape {}\n\
             export type Id = string;\n\
             export enum Color { Red }\n\
             export namespace Utils {}\n\
             export const [first, { value: second }] = pair;\n\
             export { run as execute } from './dep';\n\
             export * from './other';",
        );

        assert_eq!(index.language, DocumentLanguage::TypeScript);
        assert_eq!(
            index
                .declarations
                .iter()
                .map(|item| (item.name.as_str(), item.kind, item.exported, item.default))
                .collect::<Vec<_>>(),
            vec![
                ("greet", DeclarationKind::Function, true, false),
                ("App", DeclarationKind::Class, true, true),
                ("Shape", DeclarationKind::Interface, true, false),
                ("Id", DeclarationKind::Type, true, false),
                ("Color", DeclarationKind::Enum, true, false),
                ("Utils", DeclarationKind::Namespace, true, false),
                ("first", DeclarationKind::Variable, true, false),
                ("second", DeclarationKind::Variable, true, false),
            ]
        );
        assert_eq!(index.imports.len(), 2);
        assert_eq!(index.imports[0].specifier, "./dep");
        assert_eq!(
            index.imports[0]
                .bindings
                .iter()
                .map(|binding| (
                    binding.local_name.as_str(),
                    binding.imported_name.as_deref(),
                    binding.kind,
                    binding.type_only,
                ))
                .collect::<Vec<_>>(),
            vec![
                ("Main", Some("default"), ImportKind::Default, false),
                ("Person", Some("User"), ImportKind::Named, true),
                ("run", Some("run"), ImportKind::Named, false),
            ]
        );
        assert_eq!(index.imports[1].bindings[0].kind, ImportKind::Namespace);
        assert_eq!(index.imports[1].bindings[0].local_name, "api");
        assert!(index.exports.iter().any(|item| {
            item.name.as_deref() == Some("run")
                && item.exported_name.as_deref() == Some("execute")
                && item.specifier.as_deref() == Some("./dep")
        }));
        assert!(index.exports.iter().any(|item| {
            item.name.is_none()
                && item.exported_name.is_none()
                && item.specifier.as_deref() == Some("./other")
        }));
        assert!(index.diagnostics.is_empty());
    }

    #[test]
    fn indexes_exact_static_module_specifier_ranges() {
        let index = DocumentIndex::parse(
            "main.ts",
            "import { value } from './one';\nimport './two';\nexport { value } from './three';\nexport * from './four';",
        );

        assert_eq!(index.imports.len(), 2);
        assert_eq!(
            index.imports[0].specifier_range,
            Utf16Range {
                start: Utf16Position::new(0, 22),
                end: Utf16Position::new(0, 29)
            }
        );
        assert_eq!(
            index.imports[1].specifier_range,
            Utf16Range {
                start: Utf16Position::new(1, 7),
                end: Utf16Position::new(1, 14)
            }
        );
        assert_eq!(index.static_module_at(2, 25), Some("./three"));
        assert_eq!(index.static_module_at(3, 16), Some("./four"));
        assert!(index.static_module_at(0, 20).is_none());
    }

    #[test]
    fn identifies_only_named_reexport_specifiers_at_their_identifiers() {
        let index = DocumentIndex::parse(
            "barrel.d.ts",
            "export { Local as Public } from './dep.js';\nexport type { Typed } from './types.js';\nexport * from './star.js';\nexport * as namespace from './namespace.js';\nexport { Broken as } from './broken.js';",
        );

        for column in [9, 18] {
            let export = index.named_reexport_at(0, column).unwrap();
            assert_eq!(export.name.as_deref(), Some("Local"));
            assert_eq!(export.exported_name.as_deref(), Some("Public"));
        }
        assert!(index.named_reexport_at(0, 7).is_none());
        assert_eq!(
            index.named_reexport_at(1, 14).unwrap().name.as_deref(),
            Some("Typed")
        );
        assert!(index.named_reexport_at(2, 7).is_none());
        assert!(index.named_reexport_at(3, 7).is_none());
        assert!(index.named_reexport_at(4, 9).is_none());
    }

    #[test]
    fn indexes_only_direct_namespace_members() {
        let index = DocumentIndex::parse(
            "main.tsx",
            "import * as api from './api';\napi.value;\napi.value.deep;\nconst value = 1; ({ value });\nconst View = <api.Component />;",
        );

        assert_eq!(
            index.namespace_member_at(1, 5).unwrap().2.member_name,
            "value"
        );
        assert!(index.namespace_member_at(2, 5).is_none());
        assert!(index.namespace_member_at(3, 20).is_none());
        assert_eq!(
            index.namespace_member_at(4, 21).unwrap().2.member_name,
            "Component"
        );
    }

    #[test]
    fn selects_tsx_and_javascript_grammars() {
        let tsx = DocumentIndex::parse("View.tsx", "export const View = () => <main />;");
        let javascript = DocumentIndex::parse("main.js", "export function run() {}");

        assert_eq!(tsx.language, DocumentLanguage::Tsx);
        assert_eq!(tsx.declarations[0].name, "View");
        assert_eq!(javascript.language, DocumentLanguage::JavaScript);
        assert_eq!(javascript.declarations[0].name, "run");
    }

    #[test]
    fn ranges_use_utf16_columns() {
        let index = DocumentIndex::parse("emoji.ts", "const emoji = '😀'; export const value = 1;");
        let declaration = index
            .declarations
            .iter()
            .find(|item| item.name == "value")
            .expect("exported declaration");

        assert_eq!(declaration.range.start, Utf16Position::new(0, 33));
        assert_eq!(declaration.range.end, Utf16Position::new(0, 42));
    }

    #[test]
    fn reports_recovered_syntax_errors() {
        let index = DocumentIndex::parse("broken.ts", "export function broken( {");

        assert!(!index.diagnostics.is_empty());
        assert!(
            index
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.range.start <= diagnostic.range.end)
        );
    }

    #[test]
    fn reports_only_provable_type_errors() {
        let index = DocumentIndex::parse(
            "types.ts",
            "type Box<T> = { value: T };\n\
             interface User { name: string }\n\
             function take(value: string) {}\n\
             const numberValue: number = 'wrong';\n\
             const user: User = { name: 'ok' };\n\
             user.missing;\n\
             take(1);\n\
             take();\n\
             const box: Box<number> = { value: 'wrong' };\n\
             const unknown = unsupported?.value;",
        );
        let diagnostics: Vec<_> = index
            .type_diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect();
        assert_eq!(
            diagnostics,
            ["TS2322", "TS2339", "TS2322", "TS2554", "TS2322"]
        );
        assert!(
            index
                .type_diagnostics
                .iter()
                .all(|diagnostic| diagnostic.range.start <= diagnostic.range.end)
        );
    }

    #[test]
    fn resolves_shadowed_block_variables() {
        let index = DocumentIndex::parse(
            "scope.ts",
            "const value = 1;\n{ const value = 2; value; }\nvalue;",
        );

        let inner = index.symbol_at(1, 20).expect("inner reference");
        assert_eq!(inner.name, "value");
        assert_eq!(inner.kind, DeclarationKind::Variable);
        assert_eq!(inner.declaration.start, Utf16Position::new(1, 8));
        assert_eq!(inner.occurrences.len(), 2);

        let outer = index.symbol_at(2, 0).expect("outer reference");
        assert_eq!(outer.declaration.start, Utf16Position::new(0, 6));
        assert_eq!(outer.occurrences.len(), 2);
    }

    #[test]
    fn resolves_parameter_before_outer_binding() {
        let index = DocumentIndex::parse(
            "parameters.ts",
            "const item = 1;\nfunction use(item: number) { return item; }\nitem;",
        );

        let parameter = index.symbol_at(1, 36).expect("parameter reference");
        assert_eq!(parameter.kind, DeclarationKind::Parameter);
        assert_eq!(parameter.declaration.start, Utf16Position::new(1, 13));
        assert_eq!(
            index
                .symbol_at(2, 0)
                .expect("outer reference")
                .declaration
                .start,
            Utf16Position::new(0, 6)
        );
    }

    #[test]
    fn resolves_migration_table_callback_parameter() {
        let index = DocumentIndex::parse(
            "create_roles_table.ts",
            "export default class {\n  async up() {\n    this.schema.createTable(this.tableName, (table) => {\n      table.uuid('id').primary()\n    })\n  }\n}\n",
        );
        let table = index.symbol_at(3, 7).expect("table reference");
        assert_eq!(table.kind, DeclarationKind::Parameter);
        assert_eq!(table.declaration.start, Utf16Position::new(2, 45));

        let bare = DocumentIndex::parse("callback.ts", "const create = table => table.uuid()");
        let table = bare.symbol_at(0, 24).expect("bare table reference");
        assert_eq!(table.kind, DeclarationKind::Parameter);
        assert_eq!(table.declaration.start, Utf16Position::new(0, 15));

        let typed = DocumentIndex::parse(
            "callback.ts",
            "import { BaseSchema } from './schema'; class Migration extends BaseSchema { up() { this.schema.createTable('roles', (table) => { table.string('name'); }); } }",
        );
        let use_ = typed
            .type_member_uses
            .iter()
            .find(|use_| use_.member_name == "string")
            .expect("table string member");
        assert_eq!(use_.member_name, "string");
        assert!(matches!(
            use_.target,
            TypeMemberTarget::SuperMemberCallback { .. }
        ));

        let inherited = DocumentIndex::parse(
            "knex.d.ts",
            "interface CreateTableBuilder extends TableBuilder {} interface TableBuilder { uuid(name: string): void; }",
        );
        assert_eq!(
            inherited
                .type_member("CreateTableBuilder", "uuid")
                .expect("inherited member")
                .name,
            "uuid"
        );
    }

    #[test]
    fn indexes_getter_namespace_type_reference() {
        let index = DocumentIndex::parse(
            "schema.d.ts",
            "import type { Knex } from 'knex'; export declare class BaseSchema { get schema(): Knex.SchemaBuilder; }",
        );
        let schema = index.type_member("BaseSchema", "schema").unwrap();
        assert_eq!(
            schema.type_reference,
            Some(("Knex".to_owned(), "SchemaBuilder".to_owned()))
        );
        let (_, namespace_binding) = index
            .type_import_binding_at(0, 82)
            .expect("qualified type namespace import");
        assert_eq!(namespace_binding.local_name, "Knex");
        let (import, binding, member) = index
            .qualified_type_import_at(0, 95)
            .expect("qualified imported type");
        assert_eq!(import.specifier, "knex");
        assert_eq!(binding.local_name, "Knex");
        assert_eq!(member, "SchemaBuilder");
    }

    #[test]
    fn prefers_interface_for_merged_type_declaration() {
        let text = "declare namespace Knex { export class SchemaBuilder {} interface SchemaBuilder { createTable(): void; } }";
        let index = DocumentIndex::parse("knex.d.ts", text);
        let range = index
            .unique_type_declaration_range("SchemaBuilder")
            .expect("merged interface declaration");
        assert_eq!(range.start, Utf16Position::new(0, 65));
    }

    #[test]
    fn indexes_exported_ambient_namespace() {
        let index = DocumentIndex::parse(
            "knex.d.ts",
            "declare namespace Knex { interface SchemaBuilder { createTable(): void; } } export { Knex };",
        );
        assert_eq!(
            index
                .exported_declaration("Knex")
                .map(|declaration| declaration.kind),
            Some(DeclarationKind::Namespace)
        );
    }

    #[test]
    fn indexes_export_assignment_ambient_namespace() {
        let index = DocumentIndex::parse(
            "knex.d.ts",
            "declare function knex(): Knex.Knex; declare namespace Knex { interface SchemaBuilder {} } export = knex;",
        );
        assert_eq!(
            index
                .export_assignment_namespace("Knex")
                .map(|declaration| declaration.kind),
            Some(DeclarationKind::Namespace)
        );
    }

    #[test]
    fn indexes_exported_declared_class() {
        let index = DocumentIndex::parse(
            "schema.d.ts",
            "export declare class BaseSchema { get schema(): unknown; }",
        );
        assert_eq!(
            index
                .exported_declaration("BaseSchema")
                .map(|declaration| declaration.kind),
            Some(DeclarationKind::Class)
        );
    }

    #[test]
    fn resolves_unicode_utf16_positions() {
        let index = DocumentIndex::parse("unicode.ts", "const rocket = '😀'; rocket;");

        let symbol = index.symbol_at(0, 21).expect("UTF-16 reference");
        assert_eq!(symbol.name, "rocket");
        assert_eq!(symbol.declaration.start, Utf16Position::new(0, 6));
        assert_eq!(symbol.occurrences[1].start, Utf16Position::new(0, 21));
    }

    #[test]
    fn excludes_member_and_property_keys() {
        let index =
            DocumentIndex::parse("properties.ts", "const value = 1; ({ value }); obj.value;");

        assert!(index.symbol_at(0, 22).is_none());
        assert!(index.symbol_at(0, 35).is_none());
        assert!(index.symbol_at(0, 8).is_some());
    }

    #[test]
    fn resolves_direct_import_binding_occurrences_and_local_export_aliases() {
        let importer = DocumentIndex::parse(
            "main.ts",
            "import { source as local } from './dep';\nlocal();\n",
        );
        let binding = &importer.imports[0].bindings[0];
        let symbol = importer
            .import_binding_occurrences(binding)
            .expect("import occurrences");
        assert_eq!(binding.range.start, Utf16Position::new(0, 19));
        assert_eq!(symbol.occurrences.len(), 2);

        let target = DocumentIndex::parse(
            "dep.ts",
            "const source = () => {};\nexport { source as publicName };\n",
        );
        assert_eq!(
            target
                .exported_declaration("publicName")
                .expect("aliased export")
                .name,
            "source"
        );
    }

    #[test]
    fn indexes_type_imports_and_direct_annotated_members() {
        let index = DocumentIndex::parse(
            "main.ts",
            "import type { User as Person } from './types';\n\
             import { type Account } from './types';\n\
             interface Local { name: string; run(): void }\n\
             type Record = { title: string };\n\
             class Box { value: Local; show() { return this.value.name; } }\n\
             const user: Local = {} as Local; user.name;\n\
             function use(record: Record) { record.title; }\n\
             const optional: Local | undefined = undefined; optional?.name;\n\
             user['name']; user.name.deep;\n",
        );

        assert!(index.imports[0].bindings[0].type_only);
        assert!(index.imports[1].bindings[0].type_only);
        assert!(matches!(
            index.type_member_at(5, 40).unwrap().target,
            TypeMemberTarget::Named(_)
        ));
        assert_eq!(index.type_member_at(6, 40).unwrap().member_name, "title");
        assert_eq!(index.type_member_at(4, 54).unwrap().member_name, "name");
        assert!(index.type_member_at(7, 63).is_none());
        assert!(index.type_member_at(8, 5).is_none());
        assert!(index.type_member_at(8, 24).is_none());
        assert_eq!(
            index.type_member("Local", "name").unwrap().range.start,
            Utf16Position::new(2, 18)
        );
        assert_eq!(
            index.type_member("Record", "title").unwrap().range.start,
            Utf16Position::new(3, 16)
        );
    }

    #[test]
    fn indexes_direct_inline_object_annotation_members() {
        let index = DocumentIndex::parse(
            "main.ts",
            "const value: { name: string; run(): void } = {} as never;\n\
             value.name; value?.name; value[\"name\"]; value?.[\"name\"];\n\
             function use(parameter: { name: string }) { parameter.name; }\n\
             class Box { field: { name: string }; show() { return this.field.name; } }\n\
             value.name.deep; value[key]; value[`name`]; values.name;\n\
             const values: { name: string } = {} as never;",
        );

        for (line, column, declaration) in [
            (1, 6, Utf16Position::new(0, 15)),
            (1, 19, Utf16Position::new(0, 15)),
            (1, 32, Utf16Position::new(0, 15)),
            (1, 48, Utf16Position::new(0, 15)),
            (2, 56, Utf16Position::new(2, 26)),
            (3, 65, Utf16Position::new(3, 21)),
        ] {
            let usage = index
                .type_member_at(line, column)
                .unwrap_or_else(|| panic!("direct member use at {line}:{column}"));
            assert_eq!(usage.member_name, "name");
            assert_eq!(
                index.type_member_target(usage).unwrap().range.start,
                declaration,
                "cursor {line}:{column}"
            );
        }

        let usage = index.type_member_at(4, 6).expect("first chained member");
        assert_eq!(usage.member_name, "name");
        assert_eq!(
            index.type_member_target(usage).unwrap().range.start,
            Utf16Position::new(0, 15)
        );
        for (line, column) in [(4, 11), (4, 24), (4, 35), (4, 45)] {
            assert!(
                index.type_member_at(line, column).is_none(),
                "cursor {line}:{column}"
            );
        }

        let duplicates = DocumentIndex::parse(
            "duplicates.ts",
            "const value: { name: string } = {} as never;\n\
             const value: { name: number } = {} as never;\n\
             value.name;",
        );
        assert!(duplicates.type_member_at(2, 6).is_none());
    }

    #[test]
    fn type_member_bindings_are_lexical_and_unions_are_conservative() {
        let index = DocumentIndex::parse(
            "scope.ts",
            "interface User { name: string }\n\
             interface Other { name: string }\n\
             const value: User = {} as never;\n\
             { const value: Other = {} as never; value.name; }\n\
             value.name;\n\
             { interface User { title: string } const local: User = {} as never; local.title; }\n\
             const same: User | User = {} as never; same.name;\n\
             const different: User | Other = {} as never; different.name;\n\
             const inline: { name: string } | { name: string } = {} as never; inline.name;\n\
             const missing: User | undefined = {} as never; missing.name;",
        );

        let uses = &index.type_member_uses;
        let name_targets = uses
            .iter()
            .filter(|use_| use_.member_name == "name")
            .map(|use_| &use_.target)
            .collect::<Vec<_>>();
        assert_ne!(name_targets[0], name_targets[1]);
        assert!(matches!(
            uses.iter()
                .find(|use_| use_.member_name == "title")
                .unwrap()
                .target,
            TypeMemberTarget::Named(_)
        ));
        assert_eq!(
            name_targets
                .iter()
                .filter(|target| matches!(target, TypeMemberTarget::Union(_)))
                .count(),
            3
        );
        assert_eq!(uses.len(), 6);
    }

    #[test]
    fn indexes_direct_this_members_of_imported_superclasses() {
        let index = DocumentIndex::parse(
            "main.ts",
            "import { Base as Parent } from './base';\nimport type { TypedBase } from './base';\nclass Child extends Parent { use() { this.schema; this?.schema; this[\"schema\"]; this.schema.createTable; this[key]; } }\nclass TypedChild extends TypedBase { use() { this.schema; } }\nclass Local {}\nclass LocalChild extends Local { use() { this.schema; } }\nexport default class extends Parent { use() { return this.schema; } }",
        );
        for column in [42, 56, 69, 85] {
            assert!(matches!(
                index.type_member_at(2, column),
                Some(TypeMemberUse {
                    target: TypeMemberTarget::SuperImported(range),
                    member_name,
                    ..
                }) if *range == index.imports[0].bindings[0].range && member_name == "schema"
            ));
        }
        assert!(matches!(
            index.type_member_at(2, 92),
            Some(TypeMemberUse {
                target: TypeMemberTarget::SuperMember { .. },
                member_name,
                ..
            }) if member_name == "createTable"
        ));
        for (line, column) in [(2, 104), (3, 52), (5, 47)] {
            assert!(
                index.type_member_at(line, column).is_none(),
                "cursor {line}:{column}"
            );
        }
        assert!(matches!(
            index.type_member_at(6, 59),
            Some(TypeMemberUse {
                target: TypeMemberTarget::SuperImported(_),
                ..
            })
        ));
    }
}
