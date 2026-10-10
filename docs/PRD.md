# PRD: Galaxy — Code Editor Native macOS

Versi: 0.1 (draft)
Tanggal: 2026-10-05
Platform: macOS 14+ (Apple Silicon prioritas, Intel didukung)

## 1. Ringkasan

Galaxy adalah code editor native macOS dengan fokus utama pada performa dan efisiensi RAM. UI dibangun dengan Swift + AppKit, sedangkan core (text buffer, parsing, indexing, LSP, search) ditulis dalam Rust. Fitur difokuskan pada navigasi kode cepat: LSP, Go to File, syntax highlighting, dan workflow keyboard-first.

## 2. Masalah

- VSCode dan editor berbasis Electron memakai RAM besar (sering 500 MB–2 GB untuk workspace menengah).
- Startup dan pembukaan workspace besar terasa lambat.
- Developer butuh editor harian yang ringan tetapi tetap punya kemampuan IDE (LSP).

## 3. Tujuan

1. Editor yang terasa instan: startup, buka file, ketik, scroll, dan navigasi tanpa jeda.
2. Pemakaian RAM jauh di bawah VSCode untuk workflow yang sama.
3. Integrasi LSP yang stabil dan lengkap untuk fitur navigasi dan editing inti.
4. Workflow keyboard-first yang mempercepat kerja developer.

## 4. Non-goals (di luar scope v1)

- Plugin/extension yang bisa diinstal dari luar.
- Marketplace.
- Remote development (SSH, container, WSL).
- Collaboration/real-time multiplayer.
- Debugger (DAP).
- Dukungan Windows/Linux.
- Kompatibilitas extension/tema VSCode.
- Notebook, preview Markdown kaya, AI assistant.

## 5. Target pengguna

- Developer individu di macOS yang bekerja dengan Rust, TypeScript/JavaScript, Python, Go.
- Pengguna MacBook dengan RAM 8–16 GB yang merasa VSCode terlalu berat.
- Pengguna keyboard-heavy yang mengutamakan navigasi cepat.

## 6. Anggaran performa dan RAM (wajib)

Diukur pada MacBook Air M1 8 GB, build release. Angka RAM = physical footprint proses editor (tidak termasuk proses language server).

| Metrik | Target | Batas maksimum |
|---|---|---|
| Cold start hingga window siap | < 200 ms | 400 ms |
| RAM idle tanpa workspace | < 40 MB | 60 MB |
| RAM workspace 10k file, 5 tab terbuka | < 120 MB | 180 MB |
| Buka file 1 MB | < 30 ms | 80 ms |
| Buka file 100 MB (mode file besar) | < 500 ms | 1 s |
| Latensi keypress hingga render | < 8 ms | 16 ms |
| Scroll | 120 fps (ProMotion) | 60 fps stabil |
| Go to File, hasil pertama (10k file) | < 10 ms per ketikan | 30 ms |
| Indeks file awal (100k file) | < 1 s | 3 s |
| Workspace search (repo 1 GB) | hasil pertama < 100 ms | 300 ms |
| Syntax highlight setelah edit | < 2 ms (incremental) | 8 ms |

Prinsip:
- Tidak ada operasi blocking di main thread selain layout dan render viewport.
- Semua cache punya batas ukuran dan kebijakan eviction (LRU).
- Benchmark dan pengukuran RAM berjalan di CI; regresi > 10% memblokir merge.

## 7. Fitur

Prioritas: P0 = wajib v1, P1 = target v1, P2 = setelah v1.

### 7.1 Workspace dan File Explorer (P0)
- Buka folder via menu, drag-and-drop, dan CLI `galaxy <path>`.
- Tree explorer lazy: hanya membaca direktori yang dibuka.
- Menghormati `.gitignore`, `.ignore`, dan exclude bawaan (`.git`, `node_modules`, `target`, `dist`, `build`).
- File watcher (FSEvents) memperbarui tree dan buffer.
- Create, rename, delete (ke Trash), reveal in Finder.
- Recent workspaces dan restore session (tab, posisi cursor).

Acceptance: workspace 100k file terbuka tanpa freeze UI; perubahan file eksternal terlihat < 200 ms.

### 7.2 Text Editor (P0)
- Custom editor view (bukan `NSTextView`) dengan rendering viewport-only.
- Buffer berbasis rope (`ropey`).
- Multi-tab, split view horizontal/vertikal (P1).
- Undo/redo berbasis transaksi, dikelompokkan per sesi ketik.
- Multi-cursor dan multiple selection (`Cmd+D`, `Opt+Click`, column selection).
- Auto-indent, auto-close bracket/quote, comment toggle, move/duplicate line.
- Line numbers, current line highlight, indent guides, bracket matching.
- Soft wrap opsional.
- Find/replace dalam file, termasuk regex dan case/whole word.
- Encoding UTF-8 (utama), deteksi UTF-16 dan line ending LF/CRLF.
- Dukungan IME penuh via `NSTextInputClient`, emoji, dan grapheme cluster.
- Mode file besar (> 20 MB): tree-sitter dan LSP dimatikan, soft wrap off.

