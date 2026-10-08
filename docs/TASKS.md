# Tasks: RustCode

Referensi: `docs/PRD.md`. Setiap task selesai bila acceptance criteria terpenuhi dan benchmark terkait tidak regresi.

Format: `[ ] ID — Task` lalu kriteria selesai.

## M0 — Fondasi (2 minggu)

- [x] M0-01 — Buat Cargo workspace: `crates/rcode-core`, `crates/rcode-ffi`, `crates/rcode-bench`.
  - `cargo build` dan `cargo test` lulus.
- [x] M0-02 — Buat proyek Xcode (`app/RustCode`) via xcodegen/Tuist, target macOS 14, Swift 6.
  - App kosong terbuka dan menampilkan window.
- [x] M0-03 — Integrasi UniFFI: build universal static lib (`arm64` + `x86_64`) dan generate binding Swift.
  - Swift memanggil fungsi Rust `core_version()` dan menerima callback event.
- [x] M0-04 — Script build `scripts/build-core.sh` dipanggil dari build phase Xcode.
- [x] M0-05 — Setup `tracing` di Rust dan `os_log` di Swift, dengan log file di `~/Library/Logs/RustCode`.
- [x] M0-06 — CI GitHub Actions (macOS runner): `cargo fmt --check`, `clippy -D warnings`, `cargo test`, `xcodebuild test`.
- [x] M0-07 — Harness benchmark: `criterion` untuk core, script pengukuran startup dan RAM (`footprint`) untuk app.
  - Hasil benchmark disimpan sebagai artifact CI; regresi > 10% gagal.
- [x] M0-08 — Siapkan fixture: file 1 MB, 50k baris, 100 MB, repo 10k dan 100k file (di-generate script).

## M1 — Editor Core (5 minggu)

### Rust
- [x] M1-01 — `Document` dengan `ropey`: load, save atomik (write temp + rename), deteksi encoding dan line ending.
- [x] M1-02 — Model `Selection`/`Cursor` multi-cursor dengan posisi berbasis char offset dan konversi line/col (UTF-8, UTF-16 untuk LSP).
- [x] M1-03 — Edit operations: insert, delete, newline + auto-indent, backspace grapheme-aware, indent/outdent.
- [x] M1-04 — Undo/redo berbasis transaksi, grouping per jeda ketik 500 ms, batas langkah dan ukuran.
- [x] M1-05 — API snapshot viewport: `viewport(doc, first_line, count) -> ViewportSnapshot` (teks baris, selection, cursor).
  - Benchmark: snapshot 100 baris < 0.5 ms.
- [x] M1-06 — Unit test dan property test (`proptest`) untuk edit + undo konsisten.

### Swift
- [x] M1-07 — `EditorView: NSView` layer-backed dengan render Core Text per baris terlihat dan cache `CTLine`.
- [ ] M1-08 — Scroll native (`NSScrollView`) dengan dokumen virtual setinggi total baris.
  - 60 fps stabil di file 50k baris (Instruments).
  - Status: implementasi memakai tinggi virtual dan hanya menggambar viewport; `scripts/measure-scroll.sh 120 50000` (Release) mengukur render viewport p50 0,74 ms dan p95 1,13 ms, jauh di bawah anggaran frame 16,67 ms. Validasi Core Animation/Instruments untuk frame yang benar-benar dikomposit masih diperlukan sebelum task dicentang.
- [x] M1-09 — Gutter line number, current line highlight, cursor blink, selection rendering.
- [ ] M1-10 — Implementasi `NSTextInputClient`: marked text, IME candidate window, dead keys.
  - Uji manual: Jepang (Kotoeri), Cina Pinyin, Korea, emoji picker.
  - Status: implementasi dan test otomatis selesai (core `composition.rs`, Swift `EditorView+TextInput.swift`). Uji manual dengan IME sungguhan BELUM dilakukan; centang setelah itu.
- [x] M1-11 — Mapping keyboard: navigasi (word, line, page, doc), selection extend, clipboard.
  - Status: core `movement.rs` dan `clipboard.rs` (unit test dan proptest), FFI, serta Swift `EditorView+Navigation.swift` selesai, termasuk klik, drag, double/triple-click, Shift-click, Opt-click, Cut/Copy/Paste linewise, dan Save (⌘S, ⇧⌘S) dengan prompt perubahan belum tersimpan. Diuji otomatis saja; belum dicoba manual di app.
