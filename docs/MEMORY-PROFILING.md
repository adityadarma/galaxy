# Memory Profiling

## Rust Heap

Run the bounded workload with:

```sh
scripts/profile-memory.sh 10000
```

Use `100000` for the full workspace-index workload. The binary enables
`dhat::Alloc` only for this profiling binary and writes `dhat-heap.json` in the
repository root. Production crates and the macOS application keep the system
allocator.

The workload covers a large `Document`, a 100-line snapshot, a temporary
workspace index, and the `FileFinder` path arena. Compare retained bytes and
allocation count between commits; do not compare peak bytes alone because the
temporary workspace is intentionally deleted at process exit.

## Instruments

1. Build the app in Release from `app/Galaxy.xcodeproj`.
2. Open Instruments and record `Allocations` plus `Leaks` for `Galaxy.app`.
3. Measure idle after 2 seconds with no workspace, then open a 10k-file
   workspace and five tabs and measure after indexing settles.
4. Record physical footprint with `scripts/measure-app.sh`; Instruments is used
   to attribute allocations, not to replace the footprint budget.
5. Save traces outside the repository and record the host, build configuration,
   workload, and commit in the audit report.

## Allocator Evaluation

No allocator replacement is enabled. `mimalloc` was not added because the
current release footprint is already 28 MB idle and there is no representative
workspace allocation trace proving a benefit. Re-evaluate it only after an
Instruments trace identifies allocator contention or fragmentation, and compare
startup, footprint, and editor latency with the same workload.
