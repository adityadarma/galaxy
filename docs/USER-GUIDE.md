# Galaxy User Guide

Galaxy is a native macOS editor for opening workspaces, editing files, running
terminal tools, and using built-in code analysis.

## Local Build

Requirements:

- macOS 14 or later.
- Xcode with Swift 6.
- The Rust toolchain specified by `rust-toolchain.toml`, when available.
- `xcodegen` to generate the Xcode project.

Build and run the app locally:

```sh
scripts/build-core.sh
(cd app && xcodegen generate)
xcodebuild -project app/Galaxy.xcodeproj -scheme Galaxy \
  -configuration Debug -derivedDataPath app/build build
open app/build/Build/Products/Debug/Galaxy.app
```

The CLI helper can be installed in `~/.local/bin`:

```sh
scripts/install-cli.sh
galaxy /path/to/project
```

## Opening a Workspace

- `File > Open File or Folder…` opens a file or folder.
- `File > Open Folder…` opens a folder as a workspace.
- Explorer displays workspace files and folders.
- Click a file once to open a preview tab; editing or double-clicking pins it.
- Each tab retains its own caret and scroll position.
- Git changes are marked in Explorer, ancestor folders, and tabs.

## Editing

- `Cmd+S`: save.
- `Cmd+Shift+S`: save as a new file.
- `Cmd+Z` / `Cmd+Shift+Z`: undo / redo.
- `Cmd+F`: find.
- `Cmd+Option+F`: find and replace.
- `Ctrl+G`: go to line.
- `Cmd+P`: go to file.
- `Cmd+Shift+O`: go to symbol.
- `Cmd+D`: add the next occurrence to the multi-cursor selection.
- `Cmd+Shift+L`: select all occurrences.
- `Cmd+Option+Up` / `Cmd+Option+Down`: add a cursor above or below.
- `Cmd+/`: toggle line comment.
- `Cmd+.`: code actions.

The editor supports automatic indentation, bracket pairing, bracket matching,
UTF-16 positions, multiple cursors, IME through `NSTextInputClient`, and
transactional undo.

## Code Navigation

Galaxy's built-in engines provide the following features for PHP/Blade,
Go, Rust, Dart/Flutter, TypeScript/JavaScript, and Vue/Svelte scripts. See
[`Feature.md`](../Feature.md) for the exact per-language support matrix and
current limitations.

- Hover.
- Completion.
- Signature help.
- Go to definition.
- Find references.
- Rename symbol.
- Code actions.
- Document symbols.
- Diagnostics in the editor, gutter, scrollbar, and Problems panel.

Find References opens an inline References Peek below the active symbol. The
left pane previews source without leaving the current file; the right pane
groups references by file and shows source-line snippets. Single-click a result
to preview it, double-click or press Return to navigate, and press Escape to
close the peek. `Cmd`-click on a declaration name also opens References Peek,
matching VS Code.

Click a diagnostic in the Problems panel to open its file and location. The
breadcrumb below the file path shows the active symbol; click it to navigate to
the declaration.

Go to Definition (`F12` or `Cmd`-click) is handled by the built-in engine. For
PHP, open the Composer project containing `composer.json` so the engine can
find `vendor/` and autoload information. Blade-specific behavior requires a
Laravel application root with `artisan` and `bootstrap/app.php`; an Eloquent
dependency alone still supports PHP members but does not turn `.blade.php`
files into templates. For Go, open the folder containing `go.mod` or `go.work`;
the engine reads GOROOT and the module cache for standard packages and
dependencies. Modules that have not been downloaded with `go mod download`
have no completion or definition data.

## Terminal

- `Cmd+J`: open or close the terminal.
- Click `+` to create a terminal tab.
- Terminal tabs keep their PTY running while the panel is hidden.
- Copy and paste use the macOS clipboard.
- Double-click `path:line:column` in terminal output to open the file.
- Terminal scrollback retains up to 10,000 lines.

## Split Editor

Use the `View` menu or Command Palette:

- `Split Editor Right`.
- `Split Editor Down`.
- `Close Editor Split`.

Both panes follow the active tab and share the same document state.

## Search and Git

- `Cmd+Shift+F`: workspace search.
- Search supports regular expressions, case sensitivity, whole-word matching,
  include globs, and exclude globs.
- Results are capped at 10,000 matches and can be opened directly.
- The status bar shows the branch and changed-file count.
- The gutter displays the open file's diff against `HEAD`.

## Settings

Open settings through `View > Open User Settings (JSON)…` or workspace
settings. Global settings are stored at:

```text
~/Library/Application Support/Galaxy/settings.json
```

Workspace settings are stored at:

```text
.galaxy/settings.json
```

Workspace settings override global settings.
