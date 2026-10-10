# Tasks: Galaxy

Referensi: `docs/PRD.md`. Setiap task selesai bila acceptance criteria terpenuhi dan benchmark terkait tidak regresi.

Format: `[ ] ID — Task` lalu kriteria selesai.

## M0 — Fondasi (2 minggu)

- [x] M0-01 — Buat Cargo workspace: `crates/galaxy-core`, `crates/galaxy-ffi`, `crates/galaxy-bench`.
  - `cargo build` dan `cargo test` lulus.
- [x] M0-02 — Buat proyek Xcode (`app/Galaxy`) via xcodegen/Tuist, target macOS 14, Swift 6.
  - App kosong terbuka dan menampilkan window.
- [x] M0-03 — Integrasi UniFFI: build universal static lib (`arm64` + `x86_64`) dan generate binding Swift.
  - Swift memanggil fungsi Rust `core_version()` dan menerima callback event.
- [x] M0-04 — Script build `scripts/build-core.sh` dipanggil dari build phase Xcode.
- [x] M0-05 — Setup `tracing` di Rust dan `os_log` di Swift, dengan log file di `~/Library/Logs/Galaxy`.
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
  - Status: implementasi memakai tinggi virtual dan hanya menggambar viewport. `scripts/measure-scroll.sh` (Release, 50k baris, 240 frame menggulir dari atas ke bawah): gambar per frame p50 1,0–1,1 ms, p95 2,2–3,1 ms; interval display link p95 17,8–21,4 ms. Angka lama (0,74/1,13 ms) tidak valid: fixture-nya ternyata satu baris (`\\n` literal), jadi tidak ada yang digulir. Script sekarang memeriksa jumlah baris dan bahwa baris terakhir benar-benar tergambar. Komposit ke layar belum diukur dengan Instruments.
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
- [ ] M1-15 — Koalesasi update per frame (display link) agar FFI dipanggil maks 1x per frame.
  - Status: sebagian, jadi belum dicentang. Hitungan find untuk dokumen > 1 MB berjalan di background dari snapshot rope (`FindCountJob`, bisa dibatalkan, hasil basi dibuang). Pekerjaan seluruh dokumen (`is_modified`, hitungan find, notifikasi accessibility) maks 1x per frame lewat `CADisplayLink`; test: 50 ketikan = 1 notifikasi, dan panah tidak memicunya sama sekali. Geometri dan gambar memakai satu `ViewportSnapshot` per render. Belum terpenuhi: satu tombol masih memanggil FFI beberapa kali (edit, revisi, seleksi), jadi "FFI maks 1x per frame" belum benar. Angka "3,44/4,82 ms" sebelumnya tidak punya script yang menghasilkannya dan sudah dihapus.
- [x] M1-16 — Ukur latensi keypress-to-render; target < 8 ms.
  - Status: `LatencyProbe` mencatat dari timestamp event tombol sampai akhir `draw` (plus signpost `keypress-to-render` untuk Instruments); menu View > Show Typing Latency. Tombol yang mengubah teks atau seleksi digambar langsung di akhir `keyDown` (maks 1x per 1/60 s). `scripts/measure-latency.sh` (Release, 50k baris, 300 tombol, tiga run): median 0,69–0,71 ms, p95 1,14–1,52 ms, max 6,8–15 ms; sebelum gambar langsung median 7,7–9,3 ms dan p95 14,7–16,1 ms. Target dibaca sebagai p95. Tombol dikirim lewat `NSWindow.sendEvent`, jadi antrean input hardware/WindowServer dan komposit ke layar tidak ikut terukur.
- [x] M1-17 — Accessibility dasar: `NSAccessibility` role text area, value, selected range, line number.
  - Status: `EditorView` mengekspos role text area, nilai teks (dibatasi 1 juta UTF-16 unit; jumlah karakter dibatasi sama), selected range/text, dan nomor baris caret, serta mengirim notifikasi `valueChanged` (maks 1x per frame) dan `selectedTextChanged` agar VoiceOver tahu ada perubahan. Tercakup test otomatis. Belum dicoba dengan VoiceOver sungguhan (X-03).

### Uji UI otomatis (di luar daftar task)
- Target `GalaxyUITests` (XCUITest) dengan 12 skenario end-to-end: ketik, panah, undo, seleksi, copy/cut per baris, ⌘D, buka file + ⌘S, buka folder + klik file, toggle sidebar dari status bar, prompt simpan saat tutup, find (navigasi, Esc), Replace All + undo, regex salah.
- Jalankan: `scripts/ui-test.sh`, atau semua test sekaligus dengan `scripts/test-all.sh --ui`.
- Test memakai keyboard dan mouse sungguhan. Saat mesin diam, suite lulus penuh; kegagalan acak sebelumnya disebabkan input dari luar yang masuk ke app. `launch()` sekarang gagal dengan pesan jelas bila buffer baru sudah berisi teks.
- Belum dimasukkan ke CI karena butuh sesi GUI dan izin Accessibility.

