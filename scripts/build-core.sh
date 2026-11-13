#!/usr/bin/env bash
# Builds the Rust core as a static library and generates Swift bindings.
#
# Usage: scripts/build-core.sh [debug|release]
#
# Outputs:
#   app/Galaxy/FFI/libgalaxy_ffi.a          static lib linked by Xcode
#   app/Galaxy/FFI/include/                C header + module.modulemap
#   app/Galaxy/Generated/galaxy_ffi.swift   Swift bindings
#
# Called from the Xcode "Build Rust Core" phase. When run from Xcode the
# configuration is derived from $CONFIGURATION.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Xcode runs scripts with a minimal PATH.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

# Reuse Rust compilation results across Xcode builds when sccache is installed.
# Keep it optional so clean machines and CI can still build the project.
if command -v sccache >/dev/null 2>&1; then
  export RUSTC_WRAPPER="$(command -v sccache)"
fi

PROFILE="debug"
if [[ "${CONFIGURATION:-}" == "Release" ]]; then PROFILE="release"; fi
for arg in "$@"; do
  case "$arg" in
    debug|release) PROFILE="$arg" ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

CARGO_FLAGS=(-p galaxy-ffi --locked)
[[ "$PROFILE" == "release" ]] && CARGO_FLAGS+=(--release)

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

FFI_DIR="$ROOT/app/Galaxy/FFI"
GEN_DIR="$ROOT/app/Galaxy/Generated"
mkdir -p "$FFI_DIR/include" "$GEN_DIR"

cargo build "${CARGO_FLAGS[@]}"
STATIC_TMP="$(mktemp "$FFI_DIR/libgalaxy_ffi.a.XXXXXX")"
trap 'rm -f "$STATIC_TMP"' EXIT
cp "$ROOT/target/$PROFILE/libgalaxy_ffi.a" "$STATIC_TMP"
DYLIB="$ROOT/target/$PROFILE/libgalaxy_ffi.dylib"
mv "$STATIC_TMP" "$FFI_DIR/libgalaxy_ffi.a"
trap - EXIT

# Generate bindings into a temp dir. The static library and generated Swift
# wrappers are one ABI unit, so refresh the wrapper timestamps with each Rust
# build. Otherwise Xcode can retain an object built against an older ABI.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cargo run -q -p uniffi-bindgen --locked -- generate \
  --library "$DYLIB" --language swift --out-dir "$TMP" --no-format

mv "$TMP/galaxy_ffiFFI.modulemap" "$TMP/module.modulemap"

sync_file() {
  cp "$1" "$2"
}
sync_file "$TMP/galaxy_ffi.swift" "$GEN_DIR/galaxy_ffi.swift"
sync_file "$TMP/galaxy_ffiFFI.h" "$FFI_DIR/include/galaxy_ffiFFI.h"
sync_file "$TMP/module.modulemap" "$FFI_DIR/include/module.modulemap"

echo "galaxy core built ($PROFILE, $(uname -m))"
