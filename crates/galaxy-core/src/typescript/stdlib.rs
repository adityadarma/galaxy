//! Index of the official TypeScript `lib.*.d.ts` declarations.
//!
//! Library selection belongs here too, but is intentionally separate from the
//! declaration index. A [`StdLib`] is built from explicit root files and follows
//! their `/// <reference lib="..." />` directives in dependency-first order.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tree_sitter::Node;

use super::lib_files;
use super::*;
use crate::javascript::{lexical_normalize, referenced_tsconfig_for};
use crate::jsonc::Value as JsoncValue;

const MAX_PROJECT_ANCESTORS: usize = 64;
const MAX_TSCONFIG_EXTENDS: usize = 32;

/// A declaration site inside one TypeScript standard-library file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibLocation {
    pub file_name: String,
    pub range: Utf16Range,
}

/// The argument bounds of one call or construct signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibSignature {
    pub required_parameters: usize,
    /// `None` means that the signature has a rest parameter.
    pub maximum_parameters: Option<usize>,
    pub return_type: Option<String>,
}

/// All declarations of one property or method on a library type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibMember {
    pub locations: Vec<LibLocation>,
    pub signatures: Vec<LibSignature>,
    pub type_name: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct LibType {
    locations: Vec<LibLocation>,
    extends: Vec<String>,
    members: HashMap<String, LibMember>,
    calls: Vec<LibSignature>,
    constructs: Vec<LibSignature>,
}

#[derive(Debug, Clone, Default)]
struct LibValue {
    locations: Vec<LibLocation>,
    type_name: Option<String>,
    signatures: Vec<LibSignature>,
    members: HashMap<String, LibMember>,
}

/// Parsed declarations from a selected set of TypeScript library files.
#[derive(Debug, Clone, Default)]
pub struct StdLib {
    files: Vec<String>,
    source_directory: Option<PathBuf>,
    complete: bool,
    values: HashMap<String, LibValue>,
    types: HashMap<String, LibType>,
}

