#!/usr/bin/env bash
# Reproducible Rust heap profile for M7-02.
# Usage: scripts/profile-memory.sh [line-and-file-count]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COUNT="${1:-10000}"
cd "$ROOT"
cargo run -p galaxy-bench --release --bin memory-profile -- "$COUNT"
printf 'dhat output: %s/dhat-heap.json\n' "$ROOT"
