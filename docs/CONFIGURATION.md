# Configuration Reference

Galaxy reads the global JSON settings and merges them with the workspace
settings. Workspace settings take precedence.

## Editor

```json
{
  "editor": {
    "insertSpaces": true,
    "tabSize": 4,
    "renderWhitespace": "all",
    "inlineParameterHints": true
  }
}
```

- `editor.insertSpaces`: `true` inserts spaces; `false` inserts tabs.
- `editor.tabSize`: a positive tab width. Default `4`.
- `editor.renderWhitespace`: how whitespace is drawn. Supported values are
  `none`, `boundary`, and `all`; default `all`.
- `editor.inlineParameterHints`: show parameter names before call arguments in
  TypeScript/JavaScript, Vue/Svelte scripts, PHP, and Rust. Default `true`.
  Hints are hidden while word wrap is active. Vue and Svelte templates and
  Rust method-call syntax (`value.method(...)`) are not analysed.

Galaxy ships built-in snippets, selected by file extension, for Rust, Go,
Python, JavaScript/TypeScript, PHP/Laravel/Blade, Dart/Flutter, Java, Kotlin,
Swift, C/C++/C#, Ruby, HTML, CSS, Shell, and SQL. Snippets appear alongside
built-in engine results when the typed prefix matches.

Snippets support the placeholders `$1`, `${1:default}`, and `$0`, choices such
as `${1|one,two|}`, and the variables `TM_FILENAME`, `TM_FILENAME_BASE`,
`TM_SELECTED_TEXT`, `SELECTION`, and `CURRENT_YEAR`. Tab moves to the next
placeholder, Shift+Tab moves back, and Escape ends the session.

The editor font can be chosen from the system font list in `Settings`. To use
Fira Code, install it in Font Book, open `Settings`, and select `Fira Code`
under `Editor font`. Font size is set separately and applies to both the editor
and the terminal.

## Auto-save

```json
{
  "files": {
    "autoSave": "afterDelay",
    "autoSaveDelay": 1000
  }
}
```

`files.autoSave` accepts:

- `off`: never saves automatically.
- `afterDelay`: saves once no edits have been made for the delay.
- `onFocusChange`: saves when the window loses focus or the tab changes.

`files.autoSaveDelay` is in milliseconds and is clamped between `100` and
`60000`. The default delay is `1000` milliseconds.

Auto-save only saves files that already have a path. Untitled buffers never
trigger an automatic Save As dialog.

## Code Intelligence

Galaxy does not use external language servers and has no plugin system. Hover,
completion, definition, references, rename, code actions, outline, and
diagnostics come from built-in engines running inside the Galaxy process.
Nothing needs to be installed.

| Language | Status |
|---|---|
| PHP/Blade/Laravel | built-in engine |
| Go | built-in engine |
| TypeScript/JavaScript | built-in engine for navigation, completion, diagnostics, and type declarations |
| Rust | built-in engine for navigation, completion, outline, references, rename, and syntax diagnostics |
| Dart/Flutter | built-in engine (see below) |
| Vue, Svelte | TypeScript engine inside `<script>` blocks (see below) |
| Python, Swift | syntax highlighting only; no built-in engine yet |

Syntax highlighting also covers PHP (`.php`) and Blade files (`.blade.php`,
which use the PHP grammar for their PHP sections).

### PHP, Blade, and Frameworks

The PHP engine reads source files, `composer.json`, and `vendor/` statically.
PHP is never executed.

Plain Composer projects retain PHP intelligence in every `.php` file, including
files whose names end in `.blade.php`. A file is treated as a Blade template
only when Galaxy detects a Laravel application from both `artisan` and
`bootstrap/app.php`. Blade analysis is limited to PHP regions and statically
provable Laravel contexts.

Projects that require `laravel/framework` or `illuminate/*` can use Laravel's
Eloquent member forwarding without being treated as Laravel applications. This
does not enable Blade template handling. CodeIgniter and Symfony activate only
when their respective project markers or Composer dependencies provide evidence.

### Go

The Go engine reads `go.mod`, `go.work`, `vendor/`, GOROOT, and the module
cache statically. The `go` command is never run.

There is no full type checker, so diagnostics cover only syntax errors, unused
imports, missing standard-library imports, variables that are declared but not
used, and duplicate declarations. References and rename link an interface
method to workspace struct implementations only when the whole method set
matches structurally. External dependencies are not considered, so their
methods are never renamed.

Goravel is detected as a framework and Fiber v2/v3 as a library from the
`require` entries in `go.mod`. Both are kept apart from the generic resolver so
that later site-aware or API-specific support does not mix framework rules
with plain Go.

### Dart and Flutter

The engine indexes declarations from the app and from the packages listed in
`.dart_tool/package_config.json` for cross-file Go to Definition. The Flutter
SDK is indexed too when `FLUTTER_ROOT` is set. An internal AST parser provides
syntax diagnostics.

Static resolution covers `BuildContext` parameters, `widget` in `State<T>`,
local named routes, and lexically scoped provider/BLoC types
(`context.read<T>`, `context.watch<T>`, `Provider.of<T>`, and
`BlocProvider.of<T>`). Runtime branches, ambiguous nested scopes, and dynamic
injection are still rejected.

### Vue and Svelte

The TypeScript engine analyses the `<script>` blocks of `.vue` and `.svelte`
files: completion with auto-imports, Go to Definition, hover, references,
rename, outline, and syntax and type diagnostics. All scripts of one component
share a scope, so Vue's `<script>` and `<script setup>`, or Svelte's module and
instance scripts, see each other's names.

Templates are not analysed. A request with the cursor in the template returns
nothing, and renaming a script name the template or styles mention is refused
so the markup is never left stale. Missing-import warnings are not reported in
components, because build tools may auto-import names the engine cannot see.

### Removed settings

The `lsp.servers` and `servers` settings are no longer read. Format Document,
Go to Type Definition, Go to Implementation, Go to Workspace Symbol, semantic
tokens, and general-purpose inlay hints were removed together with the language
servers. Built-in inline parameter-name hints are available through
`editor.inlineParameterHints`.

## Local Data

- App logs: `~/Library/Logs/Galaxy`.
- Crash report: `~/Library/Logs/Galaxy/crash-report.log`.
- Session restore: `~/Library/Application Support/Galaxy/session.json`.
- User settings: `~/Library/Application Support/Galaxy/settings.json`.

Galaxy never uploads logs, crash reports, settings, or sessions automatically.