Acceptance: mengetik di file 50k baris tetap < 16 ms per keypress; IME Jepang/Cina berfungsi.

### 7.3 Syntax Highlighting (P0)
- `tree-sitter` dengan parsing incremental di background thread.
- Bahasa v1: Rust, TypeScript, TSX, JavaScript, JSON, TOML, YAML, Markdown, Python, Go, HTML, CSS, Bash.
- Grammar dibundel statis, query highlight berbasis `highlights.scm`.
- Hanya node dalam viewport (+ margin) yang dikonversi ke style run.
- Language injection (contoh: code block Markdown) — P1.
- Semantic tokens dari LSP sebagai layer tambahan — P1.
- Tema bawaan: 1 terang, 1 gelap; format tema JSON sederhana.

Acceptance: highlight tidak pernah memblok input; re-highlight setelah edit < 8 ms.

### 7.4 Go to File dan Navigasi Cepat (P0)
- `Cmd+P`: fuzzy finder file (`nucleo`), ranking memperhitungkan recency dan kedekatan path.
- `Cmd+Shift+O`: go to symbol dalam file (tree-sitter outline, LSP `documentSymbol` bila ada).
- `Cmd+T`: go to symbol workspace (LSP `workspace/symbol`) — P1.
- `Ctrl+G`: go to line.
- `Ctrl+-` / `Ctrl+Shift+-`: navigasi back/forward riwayat posisi.
- Quick switch tab terakhir (`Ctrl+Tab`).

Acceptance: hasil Go to File muncul < 30 ms per ketikan pada 100k file.

### 7.5 Command Palette dan Keybinding (P0)
- `Cmd+Shift+P`: semua command terdaftar dan dapat dicari fuzzy.
- Keybinding default mirip VSCode agar migrasi mudah.
- Override keybinding via file `keybindings.json`.
- Settings via `settings.json` (global dan per workspace `.galaxy/settings.json`).

### 7.6 LSP (P0)
- Satu proses server per (bahasa, root workspace), dijalankan saat file bahasa itu pertama dibuka.
- Deteksi server di `PATH`; konfigurasi path kustom di settings. Server tidak dibundel.
- Server v1: `rust-analyzer`, `typescript-language-server`, `pyright-langserver`, `gopls`.
- Document sync incremental, debounce perubahan.
- Cancellation (`$/cancelRequest`) untuk request usang.
- Fitur P0:
  - Diagnostics (underline + panel Problems)
  - Hover
  - Completion (dengan `resolve`, snippet dasar)
  - Go to definition, type definition, implementation
  - Find references
  - Rename symbol
  - Formatting dokumen dan format on save
  - Code actions / quick fix
  - Signature help
- Fitur P1: inlay hints, semantic tokens, document highlight, call hierarchy.
- Progress indicator (`$/progress`) dan status server di status bar.
- Auto-restart server bila crash (maks 3 kali per 5 menit).
- Timeout dan isolasi: server lambat tidak boleh memblok UI.

Acceptance: semua fitur P0 berjalan dengan `rust-analyzer` pada repo Rust nyata (misal `ripgrep`); completion tampil < 50 ms setelah respons server diterima.

### 7.7 Workspace Search (P0)
- `Cmd+Shift+F`: search seluruh workspace memakai crate `grep-*` / `ignore` (engine ripgrep).
- Regex, case sensitive, whole word, include/exclude glob.
- Hasil streaming, dibatasi (default 10k match), bisa dibatalkan.
- Replace all dengan preview — P1.

### 7.8 Git (P1)
- Branch saat ini di status bar.
- Status file di explorer (modified, added, untracked).
- Gutter diff (added/modified/deleted line) terhadap HEAD.
- Implementasi via `gix` (gitoxide); tanpa UI commit di v1.

### 7.9 Terminal Terintegrasi (P1)
- Panel terminal dengan PTY (`portable-pty`) dan parser `vte` / `alacritty_terminal`.
- Multiple terminal tab.
- Scrollback dibatasi (default 10k baris).

### 7.10 Produktivitas Developer (P1)
- Breadcrumb symbol di atas editor.
- Sticky scroll (header scope tetap terlihat).
- Auto-save opsional.
- Quick open file dari error di terminal/diagnostics (path:line:col).
- Peek definition — P2.

## 8. Arsitektur

