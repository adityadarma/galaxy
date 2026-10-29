# PRD: Galaxy — Native macOS Code Editor

Version: 0.1 (draft)
Date: 2026-10-05
Platform: macOS 14+ (Apple Silicon prioritized, Intel supported)

## 1. Summary

Galaxy is a native macOS code editor focused primarily on performance and RAM efficiency. The UI is built with Swift + AppKit, while the core (text buffer, parsing, indexing, built-in language engines, search) is written in Rust. Features focus on fast code navigation: built-in code intelligence, Go to File, syntax highlighting, and a keyboard-first workflow.

## 2. Problem

- VSCode and Electron-based editors use a lot of RAM (often 500 MB–2 GB for a medium-sized workspace).
- Startup and opening large workspaces feel slow.
- Developers need a lightweight daily editor that still has IDE capabilities (completion, navigation, diagnostics).

## 3. Goals

1. An editor that feels instant: startup, opening files, typing, scrolling, and navigation without lag.
2. RAM usage far below VSCode for the same workflow.
3. Stable built-in code intelligence for core navigation and editing features, with no external language servers to install.
4. A keyboard-first workflow that speeds up developer work.

## 4. Non-goals (out of scope for v1)

- Externally installable plugins/extensions.
- Marketplace.
- Remote development (SSH, container, WSL).
- Collaboration/real-time multiplayer.
- Debugger (DAP).
- Windows/Linux support.
- VSCode extension/theme compatibility.
- Notebooks, rich Markdown preview, AI assistant.

## 5. Target users

- Individual developers on macOS working with Rust, TypeScript/JavaScript, Python, Go.
- MacBook users with 8–16 GB RAM who find VSCode too heavy.
- Keyboard-heavy users who prioritize fast navigation.

## 6. Performance and RAM budget (required)

Measured on a MacBook Air M1 8 GB, release build. RAM figures = physical footprint of the editor process (excluding child processes such as git, terminals, and debug adapters).

| Metric | Target | Maximum limit |
|---|---|---|
| Cold start until window ready | < 200 ms | 400 ms |
| Idle RAM without workspace | < 40 MB | 60 MB |
| RAM with 10k-file workspace, 5 open tabs | < 120 MB | 180 MB |
| Open 1 MB file | < 30 ms | 80 ms |
| Open 100 MB file (large file mode) | < 500 ms | 1 s |
| Keypress-to-render latency | < 8 ms | 16 ms |
| Scroll | 120 fps (ProMotion) | stable 60 fps |
| Go to File, first result (10k files) | < 10 ms per keystroke | 30 ms |
| Initial file index (100k files) | < 1 s | 3 s |
| Workspace search (1 GB repo) | first result < 100 ms | 300 ms |
| Syntax highlight after edit | < 2 ms (incremental) | 8 ms |

Principles:
- No blocking operations on the main thread other than layout and viewport rendering.
- Every cache has a size limit and an eviction policy (LRU).
- Benchmarks and RAM measurements run in CI; a regression > 10% blocks the merge.

## 7. Features

Priority: P0 = required for v1, P1 = v1 target, P2 = after v1.

### 7.1 Workspace and File Explorer (P0)
- Open a folder via menu, drag-and-drop, and CLI `galaxy <path>`.
- Lazy tree Explorer: reads only expanded directories.
- Respects `.gitignore`, `.ignore`, and built-in excludes (`.git`, `node_modules`, `target`, `dist`, `build`).
- File watcher (FSEvents) updates the tree and buffers.
- Create, rename, delete (to Trash), reveal in Finder.
- Recent workspaces and session restore (tabs, cursor position).

Acceptance: a 100k-file workspace opens without freezing the UI; external file changes are visible in < 200 ms.

### 7.2 Text Editor (P0)
- Custom editor view (not `NSTextView`) with viewport-only rendering.
- Rope-based buffer (`ropey`).
- Multi-tab, horizontal/vertical split view (P1).
- Transaction-based undo/redo, grouped per typing session.
- Multi-cursor and multiple selection (`Cmd+D`, `Opt+Click`, column selection).
- Auto-indent, auto-close brackets/quotes, comment toggle, move/duplicate line.
- Line numbers, current line highlight, indent guides, bracket matching.
- Optional soft wrap.
- In-file find/replace, including regex and case/whole word.
- UTF-8 encoding (primary), UTF-16 detection, and LF/CRLF line ending detection.
- Full IME support via `NSTextInputClient`, emoji, and grapheme clusters.
- Large file mode (> 20 MB): tree-sitter and language engines disabled, soft wrap off.

