#!/usr/bin/env bash
# Installs `galaxy <file-or-folder>` as a symlink to Galaxy.app's helper.
#
# Usage: scripts/install-cli.sh [--prefix DIRECTORY]
# Default: ~/.local/bin (add it to PATH if it is not already there).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="$HOME/.local/bin"
if [[ "${1:-}" == "--prefix" && -n "${2:-}" && $# == 2 ]]; then
  PREFIX="$2"
elif [[ $# != 0 ]]; then
  echo "usage: scripts/install-cli.sh [--prefix DIRECTORY]" >&2
  exit 2
fi

xcodebuild -project "$ROOT/app/Galaxy.xcodeproj" -scheme Galaxy \
  -configuration Release -derivedDataPath "$ROOT/app/build" build >/dev/null

HELPER="$ROOT/app/build/Build/Products/Release/Galaxy.app/Contents/Resources/galaxy"
if [[ ! -x "$HELPER" ]]; then
  echo "Galaxy CLI helper was not built: $HELPER" >&2
  exit 1
fi
mkdir -p "$PREFIX"
ln -sfn "$HELPER" "$PREFIX/galaxy"
echo "Installed $PREFIX/galaxy"
