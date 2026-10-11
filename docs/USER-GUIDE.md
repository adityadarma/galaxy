# Galaxy User Guide

Galaxy adalah editor macOS native untuk membuka workspace, mengedit file,
menjalankan tool terminal, dan memakai fitur LSP.

## Build Lokal

Persyaratan:

- macOS 14 atau lebih baru.
- Xcode dengan Swift 6.
- Rust toolchain sesuai `rust-toolchain.toml` bila tersedia.
- `xcodegen` untuk membuat project Xcode.

Build dan jalankan aplikasi lokal:

```sh
scripts/build-core.sh
(cd app && xcodegen generate)
xcodebuild -project app/Galaxy.xcodeproj -scheme Galaxy \
  -configuration Debug -derivedDataPath app/build build
open app/build/Build/Products/Debug/Galaxy.app
```

CLI helper dapat dipasang ke `~/.local/bin`:

```sh
scripts/install-cli.sh
galaxy /path/to/project
```

## Membuka Workspace

- `File > Open File or Folder…` membuka file atau folder.
- `File > Open Folder…` membuka workspace folder.
- Explorer menampilkan file dan folder workspace.
- Klik file sekali untuk preview tab; edit atau double-click membuat tab tetap.
- Tab menyimpan posisi caret dan scroll masing-masing.
- Perubahan Git ditandai pada explorer, folder ancestor, dan tab.

## Editing

- `Cmd+S`: simpan.
- `Cmd+Shift+S`: simpan sebagai file baru.
- `Cmd+Z` / `Cmd+Shift+Z`: undo / redo.
- `Cmd+F`: find.
- `Cmd+Option+F`: find and replace.
- `Ctrl+G`: go to line.
- `Cmd+P`: go to file.
- `Cmd+Shift+O`: go to symbol.
- `Cmd+T`: go to workspace symbol.
- `Cmd+D`: tambah occurrence berikutnya ke multi-cursor.
- `Cmd+Shift+L`: pilih semua occurrence.
- `Cmd+Option+Up` / `Cmd+Option+Down`: tambah cursor di atas/bawah.
- `Cmd+/`: toggle line comment.
- `Cmd+.`: code actions.

Editor mendukung auto-indent, bracket pairing, bracket matching, UTF-16 LSP
positions, multi-cursor, IME melalui `NSTextInputClient`, dan undo transaction.

## Navigasi Kode

Jika language server tersedia, Galaxy menyediakan:

- Hover.
- Completion.
- Signature help.
- Go to definition, type definition, dan implementation.
- Find references.
- Rename symbol.
- Code actions.
- Document/workspace symbols.
- Diagnostics di editor, gutter, scrollbar, dan Problems panel.
- Semantic tokens dan inlay hints jika server mendukung capability tersebut.

Klik diagnostic di Problems panel untuk membuka file dan lokasi terkait.
Breadcrumb di bawah path file menunjukkan symbol aktif. Klik breadcrumb untuk
menuju deklarasinya.

## Terminal

- `Cmd+J`: buka/tutup terminal.
- Klik `+` untuk membuat terminal tab baru.
- Tab terminal tetap menjalankan PTY saat panel disembunyikan.
- Copy/paste memakai clipboard macOS.
- Double-click `path:line:column` pada output terminal untuk membuka file.
- Scrollback terminal menyimpan hingga 10.000 baris.

## Split Editor

Gunakan menu `View` atau Command Palette:

- `Split Editor Right`.
- `Split Editor Down`.
- `Close Editor Split`.

Kedua pane mengikuti tab aktif dan berbagi document state yang sama.

## Search dan Git

- `Cmd+Shift+F`: workspace search.
- Search mendukung regex, case-sensitive, whole word, include glob, dan
  exclude glob.
- Hasil dibatasi 10.000 match dan dapat dibuka langsung ke lokasi.
- Status bar menampilkan branch dan jumlah file berubah.
- Gutter menampilkan diff terhadap `HEAD` untuk file yang dibuka.

## Settings

Buka melalui `View > Open User Settings (JSON)…` atau workspace settings.
Settings global berada di:

```text
~/Library/Application Support/Galaxy/settings.json
```

Workspace settings berada di:

```text
.galaxy/settings.json
```

Workspace settings mengoverride global settings.
