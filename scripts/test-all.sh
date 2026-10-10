#!/usr/bin/env bash
# Runs every automated check in the order CI does.
#
#   scripts/test-all.sh          Rust (fmt, clippy, tests) + Swift unit tests
#   scripts/test-all.sh --ui     also the XCUITest suite (scripts/ui-test.sh)
#
# The UI suite drives the real keyboard and mouse. Do not type or click
# while it runs, or your input ends up in the app and the tests fail.
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n==> %s\n' "$1"; }

step "cargo fmt"
cargo fmt --all -- --check
step "cargo clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings
step "cargo test"
cargo test --workspace --locked

step "Swift unit tests"
(cd app && xcodegen generate --quiet)
xcodebuild -project app/Galaxy.xcodeproj -scheme Galaxy -configuration Debug \
  -derivedDataPath app/build test 2>&1 \
  | grep -vE "linkd.autoShortcut" \
  | grep -E "error:|Test Case .*failed|Executed [0-9]+ tests.*\) seconds$|\*\* TEST" \
  | sort -u

if [[ "${1:-}" == "--ui" ]]; then
  step "UI tests (hands off the keyboard and mouse)"
  scripts/ui-test.sh
fi
