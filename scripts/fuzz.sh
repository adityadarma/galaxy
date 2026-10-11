#!/usr/bin/env bash
# Run one M7-05 fuzz target. Requires cargo-fuzz.
# Usage: scripts/fuzz.sh [edit_operations|json_rpc] [seconds]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${1:-edit_operations}"
SECONDS_TO_RUN="${2:-60}"
cd "$ROOT"
cargo fuzz run "$TARGET" -- -max_total_time="$SECONDS_TO_RUN"
