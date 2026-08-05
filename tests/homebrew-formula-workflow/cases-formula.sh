#!/usr/bin/env bash
# The `Update formula` and `Validate updated formula` steps, executed against
# fixture formulas. Covers the four asset/stanza states that PR #313 established
# and the url/sha256 pairing guard added for issue #316.
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

case_state1_asset_and_stanza() {
    begin_case "state 1: release publishes the Intel zip, formula has the stanza"
    local box status formula
    box=$(new_sandbox "${FIXTURES}/two-stanza.rb")
    export_artifact_env true

    run_formula_step 'Update formula' "$box" "$box/update.log"
    status=$?
    assert_status "update step succeeds" 0 "$status" "$box/update.log"
    assert_file_has "records mac_x86_state=updated" "$box/github_env" "mac_x86_state=updated"

    apply_github_env "$box/github_env"
    run_formula_step 'Validate updated formula' "$box" "$box/validate.log"
    status=$?
    assert_status "validate step succeeds" 0 "$status" "$box/validate.log"

    formula="$box/homebrew-tap/Formula/all-smi.rb"
    assert_file_has "version bumped" "$formula" "version \"${NEW_VERSION}\""
    assert_eq "four sha256 stanzas" 4 "$(count_sha_stanzas "$formula")"
    assert_eq "aarch64 macOS url rewritten" "$mac_url" "$(url_of "$formula" all-smi-macos-aarch64.zip)"
    assert_eq "aarch64 macOS checksum is its own" "$SHA_MAC" "$(sha_after "$formula" all-smi-macos-aarch64.zip)"
    assert_eq "x86_64 macOS url rewritten" "$mac_x86_url" "$(url_of "$formula" all-smi-macos-x86_64.zip)"
    assert_eq "x86_64 macOS checksum is its own" "$SHA_MAC_X86" "$(sha_after "$formula" all-smi-macos-x86_64.zip)"
    assert_eq "linux aarch64 checksum is its own" "$SHA_LINUX_ARM" "$(sha_after "$formula" all-smi-linux-aarch64.tar.gz)"
    assert_eq "linux x86_64 checksum is its own" "$SHA_LINUX_X86" "$(sha_after "$formula" all-smi-linux-x86_64.tar.gz)"

    SANDBOX_STATE1="$box"
}

case_state2_neither() {
    begin_case "state 2: no Intel zip in the release, no stanza in the formula"
    local box status formula before after
    box=$(new_sandbox "${FIXTURES}/one-stanza.rb")
    export_artifact_env false

    before=$(sed -n '/on_macos do/,/^  end$/p' "$box/homebrew-tap/Formula/all-smi.rb" | grep -c 'sha256' || true)

    run_formula_step 'Update formula' "$box" "$box/update.log"
    status=$?
    assert_status "update step succeeds" 0 "$status" "$box/update.log"
    assert_file_has "records mac_x86_state=skipped" "$box/github_env" "mac_x86_state=skipped"
    assert_file_has "emits a notice, not a warning" "$box/update.log" "::notice::"

    apply_github_env "$box/github_env"
    run_formula_step 'Validate updated formula' "$box" "$box/validate.log"
    status=$?
    assert_status "validate step succeeds" 0 "$status" "$box/validate.log"

    formula="$box/homebrew-tap/Formula/all-smi.rb"
    assert_eq "three sha256 stanzas" 3 "$(count_sha_stanzas "$formula")"
    after=$(sed -n '/on_macos do/,/^  end$/p' "$formula" | grep -c 'sha256' || true)
    assert_eq "macOS section gains no stanza" "$before" "$after"

    SANDBOX_STATE2="$box"
}

case_state3_asset_without_stanza() {
    begin_case "state 3: release publishes the Intel zip, formula has no stanza"
    local box status formula
    box=$(new_sandbox "${FIXTURES}/one-stanza.rb")
    export_artifact_env true

    run_formula_step 'Update formula' "$box" "$box/update.log"
    status=$?
    assert_status "update step succeeds so the other three still ship" 0 "$status" "$box/update.log"
    assert_file_has "records mac_x86_state=stanza-missing" "$box/github_env" "mac_x86_state=stanza-missing"
    assert_file_has "emits a warning" "$box/update.log" "::warning::"

    apply_github_env "$box/github_env"
    run_formula_step 'Validate updated formula' "$box" "$box/validate.log"
    status=$?
    assert_status "validate step succeeds at three artifacts" 0 "$status" "$box/validate.log"

    formula="$box/homebrew-tap/Formula/all-smi.rb"
    assert_eq "three sha256 stanzas" 3 "$(count_sha_stanzas "$formula")"

    # The push happens, and only then does the job go red, which is the whole
    # point of this state. That final gate lives in a step-level `if`, not in a
    # step body, so it is asserted against the YAML.
    if grep -q "if: env.mac_x86_state == 'stanza-missing'" "$WORKFLOW"; then
        ok "a later step is gated on stanza-missing so the job ends red"
    else
        bad "a later step is gated on stanza-missing so the job ends red"
    fi
}

