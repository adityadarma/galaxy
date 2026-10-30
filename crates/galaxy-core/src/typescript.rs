//! Per-file TypeScript and JavaScript syntax indexing.
//!
//! This is deliberately syntactic: it does not resolve modules, infer types, or
//! attempt reference finding. All locations use LSP-compatible UTF-16 columns.

use std::path::{Path, PathBuf};

use tree_sitter::{Language, Node, Parser};

use crate::position::Utf16Position;

mod ambient;
mod diagnostics;
mod enums;
mod framework;
mod imports;
mod inheritance;
mod members;
mod symbols;
mod syntax;

pub use self::ambient::ambient_global_names;
use self::ambient::collect_global_names;
use self::diagnostics::*;
use self::enums::*;
use self::imports::*;
use self::inheritance::*;
use self::members::*;
use self::symbols::*;
use self::syntax::*;

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

fn module_keyword_before(specifier: Utf16Range, position: Utf16Position) -> bool {
    specifier.start.line == position.line
        && position <= specifier.start
        && specifier.start.col.saturating_sub(position.col) <= 6
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

/// An object key passed to a directly imported decorator invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoratorOptionUse {
    pub binding_range: Utf16Range,
    /// The directly called member, when the imported binding is an object.
    pub member_name: Option<String>,
    /// True for `@decorator({...})`, false for an ordinary `call({...})`.
    pub in_decorator: bool,
    pub key_name: String,
    pub key_range: Utf16Range,
}

/// The directly stated option shape of a callable type alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoratorOptionType {
    pub members: Vec<TypeMember>,
    pub references: Vec<String>,
}

/// An explicit named callable type annotation on a variable declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableTypeAnnotation {
    pub name: String,
    pub type_name: String,
}

/// An explicit named callable type annotation on a direct object member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableMemberTypeAnnotation {
    pub object_name: String,
    pub member_name: String,
    pub type_name: String,
}