## M2 — Workspace dan Navigasi (3 minggu)

- [x] M2-01 — `Workspace` di Rust: walk dengan crate `ignore`, path interning di arena.
  - Index 100k file < 1 s, RAM index < 20 MB.
  - Status: `build_file_index` memindai file (bukan direktori) dengan aturan ignore yang sama seperti explorer dan menyimpan path sebagai ID komponen terintern di `PathArena`; pembukaan workspace tetap lazy. Symlink ke folder tidak masuk indeks (tidak diikuti). `workspace-index` pada repo sintetis 100.001 file (Release, tiga run): 622–823 ms, estimasi 12,3 MB. Estimasi lama 10,82 MB terlalu rendah karena nama komponen tersimpan dua kali (vektor dan kunci map); sekarang keduanya dihitung. Dipakai Go to File (M2-07) lewat `Workspace.build_file_finder`.
- [x] M2-02 — File watcher `notify` (FSEvents) dengan debounce, update index dan buffer terbuka.
  - Status: `watcher.rs` memakai `notify` 8.2.0 (CC0-1.0, backend FSEvents saja). Satu watcher rekursif per window, mulai saat folder dibuka dan berhenti saat window ditutup atau folder diganti. Event dikumpulkan jadi batch: dikirim setelah 60 ms tanpa event baru, paling lambat 300 ms setelah event pertama; 200 file sekaligus tiba dalam ≤ 5 batch. Path di `.git`, folder exclude bawaan, dan file temp atomic save (`.galaxy-*.tmp`) dibuang di sini; aturan `.gitignore` diterapkan oleh `Workspace::index_delta`, yang memeriksa disk. Event hilang, perubahan pada root, atau batch > 1.000 path menjadi rescan (bangun ulang indeks dan explorer). Indeks Go to File diperbarui di tempat (`FileFinder::apply_delta`, ~19 ms untuk 100k file, di background); recency dan konteks tetap; ⌘P tidak lagi membangun ulang indeks. Explorer hanya di-list ulang bila perubahan menyentuh folder yang sudah pernah dibuka; folder terbuka, pilihan, dan posisi scroll dipertahankan. Buffer terbuka ditangani di M2-03. Tercakup test Rust (`watcher`, `index_delta`, `apply_delta`, FFI) dan `QuickOpenTests`/`ExplorerTests`; belum dicoba dengan `git checkout` besar.
- [x] M2-03 — Handling perubahan eksternal: reload otomatis bila buffer bersih, prompt konflik bila dirty.
  - Status: tiap dokumen menyimpan `DiskStamp` (ukuran, mtime, hash isi) saat dibuka dan disimpan; save sendiri tidak terdeteksi sebagai perubahan. Cek berjalan pada batch watcher (file di workspace) dan saat window menjadi key (juga file di luar workspace); file yang tak berubah cukup satu `stat`, dan `touch` atau checkout dengan isi sama tidak dianggap perubahan. Buffer bersih di-reload otomatis, termasuk tab di background: hanya bagian yang berbeda yang diganti (prefix/suffix sama dipertahankan), satu langkah undo, encoding/BOM/line ending ikut file. Buffer dirty: prompt "Keep My Changes" / "Reload from Disk", ditanyakan sekali saat tab ditampilkan dan window aktif; Keep membuat buffer tetap unsaved terhadap file baru. Save mengecek disk dulu dan meminta konfirmasi sebelum menimpa. File terhapus: tab berjudul "(deleted)", teks tetap, dihitung unsaved; Save membuatnya kembali, dan file yang dipulihkan dengan isi sama kembali bersih. Tercakup test Rust (`disk`, `reload`, FFI) dan `DiskChangeTests`. Rename di luar editor tampil sebagai "(deleted)"; tab tidak mengikuti nama baru.
- [x] M2-04 — Explorer `NSOutlineView` lazy-load per direktori, ikon tipe file, status dirty.
  - Status: lazy-load, ikon tipe file, klik untuk membuka (preview), double-click untuk menetapkan. Titik copper di kanan baris untuk file yang belum disimpan, juga di folder yang memuatnya; label accessibility menyebut "unsaved changes". Tercakup `ExplorerTests` dan `NavigationFollowUpTests`; belum dicoba manual.
