//! End-to-end TypeScript navigation regressions shared by framework packages.

use super::*;

#[test]
fn typescript_definition_follows_cross_package_inheritance() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let framework = dir
        .path()
        .join("node_modules/@framework/query/build/src/database/query_builder");
    let base = dir.path().join("node_modules/@base/macroable/build");
    std::fs::create_dir_all(&framework).unwrap();
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(
        dir.path()
            .join("node_modules/@framework/query/package.json"),
        r##"{"exports":{"./database":"./build/src/database/main.js"}}"##,
    )
    .unwrap();
    let database = framework.parent().unwrap();
    std::fs::write(
        database.join("main.d.ts"),
        "export { QueryBuilder } from './query_builder/database.js';\n",
    )
    .unwrap();
    std::fs::write(database.join("main.js"), "export {};\n").unwrap();
    std::fs::write(
        framework.join("database.d.ts"),
        "import { Chainable } from './chainable.js';\nexport declare class QueryBuilder extends Chainable {}\n",
    )
    .unwrap();
    std::fs::write(framework.join("database.js"), "export {};\n").unwrap();
    std::fs::write(
        framework.join("chainable.d.ts"),
        "import Macroable from '@base/macroable';\nexport declare abstract class Chainable extends Macroable {}\n",
    )
    .unwrap();
    std::fs::write(framework.join("chainable.js"), "export {};\n").unwrap();
    std::fs::write(
        dir.path().join("node_modules/@base/macroable/package.json"),
        r##"{"exports":"./build/index.js"}"##,
    )
    .unwrap();
    std::fs::write(
        base.join("index.d.ts"),
        "export default abstract class Macroable { static macro(name: string, value: unknown): void; }\n",
    )
    .unwrap();
    std::fs::write(base.join("index.js"), "export {};\n").unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import { QueryBuilder } from '@framework/query/database';\nQueryBuilder.macro('whereTenant', () => {});\n",
    )
    .unwrap();
    let source = std::fs::read_to_string(&main_path).unwrap();
    assert!(
        DocumentIndex::parse(&main_path, &source)
            .type_member_at(1, 14)
            .is_some()
    );
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(main.clone(), 1, 14));
    let locations = core.definition_locations(main);
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert!(
        locations[0]
            .uri
            .ends_with("@base/macroable/build/index.d.ts")
    );
    assert_eq!((locations[0].line, locations[0].column), (0, 49));
}

#[test]
fn typescript_definition_resolves_macroable_in_lucid_chainable_heritage() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let chainable = dir
        .path()
        .join("node_modules/@adonisjs/lucid/build/src/database/query_builder");
    let macroable = dir.path().join("node_modules/@poppinss/macroable/build");
    std::fs::create_dir_all(&chainable).unwrap();
    std::fs::create_dir_all(&macroable).unwrap();
    std::fs::write(
        dir.path().join("node_modules/@adonisjs/lucid/package.json"),
        r##"{"exports":{"./database/query_builder/chainable":"./build/src/database/query_builder/chainable.js"}}"##,
    )
    .unwrap();
    std::fs::write(
        dir.path()
            .join("node_modules/@poppinss/macroable/package.json"),
        r##"{"exports":"./build/index.js"}"##,
    )
    .unwrap();
    std::fs::write(
        macroable.join("index.d.ts"),
        "export default abstract class Macroable { static macro(): void; }\n",
    )
    .unwrap();
    std::fs::write(macroable.join("index.js"), "export {};\n").unwrap();
    let chainable_path = chainable.join("chainable.d.ts");
    std::fs::write(
        &chainable_path,
        "import Macroable from '@poppinss/macroable';\nexport declare abstract class Chainable extends Macroable {}\n",
    )
    .unwrap();
    std::fs::write(chainable.join("chainable.js"), "export {};\n").unwrap();
    let core = Core::new();
    let document = core
        .open_document(chainable_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(core.request_definition(document.clone(), 1, 52));
    let locations = core.definition_locations(document);
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert!(
        locations[0]
            .uri
            .ends_with("@poppinss/macroable/build/index.d.ts")
    );
    // VS Code lands on the declaration start (`abstract`), after `export default `.
    assert_eq!((locations[0].line, locations[0].column), (0, 15));
}