impl StdLib {
    /// Version of the bundled official TypeScript declarations.
    pub const fn bundled_version() -> &'static str {
        lib_files::VERSION
    }

    /// Builds an index from vendored TypeScript 5.9.3 library roots.
    ///
    /// References are loaded before the file that declares them. Missing root
    /// or referenced files are ignored so callers can use the same API with an
    /// older project-local TypeScript installation later.
    pub fn bundled(roots: &[&str]) -> Self {
        Self::from_loader(roots, None, |name| bundled_text(name).map(str::to_owned))
    }

    /// A broad editor fallback for files that do not belong to a configured
    /// TypeScript project.
    pub fn bundled_default() -> Self {
        Self::bundled(&["lib.esnext.full.d.ts"])
    }

    /// Resolves the standard library for `path` from its tsconfig/jsconfig.
    /// A project-local `node_modules/typescript/lib` wins over the bundled
    /// TypeScript 5.9.3 declarations.
    pub fn for_file(path: &Path) -> Self {
        let config = config_for_file(path);
        let options = config
            .as_deref()
            .map(load_compiler_options)
            .unwrap_or_default();
        let roots = root_libs(config.is_some(), &options);
        if roots.is_empty() {
            return Self::default();
        }

        let start = config
            .as_deref()
            .and_then(Path::parent)
            .or_else(|| path.parent())
            .unwrap_or_else(|| Path::new("."));
        if let Some(directory) = typescript_lib_directory(start) {
            if !roots.iter().all(|root| directory.join(root).is_file()) {
                let root_refs = roots.iter().map(String::as_str).collect::<Vec<_>>();
                return Self::bundled(&root_refs);
            }
            let root_refs = roots.iter().map(String::as_str).collect::<Vec<_>>();
            let library = Self::from_loader(&root_refs, Some(directory.clone()), |name| {
                std::fs::read_to_string(directory.join(name)).ok()
            });
            // An incomplete installation should not create false missing-global
            // errors. Fall back as a whole instead of mixing TS versions.
            if library.complete && !library.files.is_empty() {
                return library;
            }
        }
        let root_refs = roots.iter().map(String::as_str).collect::<Vec<_>>();
        Self::bundled(&root_refs)
    }

    fn from_loader(
        roots: &[&str],
        source_directory: Option<PathBuf>,
        mut load: impl FnMut(&str) -> Option<String>,
    ) -> Self {
        fn visit(
            name: String,
            load: &mut impl FnMut(&str) -> Option<String>,
            visiting: &mut HashSet<String>,
            loaded: &mut HashSet<String>,
            files: &mut Vec<(String, String)>,
            complete: &mut bool,
        ) {
            if loaded.contains(&name) || !visiting.insert(name.clone()) {
                return;
            }
            let Some(text) = load(&name) else {
                *complete = false;
                visiting.remove(&name);
                return;
            };
            for reference in lib_references(&text) {
                visit(
                    lib_file_name(&reference),
                    load,
                    visiting,
                    loaded,
                    files,
                    complete,
                );
            }
            visiting.remove(&name);
            loaded.insert(name.clone());
            files.push((name, text));
        }

        let mut sources = Vec::new();
        let mut visiting = HashSet::new();
        let mut loaded = HashSet::new();
        let mut complete = true;
        for root in roots {
            visit(
                lib_file_name(root),
                &mut load,
                &mut visiting,
                &mut loaded,
                &mut sources,
                &mut complete,
            );
        }

        let mut library = Self {
            source_directory,
            complete,
            ..Self::default()
        };
        for (name, text) in sources {
            library.files.push(name.clone());
            library.index_file(&name, &text);
        }
        library
    }

    /// Library files in dependency-first declaration order.
    pub fn files(&self) -> &[String] {
        &self.files
    }

    /// Directory containing project-local `lib.*.d.ts` files. `None` means the
    /// declarations came from Galaxy's bundled fallback.
    pub fn source_directory(&self) -> Option<&Path> {
        self.source_directory.as_deref()
    }

    /// Whether `name` is available in either the value or type namespace.
    pub fn has_global(&self, name: &str) -> bool {
        self.values.contains_key(name) || self.types.contains_key(name)
    }

    /// Value-space declarations, preferring `declare var`/function/namespace
    /// sites over same-named interface declarations.
    pub fn value_locations(&self, name: &str) -> Vec<LibLocation> {
        self.values
            .get(name)
            .map(|value| value.locations.clone())
            .unwrap_or_default()
    }

    /// Type-space declarations such as interfaces and type aliases.
    pub fn type_locations(&self, name: &str) -> Vec<LibLocation> {
        self.types
            .get(name)
            .map(|type_| type_.locations.clone())
            .unwrap_or_default()
    }

    /// The declared interface/type used by a global value.
    pub fn value_type(&self, name: &str) -> Option<&str> {
        self.values.get(name)?.type_name.as_deref()
    }

    /// Overloads of a global `declare function`.
    pub fn value_signatures(&self, name: &str) -> &[LibSignature] {
        self.values
            .get(name)
            .map(|value| value.signatures.as_slice())
            .unwrap_or_default()
    }

    /// Resolves a member from a namespace/global object or its declared
    /// interface, including inherited interface members.
    pub fn value_member(&self, owner: &str, member: &str) -> Option<LibMember> {
        if let Some(found) = self
            .values
            .get(owner)
            .and_then(|value| value.members.get(member))
        {
            return Some(found.clone());
        }
        let owner = self
            .values
            .get(owner)
            .and_then(|value| value.type_name.as_deref())
            .unwrap_or(owner);
        self.type_member(owner, member)
    }

    /// Resolves a member declared on an interface or inherited interface.
    pub fn type_member(&self, owner: &str, member: &str) -> Option<LibMember> {
        self.type_member_inner(owner, member, &mut HashSet::new())
    }

    /// Call signatures declared directly on an interface.
    pub fn call_signatures(&self, name: &str) -> &[LibSignature] {
        self.types
            .get(name)
            .map(|type_| type_.calls.as_slice())
            .unwrap_or_default()
    }

    /// Construct signatures declared directly on an interface.
    pub fn construct_signatures(&self, name: &str) -> &[LibSignature] {
        self.types
            .get(name)
            .map(|type_| type_.constructs.as_slice())
            .unwrap_or_default()
    }

    fn type_member_inner(
        &self,
        owner: &str,
        member: &str,
        visited: &mut HashSet<String>,
    ) -> Option<LibMember> {
        let owner = base_type_name(owner)?;
        if !visited.insert(owner.clone()) {
            return None;
        }
        let type_ = self.types.get(&owner)?;
        if let Some(found) = type_.members.get(member) {
            return Some(found.clone());
        }
        let inherited = type_
            .extends
            .iter()
            .filter_map(|parent| self.type_member_inner(parent, member, visited))
            .collect::<Vec<_>>();
        merge_members(inherited)
    }

    fn index_file(&mut self, file_name: &str, text: &str) {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_language(DocumentLanguage::TypeScript))
            .expect("TypeScript grammar");
        let Some(tree) = parser.parse(text, None) else {
            return;
        };
        let locations = Locations::new(text);
        let mut cursor = tree.root_node().walk();
        for child in tree.root_node().named_children(&mut cursor) {
            self.index_declaration(file_name, text, &locations, child, None);
        }
    }

    fn index_declaration(
        &mut self,
        file_name: &str,
        text: &str,
        locations: &Locations<'_>,
        node: Node<'_>,
        namespace: Option<&str>,
    ) {
        match node.kind() {
            "ambient_declaration" | "export_statement" | "expression_statement" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.index_declaration(file_name, text, locations, child, namespace);
                }
            }
            "interface_declaration" => {
                self.index_interface(file_name, text, locations, node, namespace)
            }
            "type_alias_declaration" => {
                if namespace.is_none() {
                    self.index_type_location(file_name, text, locations, node);
                }
            }
            "function_declaration" | "function_signature" => {
                self.index_function(file_name, text, locations, node, namespace)
            }
            "variable_declaration" | "lexical_declaration" => {
                self.index_variables(file_name, text, locations, node, namespace)
            }
            "class_declaration" | "abstract_class_declaration" => {
                self.index_class(file_name, text, locations, node, namespace)
            }
            "internal_module" | "module" | "namespace_declaration" => {
                self.index_namespace(file_name, text, locations, node, namespace)
            }
            "statement_block" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.index_declaration(file_name, text, locations, child, namespace);
                }
            }
            _ => {}
        }
    }

    fn index_interface(
        &mut self,
        file_name: &str,
        text: &str,
        locations: &Locations<'_>,
        node: Node<'_>,
        namespace: Option<&str>,
    ) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let Some(name) = node_text(text, name_node) else {
            return;
        };
        let name = qualify(namespace, name);
        let location = location(file_name, locations, name_node);
        let type_ = self.types.entry(name).or_default();
        type_.locations.push(location);

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "extends_type_clause" => {
                    let mut bases = child.walk();
                    for base in child.named_children(&mut bases) {
                        if let Some(base) = node_text(text, base).and_then(base_type_name) {
                            type_.extends.push(qualify(namespace, &base));
                        }
                    }
                }
                "interface_body" | "object_type" | "class_body" => {
                    index_members(file_name, text, locations, child, type_)
                }
                _ => {}
            }
        }
    }

    fn index_type_location(
        &mut self,
        file_name: &str,
        text: &str,
        locations: &Locations<'_>,
        node: Node<'_>,
    ) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let Some(written) = node_text(text, name) else {
            return;
        };
        self.types
            .entry(written.to_owned())
            .or_default()
            .locations
            .push(location(file_name, locations, name));
    }

    fn index_function(
        &mut self,
        file_name: &str,
        text: &str,
        locations: &Locations<'_>,
        node: Node<'_>,
        namespace: Option<&str>,
    ) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let Some(name) = node_text(text, name_node) else {
            return;
        };
        let signature = signature(text, node);
        if let Some(namespace) = namespace {
            let value = self.values.entry(namespace.to_owned()).or_default();
            let member = value.members.entry(name.to_owned()).or_default();
            member
                .locations
                .push(location(file_name, locations, name_node));
            if let Some(signature) = signature {
                member.signatures.push(signature);
            }
        } else {
            let value = self.values.entry(name.to_owned()).or_default();
            value
                .locations
                .push(location(file_name, locations, name_node));
            if let Some(signature) = signature {
                value.signatures.push(signature);
            }
        }
    }

    fn index_variables(
        &mut self,
        file_name: &str,
        text: &str,
        locations: &Locations<'_>,
        node: Node<'_>,
        namespace: Option<&str>,
    ) {
        let mut cursor = node.walk();
        for declarator in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "variable_declarator")
        {
            let Some(name_node) = declarator.child_by_field_name("name") else {
                continue;
            };
            let Some(name) = node_text(text, name_node) else {
                continue;
            };
            let type_node = declarator.child_by_field_name("type");
            if let Some(namespace) = namespace {
                let value = self.values.entry(namespace.to_owned()).or_default();
                let member = value.members.entry(name.to_owned()).or_default();
                member
                    .locations
                    .push(location(file_name, locations, name_node));
                member.type_name = type_node.and_then(|node| type_text(text, node));
            } else {
                let value = self.values.entry(name.to_owned()).or_default();
                value
                    .locations
                    .push(location(file_name, locations, name_node));
                value.type_name = type_node.and_then(|node| type_text(text, node));
                if let Some(type_node) = type_node
                    && matches!(type_node.kind(), "object_type" | "interface_body")
                {
                    let synthetic = format!("typeof {name}");
                    let type_ = self.types.entry(synthetic.clone()).or_default();
                    index_members(file_name, text, locations, type_node, type_);
                    value.type_name = Some(synthetic);
                }
            }
        }
    }

    fn index_class(
        &mut self,
        file_name: &str,
        text: &str,
        locations: &Locations<'_>,
        node: Node<'_>,
        namespace: Option<&str>,
    ) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let Some(name) = node_text(text, name_node) else {
            return;
        };
        let name = qualify(namespace, name);
        let declaration = location(file_name, locations, name_node);
        let type_ = self.types.entry(name.clone()).or_default();
        type_.locations.push(declaration.clone());
        if let Some(body) = node.child_by_field_name("body") {
            index_members(file_name, text, locations, body, type_);
        }
        if namespace.is_none() {
            let value = self.values.entry(name.clone()).or_default();
            value.locations.push(declaration);
            value.type_name = Some(name);
        }
    }

    fn index_namespace(
        &mut self,
        file_name: &str,
        text: &str,
        locations: &Locations<'_>,
        node: Node<'_>,
        parent: Option<&str>,
    ) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let Some(name) = node_text(text, name_node) else {
            return;
        };
        if name == "global" {
            if let Some(body) = node.child_by_field_name("body") {
                self.index_declaration(file_name, text, locations, body, None);
            }
            return;
        }
        let qualified = qualify(parent, name.trim_matches(['\'', '"']));
        self.values
            .entry(qualified.clone())
            .or_default()
            .locations
            .push(location(file_name, locations, name_node));
        if let Some(body) = node.child_by_field_name("body") {
            self.index_declaration(file_name, text, locations, body, Some(&qualified));
        }
    }
}

