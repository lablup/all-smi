#!/usr/bin/env bash
# The `Update formula` and `Validate updated formula` steps, executed against
# fixture formulas. Covers a normal update, the refusal of a formula that still
# carries an Intel macOS stanza, and the url/sha256 pairing guard added for
# issue #316.
#
# Sourced by run-workflow-steps.sh.

# SC2154: mac_url and friends are exported by export_artifact_env in lib.sh,
# which shellcheck does not follow across a source in a sibling file.
# SC2030/SC2031: step bodies run in a subshell on purpose, so their environment
# cannot leak between cases.
# shellcheck disable=SC2154,SC2030,SC2031

# The sha256 that follows the url stanza naming ARTIFACT. Deliberately a second,
# independent implementation of the pairing the validate step performs, so the
# two have to agree about what the file says.
sha_after() {  # file artifact
    awk -v a="$2" '
      !seen && /^[[:space:]]*url / && index($0, a) { seen = 1; next }
      seen && /^[[:space:]]*sha256 "/ {
        s = $0
        sub(/^[[:space:]]*sha256 "/, "", s)
        sub(/".*$/, "", s)
        print s
        exit
      }
    ' "$1"
}

url_of() {  # file artifact
    awk -v a="$2" '
      /^[[:space:]]*url / && index($0, a) {
        u = $0
        sub(/^[[:space:]]*url "/, "", u)
        sub(/".*$/, "", u)
        print u
        exit
      }
    ' "$1"
}

count_sha_stanzas() {  # file
    grep -c '^[[:space:]]*sha256 ' "$1" || true
}

# One sandbox per case: a tap working tree, a GITHUB_ENV file, a RUNNER_TEMP.
new_sandbox() {  # fixture -> echoes dir
    local dir
    dir=$(mktemp -d "${TMPDIR:-/tmp}/all-smi-hbw.XXXXXX")
    make_tap "$dir" "$1"
    : > "$dir/github_env"
    mkdir -p "$dir/runner_temp"
    printf '%s' "$dir"
}

run_formula_step() {  # step-name sandbox logfile -> status
    local script="$2/step.sh"
    step_script "$1" "$script" || return 127
    (
        cd "$2" || exit 127
        export GITHUB_ENV="$2/github_env"
        export RUNNER_TEMP="$2/runner_temp"
        bash "$script"
    ) > "$3" 2>&1
}

case_update() {
    begin_case "update: the formula serves Apple Silicon, Linux arm64 and Linux x86_64"
    local box status formula
    box=$(new_sandbox "${FIXTURES}/one-stanza.rb")
    export_artifact_env

    run_formula_step 'Update formula' "$box" "$box/update.log"
    status=$?
    assert_status "update step succeeds" 0 "$status" "$box/update.log"

    apply_github_env "$box/github_env"
    run_formula_step 'Validate updated formula' "$box" "$box/validate.log"
    status=$?
    assert_status "validate step succeeds" 0 "$status" "$box/validate.log"

    formula="$box/homebrew-tap/Formula/all-smi.rb"
    # The fixture still carries the `version` stanza the tap used to have.
    # Homebrew reads 0.26.0 from the url, so the stanza is redundant, and
    # `brew audit` rejects it (lablup/homebrew-tap#5).
    assert_file_lacks "redundant version stanza removed" "$formula" '  version "'
    assert_file_has "Linux installs the AMD companion" "$formula" '(lib/"all-smi").install "liball_smi_amd.so" if OS.linux?'
    assert_eq "three sha256 stanzas" 3 "$(count_sha_stanzas "$formula")"
    assert_eq "aarch64 macOS url rewritten" "$mac_url" "$(url_of "$formula" all-smi-macos-aarch64.zip)"
    assert_eq "aarch64 macOS checksum is its own" "$SHA_MAC" "$(sha_after "$formula" all-smi-macos-aarch64.zip)"
    assert_eq "linux aarch64 checksum is its own" "$SHA_LINUX_ARM" "$(sha_after "$formula" all-smi-linux-aarch64.tar.gz)"
    assert_eq "linux x86_64 checksum is its own" "$SHA_LINUX_X86" "$(sha_after "$formula" all-smi-linux-x86_64.tar.gz)"
    assert_file_lacks "no Intel macOS stanza appears" "$formula" "all-smi-macos-x86_64.zip"

    SANDBOX_UPDATED="$box"
}

case_leftover_intel_stanza() {
    begin_case "leftover Intel macOS stanza: refused before anything is pushed"
    local box status
    box=$(new_sandbox "${FIXTURES}/two-stanza.rb")
    export_artifact_env

    run_formula_step 'Update formula' "$box" "$box/update.log"
    status=$?
    assert_status "update step refuses" 1 "$status" "$box/update.log"
    assert_file_has "names the stanza to remove" "$box/update.log" \
        "still has a url stanza for all-smi-macos-x86_64.zip"

    if grep -q "mac_x86_state" "$WORKFLOW"; then
        bad "no step is gated on the old Intel macOS state" "mac_x86_state is still referenced"
    else
        ok "no step is gated on the old Intel macOS state"
    fi
}

# The regression this issue's pairing guard exists for. Every checksum in the
# file is one the run computed, every url names the right release, and the
# counts are right; only the pairing is wrong. Everything the validate step
# checked before #316 passes on it.
case_swapped_checksums() {
    begin_case "swapped checksums: url and sha256 correct apart, wrong together"
    local box status formula independent_checks_pass sha
    box="$SANDBOX_UPDATED"
    if [ -z "${box:-}" ]; then
        bad "update sandbox available" "the update case must run first"
        return
    fi
    formula="$box/homebrew-tap/Formula/all-smi.rb"
    export_artifact_env

    # Exchange the macOS aarch64 checksum with the Linux x86_64 one.
    gsed -i "s|sha256 \"${SHA_MAC}\"|sha256 \"__SWAP__\"|" "$formula"
    gsed -i "s|sha256 \"${SHA_LINUX_X86}\"|sha256 \"${SHA_MAC}\"|" "$formula"
    gsed -i "s|sha256 \"__SWAP__\"|sha256 \"${SHA_LINUX_X86}\"|" "$formula"

    assert_eq "the swap took" "$SHA_LINUX_X86" "$(sha_after "$formula" all-smi-macos-aarch64.zip)"

    # Control: the pre-#316 checks, reimplemented here, all accept this file.
    independent_checks_pass=yes
    [ "$(count_sha_stanzas "$formula")" = 3 ] || independent_checks_pass=no
    for sha in "$SHA_MAC" "$SHA_LINUX_ARM" "$SHA_LINUX_X86"; do
        grep -q "^[[:space:]]*sha256 \"${sha}\"$" "$formula" || independent_checks_pass=no
    done
    if [ "$(grep -c '^[[:space:]]*url ".*/releases/download/' "$formula")" \
         != "$(grep -c "^[[:space:]]*url \".*/releases/download/v0\\.26\\.0/" "$formula")" ]; then
        independent_checks_pass=no
    fi
    assert_eq "checking url and sha independently would accept it" yes "$independent_checks_pass"

    run_formula_step 'Validate updated formula' "$box" "$box/validate-swapped.log"
    status=$?
    assert_status "validate step rejects it" 1 "$status" "$box/validate-swapped.log"
    assert_file_has "names the pair that does not exist" "$box/validate-swapped.log" \
        "no url stanza for ${mac_url} is followed by sha256 ${SHA_MAC}"
}

# A stanza the update step never touched keeps its old url. PR #313 added this
# guard; it has to keep firing.
case_stale_url() {
    begin_case "stale url: one stanza left behind at the previous release"
    local box status formula
    box=$(new_sandbox "${FIXTURES}/one-stanza.rb")
    export_artifact_env
    run_formula_step 'Update formula' "$box" "$box/update.log" || {
        bad "update step succeeds before the stale url is planted" "$(cat "$box/update.log")"
        return
    }
    apply_github_env "$box/github_env"
    formula="$box/homebrew-tap/Formula/all-smi.rb"

    gsed -i "s|download/v${NEW_VERSION}/all-smi-linux-x86_64.tar.gz|download/v0.25.0/all-smi-linux-x86_64.tar.gz|" "$formula"

    run_formula_step 'Validate updated formula' "$box" "$box/validate-stale.log"
    status=$?
    assert_status "validate step rejects it" 1 "$status" "$box/validate-stale.log"
    assert_file_has "names the artifact left behind" "$box/validate-stale.log" \
        "all-smi-linux-x86_64.tar.gz"
}

# Homebrew reads a tag such as v0.26.0-rc.1 as 64, so for that release the
# formula has to pin its version, and validation has to accept the pin.
case_misread_tag_pins_version() {
    begin_case "misread tag: the formula pins the version Homebrew cannot scan"
    local box status formula saved_version="$NEW_VERSION" saved_base="$NEW_BASE"
    NEW_VERSION="0.26.0-rc.1"
    NEW_BASE="https://github.com/lablup/all-smi/releases/download/v${NEW_VERSION}"
    box=$(new_sandbox "${FIXTURES}/one-stanza.rb")
    export_artifact_env

    run_formula_step 'Update formula' "$box" "$box/update.log"
    status=$?
    assert_status "update step succeeds" 0 "$status" "$box/update.log"
    assert_file_has "explains the pin" "$box/update.log" "so the formula pins version ${NEW_VERSION}"

    apply_github_env "$box/github_env"
    run_formula_step 'Validate updated formula' "$box" "$box/validate.log"
    status=$?
    assert_status "validate step succeeds" 0 "$status" "$box/validate.log"

    formula="$box/homebrew-tap/Formula/all-smi.rb"
    assert_eq "exactly one version stanza" 1 "$(grep -c '^  version "' "$formula")"
    assert_file_has "pins the release version" "$formula" "  version \"${NEW_VERSION}\""

    NEW_VERSION="$saved_version"
    NEW_BASE="$saved_base"
}

run_formula_cases() {
    case_update
    case_leftover_intel_stanza
    case_swapped_checksums
    case_stale_url
    case_misread_tag_pins_version
}
