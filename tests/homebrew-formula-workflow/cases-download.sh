#!/usr/bin/env bash
# The `Download release artifacts and calculate SHA256` step, executed against
# stubbed `gh` and `curl`. Offline, so it can run anywhere and cannot be made
# green or red by the state of a real release.
#
# The stubs are assertions in their own right. The `gh` stub refuses to answer
# an assets listing that arrives without --paginate, and refuses to hand back
# the `assets` array embedded in the release object at all, so reverting either
# of those fails here rather than years later on the release that first grows
# past a page.
#
# Sourced by run-workflow-steps.sh.

# SC2030/SC2031: the step bodies are deliberately run inside a subshell so their
# environment cannot leak between cases. Locality is the point.
# shellcheck disable=SC2030,SC2031

write_download_stubs() {  # bindir
    local bindir="$1"
    mkdir -p "$bindir"

    cat > "${bindir}/gh" <<'STUB'
#!/usr/bin/env bash
set -u
printf 'gh %s\n' "$*" >> "$STUB_LOG"
[ "${1:-}" = "api" ] || { echo "stub gh: unsupported subcommand: ${1:-}" >&2; exit 2; }
shift
paginate=0
path=""
jq_filter=""
while [ $# -gt 0 ]; do
  case "$1" in
    --paginate) paginate=1; shift ;;
    --jq) jq_filter="${2:-}"; shift 2 ;;
    -*) shift ;;
    *) path="$1"; shift ;;
  esac