fn index_members(
    file_name: &str,
    text: &str,
    locations: &Locations<'_>,
    body: Node<'_>,
    type_: &mut LibType,
) {
    let mut cursor = body.walk();
    for member in body.named_children(&mut cursor) {
        match member.kind() {
            "method_signature" | "abstract_method_signature" | "method_definition" => {
                let Some(name_node) = member.child_by_field_name("name") else {
                    continue;
                };
                let Some(name) = member_name(text, name_node) else {
                    continue;
                };
                let indexed = type_.members.entry(name).or_default();
                indexed
                    .locations
                    .push(location(file_name, locations, name_node));
                if let Some(signature) = signature(text, member) {
                    indexed.signatures.push(signature);
                }
            }
            "property_signature" | "public_field_definition" | "field_definition" => {
                let Some(name_node) = member.child_by_field_name("name") else {
                    continue;
                };
                let Some(name) = member_name(text, name_node) else {
                    continue;
                };
                let indexed = type_.members.entry(name).or_default();
                indexed
                    .locations
                    .push(location(file_name, locations, name_node));
                indexed.type_name = member
                    .child_by_field_name("type")
                    .and_then(|node| type_text(text, node));
            }
            "call_signature" => {
                if let Some(signature) = signature(text, member) {
                    type_.calls.push(signature);
                }
            }
            "construct_signature" => {
                if let Some(signature) = signature(text, member) {
                    type_.constructs.push(signature);
                }
            }
            _ => {}
        }
    }
}