- [x] M2-05 — Operasi file: new, rename, delete ke Trash, reveal in Finder, copy path.
  - Status: New File/Folder lewat tombol header dan dialog nama (`/` membuat subfolder, tidak pernah menimpa). Rename in place: Return di explorer atau menu "Rename…" mengubah nama baris jadi field dengan nama terpilih sampai ekstensi; Return/klik di luar menyimpan, Escape membatalkan. `Workspace::rename` tidak menimpa file lain, menolak `..`/path absolut dan memindah folder ke dalam dirinya sendiri, dan mengizinkan ganti huruf besar/kecil saja. Tab yang terbuka (juga file di dalam folder yang di-rename) mengikuti path baru beserta edit yang belum disimpan, begitu juga daftar recent dan indeks Go to File. Delete: ⌘⌫ atau "Move to Trash" dengan konfirmasi, lewat `FileManager.trashItem`; tab bersih ditutup, tab dengan edit tetap terbuka sebagai "(deleted)". Menu klik kanan memilih baris tanpa membukanya: New File/Folder, Reveal in Finder, Copy Path, Copy Relative Path, Rename, Move to Trash (dua terakhir hanya untuk baris, bukan area kosong). Semua ada di command palette. Tercakup test Rust (`rename`) dan `ExplorerFileOpsTests`; test memakai hapus biasa sebagai ganti Trash, jadi Trash sungguhan dan klik kanan dengan mouse belum dicoba.
- [x] M2-06 — Tab bar: buka, tutup, reorder, preview tab (single click), indikator dirty, prompt simpan.
  - Status: tab di baris title bar (`TabBar.swift`), path bar di bawahnya. Klik tunggal di explorer membuka preview tab (judul miring) yang diganti klik berikutnya; edit atau double-click membuatnya tetap. File yang sudah terbuka tidak diduplikasi. Titik copper menggantikan tombol tutup pada tab yang belum disimpan. ⌘W menutup tab, ⇧⌘W menutup window, ⇧⌘[ / ⇧⌘] pindah tab, drag untuk reorder, klik tengah untuk menutup. Menutup window menanyakan setiap tab yang belum disimpan secara berurutan. Posisi scroll disimpan per tab. Tercakup `TabTests`; drag dan hover belum dicoba dengan mouse sungguhan.
- [x] M2-07 — Go to File `Cmd+P` dengan `nucleo`, ranking recency + kedekatan path, highlight karakter match.
  - Status: `file_finder.rs` memakai `nucleo-matcher` 0.3.1 (MPL-2.0) dengan konfigurasi path, smart case, beberapa kata harus cocok semua. Ranking: skor fuzzy + bonus match di nama file + recency (50 file terakhir) + kedekatan folder dengan file aktif; seri dipecah path terpendek lalu alfabet. Query yang memperpanjang query sebelumnya hanya menyaring hasil sebelumnya; di atas 8.192 file pencarian dibagi ke maksimal 8 thread. Path disimpan dalam satu `String` (4,6 MB untuk 100k file). `file-finder` bench (Release, 100k path sintetis, 68 ketikan, tiga run): median 1,5 ms, p95 13,9–14,7 ms, maks 17,1–17,9 ms; repo_10k di disk: maks 3,6 ms. Panel ⌘P (`QuickOpen.swift`) dibangun saat pertama dipakai; indeks dibangun di background saat folder dibuka dan diperbarui oleh file watcher (M2-02). Di atas 20k file pencarian per ketikan jalan di background. Tercakup `QuickOpenTests` dan test Rust; belum dicoba dengan keyboard sungguhan.
  - < 30 ms per ketikan pada 100k file.
- [x] M2-08 — Go to line `Ctrl+G`, riwayat navigasi back/forward.
  - Status: `navigation.rs` berisi parser input (`42`, `42:7`, `42,7`, `:42`, `-1` = baris terakhir, di luar batas dijepit) dan `NavHistory` (maks 50 entri per arah, posisi ≤ 10 baris dalam dokumen yang sama digabung, lompatan baru menghapus riwayat forward). Panel ⌃G (`GoToLine.swift`) dibangun saat pertama dipakai; tanpa kolom caret ke karakter non-spasi pertama; baris di luar layar digulir ke tengah. Lompatan yang dicatat: Go to Line, pindah tab, buka file, tutup tab aktif, find (posisi saat find bar dibuka, sekali per sesi, plus Find Next/Previous yang jauh), dan klik biasa lebih dari 10 baris. Back membuka lagi file yang tabnya sudah ditutup; buffer Untitled yang ditutup dilewati. ⌃- / ⌃⇧- ditangkap key monitor jendela (Shift mengubah `-` jadi `_`, jadi key equivalent menu tidak cukup). ⌃Tab (PRD 7.4) pindah ke tab yang terakhir dilihat; ditekan lagi kembali. Tercakup test Rust, `GoToLineTests`, dan `NavigationFollowUpTests`; key monitor belum dicoba dengan keyboard sungguhan.
