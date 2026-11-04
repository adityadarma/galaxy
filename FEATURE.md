# Language Feature Matrix

This document is the source of truth for language support in Galaxy. Features
are listed vertically and languages horizontally. Every language-engine change
must update this matrix and the related tests in the same pull request.

## Status

| Symbol | Meaning |
|---|---|
| ✅ | Implemented and covered by tests |
| ◐ | Implemented only for some syntax, scopes, or file types |
| ⬜ | Not implemented |
| N/A | Not applicable to this language |

Statuses `◐` and `⬜` are backlog items. Neither may be treated as complete.

## Definition of Done

A language may be called **fully supported** only after every required feature
in the matrix is marked `✅`:

1. Syntax highlighting
2. Completion
3. Hover
4. Go to Definition
5. Find References
6. Rename Symbol
7. Diagnostics
8. Code Actions / Quick Fix
9. Signature Help
10. Inline Parameter Hints
11. Document Outline
12. Snippets

Find References also includes a shared **References Peek** presentation layer:
an inline source preview, file-grouped results, keyboard navigation, and direct
navigation without leaving the originating editor position.

Every language-feature implementation must satisfy this checklist:

- [ ] A core implementation exists and never executes project code.
- [ ] An FFI API exists when the feature requires UI integration.
- [ ] The UI never performs indexing or expensive resolution on the main thread.
- [ ] Positions use zero-based lines and UTF-16 columns.
- [ ] Invalid input, partial source, and untrusted dependencies never cause a panic.
- [ ] Unit tests cover local symbols, cross-file symbols, and fail-closed cases.
- [ ] Integration tests cover project dependencies when the language has a package manager.
- [ ] Caches are bounded and stale results are rejected by revision or freshness.
- [ ] `Feature.md` is updated in the same pull request.
- [ ] `cargo fmt`, clippy, Rust tests, Swift tests, and the Release build pass.

A new language must not stop at syntax highlighting. Initial pull requests may
use `◐` or `⬜`, but every cell must become `✅` before the language is promoted
to fully supported.

## Code Intelligence Engines

### Core Features

| Feature | PHP | Blade | Go | Rust | TypeScript | TSX | JavaScript | Vue | Svelte | Dart |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Syntax highlighting | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Completion | ✅ | ◐ | ✅ | ✅ | ✅ | ✅ | ✅ | ◐ | ◐ | ✅ |
| Hover | ✅ | ◐ | ✅ | ✅ | ✅ | ✅ | ✅ | ◐ | ◐ | ✅ |
| Go to Definition | ✅ | ◐ | ✅ | ✅ | ✅ | ✅ | ✅ | ◐ | ◐ | ✅ |
| Find References | ✅ | ◐ | ✅ | ✅ | ✅ | ✅ | ✅ | ◐ | ◐ | ✅ |
| Rename Symbol | ✅ | ◐ | ✅ | ✅ | ✅ | ✅ | ✅ | ◐ | ◐ | ✅ |
| Diagnostics | ✅ | ◐ | ✅ | ✅ | ✅ | ✅ | ✅ | ◐ | ◐ | ✅ |
| Code Actions / Quick Fix | ✅ | ◐ | ✅ | ✅ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Signature Help | ✅ | ⬜ | ✅ | ✅ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Inline Parameter Hints | ✅ | ⬜ | ⬜ | ◐ | ✅ | ✅ | ✅ | ◐ | ◐ | ⬜ |
| Document Outline | ✅ | ◐ | ✅ | ✅ | ✅ | ✅ | ✅ | ◐ | ◐ | ✅ |
| Snippets | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ⬜ | ⬜ | ✅ |

### Current Limitations

| Language | Limitation that causes the `◐` status |
|---|---|
| Blade | Intelligence works only in PHP regions of a detected Laravel application and framework contexts that can be proven statically. Plain PHP and Eloquent-only Composer projects keep PHP intelligence in `.blade.php` files. Signature help and parameter hints are unavailable. |
| Rust | Inline parameter hints support free and associated functions. Method-call syntax such as `value.method(...)` is unsupported. |
| Vue | Code intelligence and parameter hints run only in `<script>` / `<script setup>`. Templates are not analyzed. |
| Svelte | Code intelligence and parameter hints run only in module and instance `<script>` blocks. Templates are not analyzed. |

### Advanced Features

The following features are important but are not a universal baseline because
they depend on a language's module model, package manager, or frameworks.

| Feature | PHP | Blade | Go | Rust | TypeScript/JS | Vue/Svelte | Dart |
|---|---:|---:|---:|---:|---:|---:|---:|
| Workspace indexing | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Dependency indexing | ✅ Composer | ✅ Composer | ✅ Modules | ✅ Cargo | ✅ `node_modules` declarations | ✅ `node_modules` declarations | ✅ package config / Flutter SDK |
| Auto-import | ◐ | ◐ | ◐ | ◐ | ✅ | ✅ scripts | ◐ |
| Framework intelligence | ✅ Laravel/Symfony/CodeIgniter; Eloquent-only Composer support | ✅ Laravel application only | ✅ Goravel/Fiber | N/A | ✅ Adonis/Vite/ecosystem declarations | ✅ Vue/Svelte conventions | ✅ Flutter/provider/BLoC |
| Built-in/global declarations | ✅ PHP stubs | ✅ PHP stubs | ✅ GOROOT | ✅ std/dependencies | ✅ ECMAScript/DOM declarations | ✅ ECMAScript + component declarations | ✅ Dart/Flutter SDK |
| References Peek UI | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ scripts | ✅ |

