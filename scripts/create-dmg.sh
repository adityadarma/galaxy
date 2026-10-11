#!/usr/bin/env bash
# Create a distributable DMG from a signed Galaxy.app.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${GALAXY_VERSION:-0.1.0}"
APP="${1:-$ROOT/app/build/Build/Products/Release/Galaxy.app}"
OUT="$ROOT/dist/Galaxy-$VERSION.dmg"

[[ -d "$APP" ]] || { printf 'app not found: %s\n' "$APP" >&2; exit 2; }
mkdir -p "$ROOT/dist"
rm -f "$OUT"
STAGING="$(mktemp -d)"
trap 'rm -rf "$STAGING"' EXIT
cp -R "$APP" "$STAGING/Galaxy.app"
ln -s /Applications "$STAGING/Applications"
hdiutil create -volname "Galaxy $VERSION" -srcfolder "$STAGING" \
  -format UDZO -imagekey zlib-level=9 "$OUT" >/dev/null
shasum -a 256 "$OUT" > "$OUT.sha256"
printf 'DMG: %s\n' "$OUT"
