#!/usr/bin/env bash
# Exercises the Release app with repeated edits and cursor navigation (M7-04).
#
# Usage:
#   scripts/soak-test.sh [--hours N] [--sample-seconds N] [--app PATH] [--no-build]
#
# Requires a logged-in GUI session and Accessibility permission for osascript.
# Do not use the keyboard or mouse while the test runs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOURS=8
SAMPLE_SECONDS=60
APP_BIN=""
BUILD=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --hours) HOURS="$2"; shift 2 ;;
    --sample-seconds) SAMPLE_SECONDS="$2"; shift 2 ;;
    --app) APP_BIN="$2"; shift 2 ;;
    --no-build) BUILD=0; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

[[ "$HOURS" =~ ^[0-9]+([.][0-9]+)?$ ]] && awk "BEGIN { exit !($HOURS > 0) }" || {
  echo "--hours must be positive" >&2
  exit 2
}
[[ "$SAMPLE_SECONDS" =~ ^[1-9][0-9]*$ ]] || { echo "--sample-seconds must be a positive integer" >&2; exit 2; }

DERIVED="$ROOT/app/build"
if [[ -z "$APP_BIN" ]]; then
  APP_BIN="$DERIVED/Build/Products/Release/Galaxy.app/Contents/MacOS/Galaxy"
fi
if [[ "$BUILD" == 1 ]]; then
  (cd "$ROOT/app" && xcodegen generate --quiet)
  xcodebuild -project "$ROOT/app/Galaxy.xcodeproj" -scheme Galaxy \
    -configuration Release -derivedDataPath "$DERIVED" build -quiet
fi
[[ -x "$APP_BIN" ]] || { echo "app binary not executable: $APP_BIN" >&2; exit 1; }

WORKSPACE="$(mktemp -d)"
LOG="$WORKSPACE/soak.csv"
SOURCE="$WORKSPACE/soak.rs"
cleanup() {
  [[ -n "${PID:-}" ]] && kill "$PID" 2>/dev/null || true
  [[ -n "${PID:-}" ]] && wait "$PID" 2>/dev/null || true
  rm -rf "$WORKSPACE"
}
trap cleanup EXIT INT TERM

for line in $(seq 1 2000); do
  printf 'fn item_%s() { let value = %s; drop(value); }\n' "$line" "$line" >>"$SOURCE"
done

"$APP_BIN" -OpenPath "$SOURCE" >/dev/null 2>&1 &
PID=$!
sleep 3
kill -0 "$PID" 2>/dev/null || { echo "Galaxy exited during launch" >&2; exit 1; }

printf 'elapsed_seconds,rss_mb\n' >"$LOG"
START="$(date +%s)"
DURATION_SECONDS="$(awk "BEGIN { print int($HOURS * 3600) }")"
NEXT_SAMPLE=0

while true; do
  NOW="$(date +%s)"
  ELAPSED=$((NOW - START))
  (( ELAPSED >= DURATION_SECONDS )) && break

  osascript <<'APPLESCRIPT'
tell application "System Events"
  tell process "Galaxy"
    keystroke "x"
    key code 123
    key code 124
    key code 125
    key code 126
    keystroke "z" using command down
    keystroke "z" using {command down, shift down}
  end tell
end tell
APPLESCRIPT

  if (( ELAPSED >= NEXT_SAMPLE )); then
    RSS_KIB="$(ps -o rss= -p "$PID" | tr -d '[:space:]')"
    [[ "$RSS_KIB" =~ ^[0-9]+$ ]] || { echo "could not sample Galaxy RSS" >&2; exit 1; }
    RSS_MB="$(awk "BEGIN { printf \"%.2f\", $RSS_KIB / 1024 }")"
    printf '%s,%s\n' "$ELAPSED" "$RSS_MB" >>"$LOG"
    NEXT_SAMPLE=$((ELAPSED + SAMPLE_SECONDS))
  fi
  sleep 1
done

MIN="$(awk -F, 'NR == 2 || (NR > 2 && $2 < min) { min = $2 } END { print min }' "$LOG")"
MAX="$(awk -F, 'NR == 2 || (NR > 2 && $2 > max) { max = $2 } END { print max }' "$LOG")"
SPREAD="$(awk "BEGIN { printf \"%.2f\", ($MAX - $MIN) / $MIN * 100 }")"
cp "$LOG" "$ROOT/app/build/soak-results.csv"
printf 'soak complete: min %s MB, max %s MB, spread %s%%\n' "$MIN" "$MAX" "$SPREAD"
printf 'samples: %s\n' "$ROOT/app/build/soak-results.csv"
awk "BEGIN { exit !($SPREAD <= 10) }" || {
  echo "RSS spread exceeded the 10% soak budget" >&2
  exit 1
}
