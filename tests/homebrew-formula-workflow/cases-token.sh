#!/usr/bin/env bash
# The `Commit and push changes to tap` step, executed for real against a local
# authenticating Git smart-HTTP server, plus the structural checks on the
# workflow that no step body can make.
#
# The claim under test is that the tap credential exists in exactly one place,
# the environment of one command, for the length of that command. Proving it
# takes a server that records what arrived on the wire and a sandbox that can be
# searched afterwards for anything left behind.
#
# Sourced by run-workflow-steps.sh.

FAKE_TOKEN="ghs_notARealToken000000000000000000000000"

start_git_server() {  # sandbox -> echoes port
    local box="$1" port
    (
        export GIT_PROJECT_ROOT="${box}/remote"
        export EXPECT_USER="x-access-token"
        export EXPECT_PASS="$FAKE_TOKEN"
        export AUTH_LOG="${box}/auth.log"
        exec python3 "${HARNESS_DIR}/git-http-server.py"
    ) > "${box}/port" 2>"${box}/server.log" &
    GIT_SERVER_PID=$!

    local waited=0
    while [ "$waited" -lt 100 ]; do
        port=$(cat "${box}/port" 2>/dev/null)
        if [ -n "$port" ]; then
            printf '%s' "$port"
            return 0
        fi
        sleep 0.1
        waited=$((waited + 1))
    done
    return 1
}

stop_git_server() {
    if [ -n "${GIT_SERVER_PID:-}" ]; then
        kill "$GIT_SERVER_PID" 2>/dev/null
        wait "$GIT_SERVER_PID" 2>/dev/null
        GIT_SERVER_PID=""
    fi
}

# A bare tap on the server side, a working clone on the client side whose
# `origin` is the http url, and a HOME configured the way a runner's is: with a
# global credential helper that would happily store anything git hands it.
new_push_sandbox() {
    local box port
    box=$(mktemp -d "${TMPDIR:-/tmp}/all-smi-hbw-push.XXXXXX")

    mkdir -p "${box}/remote"
    git init --quiet --bare --initial-branch=main "${box}/remote/tap.git"
    git -C "${box}/remote/tap.git" config http.receivepack true

    make_tap "$box" "${FIXTURES}/one-stanza.rb"
    git -C "${box}/homebrew-tap" push --quiet "${box}/remote/tap.git" main

    # XDG_CONFIG_HOME is overridden alongside HOME because `git config --global`
    # writes to $XDG_CONFIG_HOME/git/config when that file exists, and the point
    # of the next line is to plant a global credential helper in the sandbox,
    # not in whatever config the person running these tests actually uses.
    mkdir -p "${box}/home" "${box}/xdg"
    HOME="${box}/home" XDG_CONFIG_HOME="${box}/xdg" git config --global credential.helper store
    printf 'https://someone:leftover@127.0.0.1\n' > "${box}/home/.git-credentials"

    port=$(start_git_server "$box") || return 1
    git -C "${box}/homebrew-tap" remote add origin "http://127.0.0.1:${port}/tap.git"

    printf '%s' "$box"
}

run_push_step() {  # sandbox logfile -> status
    local box="$1" log="$2"
    local script="${box}/push.sh"
    step_script 'Commit and push changes to tap' "$script" || return 127
    (
        cd "$box" || exit 127
        export HOME="${box}/home"
        export XDG_CONFIG_HOME="${box}/xdg"
        export HOMEBREW_TAP_TOKEN="$FAKE_TOKEN"
        export VERSION_NO_V="$NEW_VERSION"
        # -x on purpose: the trace is one of the things being asserted about.
        bash -x "$script"
    ) > "$log" 2>&1
}

case_push_sends_scoped_credential() {
    begin_case "push step: the credential reaches the wire and nothing else"
    local box status expected_basic pushed leftovers
    box=$(new_push_sandbox)
    if [ -z "$box" ]; then
        bad "local git server started"
        return
    fi

    gsed -i 's|version "0.25.0"|version "0.26.0"|' "${box}/homebrew-tap/Formula/all-smi.rb"

    run_push_step "$box" "${box}/push.log"
    status=$?
    assert_status "push step succeeds" 0 "$status" "${box}/push.log"

    pushed=$(git -C "${box}/remote/tap.git" log -1 --format=%s main 2>/dev/null)
    assert_eq "the tap received the commit" "bump: all-smi to v${NEW_VERSION}" "$pushed"

    expected_basic="Basic $(printf 'x-access-token:%s' "$FAKE_TOKEN" | base64)"
    assert_file_has "the server was given the tap credential" "${box}/auth.log" "$expected_basic"

    # Everything the step did, traced. If the token were built into the command
    # line, `set -x` would have printed it here, which is the failure mode the
    # http.extraheader alternative has.
    assert_file_lacks "the execution trace does not contain the token" "${box}/push.log" "$FAKE_TOKEN"
    # shellcheck disable=SC2016  # the unexpanded name is exactly what is asserted
    assert_file_has "the trace shows the variable name instead" "${box}/push.log" '$HOMEBREW_TAP_TOKEN'

    assert_file_lacks "the clone's git config does not contain the token" \
        "${box}/homebrew-tap/.git/config" "$FAKE_TOKEN"
    assert_file_lacks "the global credential store did not gain the token" \
        "${box}/home/.git-credentials" "$FAKE_TOKEN"
    assert_file_has "the poisoned global store was left alone, not consulted" \
        "${box}/home/.git-credentials" "leftover"

    # The whole sandbox, which stands in for the runner's disk. auth.log holds
    # the base64 of the credential and is the server's record, not the client's.
    leftovers=$(grep -rl -- "$FAKE_TOKEN" "$box" 2>/dev/null | grep -v '/auth.log$' | grep -v '/push.sh$')
    assert_eq "no file under the workspace contains the token" "" "$leftovers"

    stop_git_server
}

case_push_is_a_noop_when_unchanged() {
    begin_case "push step: re-running for a version already in the tap"
    local box status
    box=$(new_push_sandbox)
    if [ -z "$box" ]; then
        bad "local git server started"
        return
    fi

    run_push_step "$box" "${box}/push.log"
    status=$?
    assert_status "push step succeeds without pushing" 0 "$status" "${box}/push.log"
    assert_file_has "says so" "${box}/push.log" "nothing to push"
    assert_eq "the server was never contacted" "" "$(cat "${box}/auth.log" 2>/dev/null)"

    stop_git_server
}

case_workflow_shape() {
    begin_case "workflow shape: permissions, concurrency, and where the secret is named"
    local output
    output=$(python3 "${HARNESS_DIR}/check-workflow-shape.py" "$WORKFLOW" 2>&1)
    printf '%s\n' "$output"
    PASS_COUNT=$((PASS_COUNT + $(printf '%s\n' "$output" | grep -c '^  ok    ')))
    FAIL_COUNT=$((FAIL_COUNT + $(printf '%s\n' "$output" | grep -c '^  FAIL  ')))
}

run_token_cases() {
    case_workflow_shape
    case_push_sends_scoped_credential
    case_push_is_a_noop_when_unchanged
}