fn signature(text: &str, node: Node<'_>) -> Option<LibSignature> {
    let parameters = node.child_by_field_name("parameters")?;
    let mut required = 0;
    let mut maximum = 0;
    let mut has_rest = false;
    let mut cursor = parameters.walk();
    for parameter in parameters.named_children(&mut cursor) {
        if !matches!(
            parameter.kind(),
            "required_parameter" | "optional_parameter"
        ) {
            continue;
        }
        let source = node_text(text, parameter)?.trim();
        let name = parameter
            .child_by_field_name("pattern")
            .or_else(|| parameter.child_by_field_name("name"))
            .and_then(|name| node_text(text, name))
            .unwrap_or_default()
            .trim_start_matches("...");
        if name == "this" || source.starts_with("this:") {
            continue;
        }
        maximum += 1;
        if parameter.kind() == "required_parameter" && !source.starts_with("...") {
            required += 1;
        }
        has_rest |= source.starts_with("...");
    }
    Some(LibSignature {
        required_parameters: required,
        maximum_parameters: (!has_rest).then_some(maximum),
        return_type: node
            .child_by_field_name("return_type")
            .and_then(|return_type| type_text(text, return_type)),
    })
}

fn type_text(text: &str, node: Node<'_>) -> Option<String> {
    let written = node_text(text, node)?.trim().trim_start_matches(':').trim();
    (!written.is_empty()).then(|| written.to_owned())
}