case_state4_stanza_without_asset() {
    begin_case "state 4: no Intel zip in the release, but the formula has a stanza"
    local box status
    box=$(new_sandbox "${FIXTURES}/two-stanza.rb")
    export_artifact_env false

    run_formula_step 'Update formula' "$box" "$box/update.log"
    status=$?
    assert_status "update step refuses" 1 "$status" "$box/update.log"
    assert_file_has "refuses over version skew" "$box/update.log" \
        "Refusing to bump the version while the Intel url still points at another release"
    assert_file_lacks "no state is recorded for a refused run" "$box/github_env" "mac_x86_state="
}

case_malformed_duplicate_stanza() {
    begin_case "malformed tap: two Intel stanzas"
    local box status
    box=$(new_sandbox "${FIXTURES}/two-intel-stanzas.rb")
    export_artifact_env true

    run_formula_step 'Update formula' "$box" "$box/update.log"
    status=$?
    assert_status "update step refuses" 1 "$status" "$box/update.log"
    assert_file_has "names the ambiguous artifact" "$box/update.log" \
        "expected exactly one url stanza for all-smi-macos-x86_64.zip, found 2"
}

# The regression this issue's pairing guard exists for. Every checksum in the
# file is one the run computed, every url names the right release, and the
# counts are right; only the pairing is wrong. Everything the validate step
# checked before #316 passes on it.
case_swapped_checksums() {
    begin_case "swapped checksums: url and sha256 correct apart, wrong together"
    local box status formula independent_checks_pass sha
    box="$SANDBOX_STATE1"
    if [ -z "${box:-}" ]; then
        bad "state 1 sandbox available" "state 1 must run first"
        return
    fi
    formula="$box/homebrew-tap/Formula/all-smi.rb"
    export_artifact_env true
    export mac_x86_state=updated

    # Exchange the macOS aarch64 checksum with the Linux x86_64 one.
    gsed -i "s|sha256 \"${SHA_MAC}\"|sha256 \"__SWAP__\"|" "$formula"
    gsed -i "s|sha256 \"${SHA_LINUX_X86}\"|sha256 \"${SHA_MAC}\"|" "$formula"
    gsed -i "s|sha256 \"__SWAP__\"|sha256 \"${SHA_LINUX_X86}\"|" "$formula"

    assert_eq "the swap took" "$SHA_LINUX_X86" "$(sha_after "$formula" all-smi-macos-aarch64.zip)"

    # Control: the pre-#316 checks, reimplemented here, all accept this file.
    independent_checks_pass=yes
    [ "$(count_sha_stanzas "$formula")" = 4 ] || independent_checks_pass=no
    for sha in "$SHA_MAC" "$SHA_MAC_X86" "$SHA_LINUX_ARM" "$SHA_LINUX_X86"; do
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
    box="$SANDBOX_STATE2"
    if [ -z "${box:-}" ]; then
        bad "state 2 sandbox available" "state 2 must run first"
        return
    fi
    formula="$box/homebrew-tap/Formula/all-smi.rb"
    export_artifact_env false
    export mac_x86_state=skipped

    gsed -i "s|download/v${NEW_VERSION}/all-smi-linux-x86_64.tar.gz|download/v0.25.0/all-smi-linux-x86_64.tar.gz|" "$formula"

    run_formula_step 'Validate updated formula' "$box" "$box/validate-stale.log"
    status=$?
    assert_status "validate step rejects it" 1 "$status" "$box/validate-stale.log"
    assert_file_has "names the artifact left behind" "$box/validate-stale.log" \
        "all-smi-linux-x86_64.tar.gz"
}

run_formula_cases() {
    case_state1_asset_and_stanza
    case_state2_neither
    case_state3_asset_without_stanza
    case_state4_stanza_without_asset
    case_malformed_duplicate_stanza
    case_swapped_checksums
    case_stale_url
}