### In-Memory Indexing and Telemetry

All code-intelligence engines build indexes on worker threads and retain their
installed snapshots in RAM. TypeScript-family parsed document indexes are also
persisted in the user cache between Galaxy launches. The filesystem remains the
source of truth: startup validates cached entries by path, size, and modified
time, reparses stale files, and watcher events update affected entries. Unopened
file preview text is loaded asynchronously when References Peek needs it.

| Engine | RAM index retained | Incremental update | References Peek source |
|---|---|---|---|
| PHP / Blade | Composer classes, scanned paths, parsed/open files, framework indexes | File and Composer-aware invalidation | PHP engine; Blade is limited to statically provable PHP regions |
| Go | Parsed files, package index, module/GOROOT packages, open buffers | File/package cache reset and rebuild | Go engine |
| Rust | Declarations, module files, `use` bindings, source roots, open buffers | Watcher deltas; Cargo metadata triggers rebuild | Rust engine |
| TypeScript / TSX / JavaScript | Parsed document indexes, exports, globals, and cached targeted reference queries; parsed indexes persist across launches | File-level updates invalidate affected query results and refresh the disk cache | TypeScript engine |
| Vue / Svelte | Same TypeScript index for script regions and component declarations | Same as TypeScript | Script/module regions only; templates are not analyzed |
| Dart | Parsed files, declarations, imports, retained source text, package roots | File deltas; `pubspec.yaml` triggers rebuild | Dart engine |

Each completed language build emits a structured `language index ready` log
event with these fields:

- `language`, `root`, and `build_ms`
- `files`, `symbols`, and `imports`
- `reverse_keys` and `reverse_sites` when the engine has a reverse index
- `source_bytes` for source text deliberately retained in RAM
- `heap_lower_bound_bytes`, a conservative lower bound from owned strings and
  container elements. It excludes allocator overhead and opaque parser-tree
  allocations, so it must not be reported as exact process memory.

References Peek is a shared AppKit presentation layer. Language-specific
engines produce UTF-16 locations; the UI groups them by file, preloads one-line
snippets in the background, and previews open buffers from RAM or unopened
files from disk. Syntax-only languages do not expose semantic references and
therefore do not use References Peek.

Current TypeScript-family limits are 10,000 indexed files per workspace, 1 MiB
per indexed file, eight retained workspace roots, 2,048 navigation-cache
entries, and 256 cached reference queries. Startup installs parsed workspace
snapshots without resolving every import. Unchanged files reuse a versioned
binary cache under `~/Library/Caches/Galaxy/workspace-index`; stale, missing, or
corrupt entries safely fall back to parsing source. Cache writes use a temporary
file and atomic rename. References scans in-memory import
metadata for bindings matching the requested declaration, resolves only those
candidates, and caches the exact query result until watcher changes invalidate
it. Unrelated imports never enter module resolution.

Workspace semantic engines share one process-wide indexing scheduler bounded by
the machine's logical CPU count. Their build lifecycle is consistent: discover
files serially, run independent per-file parsing through the shared budget,
merge results in deterministic path order, then install one immutable snapshot.
TypeScript, Rust, Go, and Dart use ordered parallel file work; PHP participates
in the same CPU budget while its order-sensitive framework scans remain one
scheduled unit. Small batches stay serial to avoid thread coordination cost.

## Syntax-Only Languages

The following languages have parsers and highlighting but no complete code
intelligence engine. A `✅` outline comes from a Tree-sitter tag query; `◐`
means a general syntax fallback rather than a semantic engine.

### Editing Features

| Feature | JSON | TOML | YAML | Markdown | Python | HTML | CSS | Bash | Nginx | C | C++ | C# | Java | Kotlin | Ruby | Lua | Swift | SQL | Makefile | INI | PowerShell | R |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Syntax highlighting | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ⬜ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Comment toggle | N/A | ✅ | ✅ | N/A | ✅ | N/A | N/A | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ⬜ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Document Outline | ⬜ | ⬜ | ⬜ | ⬜ | ✅ | ⬜ | ⬜ | ⬜ | ⬜ | ✅ | ✅ | ✅ | ✅ | ⬜ | ✅ | ✅ | ✅ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Snippets | ⬜ | ⬜ | ⬜ | ⬜ | ✅ | ✅ | ✅ | ✅ | ⬜ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ⬜ | ✅ | ✅ | ⬜ | ⬜ | ⬜ | ⬜ |

### Missing Code Intelligence

Every cell below is required backlog for each syntax-only language.

| Feature | JSON | TOML | YAML | Markdown | Python | HTML | CSS | Bash | Nginx | C | C++ | C# | Java | Kotlin | Ruby | Lua | Swift | SQL | Makefile | INI | PowerShell | R |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Completion | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Hover | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Go to Definition | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Find References | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Rename Symbol | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Diagnostics | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Code Actions / Quick Fix | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Signature Help | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |
| Inline Parameter Hints | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ |

## Maintenance Rules

When adding or changing a language:

1. Start with every row in the **Definition of Done**, not only the parser and highlighting.
2. Add matrix tests for each feature before changing `⬜` or `◐` to `✅`.
3. Do not mark `✅` based only on a function's existence; the feature must be usable from the editor and have integration tests.
4. Record concrete limitations under **Current Limitations**. Never hide a limitation behind `✅`.
5. A language that does not meet every required feature must remain labeled **partial support** or **syntax-only** in user documentation.