fn member_name(text: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "property_identifier" | "identifier" | "type_identifier" | "number" => {
            node_text(text, node).map(str::to_owned)
        }
        "string" | "string_fragment" => {
            node_text(text, node).map(|name| name.trim_matches(['\'', '"']).to_owned())
        }
        _ => None,
    }
}

fn location(file_name: &str, locations: &Locations<'_>, node: Node<'_>) -> LibLocation {
    LibLocation {
        file_name: file_name.to_owned(),
        range: locations.range(node),
    }
}

fn qualify(namespace: Option<&str>, name: &str) -> String {
    namespace
        .map(|namespace| format!("{namespace}.{name}"))
        .unwrap_or_else(|| name.to_owned())
}

fn base_type_name(written: &str) -> Option<String> {
    let written = written.trim().trim_start_matches("globalThis.");
    let end = written
        .find(|character: char| character == '<' || character.is_whitespace())
        .unwrap_or(written.len());
    let name = written[..end].trim();
    (!name.is_empty()).then(|| name.to_owned())
}

fn merge_members(members: Vec<LibMember>) -> Option<LibMember> {
    if members.is_empty() {
        return None;
    }
    let mut merged = LibMember::default();
    for member in members {
        merged.locations.extend(member.locations);
        merged.signatures.extend(member.signatures);
        if merged.type_name.is_none() {
            merged.type_name = member.type_name;
        }
    }
    Some(merged)
}

#[derive(Debug, Clone, Default)]
struct CompilerOptions {
    lib: Option<Vec<String>>,
    target: Option<String>,
    module: Option<String>,
    no_lib: Option<bool>,
    unresolved_extends: bool,
}

impl CompilerOptions {
    fn apply(&mut self, value: &JsoncValue) {
        let Some(options) = value.get("compilerOptions") else {
            return;
        };
        if let Some(JsoncValue::Array(libs)) = options.get("lib") {
            self.lib = Some(
                libs.iter()
                    .filter_map(JsoncValue::as_str)
                    .map(str::to_ascii_lowercase)
                    .collect(),
            );
        }
        if let Some(target) = options.get("target").and_then(JsoncValue::as_str) {
            self.target = Some(target.to_ascii_lowercase());
        }
        if let Some(module) = options.get("module").and_then(JsoncValue::as_str) {
            self.module = Some(module.to_ascii_lowercase());
        }
        if let Some(JsoncValue::Bool(no_lib)) = options.get("noLib") {
            self.no_lib = Some(*no_lib);
        }
    }
}

fn config_for_file(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    for directory in start.ancestors().take(MAX_PROJECT_ANCESTORS) {
        let tsconfig = directory.join("tsconfig.json");
        let jsconfig = directory.join("jsconfig.json");
        let config = if tsconfig.is_file() {
            tsconfig
        } else if jsconfig.is_file() {
            jsconfig
        } else {
            continue;
        };
        return referenced_tsconfig_for(&config, path).or(Some(config));
    }
    None
}

fn load_compiler_options(path: &Path) -> CompilerOptions {
    fn load(
        path: &Path,
        depth: usize,
        seen: &mut HashSet<PathBuf>,
        options: &mut CompilerOptions,
    ) -> Option<()> {
        if depth == MAX_TSCONFIG_EXTENDS {
            options.unresolved_extends = true;
            return None;
        }
        let path = lexical_normalize(path);
        if !seen.insert(path.clone()) {
            options.unresolved_extends = true;
            return None;
        }
        let source = std::fs::read_to_string(&path).ok()?;
        let value = crate::jsonc::parse(&source).ok()?;
        if !matches!(value, JsoncValue::Object(_)) {
            return None;
        }

        let extended = match value.get("extends") {
            Some(JsoncValue::String(extends)) => vec![extends.as_str()],
            Some(JsoncValue::Array(extends)) => {
                extends.iter().filter_map(JsoncValue::as_str).collect()
            }
            Some(_) => {
                options.unresolved_extends = true;
                Vec::new()
            }
            None => Vec::new(),
        };
        for extends in extended {
            let Some(parent) = resolve_extended_config(path.parent()?, extends) else {
                options.unresolved_extends = true;
                continue;
            };
            let _ = load(&parent, depth + 1, seen, options);
        }
        options.apply(&value);
        Some(())
    }

    let mut options = CompilerOptions::default();
    if load(path, 0, &mut HashSet::new(), &mut options).is_none() {
        options.unresolved_extends = true;
    }
    options
}

