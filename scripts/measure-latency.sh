#!/usr/bin/env bash
# Keypress-to-render latency (M1-16).
#
# Usage: scripts/measure-latency.sh [keys] [lines]
#
# Builds Release, opens a generated source file of <lines> lines (default
# 50000), and lets the app type <keys> keys (default 300) into the middle
# of it by itself (GALAXY_LATENCY_BENCH). Prints median, p95 and worst time
# from key event to the end of drawing. Budget (PRD 6): < 8 ms, max 16 ms.
#
# A Galaxy window appears for a few seconds. The keys are sent straight
# to that window, not through the keyboard, so you can keep working.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KEYS="${1:-300}"
LINES="${2:-50000}"
DERIVED="$ROOT/app/build"
APP_BIN="$DERIVED/Build/Products/Release/Galaxy.app/Contents/MacOS/Galaxy"

(cd "$ROOT/app" && xcodegen generate --quiet)
xcodebuild -project "$ROOT/app/Galaxy.xcodeproj" -scheme Galaxy \
  -configuration Release -derivedDataPath "$DERIVED" build -quiet

BASE="$(mktemp -t galaxy-latency)"
FIXTURE="$BASE.rs"
trap 'rm -f "$BASE" "$FIXTURE"' EXIT
python3 - "$FIXTURE" "$LINES" <<'PY'
import sys
path, n = sys.argv[1], int(sys.argv[2])
with open(path, "w") as f:
    for i in range(n):
        f.write(f"    let value_{i} = compute(input, {i}) + offset; // line {i}\n")
PY

OUT="$(GALAXY_LATENCY_BENCH="$KEYS" "$APP_BIN" -OpenPath "$FIXTURE" \
  -ApplePersistenceIgnoreState YES 2>/dev/null | grep '^latency_samples=' || true)"
if [[ -z "$OUT" ]]; then
  echo "no latency report (did the app start?)" >&2
  exit 1
fi
echo "$OUT"
if [[ "$OUT" == latency_samples=0* ]]; then
  echo "no samples recorded; see the diagnostics above" >&2
  exit 1
fi
P50="$(sed -E 's/.*latency_p50_ms=([0-9.]+).*/\1/' <<<"$OUT")"
P95="$(sed -E 's/.*latency_p95_ms=([0-9.]+).*/\1/' <<<"$OUT")"
DRAW="$(sed -E 's/.*draw_p50_ms=([0-9.]+).*/\1/' <<<"$OUT")"
python3 - "$P50" "$P95" "$DRAW" <<'PY'
import sys
p50, p95, draw = map(float, sys.argv[1:])
verdict = ("within the 8 ms target" if p95 < 8 else
           "over the 8 ms target, under the 16 ms limit" if p95 < 16 else
           "OVER the 16 ms limit")
print(f"median {p50:.2f} ms, p95 {p95:.2f} ms: {verdict}")
print(f"drawing itself: median {draw:.2f} ms")
print("not included: hardware/WindowServer input queue and compositing the frame to the screen")
PY
