# Language engine rules

Every language engine (PHP, TypeScript/JavaScript, Go, and any added later)
follows the same layering. Keep a change in the lowest layer that can hold it.

## Layers

1. Language core, in `galaxy-core`. Parsing, scopes, imports, explicit types,
   member lookup. Knows the language and nothing else.
2. Framework adapters, in `galaxy-core`, under `<language>/framework/<name>`.
   Syntax and naming rules of one framework. Pure: no file system, no cache.
3. Host, in `galaxy-ffi`. File system, package resolution, caches, conversion
   to editor locations. Knows no framework by name.

| Concern | Layer |
| --- | --- |
| Grammar, scope, symbol table | Language core |
| `@hasMany(..., { onQuery })`, Eloquent builders, route names | Framework adapter |
| `node_modules`, Composer, `go.mod` lookup, caching | Host |

## Layout

- PHP: `php/engine/` is the language core, split by service (`types`,
  `lookup`, `complete`, `navigate`, `diag`, `refs`, `assist`).
  `php/framework/<name>/` holds frameworks behind the `Framework` trait.
  The engine finds the syntax (a closure passed to a method) and asks the
  frameworks what it means (`Framework::callback_parameter_type`). It never
  names Eloquent or a builder class.
- TypeScript/JavaScript: `typescript.rs` holds the public types and
  `DocumentIndex`. Each domain has one module under `typescript/`:

  | Module | Domain |
  | --- | --- |
  | `symbols` | scopes, declarations, same-file occurrences |
  | `imports` | imports, exports, dynamic imports, namespace members |
  | `members` | explicitly typed receivers and their members |
  | `enums` | enum members and `Enum.Member` accesses |
  | `inheritance` | members reached through an imported superclass |
  | `diagnostics` | syntax errors and the small static type checker |
  | `syntax` | tree-sitter helpers shared by the others |
  | `framework/<name>` | one adapter per framework |

   Adapters register in `typescript/framework/mod.rs::collect_uses`.
- TypeScript host services: `galaxy-ffi/src/typescript_navigation.rs` owns
  module/package resolution, declaration preference, re-export traversal,
  cache invalidation, and workspace warmup. `typescript_editor.rs` owns
  completion, hover text, and outline presentation. `lib.rs` remains the
  UniFFI-facing `Core` facade and orchestrates these services; it does not
  contain TypeScript package-resolution rules.
- A new language mirrors this: a core split by service, plus
  `<language>/framework/<name>`.
- Keep one domain per file. When a file passes about 600 lines, split it by
  domain, not by size alone.

## Rules

1. Activate from evidence. An adapter turns on from an import of a known
   package or a marker file, never from a project path or a variable name
   (`$query`, `query`, `subQuery` prove nothing).
2. Fail closed. Unknown shape, ambiguity, cycle, malformed syntax, path escape
   or a non-unique target yields no result. A wrong jump is worse than none.
3. One direction. Adapters emit neutral facts (for TypeScript,
   `TypeMemberTarget::PackageDeclaration`). The host resolves them. The host
   never matches on a framework name, so adding a framework never edits it.
4. Declaration driven. Resolve to what the installed package declares, not to
   a hard-coded copy of its API. Name a package by its public entry (an
   `exports` subpath such as `@adonisjs/lucid/types/querybuilder`), never by
   an internal build path or a path relative to another package file.
5. No new runtime. No LSP, Node, npm, or external resolver executables.
6. Portable tests. Use temp-dir fixtures that mimic the package layout. Never
   depend on a local project. Real projects are for diagnosis only.
7. Every adapter ships a positive test and a negative test (same names, wrong
   import, must stay unresolved).

## Adding a framework

1. Create the adapter module in the language's `framework/` folder.
2. Register it in that folder's `mod.rs`.
3. Emit neutral targets; extend the neutral enum only if no existing variant
   fits, and keep the new variant framework-agnostic.
4. Add positive and negative tests.
5. Run `cargo clippy --workspace --all-targets --locked -- -D warnings`.