fn resolve_extended_config(directory: &Path, extends: &str) -> Option<PathBuf> {
    if extends.is_empty() || extends.contains('\\') || Path::new(extends).is_absolute() {
        return None;
    }
    if extends.starts_with('.') {
        return config_candidate(&directory.join(extends));
    }

    for ancestor in directory.ancestors().take(MAX_PROJECT_ANCESTORS) {
        let candidate = ancestor.join("node_modules").join(extends);
        if let Some(path) = config_candidate(&candidate) {
            return Some(path);
        }
        let Some(package_root) = package_root(&ancestor.join("node_modules"), extends) else {
            continue;
        };
        let package_json = package_root.join("package.json");
        if let Some(config) = std::fs::read_to_string(package_json)
            .ok()
            .and_then(|source| crate::jsonc::parse(&source).ok())
            .and_then(|value| {
                value
                    .get("tsconfig")
                    .and_then(JsoncValue::as_str)
                    .map(str::to_owned)
            })
            .and_then(|config| config_candidate(&package_root.join(config)))
        {
            return Some(config);
        }
        if let Some(config) = config_candidate(&package_root.join("tsconfig.json")) {
            return Some(config);
        }
    }
    None
}

fn package_root(node_modules: &Path, specifier: &str) -> Option<PathBuf> {
    let mut parts = specifier.split('/');
    let first = parts.next()?;
    let package = if first.starts_with('@') {
        format!("{first}/{}", parts.next()?)
    } else {
        first.to_owned()
    };
    let root = node_modules.join(package);
    root.is_dir().then_some(root)
}

fn config_candidate(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(lexical_normalize(path));
    }
    if path.extension().is_none() {
        let json = path.with_extension("json");
        if json.is_file() {
            return Some(lexical_normalize(&json));
        }
    }
    if path.is_dir() {
        let config = path.join("tsconfig.json");
        if config.is_file() {
            return Some(lexical_normalize(&config));
        }
    }
    None
}

fn root_libs(configured: bool, options: &CompilerOptions) -> Vec<String> {
    if options.no_lib == Some(true) {
        return Vec::new();
    }
    if options.unresolved_extends {
        return vec!["lib.esnext.full.d.ts".to_owned()];
    }
    if let Some(libs) = &options.lib {
        return libs.iter().map(|name| lib_file_name(name)).collect();
    }
    if !configured {
        return vec!["lib.esnext.full.d.ts".to_owned()];
    }

    let target = if let Some(target) = options.target.as_deref() {
        target
    } else {
        match options.module.as_deref() {
            Some("node16" | "node18") => "es2022",
            Some("node20") => "es2023",
            Some("nodenext") => "esnext",
            _ => "es5",
        }
    };
    vec![default_lib_for_target(target)]
}

fn default_lib_for_target(target: &str) -> String {
    match target.trim().to_ascii_lowercase().as_str() {
        "es2015" | "es6" => "lib.es6.d.ts".to_owned(),
        "es2016" | "es7" => "lib.es2016.full.d.ts".to_owned(),
        "es2017" | "es2018" | "es2019" | "es2020" | "es2021" | "es2022" | "es2023" | "es2024" => {
            format!("lib.{}.full.d.ts", target.trim().to_ascii_lowercase())
        }
        "esnext" | "latest" => "lib.esnext.full.d.ts".to_owned(),
        _ => "lib.d.ts".to_owned(),
    }
}

fn typescript_lib_directory(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .take(MAX_PROJECT_ANCESTORS)
        .map(|directory| directory.join("node_modules/typescript/lib"))
        .find(|directory| directory.join("lib.d.ts").is_file())
}

fn bundled_text(name: &str) -> Option<&'static str> {
    lib_files::FILES
        .iter()
        .find_map(|(file_name, text)| (*file_name == name).then_some(*text))
}

fn lib_references(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with("///") || !line.contains("<reference") {
                return None;
            }
            let marker = "lib=\"";
            let start = line.find(marker)? + marker.len();
            let end = line[start..].find('"')? + start;
            Some(line[start..end].to_ascii_lowercase())
        })
        .collect()
}

