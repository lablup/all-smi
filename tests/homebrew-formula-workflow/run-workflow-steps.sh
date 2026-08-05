#!/usr/bin/env bash
#
# Executes the step bodies of .github/workflows/update_homebrew_formula.yml
# against fixtures, offline.
#
# The workflow pushes to lablup/homebrew-tap, a production Homebrew tap, so it
# cannot be exercised by running it. What can be exercised is every step body it
# contains, and that is what this does: the bodies are read out of the committed
# YAML by name, so the thing under test is the file that ships rather than a
# copy that drifts away from it.
#
# What is covered:
#   - the four release-asset / formula-stanza states, plus a malformed tap
#     (cases-formula.sh)
#   - the url/sha256 pairing guard, including a control showing that checking
#     them independently accepts a formula with the pairs exchanged
#     (cases-formula.sh)
#   - artifacts landing outside the tap clone, and the release asset list being
#     read from the paginated assets collection (cases-download.sh)
#   - the push credential reaching git without touching disk, argv or the trace
#     (cases-token.sh)
#
# What is not covered, and cannot be from here: a real push to the tap, and
# `brew install` from the result.
#
# Usage:  tests/homebrew-formula-workflow/run-workflow-steps.sh [case-group ...]
#         case groups: formula download token   (default: all three)
#
# Runs on macOS, matching the workflow's `runs-on: macos-latest`, because the
# step bodies call gsed, brew style and BSD awk.

set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# shellcheck source=lib.sh
. "${HERE}/lib.sh"
# shellcheck source=cases-formula.sh
. "${HERE}/cases-formula.sh"
# shellcheck source=cases-download.sh
. "${HERE}/cases-download.sh"
# shellcheck source=cases-token.sh
. "${HERE}/cases-token.sh"

require_tools python3 git awk gsed ruby brew shasum unzip zip tar base64 || exit 1

if ! python3 -c 'import yaml' > /dev/null 2>&1; then
    printf 'python3 needs PyYAML to read the workflow file (pip install pyyaml)\n' >&2
    exit 1
fi

if [ ! -f "$WORKFLOW" ]; then
    printf 'workflow not found: %s\n' "$WORKFLOW" >&2
    exit 1
fi

groups=("$@")
if [ "${#groups[@]}" -eq 0 ]; then
    groups=(formula download token)
fi

printf 'workflow: %s\n' "$WORKFLOW"

for group in "${groups[@]}"; do
    case "$group" in
        formula) run_formula_cases ;;
        download) run_download_cases ;;
        token) run_token_cases ;;
        *)
            printf 'unknown case group: %s\n' "$group" >&2
            exit 2
            ;;
    esac
done

printf '\n%d passed, %d failed\n' "$PASS_COUNT" "$FAIL_COUNT"
[ "$FAIL_COUNT" -eq 0 ]
