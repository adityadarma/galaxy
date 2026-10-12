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
LSP ketika prefix yang diketik cocok.

Font editor dapat dipilih dari daftar font sistem melalui `Settings`. Untuk
menggunakan Fira Code, install font tersebut di Font Book, buka `Settings`,
lalu pilih `Fira Code` pada `Editor font`. Ukuran font diatur terpisah dan
berlaku untuk editor serta terminal.

Untuk Go to Definition dan fitur Laravel-aware lainnya, install Laravel LSP:

```sh
composer global require laravel/lsp
```

Pastikan executable `laravel-lsp` tersedia di `PATH` atau di salah satu lokasi
Composer global yang umum. Galaxy hanya memulai Laravel LSP untuk workspace
yang memiliki file `artisan`. Konfigurasi custom dapat ditempatkan di settings
workspace:

```json
{
  "lsp": {
    "servers": {
      "php": {
        "command": "laravel-lsp",
        "args": []
      }
    }
  }
}
```

Untuk project dengan frontend dan backend terpisah, buka folder parent yang
memuat keduanya sebagai workspace, misalnya:

```text
project/
  frontend/  # tsconfig.json atau package.json
  backend/   # Laravel/PHP
```

TypeScript LSP akan memakai workspace parent sebagai root dan menemukan
`tsconfig.json` atau `package.json` dari folder frontend. Laravel LSP tetap
harus diaktifkan secara manual melalui konfigurasi PHP agar tidak berjalan
otomatis pada setiap file PHP.

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

## Language Server

Language server dikonfigurasi melalui resolver built-in berdasarkan bahasa:

| Bahasa | Executable default |
|---|---|
| Rust | `rust-analyzer` |
| TypeScript/JavaScript | `typescript-language-server --stdio` |
| Python | `pyright-langserver --stdio` |
| Go | `gopls` |
| PHP/Blade (Laravel) | `laravel-lsp` |

Executable harus tersedia di `PATH` atau lokasi fallback platform. Galaxy
membagi satu server per kombinasi bahasa dan workspace bila lifecycle server
mendukungnya. Document `didOpen` dan request fitur baru dikirim setelah server
menyelesaikan handshake `initialize`/`initialized`.

Override server dapat ditaruh dalam settings JSON melalui `servers`. Nilai
`restartBackoffMs` berisi tepat tiga delay retry dalam milidetik; defaultnya
`[0, 1000, 2000]` dan maksimum tetap tiga restart per lima menit.

```json
{
  "servers": {
    "rust": {
      "command": "rust-analyzer",
      "restartBackoffMs": [0, 1000, 2000]
    }
  }
}
```

Feature LSP yang bergantung pada server nyata akan kosong atau dilewati jika
executable tidak terpasang atau handshake gagal; editor tetap dapat dipakai
sebagai editor biasa. Go to Definition membutuhkan server yang mendukung
`textDocument/definition` dan project root yang benar.

## Local Data

- Log aplikasi: `~/Library/Logs/Galaxy`.
- Crash report: `~/Library/Logs/Galaxy/crash-report.log`.
- Session restore: `~/Library/Application Support/Galaxy/session.json`.
- User settings: `~/Library/Application Support/Galaxy/settings.json`.

Galaxy tidak meng-upload log, crash report, settings, atau session secara
otomatis.
