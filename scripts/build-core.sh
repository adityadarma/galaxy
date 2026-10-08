#!/usr/bin/env bash
# Builds the Rust core as a static library and generates Swift bindings.
#
# Usage: scripts/build-core.sh [debug|release] [--universal]
#
# Outputs:
#   app/RustCode/FFI/librcode_ffi.a          static lib linked by Xcode
#   app/RustCode/FFI/include/                C header + module.modulemap
#   app/RustCode/Generated/rcode_ffi.swift   Swift bindings
#
# Called from the Xcode "Build Rust Core" phase. When run from Xcode the
# configuration is derived from $CONFIGURATION.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Xcode runs scripts with a minimal PATH.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

PROFILE="debug"
UNIVERSAL=0
if [[ "${CONFIGURATION:-}" == "Release" ]]; then PROFILE="release"; fi
for arg in "$@"; do
  case "$arg" in
    debug|release) PROFILE="$arg" ;;
    --universal) UNIVERSAL=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

# Under Xcode, follow $ARCHS (Release builds arm64 + x86_64).
if [[ -n "${ARCHS:-}" && "${ARCHS}" == *x86_64* ]]; then UNIVERSAL=1; fi

CARGO_FLAGS=(-p rcode-ffi --locked)
[[ "$PROFILE" == "release" ]] && CARGO_FLAGS+=(--release)

export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

FFI_DIR="$ROOT/app/RustCode/FFI"
GEN_DIR="$ROOT/app/RustCode/Generated"
mkdir -p "$FFI_DIR/include" "$GEN_DIR"

if [[ "$UNIVERSAL" == 1 ]]; then
  TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
  LIBS=()
  for t in "${TARGETS[@]}"; do
    if ! rustup target list --installed | grep -qx "$t"; then
      rustup target add "$t" >/dev/null
    fi
    cargo build "${CARGO_FLAGS[@]}" --target "$t"
    LIBS+=("$ROOT/target/$t/$PROFILE/librcode_ffi.a")
  done
  lipo -create "${LIBS[@]}" -output "$FFI_DIR/librcode_ffi.a.tmp"
  DYLIB="$ROOT/target/aarch64-apple-darwin/$PROFILE/librcode_ffi.dylib"
else
  cargo build "${CARGO_FLAGS[@]}"
  cp "$ROOT/target/$PROFILE/librcode_ffi.a" "$FFI_DIR/librcode_ffi.a.tmp"
  DYLIB="$ROOT/target/$PROFILE/librcode_ffi.dylib"
fi
mv "$FFI_DIR/librcode_ffi.a.tmp" "$FFI_DIR/librcode_ffi.a"

# Generate bindings into a temp dir, then copy only on change so Xcode
# does not recompile Swift every build.
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cargo run -q -p uniffi-bindgen --locked -- generate \
  --library "$DYLIB" --language swift --out-dir "$TMP" --no-format

mv "$TMP/rcode_ffiFFI.modulemap" "$TMP/module.modulemap"

sync_file() {
  if ! cmp -s "$1" "$2"; then cp "$1" "$2"; fi
}
sync_file "$TMP/rcode_ffi.swift" "$GEN_DIR/rcode_ffi.swift"
sync_file "$TMP/rcode_ffiFFI.h" "$FFI_DIR/include/rcode_ffiFFI.h"
sync_file "$TMP/module.modulemap" "$FFI_DIR/include/module.modulemap"

echo "rcode core built ($PROFILE$([[ $UNIVERSAL == 1 ]] && echo ', universal'))"