/// The declaration target of a direct type member use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeMemberTarget {
    Named(Utf16Range),
    Imported(Utf16Range),
    /// A static member (an enum member) reached through a value import. The
    /// host resolves the export and accepts it only when it is an enum.
    ImportedValue(Utf16Range),
    /// A static member reached through a named or default import of a class.
    ImportedClass(Utf16Range),
    /// A method on an instance exported by a module factory such as `Env.create`.
    ImportedFactory(Utf16Range),
    /// A member of a named value import whose declaration has a type
    /// annotation, e.g. Inertia's `export declare const router: Router`.
    ImportedInstance(Utf16Range),
    /// A direct member of `this` in a class extending this value import.
    SuperImported(Utf16Range),
    /// A direct member of `this` in a class extending a local superclass.
    SuperLocal(Utf16Range),
    /// A member reached through one or more fluent method calls on an imported superclass.
    SuperImportedChain {
        binding: Utf16Range,
        chain: Vec<String>,
    },
    /// A member reached through one or more fluent method calls on a local superclass.
    SuperLocalChain {
        declaration: Utf16Range,
        chain: Vec<String>,
    },
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
    /// A member of `type_name` declared by the module that `specifier`
    /// names, resolved from the importing file like any import. Emitted by
    /// framework adapters from a public package entry, never an internal path.
    PackageDeclaration {
        specifier: &'static str,
        type_name: &'static str,
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
    /// Names referenced (as values or types) that nothing in this file
    /// declares or imports. Globals are included; the host matches these
    /// against workspace exports to report missing imports.
    pub unresolved_references: Vec<(String, Utf16Range)>,
    /// Names this file adds to the global scope: every top-level declaration
    /// of a script, or the `declare global` blocks and `export as namespace`
    /// of a module.
    pub global_names: Vec<String>,
    export_assignment: bool,
    namespace_members: Vec<NamespaceMemberUse>,
    symbols: Vec<SymbolOccurrences>,
    import_symbols: Vec<SymbolOccurrences>,
    type_import_references: Vec<(Utf16Range, Utf16Range)>,
    qualified_type_imports: Vec<QualifiedTypeImport>,
    type_member_uses: Vec<TypeMemberUse>,
    type_declarations: Vec<TypeDeclaration>,
    default_export_factory_import: Option<Utf16Range>,
    decorator_option_uses: Vec<DecoratorOptionUse>,
    decorator_option_types: Vec<(String, DecoratorOptionType)>,
    callable_type_annotations: Vec<CallableTypeAnnotation>,
    function_option_types: Vec<(String, String)>,
    callable_member_type_annotations: Vec<CallableMemberTypeAnnotation>,
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
        let decorator_option_types = collect_decorator_option_types(root, text, &locations);
        let callable_type_annotations = collect_callable_type_annotations(root, text);
        let function_option_types = collect_function_option_types(root, text);
        let callable_member_type_annotations = collect_callable_member_type_annotations(root, text);
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
        let default_export_factory_import =
            collect_default_export_factory_import(root, text, &imports);
        let inherited_member_uses = collect_inherited_member_uses(
            root,
            text,
            &locations,
            &imports,
            &symbols,
            &import_symbols,
        );
        let enum_uses = collect_enum_member_uses(
            root,
            text,
            &locations,
            &imports,
            &type_declarations,
            &symbols,
            &import_symbols,
        );
        let decorator_option_uses =
            collect_decorator_option_uses(root, text, &locations, &imports, &import_symbols);
        let framework_uses = framework::collect_uses(
            root,
            &framework::Context {
                text,
                locations: &locations,
                imports: &imports,
                import_symbols: &import_symbols,
            },
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
        type_member_uses.extend(enum_uses);
        type_member_uses.extend(framework_uses);

        let type_diagnostics = type_diagnostics(&path, text, root, &locations);
        Self {
            path,
            language,
            declarations,
            imports,
            dynamic_imports,
            exports,
            diagnostics: diagnostics(root, &locations),
            type_diagnostics,
            unresolved_references: unresolved_references(text, root, &locations),
            global_names: collect_global_names(root, text),
            export_assignment,
            namespace_members,
            symbols,
            import_symbols,
            type_import_references,
            qualified_type_imports,
            type_member_uses,
            type_declarations,
            default_export_factory_import,
            decorator_option_uses,
            decorator_option_types,
            callable_type_annotations,
            function_option_types,
            callable_member_type_annotations,
        }
    }

    /// Whether the file has a top-level import or export, which keeps its
    /// declarations out of the global scope.
    pub fn is_module(&self) -> bool {
        !self.imports.is_empty() || !self.exports.is_empty() || self.export_assignment
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

    /// Returns a named/default import used in a type position at the cursor,
    /// including a property or parameter annotation. TypeScript permits a
    /// regular import for interfaces in declaration files.
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

    /// Returns a module specifier when the cursor is on the `from` keyword just
    /// before it. Narrower than `module_statement_at`: bindings and names in the
    /// statement keep their own resolvers and fail closed.
    pub fn module_keyword_at(&self, line: usize, col: usize) -> Option<&str> {
        let position = Utf16Position::new(line, col);
        self.imports
            .iter()
            .find(|import| module_keyword_before(import.specifier_range, position))
            .map(|import| import.specifier.as_str())
            .or_else(|| {
                self.exports.iter().find_map(|export| {
                    (export.syntax_valid
                        && export
                            .specifier_range
                            .is_some_and(|range| module_keyword_before(range, position)))
                    .then_some(export.specifier.as_deref())
                    .flatten()
                })
            })
    }

    /// Returns a module specifier when the cursor is on its full import/export
    /// statement, including the `from` keyword before the string literal.
    pub fn module_statement_at(&self, line: usize, col: usize) -> Option<&str> {
        let position = Utf16Position::new(line, col);
        self.imports
            .iter()
            .find(|import| {
                contains(import.range, position)
                    || module_keyword_before(import.specifier_range, position)
            })
            .map(|import| import.specifier.as_str())
            .or_else(|| {
                self.exports.iter().find_map(|export| {
                    (export.syntax_valid
                        && export.specifier.is_some()
                        && (contains(export.range, position)
                            || export
                                .specifier_range
                                .is_some_and(|range| module_keyword_before(range, position))))
                    .then_some(export.specifier.as_deref())
                    .flatten()
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
        let exports = self
            .exports
            .iter()
            .filter(|item| {
                item.exported_name.as_deref() == Some(exported_name)
                    && item.specifier.is_none()
                    && !item.type_only
            })
            .collect::<Vec<_>>();
        // Overloads export one name several times, e.g. Inertia's
        // `export default function useForm(...)` signatures.
        let name = exports.first()?.name.as_deref()?;
        if exports
            .iter()
            .any(|item| item.name.as_deref() != Some(name))
        {
            return None;
        }
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
        match declarations.as_slice() {
            [declaration] => Some(declaration),
            // Overload signatures: the first one is the definition target.
            [first, rest @ ..]
                if rest.iter().all(|item| {
                    item.kind == first.kind && first.kind == DeclarationKind::Function
                }) =>
            {
                Some(first)
            }
            _ => None,
        }
    }

    /// Returns one unambiguous runtime declaration by name. Used as a
    /// navigation fallback while a target module has incomplete export syntax.
    pub fn unique_runtime_declaration(&self, name: &str) -> Option<&Declaration> {
        let declarations = self
            .declarations
            .iter()
            .filter(|declaration| {
                declaration.name == name
                    && !matches!(
                        declaration.kind,
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

    pub fn default_export_factory_import(&self) -> Option<(&Import, &ImportBinding)> {
        self.import_binding_by_range(self.default_export_factory_import?)
    }

    /// Returns the module supplying a re-exported default value.
    pub fn default_export_reexport_specifier(&self) -> Option<&str> {
        self.exports
            .iter()
            .find(|export| export.default && export.specifier.is_some())
            .and_then(|export| export.specifier.as_deref())
    }

    /// Returns an object option key used with a directly imported decorator.
    pub fn decorator_option_at(&self, line: usize, col: usize) -> Option<&DecoratorOptionUse> {
        let position = Utf16Position::new(line, col);
        self.decorator_option_uses
            .iter()
            .find(|use_| contains(use_.key_range, position))
    }

    /// Returns a callable type alias's directly stated option members and references.
    pub fn decorator_option_type(&self, name: &str) -> Option<&DecoratorOptionType> {
        let types = self
            .decorator_option_types
            .iter()
            .filter(|(candidate, _)| candidate == name)
            .map(|(_, option)| option)
            .collect::<Vec<_>>();
        if types.len() == 1 {
            Some(types[0])
        } else {
            None
        }
    }

    /// First-parameter type names of each declaration or overload of a function,
    /// in source order.
    pub fn function_option_types(&self, name: &str) -> Vec<&str> {
        self.function_option_types
            .iter()
            .filter(|(function, _)| function == name)
            .map(|(_, type_name)| type_name.as_str())
            .collect()
    }

    /// Returns a variable's direct named callable type annotation.
    pub fn callable_type_annotation(&self, name: &str) -> Option<&str> {
        let annotations = self
            .callable_type_annotations
            .iter()
            .filter(|annotation| annotation.name == name)
            .collect::<Vec<_>>();
        if annotations.len() == 1 {
            Some(annotations[0].type_name.as_str())
        } else {
            None
        }
    }

    /// Returns an object's direct member callable type annotation.
    pub fn callable_member_type_annotation(
        &self,
        object_name: &str,
        member_name: &str,
    ) -> Option<&str> {
        let annotations = self
            .callable_member_type_annotations
            .iter()
            .filter(|annotation| {
                annotation.object_name == object_name && annotation.member_name == member_name
            })
            .collect::<Vec<_>>();
        if annotations.len() == 1 {
            Some(annotations[0].type_name.as_str())
        } else {
            None
        }
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

    /// Direct parent type names declared by one unambiguous local class.
    pub fn type_parents(&self, name: &str) -> Option<&[String]> {
        let declarations = self
            .type_declarations
            .iter()
            .filter(|item| item.name == name && item.kind == DeclarationKind::Class)
            .collect::<Vec<_>>();
        (declarations.len() == 1).then(|| declarations[0].extends.as_slice())
    }

    /// A unique named/default value import by its local binding name.
    pub fn value_import(&self, name: &str) -> Option<(&Import, &ImportBinding)> {
        let bindings = self
            .imports
            .iter()
            .flat_map(|import| import.bindings.iter().map(move |binding| (import, binding)))
            .filter(|(_, binding)| {
                binding.local_name == name
                    && !binding.type_only
                    && matches!(binding.kind, ImportKind::Named | ImportKind::Default)
            })
            .collect::<Vec<_>>();
        (bindings.len() == 1).then(|| bindings[0])
    }

    pub fn type_declaration_at(&self, range: Utf16Range) -> Option<&[TypeMember]> {
        self.type_declarations
            .iter()
            .find(|item| item.range == range)
            .map(|item| item.members.as_slice())
    }

    /// Direct members declared by local classes, interfaces, and object types.
    /// Completion filters these by the receiver and prefix at the call site.
    pub fn local_type_members(&self) -> impl Iterator<Item = &TypeMember> {
        self.type_declarations
            .iter()
            .flat_map(|declaration| declaration.members.iter())
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

    pub fn type_member_by_declaration(
        &self,
        declaration_range: Utf16Range,
        member_name: &str,
    ) -> Option<&TypeMember> {
        let declaration = self
            .type_declarations
            .iter()
            .find(|item| item.range == declaration_range)?;
        self.type_member(&declaration.name, member_name)
    }

    pub fn type_name_by_declaration(&self, declaration_range: Utf16Range) -> Option<&str> {
        self.type_declarations
            .iter()
            .find(|item| item.range == declaration_range)
            .map(|item| item.name.as_str())
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
            | TypeMemberTarget::ImportedValue(_)
            | TypeMemberTarget::ImportedClass(_)
            | TypeMemberTarget::ImportedFactory(_)
            | TypeMemberTarget::ImportedInstance(_)
            | TypeMemberTarget::SuperImported(_)
            | TypeMemberTarget::SuperLocal(_)
            | TypeMemberTarget::SuperImportedChain { .. }
            | TypeMemberTarget::SuperLocalChain { .. }
            | TypeMemberTarget::SuperMember { .. }
            | TypeMemberTarget::SuperMemberCallback { .. }
            | TypeMemberTarget::PackageDeclaration { .. }
            | TypeMemberTarget::Union(_) => None,
        }
    }
}

struct Locations<'a> {
    _text: &'a str,
    line_starts: Vec<usize>,
    utf16_offsets: Vec<usize>,
}

impl<'a> Locations<'a> {
    fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(offset, _)| offset + 1));
        let mut utf16_offsets = vec![0; text.len() + 1];
        let mut utf16 = 0;
        for (byte, character) in text.char_indices() {
            let end = byte + character.len_utf8();
            utf16_offsets[byte..end].fill(utf16);
            utf16 += character.len_utf16();
        }
        utf16_offsets[text.len()] = utf16;
        Self {
            _text: text,
            line_starts,
            utf16_offsets,
        }
    }

    fn position(&self, byte: usize) -> Utf16Position {
        let byte = byte.min(self.utf16_offsets.len() - 1);
        let line = self.line_starts.partition_point(|start| *start <= byte) - 1;
        let start = self.line_starts[line];
        Utf16Position::new(line, self.utf16_offsets[byte] - self.utf16_offsets[start])
    }

    fn range(&self, node: Node<'_>) -> Utf16Range {
        Utf16Range {
            start: self.position(node.start_byte()),
            end: self.position(node.end_byte()),
        }
    }
}

#[cfg(test)]
mod tests;
