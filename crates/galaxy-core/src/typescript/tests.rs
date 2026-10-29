use super::*;

#[test]
fn indexes_ambient_exported_class_members() {
    let index = DocumentIndex::parse(
        "node_modules/@adonisjs/core/build/src/env.d.ts",
        "export declare class Env {\n  static create(value: URL): Promise<Env>;\n  get(name: string): string;\n}\n",
    );

    assert!(
        index
            .exported_declaration("Env")
            .is_some_and(|declaration| declaration.kind == DeclarationKind::Class)
    );
    assert!(index.type_member("Env", "create").is_some());
}

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
fn indexes_named_default_class_extending_a_call_expression() {
    let index = DocumentIndex::parse(
        "app/models/user.ts",
        "export default class User extends compose(\n  BaseModel,\n  Trait\n) {}",
    );

    let declaration = index
        .declarations
        .iter()
        .find(|declaration| declaration.name == "User")
        .expect("named default class declaration");
    assert_eq!(declaration.kind, DeclarationKind::Class);
    assert!(declaration.exported);
    assert!(declaration.default);
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
    assert_eq!(index.module_statement_at(0, 18), Some("./one"));
    assert_eq!(index.module_statement_at(2, 24), Some("./three"));
    assert_eq!(index.module_statement_at(3, 14), Some("./four"));
    assert_eq!(index.module_statement_at(3, 10), Some("./four"));
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
fn does_not_require_defaulted_function_parameters() {
    let index = DocumentIndex::parse(
        "validators.spec.ts",
        "function makeFile(extname: string, size = 1024) {}\nmakeFile('jpg');\n",
    );

    assert!(
        index
            .type_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TS2554"),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn accepts_arguments_for_arrow_functions_with_defaulted_parameters() {
    let index = DocumentIndex::parse(
        "auth_service.spec.ts",
        "const seed = (overrides: Record<string, any> = {}) => overrides;\nseed({ status: 1 });\n",
    );

    assert!(
        index
            .type_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TS2554"),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn permits_extending_date_prototype() {
    let index = DocumentIndex::parse(
        "extensions.ts",
        "interface Date { toFormat(format?: string): string }\n\
         Date.prototype.toFormat = function(format: string = 'yyyy-MM-dd') { return format; };\n",
    );

    assert!(
        index
            .type_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TS2339"),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn permits_date_now_and_class_constructor_arguments() {
    let index = DocumentIndex::parse(
        "values.ts",
        "const id = String(Date.now());\n\
         class VineMultipartFile { constructor(options?: object, values?: unknown[], name?: string) {} }\n\
         new VineMultipartFile({}, [], 'file');\n",
    );

    assert!(
        index
            .type_diagnostics
            .iter()
            .all(|diagnostic| !matches!(diagnostic.code.as_str(), "TS2339" | "TS2554")),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn permits_macros_for_all_macroable_subclasses() {
    let index = DocumentIndex::parse(
        "query_builder.ts",
        "import Macroable from '@poppinss/macroable';\n\
         class DatabaseQueryBuilder extends Macroable {}\n\
         class ModelQueryBuilder extends DatabaseQueryBuilder {}\n\
         DatabaseQueryBuilder.macro('whereTenant', () => {});\n\
         ModelQueryBuilder.macro('visibleTo', () => {});\n",
    );

    assert!(
        index
            .type_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TS2339"),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn permits_macros_for_imported_lucid_builders() {
    let index = DocumentIndex::parse(
        "model.ts",
        "import { ModelQueryBuilder as Builder } from '@adonisjs/lucid/orm';\n\
         Builder.macro('whereEncrypted', () => {});\n",
    );

    assert!(
        index
            .type_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TS2339"),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn permits_macros_for_database_query_builder_from_lucid_database() {
    let index = DocumentIndex::parse(
        "provider.ts",
        "import { DatabaseQueryBuilder } from '@adonisjs/lucid/database';\n\
         DatabaseQueryBuilder.macro('whereTenant', function () {});\n",
    );

    assert!(
        index
            .type_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TS2339"),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn permits_macros_for_lucid_builders_with_module_augmentation() {
    // Mirrors src/query_builder/extensions.ts: augmenting the builder interface
    // must not hide the static `macro` inherited from Macroable.
    let index = DocumentIndex::parse(
        "extensions.ts",
        "import { ModelQueryBuilder } from '@adonisjs/lucid/orm'\n\
         import { DatabaseQueryBuilder } from '@adonisjs/lucid/database'\n\
         DatabaseQueryBuilder.macro('whereEncrypted', function (this: DatabaseQueryBuilder, columns: string) {\n\
           return this.whereRaw('x', [columns])\n\
         })\n\
         ModelQueryBuilder.macro('whereEncrypted', function (this: ModelQueryBuilder, columns: string) {\n\
           return this.whereRaw('x', [columns])\n\
         })\n\
         declare module '@adonisjs/lucid/database' {\n\
           interface DatabaseQueryBuilder {\n\
             whereEncrypted(columns: string, value: any): this\n\
           }\n\
         }\n\
         declare module '@adonisjs/lucid/orm' {\n\
           interface ModelQueryBuilder {\n\
             whereEncrypted(columns: string, value: any): this\n\
           }\n\
         }\n",
    );

    assert!(
        index
            .type_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TS2339"),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn permits_macros_inside_lucid_model_query_builder_declaration() {
    let index = DocumentIndex::parse(
        "node_modules/@adonisjs/lucid/build/src/orm/query_builder/index.d.ts",
        "import { Chainable } from '../../../database/query_builder/chainable.js';\n\
         export declare class ModelQueryBuilder extends Chainable {}\n\
         ModelQueryBuilder.macro('whereEncryptedLike', () => {});\n",
    );

    assert!(
        index
            .type_diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "TS2339"),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn indexes_imported_superclasses_as_type_references() {
    let index = DocumentIndex::parse(
        "model_query_builder.d.ts",
        "import { Chainable } from '../../database/query_builder/chainable.js';\n\
         export declare class ModelQueryBuilder extends Chainable {}\n",
    );

    let (import, binding) = index
        .type_import_binding_at(1, 47)
        .expect("imported superclass binding");
    assert_eq!(
        import.specifier,
        "../../database/query_builder/chainable.js"
    );
    assert_eq!(binding.local_name, "Chainable");
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
    let index = DocumentIndex::parse("properties.ts", "const value = 1; ({ value }); obj.value;");

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
fn indexes_direct_this_members_of_local_classes() {
    let index = DocumentIndex::parse(
        "main.ts",
        "class CdnHelper {\n  private static readonly CDN_URL = 'https://example.test';\n  static generateUrl() { return this.CDN_URL; }\n}\n",
    );

    let usage = index.type_member_at(2, 37).expect("this field use");
    assert_eq!(usage.member_name, "CDN_URL");
    assert_eq!(
        index.type_member_target(usage).unwrap().range.start,
        Utf16Position::new(1, 26)
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

#[test]
fn indexes_direct_this_members_of_local_superclasses() {
    let index = DocumentIndex::parse(
        "main.ts",
        "class baseService { setMessage() {} }\nclass Service extends baseService { use() { this.setMessage(); } }",
    );
    assert!(matches!(
        index.type_member_at(1, 53),
        Some(TypeMemberUse {
            target: TypeMemberTarget::SuperLocal(_),
            member_name,
            ..
        }) if member_name == "setMessage"
    ));
}

#[test]
fn indexes_fluent_this_members_of_local_superclasses() {
    let index = DocumentIndex::parse(
        "main.ts",
        "declare class baseService { protected setCode(code: number): this; protected setMessage(message: string): this; }\nclass Service extends baseService { use() { return this.setCode(200).setMessage('ok'); } }",
    );
    assert!(matches!(
        index.type_member_at(1, 70),
        Some(TypeMemberUse {
            target: TypeMemberTarget::SuperLocalChain { chain, .. },
            member_name,
            ..
        }) if chain.as_slice() == ["setCode"] && member_name == "setMessage"
    ));
}

#[test]
fn interface_augmentation_does_not_shadow_imported_values() {
    // Mirrors src/vinejs/extensions.ts: `interface Vine` inside
    // `declare module` names a type only, so `Vine.macro` stays unchecked.
    let index = DocumentIndex::parse(
        "extensions.ts",
        "import { Vine, VineString } from '@vinejs/vine'\n\
         Vine.macro('fileSecure', function (this: Vine) {})\n\
         VineString.macro('password', function (this: VineString) { return this })\n\
         declare module '@vinejs/vine' {\n\
           interface Vine { fileSecure(): void }\n\
           interface VineString { password(): this }\n\
         }\n",
    );

    assert!(
        index.type_diagnostics.is_empty(),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn global_interface_augmentation_keeps_builtin_prototype() {
    // Mirrors src/global.ts.
    let index = DocumentIndex::parse(
        "global.ts",
        "declare global {\n\
           interface String { censoredText: (length?: number) => string }\n\
         }\n\
         String.prototype.censoredText = function (length?: number): string { return '' }\n",
    );

    assert!(
        index.type_diagnostics.is_empty(),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn derived_class_without_constructor_inherits_superclass_arity() {
    // Mirrors app/utilities/storage.ts: `Storage` inherits flydrive's
    // `Disk` constructor, so `new Storage(config)` is valid.
    let index = DocumentIndex::parse(
        "storage.ts",
        "import { Disk } from 'flydrive'\n\
         export class Storage extends Disk {\n\
           use(driver: string): Storage { return new Storage(driver) }\n\
         }\n\
         new Storage({})\n",
    );

    assert!(
        index.type_diagnostics.is_empty(),
        "{:?}",
        index.type_diagnostics
    );
}

#[test]
fn class_without_superclass_still_checks_constructor_arity() {
    let index = DocumentIndex::parse("main.ts", "class Point {}\nnew Point(1)\n");

    assert!(
        index
            .type_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "TS2554"),
        "{:?}",
        index.type_diagnostics
    );
}
