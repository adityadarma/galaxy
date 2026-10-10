#!/usr/bin/env bash
# Runs the XCUITest suite against the real app.
#
#   scripts/ui-test.sh                 run all UI tests
#   scripts/ui-test.sh -only-testing:GalaxyUITests/EditorUITests/testFoo
#
# Needs a logged-in GUI session, and the first run needs Accessibility
# permission for the test runner (System Settings > Privacy & Security >
# Accessibility). Screenshots are kept in app/build/ui-results.xcresult.
#
# The tests drive the real keyboard and mouse. Typing or clicking while they
# run sends your input to the app (seen as stray text such as "k di" in a
# fresh buffer) and makes tests fail. Leave the machine idle until it ends.
set -euo pipefail
cd "$(dirname "$0")/.."

(cd app && xcodegen generate --quiet)
rm -rf app/build/ui-results.xcresult
xcodebuild -project app/Galaxy.xcodeproj -scheme GalaxyUI \
  -configuration Debug -derivedDataPath app/build \
  -resultBundlePath app/build/ui-results.xcresult \
  test "$@" 2>&1 | grep -vE "linkd.autoShortcut" | grep -E "error:|Test Case|Executed [0-9]+ tests|\*\* TEST|Testing failed|not authorized|Accessibility" || true
echo "results: app/build/ui-results.xcresult"
