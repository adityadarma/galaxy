# Performance Audit

Baseline collected on 2026-10-11 from the local Apple Silicon macOS host. App
measurements use Release builds. Physical footprint covers the Galaxy process,
including its built-in language engines, and excludes child processes such as
git, terminals, and debug adapters.

| Metric | Target | Observed | Result |
|---|---:|---:|---|
| Cold start to first frame | < 200 ms | warm median 204.8 ms; first cold run 939.6 ms | needs follow-up |
| Idle footprint, no workspace | < 40 MB | median 28 MB | pass |
| Workspace footprint, 10k files / 5 tabs | < 120 MB | median 55 MB app (3 runs, 2026-10-13, `scripts/measure-footprint.sh`); 11.6 MB core-only peak RSS; 97 MB core-only with an extra 1 MB + 1.5 MB tab (was 234 MB before incremental reparse) | pass |
| Open 1 MB file | < 30 ms | no dedicated app harness | gap |
| Open 100 MB large-file mode | < 500 ms | core load benchmark exists; app open timing missing | gap |
| Keypress to render | < 8 ms p95 | 1.14-1.52 ms p95 in existing Release runs | pass |
| Scroll | 120 fps target, 60 fps limit | draw p95 2.2-3.1 ms; display-link p95 17.8-21.4 ms | pass at 60 fps; 120 fps unverified |
| Go to File, 10k files | < 10 ms per keystroke | 1.5 ms median; 13.9-14.7 ms p95 | within limit; target p95 follow-up |
| Initial index, 100k files | < 1 s | 622-823 ms; estimated 12.3 MB | pass |
| Workspace search, 1 GB repo | first result < 100 ms | no repeatable 1 GB app harness | gap |
| Incremental syntax highlight | < 2 ms | core incremental parser coverage exists; end-to-end timing missing | gap |
| Completion pipeline | queue/engine/popup stages visible | structured local telemetry | instrumented |

## Actions

- Keep the startup budget open: warm median is 4.8 ms over target and the first
  cold launch is dominated by process/dyld/AppKit startup.
- Add app harnesses for workspace footprint, file open, 1 GB search, and
  end-to-end syntax highlighting before release sign-off.
- Keep `scripts/measure-app.sh`, `scripts/measure-latency.sh`, and
  `scripts/measure-scroll.sh` as the repeatable baseline commands.
- Rust heap profiling procedure is documented in `docs/MEMORY-PROFILING.md`;
  `scripts/profile-memory.sh` uses the isolated `memory-profile` binary.
- Completion logs report `completion_dispatch_ms`, `queue_ms`, `engine_ms`, and
  `completion_popup_ms`. Use them to isolate typing latency before changing an
  engine or UI ranking policy.
