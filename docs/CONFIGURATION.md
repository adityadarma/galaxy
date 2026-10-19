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
Composer global yang umum. Server utama untuk PHP dan Blade adalah yang pertama
ditemukan dari `intelephense --stdio` lalu `phpactor language-server`.
Server ini menangani class, method, dan function.

Di workspace dengan file `artisan`, `laravel-lsp` berjalan bersama server
utama untuk navigasi string Laravel (`config()`, `route()`, `view()`). Hasil
Go to Definition dan diagnostics dari keduanya digabung. Jika hanya
`laravel-lsp` yang terpasang, server itu menjadi server utama. PHP dan Blade
berbagi proses per workspace. File `.blade.php` dikirim dengan `languageId` `blade`, file PHP
lain dengan `php`. Install fallback dengan `npm i -g intelephense` atau
`composer global require phpactor/phpactor`.

Konfigurasi custom dibaca dari settings global dan `.galaxy/settings.json`
workspace (workspace menang per bahasa). Entry `php` juga berlaku untuk Blade
kecuali ada entry `blade` tersendiri. Perubahan berlaku saat server start atau
restart:

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
`tsconfig.json` atau `package.json` dari folder frontend. Karena `artisan`
dicari di root workspace, `laravel-lsp` tidak otomatis dipakai pada layout ini;
Galaxy memakai `intelephense`/`phpactor` bila tersedia, atau server dari entry
`lsp.servers.php`.

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

| Bahasa | Plugin bawaan (Extensions, Install) |
|---|---|
| Rust | `rust-analyzer` (unduh binary rilis resmi) |
| TypeScript/JavaScript | `typescript-language-server --stdio` (npm) |
| Python | `pyright-langserver --stdio` (npm) |
| Go | `gopls` (`go install`) |
| PHP/Blade | `intelephense --stdio` (npm) atau `phpactor language-server` (composer), ditambah `laravel-lsp` di project Laravel |
| Vue | `vue-language-server --stdio` (npm), lengkap dengan TypeScript sendiri |

Semua server bawaan tidak terpasang secara default dan tidak memakai `PATH`.
Pasang dari panel Extensions (tombol Install); server masuk ke folder plugin
Galaxy. Untuk memakai executable sendiri, isi `lsp.servers` di settings.

Executable override di `lsp.servers` boleh berupa path absolut atau nama yang
ada di `PATH`. Galaxy membagi satu server per kombinasi bahasa dan workspace bila lifecycle server
mendukungnya. Document `didOpen` dan request fitur baru dikirim setelah server
menyelesaikan handshake `initialize`/`initialized`.

Override server dapat ditaruh dalam settings JSON melalui `lsp.servers`
(`servers` di top level masih diterima). Nilai
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
