# Configuration Reference

Galaxy membaca JSON settings global lalu menggabungkannya dengan workspace
settings. File workspace memiliki precedence lebih tinggi.

## Editor

```json
{
  "editor": {
    "insertSpaces": true,
    "tabSize": 4
  }
}
```

- `editor.insertSpaces`: `true` menggunakan spaces; `false` menggunakan tabs.
- `editor.tabSize`: ukuran tab positif. Default `4`.

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

Executable harus tersedia di `PATH`. Galaxy membagi satu server per kombinasi
bahasa dan workspace bila lifecycle server mendukungnya.

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
executable tidak terpasang; editor tetap dapat dipakai sebagai editor biasa.

## Local Data

- Log aplikasi: `~/Library/Logs/Galaxy`.
- Crash report: `~/Library/Logs/Galaxy/crash-report.log`.
- Session restore: `~/Library/Application Support/Galaxy/session.json`.
- User settings: `~/Library/Application Support/Galaxy/settings.json`.

Galaxy tidak meng-upload log, crash report, settings, atau session secara
otomatis.