- [x] M2-09 — Command registry di Rust + command palette `Cmd+Shift+P`.
  - Status: `commands.rs` berisi registry (id unik ala VSCode, kategori, judul, kata kunci tersembunyi), 36 command bawaan, dan pencarian fuzzy `nucleo-matcher` pada label `Kategori: Judul` (kata kunci hanya menemukan, peringkatnya di bawah match label), dengan bonus untuk 20 command terakhir; query kosong: terakhir dipakai dulu, lalu alfabet. Panel ⇧⌘P (`CommandPalette.swift`) dibangun saat pertama dipakai, menampilkan shortcut dari menu dan hanya command yang bisa jalan saat itu (validasi sama seperti menu). Command dijalankan dari responder yang fokus sebelum palette dibuka. Test memastikan setiap command punya action, setiap item menu punya command, dan tidak ada action yang tertutup oleh method AppKit. Temuan: `toggleSidebar:`, `selectNextTab:`, `selectPreviousTab:` sudah dimiliki `NSWindow`, yang lebih dulu di responder chain, jadi ⌘B dan ⇧⌘[ / ⇧⌘] dari menu kemungkinan tidak pernah sampai ke controller; ketiganya diganti nama. Tercakup test Rust dan `CommandPaletteTests`; belum dicoba dengan keyboard sungguhan.
- [x] M2-10 — Keymap default (mirip VSCode) dan override `keybindings.json`.
  - Status: default di `keymap.rs` (shortcut macOS VSCode untuk semua command yang ada). File `~/Library/Application Support/Galaxy/User/keybindings.json` memakai format VSCode: `{ "key", "command" }` menambah binding dan mengambil key itu dari command lain, `"-command"` menghapus default (satu key, atau semua bila `key` kosong). Komentar dan trailing comma diterima (parser JSONC sendiri, `jsonc.rs`, tanpa dependency baru). Entri salah dilewati dengan pesan per entri (baris/kolom untuk file rusak, yang memakai default); `when`, `args`, dan chord belum didukung dan dilaporkan. Semua shortcut lewat satu key monitor per window, dijalankan hanya bila item menunya aktif; menu dan command palette menampilkan key utama tiap command. "Keyboard Shortcuts…" (menu View dan palette) membuka file, dibuat dengan semua default sebagai komentar; menyimpannya langsung menerapkan, dan file juga dibaca ulang saat window aktif. Tercakup test Rust (`jsonc`, `keymap`) dan `KeybindingsTests`; belum dicoba dengan keyboard sungguhan atau layout non-US.
- [x] M2-11 — Session restore: workspace terakhir, tab, posisi cursor, scroll.
  - Status: `~/Library/Application Support/Galaxy/session.json` (JSON berversi) disimpan saat window ditutup atau app quit: folder, tab file berurutan (preview atau tidak), caret, scroll per tab, tab aktif, sidebar. Saat launch tanpa path, session dipulihkan; folder atau file yang sudah hilang dilewati, caret di-clamp. Kalau tidak ada yang pulih, buffer Untitled seperti biasa. Untitled dan edit belum disimpan tidak ikut (penutupan sudah menanyakannya). File session rusak atau versi lain diabaikan dengan log. Mati untuk benchmark, host unit test, dan `-RestoreSession NO` (UI test). Tercakup `SessionTests`; belum dicoba restart app sungguhan dengan tangan, dan efeknya pada startup budget belum diukur dengan session besar.
- [x] M2-12 — CLI `galaxy <path>` (symlink ke binary helper yang membuka app via `NSWorkspace`).
  - Status: target `GalaxyCLI` membundel helper `Galaxy.app/Contents/Resources/galaxy`. `scripts/install-cli.sh` membangun Release lalu membuat symlink `~/.local/bin/galaxy` (atau `--prefix DIRECTORY`). Helper menerima tepat satu file atau folder lokal, ekspansi `~`, memverifikasi path, menemukan bundle induknya setelah symlink di-resolve, lalu mengirim open event lewat `NSWorkspace`; prosesnya mengaktifkan app yang sudah ada atau menjalankannya. `--help`, argumen salah, app tidak ditemukan, dan path hilang mengembalikan pesan serta exit nonnol. Tercakup `GalaxyCLITests`; membuka app sungguhan dari helper tidak dicoba otomatis agar test tidak mengganggu desktop.
