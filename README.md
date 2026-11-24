# Galaxy

A native macOS code editor built for speed and low memory use. The UI is Swift
and AppKit; the text buffer, parsing, indexing, search and language engines are
written in Rust. Code intelligence is built in, so there are no language
servers to install.

> Galaxy is in beta. Expect rough edges, and keep backups of work you care about.

## Features

- **Built-in code intelligence** for PHP, Blade, Go, Rust, TypeScript, TSX,
  JavaScript, Vue, Svelte and Dart: completion, hover, go to definition, find
  references with an inline peek, rename, diagnostics and document outline.
  Framework support covers Laravel, Symfony, CodeIgniter, Goravel, Fiber,
  Flutter and more. See [FEATURE.md](FEATURE.md) for the full matrix.
- **Syntax highlighting** for over 20 more languages, including Python, C, C++,
  C#, Java, Swift, Ruby, HTML, CSS, JSON, YAML, TOML, Markdown and SQL.
- **Fast navigation:** Go to File, Go to Symbol, Command Palette, workspace
  search with regex and globs, breadcrumbs and sticky scroll.
- **Editing:** multiple cursors, snippets, bracket matching, auto-closing and
  linked tag editing, split editors, minimap, format on save and auto save.
- **Git:** Explorer and gutter decorations, source control panel, inline blame,
  commit graph and branch picker.
- **Integrated terminal** with tabs and clickable `path:line:column` links.
- **Optional AI** through any OpenAI-compatible endpoint (OpenAI, OpenRouter,
  LM Studio, Ollama): inline code suggestions and commit messages. API keys are
  stored in the macOS Keychain.
- **Command line tool:** `galaxy .` opens the current folder.
- Light and dark themes, and automatic updates through Sparkle.

## Performance

Measured on Apple Silicon with Release builds. Full numbers and method are in
[docs/PERFORMANCE.md](docs/PERFORMANCE.md).

| Metric | Target | Measured |
|---|---:|---:|
| Idle memory, no workspace | < 40 MB | 28 MB |
| Memory, 10k-file workspace with 5 tabs | < 120 MB | 55 MB |
| Keypress to render, p95 | < 8 ms | 1.1–1.5 ms |
| Go to File over 10k files, median | < 10 ms | 1.5 ms |
| Initial index of 100k files | < 1 s | 622–823 ms |

## Install

Requires macOS 14 or later.

1. Download the ZIP for your Mac from the
   [latest release](https://github.com/adityadarma/galaxy/releases/latest):
   `arm64` for Apple Silicon, `x86_64` for Intel.
2. Unzip it and move `Galaxy.app` to `/Applications`.
3. Builds are self-signed, so macOS blocks the first launch. Open
   **System Settings > Privacy & Security** and click **Open Anyway** for
   Galaxy. Later updates install without this step.

## Build from Source

Requirements: Xcode with Swift 6, the Rust toolchain pinned in
`rust-toolchain.toml`, and [XcodeGen](https://github.com/yonaskolb/XcodeGen).

```sh
scripts/build-core.sh
(cd app && xcodegen generate)
xcodebuild -project app/Galaxy.xcodeproj -scheme Galaxy \
  -configuration Debug -derivedDataPath app/build build
open app/build/Build/Products/Debug/Galaxy.app
```

Run the Rust and Swift test suites with `scripts/test-all.sh`.

## Documentation

- [User Guide](docs/USER-GUIDE.md): shortcuts, terminal, search, Git and settings.
- [Configuration](docs/CONFIGURATION.md): `settings.json` keys.
- [Language Feature Matrix](FEATURE.md): what each language engine supports.
- [Releases](docs/RELEASE.md): how signed builds are published.