- [x] M1-12 — Multi-cursor: `Cmd+D`, `Opt+Click`, `Opt+Shift+drag` column selection.
  - Status: core `multicursor.rs` (unit test dan proptest), FFI, dan Swift selesai: ⌘D, ⇧⌘L, ⌥⌘↑/↓, Opt+Click, Opt+Shift+drag. Diuji otomatis saja; belum dicoba manual di app.
- [x] M1-13 — Find/replace in-file (regex, case, whole word) dengan highlight match di viewport.
  - Status: core `find.rs` (crate `regex` dipin `=1.13.1`), FFI, dan Swift `FindBar` selesai: ⌘F, ⌥⌘F, ⌘G, ⇧⌘G, ⌘E, Enter/Shift+Enter, Alt+Enter untuk pilih semua, replace satu dan semua (`$1` di mode regex), hitungan "n of m", dan highlight di viewport. Pencarian per baris, jadi pola tidak bisa melewati baris (`\n` tidak cocok). Diuji otomatis saja; belum dicoba manual di app, dan belum diukur pada file besar.
- [x] M1-14 — Mode file besar (> 20 MB): fitur berat dimatikan, banner info.
  - Status: `Document::is_large` (> 20 MB) memunculkan banner dan mematikan hitungan find live (scan 20 MB ≈ 93 ms). Baris > 10.000 char dipotong di viewport untuk semua file: snapshot baris 10 MB turun dari 3,1 ms ke 1,2 µs; hit-test memakai teks yang sama. Tree-sitter, LSP, dan soft wrap belum ada, jadi belum ada yang perlu dimatikan. Load 100 MB di memori ≈ 41 ms (bench `large/from_bytes_100mb`); waktu buka di app belum diukur.
- [x] M1-15 — Koalesasi update per frame (display link) agar FFI dipanggil maks 1x per frame.
  - Status: hitungan find untuk dokumen > 1 MB berjalan di background dari snapshot rope (`FindCountJob`, bisa dibatalkan, hasil basi dibuang). Pekerjaan seluruh dokumen (`is_modified`, hitungan find) dijalankan maks 1x per frame lewat `CADisplayLink`; test memastikan 50 ketikan = 1 notifikasi. Metadata total baris, primary caret, dan paint memakai satu `ViewportSnapshot` per render. Input normal tetap dapat gambar langsung, tetapi burst lebih cepat dari refresh ditahan sampai display-link berikutnya. Benchmark Release 50k baris: median 3,44 ms, p95 4,82 ms.
- [x] M1-16 — Ukur latensi keypress-to-render; target < 8 ms.
  - Status: `LatencyProbe` mencatat dari timestamp event tombol sampai akhir `draw` (plus signpost `keypress-to-render` untuk Instruments); menu View > Show Typing Latency. Setelah redraw viewport segera di akhir `keyDown` (kerja seluruh dokumen tetap dikoalesasi per frame), `scripts/measure-latency.sh 100 50000` pada Release menghasilkan median 1,35 ms, p95 2,96 ms, max 13,91 ms; target <8 ms terpenuhi. Tombol dikirim lewat `NSWindow.sendEvent`, jadi antrean hardware/WindowServer dan komposit ke layar tidak ikut terukur.
- [x] M1-17 — Accessibility dasar: `NSAccessibility` role text area, value, selected range, line number.
  - Status: `EditorView` mengekspos role text area, nilai teks (dibatasi 1 juta UTF-16 unit untuk file besar), selected range/text, jumlah karakter, dan nomor baris caret. Tercakup test otomatis. Uji manual VoiceOver tetap dilacak oleh X-03.

### Uji UI otomatis (di luar daftar task)
- Target `RustCodeUITests` (XCUITest) dengan 12 skenario end-to-end: ketik, panah, undo, seleksi, copy/cut per baris, ⌘D, buka file + ⌘S, buka folder + klik file, toggle sidebar, prompt simpan saat tutup, find (navigasi, Esc), Replace All + undo, regex salah.
- Jalankan: `scripts/ui-test.sh`, atau semua test sekaligus dengan `scripts/test-all.sh --ui`.
- Test memakai keyboard dan mouse sungguhan. Saat mesin diam, suite lulus penuh; kegagalan acak sebelumnya disebabkan input dari luar yang masuk ke app. `launch()` sekarang gagal dengan pesan jelas bila buffer baru sudah berisi teks.
- Belum dimasukkan ke CI karena butuh sesi GUI dan izin Accessibility.