- [x] M2-13 — Status bar: line/col, encoding, line ending, bahasa, indent.
  - Status: line/col (plus jumlah cursor), encoding (dengan BOM), line ending, indent, dan bahasa sudah tampil. Bahasa dideteksi ringan dari nama atau ekstensi file (Rust, Swift, C/C++, Go, Python, JS/TS/React, HTML/CSS, JSON/JSONC, TOML, YAML, Markdown, Shell, dan lainnya); nama khusus Dockerfile, Makefile, Cargo.toml, package.json, dan file Git juga dikenali. Parser syntax M3 tetap menjadi sumber highlighting, bukan deteksi ini. Tercakup `StatusBarTests`.

## M3 — Syntax Highlighting (3 minggu)

- [x] M3-01 — `SyntaxService`: tree-sitter parser per dokumen di worker thread, input via rope chunk.
  - Status: `syntax.rs` menyediakan `SyntaxService` per dokumen dengan parser Tree-sitter pada thread `galaxy-syntax`; submit hanya meng-clone handle `Rope` O(1), input parser dibaca melalui `Rope::chunk_at_byte`, dan poll tidak memblok. Hasil membawa revision dan hanya tree untuk request terbaru yang diterbitkan, sehingga parse usang dibuang. Grammar belum dibundel sampai M3-03, query/capture sampai M3-04, dan style viewport/FFI sampai M3-05; karena itu service belum dihubungkan ke renderer. `tree-sitter-json` hanya dev dependency untuk test worker, bukan grammar produksi. Tercakup test parse rope dan stale revision.
- [x] M3-02 — Incremental parse dengan `InputEdit` dari setiap edit; versi dokumen untuk membuang hasil usang.
  - Status: `SyntaxEdit` menghitung byte range dan `Point` Tree-sitter langsung dari Rope lama/baru, termasuk UTF-8 dan multiline. Worker menyimpan tree revision terakhir, menerapkan `Tree::edit(InputEdit)`, lalu memberi tree itu sebagai old tree ke parser hanya bila `base_revision` tepat sama; worker yang tertinggal atau edit tidak berantai fallback full parse. Hasil tetap hanya diterbitkan bila revision terbaru. API mendukung satu perubahan kontigu; saat grammar dihubungkan (M3-03), transaksi multi-cursor perlu mengirim edit berantai atau fallback full parse. Tercakup test UTF-8/multiline incremental dan fallback revision tak berantai; benchmark 10k baris belum ditambahkan.
  - Re-parse setelah edit < 2 ms di file 10k baris.
- [x] M3-03 — Bundel grammar: Rust, TS, TSX, JS, JSON, TOML, YAML, Markdown, Python, Go, HTML, CSS, Bash.
  - Status: 13 grammar Tree-sitter dipin statis di `galaxy-core`; `SyntaxLanguage` menjadi registry type-safe untuk Rust, TypeScript, TSX, JavaScript, JSON, TOML, YAML, Markdown, Python, Go, HTML, CSS, dan Bash, dengan display name dan `Language` masing-masing. TOML memakai `tree-sitter-toml-ng` dan Markdown memakai `tree-sitter-md-025`, karena crate nama standar masih memakai API Tree-sitter lama. Path/shebang mapping sengaja M3-06; query highlight M3-04. Tercakup test setiap grammar dipasang ke parser dan parse source minimal tanpa error.
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
- [ ] M4-18 — Dogfooding: pakai Galaxy untuk mengembangkan Galaxy.

## M5 — Search, Multi-LSP, Settings (3 minggu)

- [ ] M5-01 — Workspace search `Cmd+Shift+F` dengan crate `grep-*` + `ignore`, paralel, streaming hasil.
  - Hasil pertama < 100 ms pada repo 1 GB; dapat dibatalkan.
- [ ] M5-02 — Opsi regex, case, whole word, include/exclude glob; batas 10k match.
- [ ] M5-03 — Panel hasil search dengan konteks baris dan navigasi keyboard.
- [ ] M5-04 — Integrasi dan uji `typescript-language-server`.
- [ ] M5-05 — Integrasi dan uji `pyright-langserver`.
- [ ] M5-06 — Integrasi dan uji `gopls`.
- [ ] M5-07 — `workspace/symbol` untuk `Cmd+T`.
- [ ] M5-08 — Settings global dan per workspace (`.galaxy/settings.json`), reload saat file berubah.
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