Acceptance: typing in a 50k-line file stays < 16 ms per keypress; Japanese/Chinese IME works.

### 7.3 Syntax Highlighting (P0)
- `tree-sitter` with incremental parsing on a background thread.
- v1 languages: Rust, TypeScript, TSX, JavaScript, JSON, TOML, YAML, Markdown, Python, Go, HTML, CSS, Bash.
- Grammars statically bundled; highlight queries based on `highlights.scm`.
- Only nodes within the viewport (+ margin) are converted to style runs.
- Language injection (e.g. Markdown code blocks) — P1.
- Semantic tokens from the built-in engines as an additional layer — P2.
- Built-in themes: 1 light, 1 dark; simple JSON theme format.

Acceptance: highlighting never blocks input; re-highlight after edit < 8 ms.

### 7.4 Go to File and Quick Navigation (P0)
- `Cmd+P`: fuzzy file finder (`nucleo`); ranking accounts for recency and path proximity.
- `Cmd+Shift+O`: go to symbol in file (outline from the built-in engine, tree-sitter outline as fallback).
- `Cmd+T`: go to workspace symbol — P2 (removed together with the language server client).
- `Ctrl+G`: go to line.
- `Ctrl+-` / `Ctrl+Shift+-`: back/forward navigation through position history.
- Quick switch to the last tab (`Ctrl+Tab`).

Acceptance: Go to File results appear in < 30 ms per keystroke on 100k files.

### 7.5 Command Palette and Keybindings (P0)
- `Cmd+Shift+P`: all registered commands, fuzzy-searchable.
- Default keybindings similar to VSCode for easy migration.
- Keybinding overrides via a `keybindings.json` file.
- Settings via `settings.json` (global and per workspace `.galaxy/settings.json`).

### 7.6 Code Intelligence (P0)
- Built-in engines run inside the Galaxy process; no external language servers, Node, or plugins are installed or launched.
- Languages: PHP/Blade/Laravel, Go, TypeScript/JavaScript, Rust, Dart/Flutter. Other languages get syntax highlighting only.
- Engines index the workspace in the background and read dependencies statically (`composer.json`/`vendor/`, `go.mod`/module cache, `node_modules` type declarations, `.dart_tool/package_config.json`); project code is never executed.
- P0 features:
  - Diagnostics (underline + Problems panel)
  - Hover
  - Completion (with auto-import edits and basic snippets)
  - Go to definition
  - Find references
  - Rename symbol
  - Code actions / quick fix
  - Signature help
  - Document outline
- P2 features: type definition, implementation, workspace symbols, document formatting, inlay hints, semantic tokens, call hierarchy.
- Isolation: indexing runs off the main thread, and a request that cannot finish within its budget returns no result instead of blocking the UI.

Acceptance: supported features work on real repositories per language (e.g. `ripgrep` for Rust); completion appears < 50 ms after engine results are available.

### 7.7 Workspace Search (P0)
- `Cmd+Shift+F`: search the entire workspace using the `grep-*` / `ignore` crates (ripgrep engine).
- Regex, case sensitive, whole word, include/exclude glob.
- Streaming results, capped (default 10k matches), cancellable.
- Replace all with preview — P1.

### 7.8 Git (P1)
- Current branch in the status bar.
- File status in the Explorer (modified, added, untracked).
- Gutter diff (added/modified/deleted lines) against HEAD.
- Implemented via `gix` (gitoxide); no commit UI in v1.

### 7.9 Integrated Terminal (P1)
- Terminal panel with PTY (`portable-pty`) and `vte` / `alacritty_terminal` parser.
- Multiple terminal tabs.
- Capped scrollback (default 10k lines).

### 7.10 Developer Productivity (P1)
- Symbol breadcrumb above the editor.
- Sticky scroll (scope headers stay visible).
- Optional auto-save.
- Quick open file from errors in terminal/diagnostics (path:line:col).
- Peek definition — P2.

## 8. Architecture

