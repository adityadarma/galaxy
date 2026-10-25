#!/usr/bin/env bash
# Build, sign, notarize, and package Galaxy.
# Required for distribution: DEVELOPER_ID_APPLICATION.
# Optional notarization: APPLE_ID, APPLE_TEAM_ID, and APPLE_APP_PASSWORD.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${GALAXY_VERSION:-0.1.0}"
OUT="$ROOT/dist/Galaxy-$VERSION"
APP="$ROOT/app/build/Build/Products/Release/Galaxy.app"
IDENTITY="${DEVELOPER_ID_APPLICATION:-}"

if [[ -z "$IDENTITY" ]]; then
  printf '%s\n' 'DEVELOPER_ID_APPLICATION is required for a distribution package.' >&2
  exit 2
fi

rm -rf "$OUT"
mkdir -p "$OUT"
(cd "$ROOT/app" && xcodegen generate --quiet)
xcodebuild -project "$ROOT/app/Galaxy.xcodeproj" -scheme Galaxy \
  -configuration Release -destination "platform=macOS,arch=$(uname -m)" \
  -derivedDataPath "$ROOT/app/build" \
  CODE_SIGN_IDENTITY="$IDENTITY" CODE_SIGN_STYLE=Manual build -quiet

codesign --deep --force --options runtime --timestamp --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"
ditto -c -k --keepParent "$APP" "$OUT/Galaxy-$VERSION.zip"

if [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" ]]; then
  xcrun notarytool submit "$OUT/Galaxy-$VERSION.zip" \
    --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
    --password "$APPLE_APP_PASSWORD" --wait
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
fi

shasum -a 256 "$OUT/Galaxy-$VERSION.zip" > "$OUT/SHA256SUMS"
printf 'release package: %s\n' "$OUT/Galaxy-$VERSION.zip"