done
case "$path" in
  */releases/tags/*)
    case "$jq_filter" in
      *assets*)
        echo "stub gh: read the assets array embedded in the release object; use the paginated assets collection instead" >&2
        exit 3
        ;;
    esac
    printf '%s\n' "$STUB_RELEASE_ID"
    ;;
  */releases/*/assets*)
    if [ "$paginate" -ne 1 ]; then
      echo "stub gh: assets listed without --paginate" >&2
      exit 4
    fi
    cat "$STUB_ASSETS_FILE"
    ;;
  *)
    echo "stub gh: unexpected api path: $path" >&2
    exit 5
    ;;
esac
STUB

    cat > "${bindir}/curl" <<'STUB'
#!/usr/bin/env bash
set -u
url=""
dest=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) dest="${2:-}"; shift 2 ;;
    --retry) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
printf 'curl %s -> %s\n' "$url" "$dest" >> "$STUB_LOG"
case "$url" in
  *.zip) cp "$STUB_ZIP" "$dest" ;;
  *.tar.gz) cp "$STUB_TGZ" "$dest" ;;
  *) echo "stub curl: unexpected url: $url" >&2; exit 22 ;;
esac
STUB

    chmod +x "${bindir}/gh" "${bindir}/curl"
}

make_stub_archives() {  # dir
    mkdir -p "$1/payload"
    printf 'stub binary\n' > "$1/payload/all-smi"
    (cd "$1/payload" && zip -qr "$1/stub.zip" all-smi)
    (cd "$1/payload" && tar -czf "$1/stub.tar.gz" all-smi)
}

# ASSET_LINES: names the stubbed release publishes, one per line.
run_download_step() {  # sandbox asset-lines logfile -> status
    local box="$1" asset_lines="$2" log="$3"
    local script="${box}/download.sh"
    step_script 'Download release artifacts and calculate SHA256' "$script" || return 127
    printf '%s\n' "$asset_lines" > "${box}/assets.txt"
    (
        cd "$box" || exit 127
        export PATH="${box}/bin:$PATH"
        export GITHUB_ENV="${box}/github_env"
        export RUNNER_TEMP="${box}/runner_temp"
        export GITHUB_REPOSITORY="lablup/all-smi"
        export VERSION="v${NEW_VERSION}"
        export STUB_LOG="${box}/stub.log"
        export STUB_RELEASE_ID="362933399"
        export STUB_ASSETS_FILE="${box}/assets.txt"
        export STUB_ZIP="${box}/stub.zip"
        export STUB_TGZ="${box}/stub.tar.gz"
        bash "$script"
    ) > "$log" 2>&1
}

new_download_sandbox() {
    local dir
    dir=$(mktemp -d "${TMPDIR:-/tmp}/all-smi-hbw-dl.XXXXXX")
    make_tap "$dir" "${FIXTURES}/one-stanza.rb"
    : > "$dir/github_env"
    : > "$dir/stub.log"
    mkdir -p "$dir/runner_temp"
    write_download_stubs "$dir/bin"
    make_stub_archives "$dir"
    printf '%s' "$dir"
}

THREE_ASSETS='all-smi-macos-aarch64.zip
all-smi-macos-aarch64.zip.sha256
all-smi-linux-aarch64.tar.gz
all-smi-linux-x86_64.tar.gz'

case_download_keeps_tap_clean() {
    begin_case "download step: artifacts land outside the tap clone"
    local box status dirty artifacts assets
    box=$(new_download_sandbox)

    # The Intel zip is deliberately the last name in a long list, so a listing
    # that stopped early would report it absent.
    assets="$THREE_ASSETS"
    local i=0
    while [ "$i" -lt 200 ]; do
        assets="${assets}
filler-asset-${i}.txt"
        i=$((i + 1))
    done
    assets="${assets}
all-smi-macos-x86_64.zip"

    run_download_step "$box" "$assets" "$box/download.log"
    status=$?
    assert_status "download step succeeds" 0 "$status" "$box/download.log"

    artifacts="${box}/runner_temp/all-smi-artifacts"
    for name in mac.zip linux-arm.tar.gz linux-x86.tar.gz mac-x86.zip; do
        if [ -s "${artifacts}/${name}" ]; then
            ok "downloaded ${name} into RUNNER_TEMP"
        else
            bad "downloaded ${name} into RUNNER_TEMP" "missing: ${artifacts}/${name}"
        fi
    done

    dirty=$(git -C "${box}/homebrew-tap" status --porcelain)
    assert_eq "tap working tree is untouched" "" "$dirty"
    if [ -e "${box}/homebrew-tap/tmp" ]; then
        bad "no tmp directory inside the tap clone" "${box}/homebrew-tap/tmp exists"
    else
        ok "no tmp directory inside the tap clone"
    fi

    assert_file_has "the Intel zip at the end of the list is still seen" \
        "$box/github_env" "mac_x86_present=true"
    assert_file_has "asset list came from the paginated assets collection" \
        "$box/stub.log" "/assets?per_page=100"
    assert_file_has "asset list was requested with --paginate" \
        "$box/stub.log" "--paginate"
    assert_file_has "checksum was recorded for the macOS zip" "$box/github_env" "mac_sha="
}

case_download_absent_intel_asset() {
    begin_case "download step: release without the Intel zip"
    local box status
    box=$(new_download_sandbox)

    run_download_step "$box" "$THREE_ASSETS" "$box/download.log"
    status=$?
    assert_status "download step succeeds" 0 "$status" "$box/download.log"
    assert_file_has "records mac_x86_present=false" "$box/github_env" "mac_x86_present=false"
    assert_file_has "emits a notice" "$box/download.log" "::notice::"
    if [ -e "${box}/runner_temp/all-smi-artifacts/mac-x86.zip" ]; then
        bad "no Intel zip is downloaded" "mac-x86.zip should not exist"
    else
        ok "no Intel zip is downloaded"
    fi
}

case_download_rejects_bad_version() {
    begin_case "download step: a tag that is not a plain version is refused"
    local box status script
    box=$(new_download_sandbox)
    script="${box}/download.sh"
    step_script 'Download release artifacts and calculate SHA256' "$script"
    printf '%s\n' "$THREE_ASSETS" > "${box}/assets.txt"
    (
        cd "$box" || exit 127
        export PATH="${box}/bin:$PATH"
        export GITHUB_ENV="${box}/github_env"
        export RUNNER_TEMP="${box}/runner_temp"
        export GITHUB_REPOSITORY="lablup/all-smi"
        export VERSION="v0.0.0/../../../attacker/repo/releases/download/v1"
        export STUB_LOG="${box}/stub.log"
        export STUB_RELEASE_ID="1"
        export STUB_ASSETS_FILE="${box}/assets.txt"
        export STUB_ZIP="${box}/stub.zip"
        export STUB_TGZ="${box}/stub.tar.gz"
        bash "$script"
    ) > "$box/download.log" 2>&1
    status=$?
    assert_status "download step refuses the traversal" 1 "$status" "$box/download.log"
    assert_file_has "explains why" "$box/download.log" "Refusing to run for tag"
    assert_eq "nothing was downloaded" "" "$(cat "$box/stub.log")"
}

run_download_cases() {
    case_download_keeps_tap_clean
    case_download_absent_intel_asset
    case_download_rejects_bad_version
}
