#!/usr/bin/env bash
# Native-scroll benchmark (M1-08).
#
# Usage: scripts/measure-scroll.sh [frames] [lines]
#
# Builds Release, opens a generated source file and scrolls through it at
# roughly 60 Hz. The draw p95 must fit into the 16.67 ms frame budget; the
# display-link interval shows whether the main thread stayed responsive. This is a
# repeatable smoke test; inspect the same run in Core Animation Instruments
# before treating it as a display-compositor measurement.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FRAMES="${1:-240}"
LINES="${2:-50000}"
DERIVED="$ROOT/app/build"
APP_BIN="$DERIVED/Build/Products/Release/RustCode.app/Contents/MacOS/RustCode"

(cd "$ROOT/app" && xcodegen generate --quiet)
xcodebuild -project "$ROOT/app/RustCode.xcodeproj" -scheme RustCode \
  -configuration Release -derivedDataPath "$DERIVED" build -quiet

BASE="$(mktemp -t rcode-scroll)"
FIXTURE="$BASE.rs"
trap 'rm -f "$BASE" "$FIXTURE"' EXIT
python3 - "$FIXTURE" "$LINES" <<'PY'
import sys
path, n = sys.argv[1], int(sys.argv[2])
with open(path, "w") as f:
    for i in range(n):
        f.write(f"    let value_{i} = compute(input, {i}) + offset; // line {i}\n")
PY
# Guard against the fixture silently collapsing to one line again.
GOT="$(wc -l <"$FIXTURE" | tr -d ' ')"
if [[ "$GOT" != "$LINES" ]]; then
  echo "fixture has $GOT lines, expected $LINES" >&2
  exit 1
fi

OUT="$(RCODE_SCROLL_BENCH="$FRAMES" "$APP_BIN" -OpenPath "$FIXTURE" \
  -ApplePersistenceIgnoreState YES 2>/dev/null | grep '^scroll_frames=' || true)"
if [[ -z "$OUT" ]]; then
  echo "no scroll benchmark report (did the app start?)" >&2
  exit 1
fi
echo "$OUT"
LAST="$(sed -E 's/.*scroll_last_line=([0-9]+).*/\1/' <<<"$OUT")"
TOTAL="$(sed -E 's/.*scroll_line_count=([0-9]+).*/\1/' <<<"$OUT")"
if (( TOTAL < LINES || LAST < TOTAL - 200 )); then
  echo "did not scroll through the document: last drawn line $LAST of $TOTAL" >&2
  exit 1
fi
DRAW_P95="$(sed -E 's/.*scroll_draw_p95_ms=([0-9.]+).*/\1/' <<<"$OUT")"
INTERVAL_P95="$(sed -E 's/.*scroll_interval_p95_ms=([0-9.]+).*/\1/' <<<"$OUT")"
python3 - "$DRAW_P95" "$INTERVAL_P95" <<'PY'
import sys
draw, interval = map(float, sys.argv[1:])
budget = 1000 / 60
verdict = "within" if draw < budget else "OVER"
print(f"draw p95 {draw:.2f} ms: {verdict} the 60 fps frame budget ({budget:.2f} ms)")
print(f"display-link interval p95 {interval:.2f} ms (lower is steadier)")
if draw >= budget:
    raise SystemExit(1)
PY
