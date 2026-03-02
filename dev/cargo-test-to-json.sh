#!/usr/bin/env bash
# Converts cargo test output to TDD guard test.json format.
# Usage: cargo test --lib MODULE 2>&1 | bash dev/cargo-test-to-json.sh
#
# Writes to .claude/tdd-guard-superpowers/data/test.json

set -euo pipefail

OUTPUT_FILE=".claude/tdd-guard-superpowers/data/test.json"
MODULE_ID=""
TESTS="[]"
REASON="passed"

while IFS= read -r line; do
    echo "$line"

    # Detect module from test names like "test providers::gemini_sanitize::tests::foo ... ok"
    if [[ "$line" =~ ^test[[:space:]]+(.*)::(tests|test)::([^[:space:]]+)[[:space:]]+\.\.\.[[:space:]]+(ok|FAILED) ]]; then
        full="${BASH_REMATCH[1]}::${BASH_REMATCH[2]}::${BASH_REMATCH[3]}"
        module="${BASH_REMATCH[1]}"
        name="${BASH_REMATCH[3]}"
        state="passed"
        if [[ "${BASH_REMATCH[4]}" == "FAILED" ]]; then
            state="failed"
            REASON="failed"
        fi
        MODULE_ID="$module"
        TESTS=$(echo "$TESTS" | python3 -c "
import sys, json
tests = json.load(sys.stdin)
tests.append({'name': '$name', 'fullName': '$full', 'state': '$state'})
json.dump(tests, sys.stdout)
")
    fi

    if [[ "$line" =~ "test result:" ]]; then
        if [[ "$line" =~ "FAILED" ]]; then
            REASON="failed"
        fi
    fi
done

if [[ -n "$MODULE_ID" ]]; then
    python3 -c "
import json
tests = json.loads('$TESTS')
result = {'testModules': [{'moduleId': '$MODULE_ID', 'tests': tests}], 'reason': '$REASON'}
with open('$OUTPUT_FILE', 'w') as f:
    json.dump(result, f, indent=2)
print('Wrote', len(tests), 'test results to $OUTPUT_FILE')
"
fi