## M2 — Workspace dan Navigasi (3 minggu)

- [x] M2-01 — `Workspace` di Rust: walk dengan crate `ignore`, path interning di arena.
  - Index 100k file < 1 s, RAM index < 20 MB.
  - Status: `build_file_index` memindai file (bukan direktori) dengan aturan ignore yang sama seperti explorer dan menyimpan path sebagai ID komponen terintern di `PathArena`; pembukaan workspace tetap lazy. `workspace-index` mengukur 100.001 file dalam 624,33 ms dengan estimasi 10,82 MB pada Release.
- [ ] M2-02 — File watcher `notify` (FSEvents) dengan debounce, update index dan buffer terbuka.
- [ ] M2-03 — Handling perubahan eksternal: reload otomatis bila buffer bersih, prompt konflik bila dirty.
- [ ] M2-04 — Explorer `NSOutlineView` lazy-load per direktori, ikon tipe file, status dirty.
  - Status: lazy-load, ikon, dan klik untuk membuka file sudah ada. Indikator dirty belum. Belum dicoba manual.
- [ ] M2-05 — Operasi file: new, rename, delete ke Trash, reveal in Finder, copy path.
- [ ] M2-06 — Tab bar: buka, tutup, reorder, preview tab (single click), indikator dirty, prompt simpan.
- [ ] M2-07 — Go to File `Cmd+P` dengan `nucleo`, ranking recency + kedekatan path, highlight karakter match.
  - < 30 ms per ketikan pada 100k file.
- [ ] M2-08 — Go to line `Ctrl+G`, riwayat navigasi back/forward.
- [ ] M2-09 — Command registry di Rust + command palette `Cmd+Shift+P`.
- [ ] M2-10 — Keymap default (mirip VSCode) dan override `keybindings.json`.
- [ ] M2-11 — Session restore: workspace terakhir, tab, posisi cursor, scroll.
- [ ] M2-12 — CLI `rcode <path>` (symlink ke binary helper yang membuka app via `NSWorkspace`).
- [ ] M2-13 — Status bar: line/col, encoding, line ending, bahasa, indent.

## M3 — Syntax Highlighting (3 minggu)

- [ ] M3-01 — `SyntaxService`: tree-sitter parser per dokumen di worker thread, input via rope chunk.
- [ ] M3-02 — Incremental parse dengan `InputEdit` dari setiap edit; versi dokumen untuk membuang hasil usang.
  - Re-parse setelah edit < 2 ms di file 10k baris.
- [ ] M3-03 — Bundel grammar: Rust, TS, TSX, JS, JSON, TOML, YAML, Markdown, Python, Go, HTML, CSS, Bash.
- [ ] M3-04 — Query highlight per bahasa, mapping capture ke scope tema.
- [ ] M3-05 — Hasilkan style run hanya untuk viewport + margin, sertakan dalam `ViewportSnapshot`.
- [ ] M3-06 — Deteksi bahasa dari ekstensi, nama file, dan shebang.
- [ ] M3-07 — Sistem tema JSON, tema bawaan terang + gelap, ikut appearance macOS.
- [ ] M3-08 — Bracket matching, indent guides, auto-close bracket/quote berbasis syntax.
- [ ] M3-09 — Comment toggle berbasis config bahasa.
- [ ] M3-10 — Outline symbol via tree-sitter `tags.scm` untuk `Cmd+Shift+O`.
- [ ] M3-11 — Buang syntax tree untuk tab tidak aktif > 5 menit; ukur penghematan RAM.

## M4 — LSP (5 minggu)

- [ ] M4-01 — Transport JSON-RPC over stdio (`tokio::process`), framing `Content-Length`, log trafik opsional.
- [ ] M4-02 — `LspManager`: lifecycle `initialize`/`initialized`/`shutdown`/`exit`, satu server per (bahasa, root).
- [ ] M4-03 — Konfigurasi server: deteksi di `PATH`, override di settings, `initializationOptions`.
- [ ] M4-04 — Document sync: `didOpen`, `didChange` incremental dengan debounce, `didSave`, `didClose`.
  - Konversi posisi UTF-16 benar untuk teks emoji/CJK (test).