```text
┌───────────────────────── Swift / AppKit ─────────────────────────┐
│ Window, Tabs, Explorer, Palette, Panels, Status bar              │
│ EditorView (NSView + Core Text, layer-backed, viewport-only)     │
│ NSTextInputClient (IME), Accessibility                           │
└───────────────▲──────────────────────────────┬───────────────────┘
                │ snapshot viewport, events    │ commands, input
┌───────────────┴──────────── UniFFI ──────────▼───────────────────┐
│                         Rust core (galaxy-core)                  │
│ Document store (ropey) · Undo · Selections · Edit ops            │
│ Syntax service (tree-sitter, worker thread)                      │
│ Workspace: file index, FSEvents watcher (notify), ignore         │
│ Fuzzy finder (nucleo) · Search (grep crates)                     │
│ Language engines (in-process): PHP, Go, TS/JS, Rust, Dart        │
│ Git (gix) · Terminal (portable-pty + vte)                        │
│ Settings/keymap · Session store                                  │
└──────────────────────────────────────────────────────────────────┘
```

Design decisions:
- Rust is the source of truth for document state. Swift only receives viewport snapshots (lines, style runs, selections, diagnostics) and sends commands.
- Rust-to-Swift communication uses event callbacks coalesced per frame (`CVDisplayLink`/`CADisplayLink`) to avoid flooding updates.
- Initial rendering uses Core Text with a `CTLine` cache per visible line. Migrate to Metal only if profiling shows Core Text is the bottleneck.
- Separate worker threads for syntax, index, and search; `tokio` runtime for child-process I/O (debug adapters).
- No WebView anywhere.

## 9. RAM efficiency strategy

- Rope with shared chunks (structural sharing) for cheap snapshots.
- Files that are not open are never loaded into memory; the index stores only paths (interned, arena).
- Tabs inactive for a long time: syntax tree is discarded and reparsed when reopened.
- Capped undo history (default 1000 steps or 32 MB per document).
- `CTLine` and glyph caches only for the viewport + margin.
- `mimalloc` allocator evaluated against the system allocator.
- Response to memory pressure (`DispatchSource.makeMemoryPressureSource`): clear non-essential caches.
- Measurement uses `footprint`, Instruments (Allocations, Leaks), and `dhat` in Rust.

## 10. Technology stack

| Area | Choice |
|---|---|
| UI | Swift 6, AppKit |
| Bridge | UniFFI |
| Text buffer | `ropey` |
| Syntax | `tree-sitter` + bundled grammars |
| Async | `tokio` |
| File watch | `notify` (FSEvents) |
| Ignore/walk | `ignore` |
| Fuzzy | `nucleo` |
| Search | `grep-searcher`, `grep-regex` |
| Git | `gix` |
| Terminal | `portable-pty`, `alacritty_terminal` |
| Config | `serde`, `serde_json` |
| Logging | `tracing` |
| Build | Cargo workspace + Xcode project (xcodegen/Tuist) |

## 11. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Custom editor view is complex (IME, a11y, bidi) | High | Early spike in M1; follow `NSTextInputClient` strictly; test IME from the start |
| Per-frame FFI overhead | Medium | Batch snapshots per frame; avoid per-character calls |
| Built-in engines miss language semantics a full compiler would catch | High | Engines stay conservative: return no result rather than a wrong one; diagnostics limited to checks they can prove; test against real repositories |
| High RAM for Rust analysis | Medium | Built-in engine caps indexing and work per request; show Galaxy process usage in the status bar |
| Scope creep | High | Non-goals locked; new features only after v1 |
| Core Text rendering not fast enough | Medium | Benchmark in M1; Metal fallback path |

## 12. Success metrics

- All budgets in section 6 are met in the v1 build.
- Usable daily (dogfooding) to develop Galaxy itself starting from M4.
- Crash-free sessions > 99.5%.
- No memory leaks after an 8-hour session (RAM stable within ±10%).

## 13. Milestones

| Milestone | Scope | Estimate |
|---|---|---|
| M0 | Project setup, bridge, CI, benchmark harness | 2 weeks |
| M1 | Core editor view, rope, input, IME, rendering | 5 weeks |
| M2 | Workspace, Explorer, tabs, Go to File, palette | 3 weeks |
| M3 | Tree-sitter syntax highlighting, themes | 3 weeks |
| M4 | Built-in language engines, diagnostics, navigation | 5 weeks |
| M5 | Workspace search, more language engines, settings | 3 weeks |
| M6 | Git, terminal, productivity | 4 weeks |
| M7 | Hardening, performance, RAM, beta release | 3 weeks |

Total estimate: ~28 weeks for 1–2 experienced developers.
