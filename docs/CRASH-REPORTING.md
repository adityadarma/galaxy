# Local Crash Reporting

Galaxy writes crash reports locally to:

```text
~/Library/Logs/Galaxy/crash-report.log
```

The Rust panic hook records panic location, message, and Rust backtrace. The
macOS uncaught-exception handler records exception name, reason, and symbolized
stack frames when AppKit exposes them. Reports are append-only and are never
uploaded automatically.

Reports intentionally exclude document contents, file text, clipboard data,
workspace paths where possible, and environment variables. Signal crashes are
not intercepted because filesystem and Foundation operations are unsafe from a
signal handler; macOS crash reports remain the source for those failures.
