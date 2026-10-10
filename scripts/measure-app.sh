#!/usr/bin/env bash
# Measures app cold start and idle RAM (M0-07).
#
# Usage: scripts/measure-app.sh [runs] [--json out.json]
#
# Builds Release, launches the app N times with GALAXY_BENCH=1, and reports:
#   startup_ms      process start -> first window frame (median)
#   footprint_mb    physical footprint after 2 s idle (median)
#
# Budgets (docs/PRD.md section 6): startup < 200 ms, idle RAM < 40 MB.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNS=5
JSON_OUT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --json) JSON_OUT="$2"; shift 2 ;;
    *) RUNS="$1"; shift ;;
  esac
done

DERIVED="$ROOT/app/build"
APP_BIN="$DERIVED/Build/Products/Release/Galaxy.app/Contents/MacOS/Galaxy"

(cd "$ROOT/app" && xcodegen generate --quiet)
xcodebuild -project "$ROOT/app/Galaxy.xcodeproj" -scheme Galaxy \
  -configuration Release -derivedDataPath "$DERIVED" build -quiet

median() { sort -n | awk '{a[NR]=$1} END {print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}'; }

# Parses `footprint` output ("Footprint: 12 MB" or "... 12345 KB") into MB.
footprint_mb() {
  footprint "$1" 2>/dev/null | awk '
    /phys_footprint:|Footprint:/ {
      for (i = 1; i <= NF; i++) if ($i ~ /^[0-9.]+$/) { v = $i; u = $(i+1); break }
      if (u ~ /^KB/) v /= 1024; else if (u ~ /^GB/) v *= 1024
      printf "%.1f\n", v; exit
    }'
}

STARTUPS=()
FOOTPRINTS=()
for i in $(seq 1 "$RUNS"); do
  OUT="$(mktemp)"
  GALAXY_BENCH=1 "$APP_BIN" >"$OUT" 2>/dev/null &
  PID=$!
  for _ in $(seq 1 100); do
    grep -q startup_ms "$OUT" && break
    sleep 0.05
  done
  sleep 2
  S="$(grep -o 'startup_ms=[0-9.]*' "$OUT" | cut -d= -f2 || true)"
  F="$(footprint_mb "$PID" || true)"
  kill "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
  rm -f "$OUT"
  [[ -n "$S" ]] && STARTUPS+=("$S")
  [[ -n "$F" ]] && FOOTPRINTS+=("$F")
  printf "run %d: startup %s ms, footprint %s MB\n" "$i" "${S:-?}" "${F:-?}"
done

if [[ ${#STARTUPS[@]} -eq 0 ]]; then
  echo "no startup samples collected" >&2
  exit 1
fi

S_MED="$(printf '%s\n' "${STARTUPS[@]}" | median)"
F_MED="n/a"
[[ ${#FOOTPRINTS[@]} -gt 0 ]] && F_MED="$(printf '%s\n' "${FOOTPRINTS[@]}" | median)"
echo "median: startup ${S_MED} ms, footprint ${F_MED} MB"

if [[ -n "$JSON_OUT" ]]; then
  printf '{"startup_ms": %s, "footprint_mb": %s, "runs": %d}\n' \
    "$S_MED" "$([[ $F_MED == n/a ]] && echo null || echo "$F_MED")" "$RUNS" >"$JSON_OUT"
fi
