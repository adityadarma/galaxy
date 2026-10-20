# Configuration Reference

Galaxy membaca JSON settings global lalu menggabungkannya dengan workspace
settings. File workspace memiliki precedence lebih tinggi.

## Editor

```json
{
  "editor": {
    "insertSpaces": true,
    "tabSize": 4,
    "renderWhitespace": "all"
  }
}
```

- `editor.insertSpaces`: `true` menggunakan spaces; `false` menggunakan tabs.
- `editor.tabSize`: ukuran tab positif. Default `4`.
- `editor.renderWhitespace`: cara menampilkan whitespace. Nilai yang didukung
  `none`, `boundary`, dan `all`; default `all`.

Galaxy menyediakan snippet bawaan berdasarkan ekstensi file untuk Rust, Go,
Python, JavaScript/TypeScript, PHP/Laravel/Blade, Dart/Flutter, Java, Kotlin,
Swift, C/C++/C#, Ruby, HTML, CSS, Shell, dan SQL. Snippet muncul bersama hasil
engine bawaan ketika prefix yang diketik cocok.

Font editor dapat dipilih dari daftar font sistem melalui `Settings`. Untuk
menggunakan Fira Code, install font tersebut di Font Book, buka `Settings`,
lalu pilih `Fira Code` pada `Editor font`. Ukuran font diatur terpisah dan
berlaku untuk editor serta terminal.

PHP, Blade, dan Laravel ditangani engine bawaan Galaxy, tanpa language server
eksternal. Engine membaca source, `composer.json`, dan `vendor/` secara statis
(PHP tidak dijalankan). Tidak ada yang perlu di-install.

Syntax highlighting saat ini juga mencakup PHP (`.php`) dan file Blade
(`.blade.php`, memakai grammar PHP untuk bagian PHP). Snippet mendukung
placeholder `$1`, `${1:default}`, `$0`, choice seperti
`${1|one,two|}`, variable `TM_FILENAME`, `TM_FILENAME_BASE`,
`TM_SELECTED_TEXT`, `SELECTION`, dan `CURRENT_YEAR`. Tab berpindah antar
placeholder; Shift+Tab kembali; Escape membatalkan session.

## Auto-save

```json
{
  "files": {
    "autoSave": "afterDelay",
    "autoSaveDelay": 1000
  }
}
```

`files.autoSave` menerima:

- `off`: tidak menyimpan otomatis.
- `afterDelay`: menyimpan setelah tidak ada edit selama delay.
- `onFocusChange`: menyimpan saat window kehilangan focus atau tab berpindah.

`files.autoSaveDelay` memakai milliseconds dan dibatasi antara `100` dan
`60000`. Default delay adalah `1000` milliseconds.

Auto-save hanya menyimpan file yang sudah memiliki path. Buffer Untitled tidak
memicu dialog Save As otomatis.

## Intelijen Kode

Galaxy tidak memakai language server eksternal dan tidak punya sistem plugin.
Hover, completion, definition, references, rename, code actions, outline, dan
diagnostics datang dari engine bawaan di dalam proses Galaxy.

| Bahasa | Status |
|---|---|
| PHP/Blade/Laravel | engine bawaan |
| Rust, TypeScript/JavaScript, Python, Go, Swift, Dart, Vue, Svelte | syntax highlighting saja; engine bawaan belum ada |

Pengaturan `lsp.servers` dan `servers` tidak dibaca lagi. Format Document,
Go to Type Definition, Go to Implementation, Go to Workspace Symbol, semantic
tokens, dan inlay hints dihapus bersama language server dan belum ada
penggantinya.

## Local Data

- Log aplikasi: `~/Library/Logs/Galaxy`.
- Crash report: `~/Library/Logs/Galaxy/crash-report.log`.
- Session restore: `~/Library/Application Support/Galaxy/session.json`.
- User settings: `~/Library/Application Support/Galaxy/settings.json`.

Galaxy tidak meng-upload log, crash report, settings, atau session secara
otomatis.
