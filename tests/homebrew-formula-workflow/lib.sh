#!/usr/bin/env bash
# Shared helpers for the update_homebrew_formula.yml step tests. Sourced by
# run-workflow-steps.sh, never executed on its own.
#
# SC2034: everything defined here is consumed by the sibling case files, which
# are analysed separately.
# shellcheck disable=SC2034

HARNESS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "${HARNESS_DIR}/../.." && pwd)
# Overridable so the harness can be pointed at a deliberately broken copy, which
# is how you check that a test would actually fail if the workflow regressed.
WORKFLOW="${ALL_SMI_WORKFLOW:-${REPO_ROOT}/.github/workflows/update_homebrew_formula.yml}"
FIXTURES="${HARNESS_DIR}/fixtures"

PASS_COUNT=0
FAIL_COUNT=0
CURRENT_CASE="(none)"

# Background servers are recorded here rather than in a variable, because the
# sandbox builders that start them run inside `$(...)` and a variable set in a
# command substitution never reaches the caller. A file crosses that boundary.
SERVER_PIDFILE=$(mktemp "${TMPDIR:-/tmp}/all-smi-hbw-servers.XXXXXX")

# Synthetic values for the release being written. Distinct enough per artifact
# that a rewrite landing on the wrong stanza is visible in the output rather
# than merely wrong.
NEW_VERSION="0.26.0"
NEW_BASE="https://github.com/lablup/all-smi/releases/download/v${NEW_VERSION}"
SHA_MAC="aaaaaaaa11111111aaaaaaaa11111111aaaaaaaa11111111aaaaaaaa11111111"
SHA_MAC_X86="bbbbbbbb22222222bbbbbbbb22222222bbbbbbbb22222222bbbbbbbb22222222"
SHA_LINUX_ARM="cccccccc33333333cccccccc33333333cccccccc33333333cccccccc33333333"
SHA_LINUX_X86="dddddddd44444444dddddddd44444444dddddddd44444444dddddddd44444444"

begin_case() {
    CURRENT_CASE="$1"
    printf '\n--- %s\n' "$CURRENT_CASE"
}

ok() {
    PASS_COUNT=$((PASS_COUNT + 1))
    printf '  ok    %s\n' "$1"
}

bad() {
    FAIL_COUNT=$((FAIL_COUNT + 1))
    printf '  FAIL  %s\n' "$1"
    if [ -n "${2:-}" ]; then
        printf '        %s\n' "$2"
    fi
}

assert_eq() {  # label expected actual
    if [ "$2" = "$3" ]; then
        ok "$1"
    else
        bad "$1" "expected [$2], got [$3]"
    fi
}

assert_status() {  # label expected_status actual_status logfile
    if [ "$2" = "$3" ]; then
        ok "$1"
    else
        bad "$1" "expected exit $2, got $3; log: ${4:-none}"
    fi
}

assert_file_has() {  # label file pattern
    if grep -qF -- "$3" "$2"; then
        ok "$1"
    else
        bad "$1" "pattern not found in $2: $3"
    fi
}

assert_file_lacks() {  # label file pattern
    if grep -qF -- "$3" "$2"; then
        bad "$1" "pattern unexpectedly found in $2: $3"
    else
        ok "$1"
    fi
}

require_tools() {
    local missing=""
    local tool
    for tool in "$@"; do
        command -v "$tool" > /dev/null 2>&1 || missing="${missing} ${tool}"
    done
    if [ -n "$missing" ]; then
        printf 'missing required tools:%s\n' "$missing" >&2
        printf 'these tests run the workflow step bodies verbatim, so they need\n' >&2
        printf 'the same tools macos-latest provides.\n' >&2
        return 1
    fi
}

# Writes the named step's run: body to a file and echoes the path. The body is
# taken from the committed workflow, which is the entire point: the tests run
# what ships.
step_script() {  # step-name dest
    python3 "${HARNESS_DIR}/extract-step.py" "$WORKFLOW" "$1" > "$2"
}

# Mirrors what the Actions runner does between steps: everything a step appended
# to $GITHUB_ENV becomes an environment variable for the steps after it.
apply_github_env() {  # env-file
    local line key value
    while IFS= read -r line; do
        case "$line" in
            *=*) ;;
            *) continue ;;
        esac
        key="${line%%=*}"
        value="${line#*=}"
        export "$key=$value"
    done < "$1"
}

# A tap working tree holding FIXTURE, committed on `main`, so the update and
# commit steps see the same shape they see in CI.
make_tap() {  # dir fixture
    mkdir -p "$1/homebrew-tap/Formula"
    cp "$2" "$1/homebrew-tap/Formula/all-smi.rb"
    git -C "$1/homebrew-tap" init --quiet --initial-branch=main
    git -C "$1/homebrew-tap" config user.name "GitHub Action"
    git -C "$1/homebrew-tap" config user.email "actions@github.com"
    git -C "$1/homebrew-tap" add Formula/all-smi.rb
    git -C "$1/homebrew-tap" commit --quiet -m "fixture"
}

# The environment the download step would have left behind for the update and
# validate steps. mac_x86_present is the caller's choice, because it is exactly
# the axis the four asset/stanza states turn on.
export_artifact_env() {  # mac_x86_present
    export VERSION_NO_V="$NEW_VERSION"
    export mac_url="${NEW_BASE}/all-smi-macos-aarch64.zip"
    export mac_sha="$SHA_MAC"
    export linux_arm_url="${NEW_BASE}/all-smi-linux-aarch64.tar.gz"
    export linux_arm_sha="$SHA_LINUX_ARM"
    export linux_x86_url="${NEW_BASE}/all-smi-linux-x86_64.tar.gz"
    export linux_x86_sha="$SHA_LINUX_X86"
    export mac_x86_present="$1"
    if [ "$1" = "true" ]; then
        export mac_x86_url="${NEW_BASE}/all-smi-macos-x86_64.zip"
        export mac_x86_sha="$SHA_MAC_X86"
    else
        unset mac_x86_url mac_x86_sha
    fi
}