```text
┌───────────────────────── Swift / AppKit ─────────────────────────┐
│ Window, Tabs, Explorer, Palette, Panels, Status bar              │
│ EditorView (NSView + Core Text, layer-backed, viewport-only)     │
│ NSTextInputClient (IME), Accessibility                           │
└───────────────▲──────────────────────────────┬───────────────────┘
                │ snapshot viewport, events    │ commands, input
┌───────────────┴──────────── UniFFI ──────────▼───────────────────┐
│                         Rust core (galaxy-core)                   │
│ Document store (ropey) · Undo · Selections · Edit ops            │
│ Syntax service (tree-sitter, worker thread)                      │
│ Workspace: file index, FSEvents watcher (notify), ignore         │
│ Fuzzy finder (nucleo) · Search (grep crates)                     │
│ LSP manager (tokio, JSON-RPC over stdio)                         │
│ Git (gix) · Terminal (portable-pty + vte)                        │
│ Settings/keymap · Session store                                  │
└──────────────────────────────────────────────────────────────────┘
```

Keputusan desain:
- Rust adalah sumber kebenaran state dokumen. Swift hanya menerima snapshot viewport (baris, style run, selection, diagnostics) dan mengirim perintah.
- Komunikasi Rust ke Swift melalui callback event yang dikoalesasi per frame (`CVDisplayLink`/`CADisplayLink`) untuk menghindari banjir update.
- Rendering awal memakai Core Text dengan cache `CTLine` per baris terlihat. Migrasi ke Metal hanya bila profiling menunjukkan Core Text menjadi bottleneck.
- Worker thread terpisah untuk syntax, index, search; runtime `tokio` untuk LSP dan I/O.
- Tidak ada WebView di mana pun.

## 9. Strategi efisiensi RAM

- Rope dengan chunk berbagi (structural sharing) untuk snapshot murah.
- File yang tidak terbuka tidak pernah dimuat ke memori; index hanya menyimpan path (interned, arena).
- Tab yang tidak aktif lama: syntax tree dibuang, diparse ulang saat dibuka kembali.
- Undo history dibatasi (default 1000 langkah atau 32 MB per dokumen).
- Cache `CTLine` dan glyph hanya untuk viewport + margin.
- Allocator `mimalloc` dievaluasi terhadap system allocator.
- Respons terhadap memory pressure (`DispatchSource.makeMemoryPressureSource`): kosongkan cache non-esensial.
- Pengukuran memakai `footprint`, Instruments (Allocations, Leaks), dan `dhat` di Rust.

## 10. Stack teknologi

| Area | Pilihan |
|---|---|
| UI | Swift 6, AppKit |
| Bridge | UniFFI |
| Text buffer | `ropey` |
| Syntax | `tree-sitter` + grammar bundel |
| Async | `tokio` |
| LSP types | `lsp-types` |
| File watch | `notify` (FSEvents) |
| Ignore/walk | `ignore` |
| Fuzzy | `nucleo` |
| Search | `grep-searcher`, `grep-regex` |
| Git | `gix` |
| Terminal | `portable-pty`, `alacritty_terminal` |
| Config | `serde`, `serde_json` |
| Logging | `tracing` |
| Build | Cargo workspace + Xcode project (xcodegen/Tuist) |

## 11. Risiko

| Risiko | Dampak | Mitigasi |
|---|---|---|
| Custom editor view rumit (IME, a11y, bidi) | Tinggi | Spike awal di M1; ikuti `NSTextInputClient` ketat; uji IME sejak awal |
| Overhead FFI per frame | Sedang | Snapshot batch per frame, hindari panggilan per karakter |
| Variasi perilaku LSP server | Tinggi | Uji dengan 4 server nyata; capability negotiation ketat |
| RAM language server besar (rust-analyzer > 1 GB) | Sedang | Di luar kendali editor; tampilkan RAM server di status bar, opsi stop server |
| Scope creep | Tinggi | Non-goals dikunci; fitur baru hanya setelah v1 |
| Rendering Core Text tidak cukup cepat | Sedang | Benchmark M1; jalur cadangan Metal |

## 12. Metrik keberhasilan

- Seluruh anggaran di bagian 6 terpenuhi pada build v1.
- Dapat dipakai harian (dogfooding) untuk mengembangkan Galaxy sendiri sejak M4.
- Crash-free session > 99.5%.
- Tidak ada kebocoran memori setelah sesi 8 jam (RAM stabil ±10%).

## 13. Milestone

| Milestone | Isi | Estimasi |
|---|---|---|
| M0 | Setup proyek, bridge, CI, harness benchmark | 2 minggu |
| M1 | Editor view inti, rope, input, IME, render | 5 minggu |
| M2 | Workspace, explorer, tab, Go to File, palette | 3 minggu |
| M3 | Syntax highlighting tree-sitter, tema | 3 minggu |
| M4 | LSP (rust-analyzer), diagnostics, navigasi | 5 minggu |
| M5 | Workspace search, multi-LSP, settings | 3 minggu |
| M6 | Git, terminal, produktivitas | 4 minggu |
| M7 | Hardening, performa, RAM, rilis beta | 3 minggu |

Total estimasi: ±28 minggu untuk 1–2 developer berpengalaman.