- [ ] M4-05 — Request tracking dengan ID, timeout, dan `$/cancelRequest` untuk request usang.
- [ ] M4-06 — Diagnostics: underline di editor, marker di gutter dan scrollbar, panel Problems.
- [ ] M4-07 — Hover popover (Markdown sederhana, code block di-highlight).
- [ ] M4-08 — Completion: popup, filter lokal fuzzy, `completionItem/resolve`, snippet dasar (`$1`, `$0`).
- [ ] M4-09 — Signature help.
- [ ] M4-10 — Go to definition / type definition / implementation (`F12`, `Cmd+Click`).
- [ ] M4-11 — Find references dengan panel hasil.
- [ ] M4-12 — Rename symbol: input inline, terapkan `WorkspaceEdit` ke file terbuka dan tertutup, satu langkah undo per file.
- [ ] M4-13 — Code actions / quick fix (`Cmd+.`).
- [ ] M4-14 — Formatting dokumen dan format on save.
- [ ] M4-15 — `$/progress` dan status server di status bar, termasuk RAM proses server.
- [ ] M4-16 — Auto-restart server crash (maks 3x / 5 menit) dan command Restart/Stop server.
- [ ] M4-17 — Uji end-to-end dengan `rust-analyzer` pada repo `ripgrep`.
- [ ] M4-18 — Dogfooding: pakai RustCode untuk mengembangkan RustCode.

## M5 — Search, Multi-LSP, Settings (3 minggu)

- [ ] M5-01 — Workspace search `Cmd+Shift+F` dengan crate `grep-*` + `ignore`, paralel, streaming hasil.
  - Hasil pertama < 100 ms pada repo 1 GB; dapat dibatalkan.
- [ ] M5-02 — Opsi regex, case, whole word, include/exclude glob; batas 10k match.
- [ ] M5-03 — Panel hasil search dengan konteks baris dan navigasi keyboard.
- [ ] M5-04 — Integrasi dan uji `typescript-language-server`.
- [ ] M5-05 — Integrasi dan uji `pyright-langserver`.
- [ ] M5-06 — Integrasi dan uji `gopls`.
- [ ] M5-07 — `workspace/symbol` untuk `Cmd+T`.
- [ ] M5-08 — Settings global dan per workspace (`.rcode/settings.json`), reload saat file berubah.
- [ ] M5-09 — UI settings minimal: buka file JSON dengan validasi schema dan completion.

## M6 — Git, Terminal, Produktivitas (4 minggu)

- [ ] M6-01 — Git via `gix`: branch saat ini, status file, refresh via watcher.
- [ ] M6-02 — Dekorasi status Git di explorer dan tab.
- [ ] M6-03 — Gutter diff terhadap HEAD, dihitung incremental di background.
- [ ] M6-04 — Terminal panel: `portable-pty` + `alacritty_terminal`, render Core Text grid.
- [ ] M6-05 — Multiple terminal tab, resize, copy/paste, scrollback 10k baris.
- [ ] M6-06 — Link `path:line:col` di terminal dan diagnostics dapat diklik.
- [ ] M6-07 — Split editor horizontal/vertikal.
- [ ] M6-08 — Breadcrumb symbol.
- [ ] M6-09 — Sticky scroll berbasis tree-sitter scope.
- [ ] M6-10 — Inlay hints dan semantic tokens (LSP).
- [ ] M6-11 — Auto-save opsional (on focus change / delay).

## M7 — Hardening dan Rilis Beta (3 minggu)

- [ ] M7-01 — Audit semua anggaran performa PRD bagian 6; perbaiki yang meleset.
- [ ] M7-02 — Profiling RAM: Instruments Allocations/Leaks, `dhat`; evaluasi `mimalloc`.
- [ ] M7-03 — Respons memory pressure: kosongkan cache non-esensial.
- [ ] M7-04 — Soak test 8 jam (script edit + navigasi otomatis); RAM stabil ±10%.
- [ ] M7-05 — Fuzz test edit ops dan parser JSON-RPC (`cargo-fuzz`).
- [ ] M7-06 — Crash reporting lokal (panic hook Rust + crash log macOS), tanpa upload otomatis.
- [ ] M7-07 — Code signing, notarization, DMG, auto-update (Sparkle).
- [ ] M7-08 — Dokumentasi pengguna: shortcut, settings, konfigurasi LSP.
- [ ] M7-09 — Rilis beta tertutup dan kumpulkan feedback.

## Task lintas milestone

- [ ] X-01 — Setiap PR yang menyentuh hot path wajib menyertakan hasil benchmark.
- [ ] X-02 — Dashboard tren RAM dan startup per commit di CI.
- [ ] X-03 — Uji IME dan accessibility (VoiceOver) di setiap milestone.