#[test]
fn typescript_definition_follows_local_import_reexport() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let database = dir
        .path()
        .join("node_modules/@adonisjs/lucid/build/src/database");
    let query_builder = database.join("query_builder");
    std::fs::create_dir_all(&query_builder).unwrap();
    std::fs::write(
        dir.path().join("node_modules/@adonisjs/lucid/package.json"),
        r##"{"exports":{"./database":"./build/src/database/main.js"}}"##,
    )
    .unwrap();
    // Lucid imports the builder, then re-exports the local binding without `from`.
    std::fs::write(
        database.join("main.d.ts"),
        "import Macroable from '@poppinss/macroable';\nimport { DatabaseQueryBuilder } from './query_builder/database.js';\nexport { DatabaseQueryBuilder };\nexport declare class Database extends Macroable {}\n",
    )
    .unwrap();
    std::fs::write(database.join("main.js"), "export {};\n").unwrap();
    std::fs::write(
        query_builder.join("database.d.ts"),
        "export declare class DatabaseQueryBuilder {}\n",
    )
    .unwrap();
    std::fs::write(query_builder.join("database.js"), "export {};\n").unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import { DatabaseQueryBuilder } from '@adonisjs/lucid/database';\nDatabaseQueryBuilder.macro('x', () => {});\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    for (line, column) in [(0, 12), (1, 5)] {
        assert!(
            core.request_definition(main.clone(), line, column),
            "cursor {line}:{column}"
        );
        let locations = core.definition_locations(main.clone());
        assert_eq!(locations.len(), 1, "{locations:?}");
        assert!(
            locations[0]
                .uri
                .ends_with("database/query_builder/database.d.ts"),
            "cursor {line}:{column}: {locations:?}"
        );
        // The class name after `export declare class `, not the module root.
        assert_eq!((locations[0].line, locations[0].column), (0, 21));
    }
}

#[test]
fn typescript_definition_follows_lucid_builder_macro_to_macroable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let lucid = dir.path().join("node_modules/@adonisjs/lucid");
    let database = lucid.join("build/src/database");
    let query_builder = database.join("query_builder");
    let macroable = dir.path().join("node_modules/@poppinss/macroable");
    std::fs::create_dir_all(&query_builder).unwrap();
    std::fs::create_dir_all(macroable.join("build")).unwrap();
    std::fs::write(
        lucid.join("package.json"),
        r##"{"type":"module","exports":{"./database":"./build/src/database/main.js"}}"##,
    )
    .unwrap();
    std::fs::write(
        macroable.join("package.json"),
        r##"{"type":"module","main":"build/index.js","exports":{".":"./build/index.js"}}"##,
    )
    .unwrap();
    std::fs::write(
        macroable.join("build/index.d.ts"),
        "export default abstract class Macroable {\n    protected static instanceMacros: Set<unknown>;\n    static macro<T extends {\n        new (...args: any[]): any;\n    }, K extends keyof InstanceType<T>>(this: T, name: K, value: InstanceType<T>[K]): void;\n}\n",
    )
    .unwrap();
    std::fs::write(macroable.join("build/index.js"), "export {};\n").unwrap();
    std::fs::write(
        database.join("main.d.ts"),
        "import Macroable from '@poppinss/macroable';\nimport { DatabaseQueryBuilder } from './query_builder/database.js';\nexport { DatabaseQueryBuilder };\nexport declare class Database extends Macroable {}\n",
    )
    .unwrap();
    std::fs::write(database.join("main.js"), "export {};\n").unwrap();
    std::fs::write(
        query_builder.join("database.d.ts"),
        "import { type DatabaseQueryBuilderContract } from '../../types/querybuilder.js';\nimport { Chainable } from './chainable.js';\nexport declare class DatabaseQueryBuilder extends Chainable implements DatabaseQueryBuilderContract {\n    protected static macros: {};\n}\n",
    )
    .unwrap();
    std::fs::write(query_builder.join("database.js"), "export {};\n").unwrap();
    std::fs::write(
        query_builder.join("chainable.d.ts"),
        "import Macroable from '@poppinss/macroable';\nexport declare abstract class Chainable extends Macroable {\n    hasAggregates: boolean;\n}\n",
    )
    .unwrap();
    std::fs::write(query_builder.join("chainable.js"), "export {};\n").unwrap();
    let main_path = dir.path().join("main.ts");
    std::fs::write(
        &main_path,
        "import { DatabaseQueryBuilder } from '@adonisjs/lucid/database';\nDatabaseQueryBuilder.macro('whereTenant', function () {});\n",
    )
    .unwrap();
    let core = Core::new();
    let main = core
        .open_document(main_path.to_string_lossy().into_owned())
        .unwrap();

    assert!(
        core.request_definition(main.clone(), 1, 23),
        "no definition for macro"
    );
    let locations = core.definition_locations(main);
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert!(
        locations[0]
            .uri
            .ends_with("@poppinss/macroable/build/index.d.ts"),
        "{locations:?}"
    );
    assert_eq!((locations[0].line, locations[0].column), (2, 11));
}
