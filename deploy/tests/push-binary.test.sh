#!/usr/bin/env bash
# Tests for deploy/push-binary.sh.
#
# Run: ./deploy/tests/push-binary.test.sh
#
# Every case drives the script with DRY_RUN=1 and asserts on the scp/ssh command
# lines it would have run. No droplet, no network, no ssh key required.
set -uo pipefail

SCRIPT_UNDER_TEST="$(cd "$(dirname "$0")/.." && pwd)/push-binary.sh"
FAILURES=0
CASES=0

# Runs push-binary.sh in dry-run mode and asserts the output contains a line.
assert_emits() {
    local name=$1 expected=$2
    shift 2
    CASES=$((CASES + 1))
    local output
    output=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" "$@" 2>&1)
    if printf '%s\n' "$output" | grep -Fqx -- "$expected"; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        echo "       expected line: $expected"
        echo "       actual output:"
        printf '%s\n' "$output" | sed 's/^/         /'
        FAILURES=$((FAILURES + 1))
    fi
}

# Asserts the script exits non-zero (dry-run still validates arguments).
assert_fails() {
    local name=$1
    shift
    CASES=$((CASES + 1))
    if DRY_RUN=1 "$SCRIPT_UNDER_TEST" "$@" >/dev/null 2>&1; then
        echo "FAIL — $name (expected non-zero exit, got 0)"
        FAILURES=$((FAILURES + 1))
    else
        echo "ok   — $name"
    fi
}

# Extracts the remote install command the script would issue and runs it locally
# against a stub sudo that denies it, asserting the failure reaches the caller.
# This checks behaviour rather than a chosen operator, so it keeps holding if the
# command is rewritten some other way.
assert_install_failure_propagates() {
    local name=$1 binary=$2
    CASES=$((CASES + 1))
    local remote stub status
    remote=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$binary" \
        | grep -F 'sudo install -m 755' \
        | sed 's|^ssh -p [0-9]* [^ ]* ||')
    if [[ -z "$remote" ]]; then
        echo "FAIL — $name (no install command found in the dry run)"
        FAILURES=$((FAILURES + 1))
        return
    fi

    stub=$(mktemp -d)
    printf '#!/bin/sh\nexit 1\n' >"$stub/sudo"  # the install is denied
    printf '#!/bin/sh\nexit 0\n' >"$stub/rm"    # the cleanup would succeed
    chmod +x "$stub/sudo" "$stub/rm"
    PATH="$stub:$PATH" bash -c "$remote" >/dev/null 2>&1
    status=$?
    /bin/rm -rf "$stub"

    if [[ "$status" -ne 0 ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name (a denied install exited 0)"
        echo "       remote command: $remote"
        FAILURES=$((FAILURES + 1))
    fi
}

BIN=target/x86_64-unknown-linux-musl/release/gl-serv

echo "== push-binary.sh =="

# Port 22 is implied when the caller omits it, so deploy.sh's two-arg form works.
assert_emits push_binary_defaults_to_ssh_port_22 \
    "scp -P 22 $BIN goopy@dev.example.com:/tmp/gl-serv" \
    goopy@dev.example.com "$BIN"

# scp takes -P and ssh takes -p; a custom port has to reach both spellings.
assert_emits push_binary_honours_custom_ssh_port_for_scp \
    "scp -P 2222 $BIN goopy@dev.example.com:/tmp/gl-serv" \
    goopy@dev.example.com "$BIN" 2222
assert_emits push_binary_honours_custom_ssh_port_for_ssh \
    "ssh -p 2222 goopy@dev.example.com sudo systemctl restart gl-serv" \
    goopy@dev.example.com "$BIN" 2222

# This exact string is whitelisted in deploy/sudoers.goopy — any drift here
# (a different mode, path, or argument order) becomes a sudo denial on deploy.
assert_emits push_binary_pins_the_sudoers_install_command \
    "ssh -p 22 goopy@dev.example.com sudo install -m 755 /tmp/gl-serv /opt/goopy-life/bin/gl-serv && rm /tmp/gl-serv" \
    goopy@dev.example.com "$BIN"

# A ';' here would report rm's exit status instead of install's, so a sudo
# denial would leave the deploy green while the old binary kept running.
assert_install_failure_propagates push_binary_propagates_a_failed_install "$BIN"

# A deploy that does not verify the restart reports success while the API is down.
assert_emits push_binary_verifies_the_service_is_active_after_restart \
    "ssh -p 22 goopy@dev.example.com sleep 8; systemctl is-active --quiet gl-serv" \
    goopy@dev.example.com "$BIN"

# Both positional arguments are mandatory — a missing one must not half-deploy.
assert_fails push_binary_requires_a_target goopy@dev.example.com
assert_fails push_binary_requires_a_binary_path

# Outside dry-run the binary must exist, so a failed build cannot ship the
# previous artifact (or nothing at all) to the droplet.
CASES=$((CASES + 1))
if DRY_RUN=0 "$SCRIPT_UNDER_TEST" goopy@dev.example.com /nonexistent/gl-serv >/dev/null 2>&1; then
    echo "FAIL — push_binary_rejects_a_missing_binary (expected non-zero exit, got 0)"
    FAILURES=$((FAILURES + 1))
else
    echo "ok   — push_binary_rejects_a_missing_binary"
fi

echo
if [[ "$FAILURES" -eq 0 ]]; then
    echo "$CASES passed, 0 failed"
else
    echo "$((CASES - FAILURES)) passed, $FAILURES failed"
    exit 1
fi
