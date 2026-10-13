#!/usr/bin/env bash
# Measures app physical footprint with a 10k-file workspace and 5 open tabs
# (docs/PERFORMANCE.md budget: < 120 MB). Language servers are separate
# processes and are not counted.
#
# Usage: scripts/measure-footprint.sh [runs] [--tabs N] [--repo DIR] [--json out.json]
#
# Also runs the core-only workload (`workspace-footprint`) so Rust-side
# memory can be compared with the whole app.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNS=3
TABS=5
REPO="$ROOT/fixtures/repo_10k"
JSON_OUT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --tabs) TABS="$2"; shift 2 ;;
    --repo) REPO="$2"; shift 2 ;;
    --json) JSON_OUT="$2"; shift 2 ;;
    *) RUNS="$1"; shift ;;
  esac
done

cd "$ROOT"
if [[ ! -d "$REPO" ]]; then
  cargo run -p galaxy-bench --release --bin gen-fixtures -- fixtures
fi

echo "== core workload =="
cargo run -q -p galaxy-bench --release --bin workspace-footprint -- "$REPO" "$TABS"

DERIVED="$ROOT/app/build"
APP_BIN="$DERIVED/Build/Products/Release/Galaxy.app/Contents/MacOS/Galaxy"
(cd "$ROOT/app" && xcodegen generate --quiet)
xcodebuild -project "$ROOT/app/Galaxy.xcodeproj" -scheme Galaxy \
  -configuration Release -derivedDataPath "$DERIVED" build -quiet

# Tabs: the first N source files with a grammar, sorted for repeatability.
FILES="$(find "$REPO/src" -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.py' -o -name '*.go' \) \
  | LC_ALL=C sort | awk -v n="$TABS" 'NR <= n' | paste -sd: -)"
# (awk reads all input; `head` would SIGPIPE `sort` under pipefail.)

median() { sort -n | awk '{a[NR]=$1} END {print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}'; }

footprint_mb() {
  footprint "$1" 2>/dev/null | awk '
    /phys_footprint:|Footprint:/ {
      for (i = 1; i <= NF; i++) if ($i ~ /^[0-9.]+$/) { v = $i; u = $(i+1); break }
      if (u ~ /^KB/) v /= 1024; else if (u ~ /^GB/) v *= 1024
      printf "%.1f\n", v; exit
    }'
}

echo "== app: $TABS tabs in $(basename "$REPO") =="
SAMPLES=()
for i in $(seq 1 "$RUNS"); do
  OUT="$(mktemp)"
  GALAXY_BENCH=1 GALAXY_BENCH_OPEN_FILES="$FILES" \
    "$APP_BIN" -OpenPath "$REPO" >"$OUT" 2>/dev/null &
  PID=$!
  for _ in $(seq 1 200); do
    grep -q startup_ms "$OUT" && break
    sleep 0.05
  done
  # Let indexing, the file finder build and background parses settle.
  sleep 5
  F="$(footprint_mb "$PID" || true)"
  kill "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
  rm -f "$OUT"
  [[ -n "$F" ]] && SAMPLES+=("$F")
  printf "run %d: footprint %s MB\n" "$i" "${F:-?}"
done

if [[ ${#SAMPLES[@]} -eq 0 ]]; then
  echo "no footprint samples collected" >&2
  exit 1
fi
MED="$(printf '%s\n' "${SAMPLES[@]}" | median)"
echo "median: footprint ${MED} MB (budget 120 MB)"
if [[ -n "$JSON_OUT" ]]; then
  printf '{"footprint_mb": %s, "tabs": %d, "runs": %d}\n' "$MED" "$TABS" "$RUNS" >"$JSON_OUT"
fi