fn lib_file_name(name: &str) -> String {
    let name = name.trim().to_ascii_lowercase();
    if name.starts_with("lib.") && name.ends_with(".d.ts") {
        return name;
    }
    match name.as_str() {
        "es6" => "lib.es2015.d.ts".to_owned(),
        "es7" => "lib.es2016.d.ts".to_owned(),
        "esnext.array" => "lib.es2023.array.d.ts".to_owned(),
        "esnext.symbol" => "lib.es2019.symbol.d.ts".to_owned(),
        "esnext.asynciterable" => "lib.es2018.asynciterable.d.ts".to_owned(),
        "esnext.bigint" => "lib.es2020.bigint.d.ts".to_owned(),
        "esnext.promise" => "lib.es2024.promise.d.ts".to_owned(),
        "esnext.weakref" => "lib.es2021.weakref.d.ts".to_owned(),
        "esnext.object" => "lib.es2024.object.d.ts".to_owned(),
        "esnext.regexp" => "lib.es2024.regexp.d.ts".to_owned(),
        // TypeScript 5.9's last duplicate lib-map entry wins.
        "esnext.string" => "lib.es2024.string.d.ts".to_owned(),
        _ => format!("lib.{name}.d.ts"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_library_expands_references_before_roots() {
        let library = StdLib::bundled(&["lib.es2020.full.d.ts"]);
        assert_eq!(
            library.files().last().map(String::as_str),
            Some("lib.es2020.full.d.ts")
        );
        let es5 = library
            .files()
            .iter()
            .position(|name| name == "lib.es5.d.ts")
            .unwrap();
        let es2020 = library
            .files()
            .iter()
            .position(|name| name == "lib.es2020.d.ts")
            .unwrap();
        assert!(es5 < es2020);
        assert!(library.files().contains(&"lib.dom.d.ts".to_owned()));
        assert_eq!(StdLib::bundled_version(), "5.9.3");
    }

    #[test]
    fn indexes_global_values_types_and_interface_members() {
        let library = StdLib::bundled(&["lib.es2020.full.d.ts"]);
        assert!(library.has_global("Date"));
        assert_eq!(library.value_type("Date"), Some("DateConstructor"));
        assert!(
            library
                .value_locations("Date")
                .iter()
                .any(|location| location.file_name == "lib.es5.d.ts")
        );
        assert!(
            library
                .type_locations("DateConstructor")
                .iter()
                .any(|location| location.file_name == "lib.es5.d.ts")
        );

        let utc = library.value_member("Date", "UTC").unwrap();
        assert!(utc.locations.iter().any(|location| {
            location.file_name == "lib.es5.d.ts" && location.range.start.line == 955
        }));
        assert!(utc.signatures.iter().any(|signature| {
            signature.required_parameters == 2 && signature.maximum_parameters == Some(7)
        }));
        assert!(utc.signatures.iter().any(|signature| {
            signature.required_parameters == 1 && signature.maximum_parameters == Some(7)
        }));
    }

    #[test]
    fn indexes_inherited_and_namespace_members() {
        let library = StdLib::bundled_default();
        let starts_with = library.type_member("String", "startsWith").unwrap();
        assert!(
            starts_with
                .locations
                .iter()
                .any(|location| location.file_name == "lib.es2015.core.d.ts")
        );

        let number_format = library.value_member("Intl", "NumberFormat").unwrap();
        assert!(
            number_format
                .locations
                .iter()
                .any(|location| location.file_name == "lib.es5.d.ts")
        );
    }

    #[test]
    fn indexes_array_map_from_the_official_library() {
        let library = StdLib::bundled_default();
        let member = library.type_member("Array", "map").unwrap();
        assert!(!member.locations.is_empty());
        assert!(!member.signatures.is_empty());
    }

    #[test]
    fn indexes_global_declarations_inside_external_modules() {
        let library = StdLib::bundled(&["lib.esnext.iterator.d.ts"]);
        assert!(library.has_global("IteratorObject"));
        assert!(library.type_member("IteratorObject", "map").is_some());
    }

    #[test]
    fn indexes_declared_classes() {
        let library = StdLib::bundled(&["lib.scripthost.d.ts"]);
        assert!(library.has_global("SafeArray"));
        assert!(
            library
                .type_member("SafeArray", "SafeArray_typekey")
                .is_some()
        );
    }

    #[test]
    fn maps_typescript_lib_aliases() {
        assert_eq!(lib_file_name("es6"), "lib.es2015.d.ts");
        assert_eq!(lib_file_name("DOM.Iterable"), "lib.dom.iterable.d.ts");
        assert_eq!(lib_file_name("esnext.string"), "lib.es2024.string.d.ts");
    }

    #[test]
    fn selects_explicit_lib_and_target_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src/main.ts");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "").unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "lib": ["ES2015.Collection"] } }"#,
        )
        .unwrap();
        let explicit = StdLib::for_file(&source);
        assert_eq!(explicit.files(), &["lib.es2015.collection.d.ts".to_owned()]);
        assert!(explicit.has_global("Map"));
        assert!(!explicit.has_global("Date"));

        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "target": "ES2020" } }"#,
        )
        .unwrap();
        let targeted = StdLib::for_file(&source);
        assert_eq!(
            targeted.files().last().map(String::as_str),
            Some("lib.es2020.full.d.ts")
        );
        assert!(targeted.has_global("BigInt"));
    }

    #[test]
    fn selects_referenced_config_for_solution_projects() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src/main.ts");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "").unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "files": [], "references": [{ "path": "./tsconfig.app.json" }] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tsconfig.app.json"),
            r#"{ "compilerOptions": { "lib": ["ES2015.Promise"] }, "include": ["src"] }"#,
        )
        .unwrap();

        let library = StdLib::for_file(&source);
        assert_eq!(library.files(), &["lib.es2015.promise.d.ts".to_owned()]);
        assert!(library.has_global("Promise"));
        assert!(!library.has_global("Map"));
    }

    #[test]
    fn follows_package_extends_and_child_options_win() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src/main.ts");
        let preset = dir.path().join("node_modules/@scope/preset");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&preset).unwrap();
        std::fs::write(&source, "").unwrap();
        std::fs::write(
            preset.join("package.json"),
            r#"{ "tsconfig": "base.json" }"#,
        )
        .unwrap();
        std::fs::write(
            preset.join("base.json"),
            r#"{ "compilerOptions": { "lib": ["ES2015.Collection"] } }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "extends": "@scope/preset", "compilerOptions": { "lib": ["ES2015.Promise"] } }"#,
        )
        .unwrap();

        let library = StdLib::for_file(&source);
        assert!(library.has_global("Promise"));
        assert!(!library.has_global("Map"));
    }

    #[test]
    fn follows_extends_arrays_and_package_subpaths() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src/main.ts");
        let preset = dir.path().join("node_modules/@scope/preset");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&preset).unwrap();
        std::fs::write(&source, "").unwrap();
        std::fs::write(
            preset.join("browser.json"),
            r#"{ "compilerOptions": { "lib": ["DOM"] } }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("base.json"),
            r#"{ "compilerOptions": { "target": "ES2015" } }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "extends": ["./base.json", "@scope/preset/browser.json"] }"#,
        )
        .unwrap();

        let library = StdLib::for_file(&source);
        assert_eq!(
            library.files().last().map(String::as_str),
            Some("lib.dom.d.ts")
        );
        assert!(library.has_global("Document"));
    }

    #[test]
    fn reads_project_local_typescript_lib() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src/main.ts");
        let lib = dir.path().join("node_modules/typescript/lib");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(&source, "").unwrap();
        std::fs::write(lib.join("lib.d.ts"), "interface DefaultMarker {}\n").unwrap();
        std::fs::write(
            lib.join("lib.es2015.collection.d.ts"),
            "interface ProjectLocalCollection {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "lib": ["ES2015.Collection"] } }"#,
        )
        .unwrap();

        let library = StdLib::for_file(&source);
        assert_eq!(library.source_directory(), Some(lib.as_path()));
        assert!(library.has_global("ProjectLocalCollection"));
        assert!(!library.has_global("Map"));
    }

    #[test]
    fn unresolved_extends_fails_open_and_no_lib_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("main.ts");
        std::fs::write(&source, "").unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "extends": "missing-preset", "compilerOptions": { "target": "ES5" } }"#,
        )
        .unwrap();
        let fallback = StdLib::for_file(&source);
        assert_eq!(
            fallback.files().last().map(String::as_str),
            Some("lib.esnext.full.d.ts")
        );

        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "noLib": true } }"#,
        )
        .unwrap();
        let empty = StdLib::for_file(&source);
        assert!(empty.files().is_empty());
        assert!(!empty.has_global("Date"));
    }

    #[test]
    fn node_module_defaults_raise_the_implicit_target() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("main.ts");
        std::fs::write(&source, "").unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "module": "node20" } }"#,
        )
        .unwrap();
        let library = StdLib::for_file(&source);
        assert_eq!(
            library.files().last().map(String::as_str),
            Some("lib.es2023.full.d.ts")
        );
    }

    #[test]
    fn incomplete_project_typescript_falls_back_to_bundled_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("main.ts");
        let lib = dir.path().join("node_modules/typescript/lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(&source, "").unwrap();
        std::fs::write(lib.join("lib.d.ts"), "/// <reference lib=\"es5\" />\n").unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "target": "ES5" } }"#,
        )
        .unwrap();

        let library = StdLib::for_file(&source);
        assert_eq!(library.source_directory(), None);
        assert!(library.has_global("Date"));
    }
}
