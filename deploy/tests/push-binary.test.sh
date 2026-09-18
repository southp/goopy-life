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
    local name=$1 binary=$2 config=$3
    CASES=$((CASES + 1))
    local remote stub status
    remote=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$binary" "$config" \
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


# Asserts the config is staged in the same directory it is renamed into, which
# is what makes the swap atomic. Compares the two paths the script actually
# emits rather than restating them, so a change to either one is caught here.
assert_config_swap_is_atomic() {
    local name=$1 binary=$2 config=$3
    CASES=$((CASES + 1))
    local output staged destination
    output=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$binary" "$config")
    staged=$(printf '%s\n' "$output" | grep '^scp ' | grep -v ':/tmp/gl-serv$' | sed 's/.*://')
    destination=$(printf '%s\n' "$output" | sed -n 's/.* mv [^ ]* \([^ ]*\) .*/\1/p')

    if [[ -n "$staged" && -n "$destination" && "$(dirname "$staged")" == "$(dirname "$destination")" ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        echo "       staged:      ${staged:-<none>}"
        echo "       destination: ${destination:-<none>}"
        FAILURES=$((FAILURES + 1))
    fi
}

# Asserts the config gate sits between the uploads and the install. Ordering is
# the whole point of this step: a gate that ran after the swap would report a
# failure the old binary was still in a position to prevent.
assert_config_gate_is_ordered() {
    local name=$1 binary=$2 config=$3
    CASES=$((CASES + 1))
    local output last_upload gate install
    output=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$binary" "$config")
    last_upload=$(printf '%s\n' "$output" | grep -n '^scp ' | tail -1 | cut -d: -f1)
    gate=$(printf '%s\n' "$output" | grep -n -- '--check-config' | head -1 | cut -d: -f1)
    install=$(printf '%s\n' "$output" | grep -n 'sudo install -m 755' | head -1 | cut -d: -f1)

    if [[ -n "$last_upload" && -n "$gate" && -n "$install" \
        && "$last_upload" -lt "$gate" && "$gate" -lt "$install" ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        echo "       last upload: ${last_upload:-<none>}"
        echo "       gate:        ${gate:-<none>}"
        echo "       install:     ${install:-<none>}"
        FAILURES=$((FAILURES + 1))
    fi
}

# Drives the script for real against stub scp/ssh, with the stub failing the
# config gate. Asserts the deploy stops there: nothing installed, nothing
# restarted, so the host keeps serving the pair it already had.
assert_failed_gate_aborts_before_install() {
    local name=$1
    CASES=$((CASES + 1))
    local stub log status
    stub=$(mktemp -d)
    log="$stub/calls"
    printf '#!/bin/sh\nexit 0\n' >"$stub/scp"
    cat >"$stub/ssh" <<STUB
#!/bin/sh
echo "\$*" >>"$log"
case "\$*" in
    *--check-config*) exit 1 ;;
esac
exit 0
STUB
    chmod +x "$stub/scp" "$stub/ssh"

    # Any two existing files stand in for the binary and the config: the
    # transfer is stubbed, only the existence check ahead of it is real.
    PATH="$stub:$PATH" DRY_RUN=0 "$SCRIPT_UNDER_TEST" goopy@dev.example.com \
        "$SCRIPT_UNDER_TEST" "$SCRIPT_UNDER_TEST" >/dev/null 2>&1
    status=$?

    local installed="" restarted=""
    installed=$(grep -c 'sudo install' "$log" 2>/dev/null || true)
    restarted=$(grep -c 'systemctl restart' "$log" 2>/dev/null || true)
    /bin/rm -rf "$stub"

    if [[ "$status" -ne 0 && "${installed:-0}" -eq 0 && "${restarted:-0}" -eq 0 ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        echo "       exit status: $status (expected non-zero)"
        echo "       install calls: ${installed:-0}, restart calls: ${restarted:-0}"
        FAILURES=$((FAILURES + 1))
    fi
}

# Asserts that outside dry-run a non-existent input file aborts before any
# command runs. Takes the binary and config paths so either can be the missing
# one.
assert_missing_file_rejected() {
    local name=$1 binary=$2 config=$3
    CASES=$((CASES + 1))
    if DRY_RUN=0 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$binary" "$config" >/dev/null 2>&1; then
        echo "FAIL — $name (expected non-zero exit, got 0)"
        FAILURES=$((FAILURES + 1))
    else
        echo "ok   — $name"
    fi
}

BIN=target/x86_64-unknown-linux-musl/release/gl-serv
CFG=deploy/config/dev.toml
REMOTE_CFG=/opt/goopy-life/config.toml

echo "== push-binary.sh =="

# Port 22 is implied when the caller omits it, so deploy.sh's three-arg form works.
assert_emits push_binary_defaults_to_ssh_port_22 \
    "scp -P 22 $BIN goopy@dev.example.com:/tmp/gl-serv" \
    goopy@dev.example.com "$BIN" "$CFG"

# scp takes -P and ssh takes -p; a custom port has to reach both spellings.
assert_emits push_binary_honours_custom_ssh_port_for_scp \
    "scp -P 2222 $BIN goopy@dev.example.com:/tmp/gl-serv" \
    goopy@dev.example.com "$BIN" "$CFG" 2222
assert_emits push_binary_honours_custom_ssh_port_for_ssh \
    "ssh -p 2222 goopy@dev.example.com sudo systemctl restart gl-serv" \
    goopy@dev.example.com "$BIN" "$CFG" 2222

# The config travels with the binary rather than being hand-maintained on the
# droplet, which is what stopped it drifting out of sync with the schema.
assert_emits push_binary_ships_the_config_alongside_the_binary \
    "scp -P 22 $CFG goopy@dev.example.com:$REMOTE_CFG.new" \
    goopy@dev.example.com "$BIN" "$CFG"

# The gate runs the uploaded binary against the STAGED config, not the one
# currently installed: the installed file is about to be replaced, so checking
# it would pass on a broken incoming pair and fail on a fine one.
assert_emits push_binary_checks_the_staged_config_with_the_new_binary \
    "ssh -p 22 goopy@dev.example.com chmod +x /tmp/gl-serv && /tmp/gl-serv --check-config --config $REMOTE_CFG.new" \
    goopy@dev.example.com "$BIN" "$CFG"

assert_config_gate_is_ordered push_binary_checks_the_config_before_installing "$BIN" "$CFG"

# The point of checking early: a bad config must cost a failed deploy, not an
# outage. `systemctl is-active` at the end catches the same failure, but only
# once the old binary has already been stopped.
assert_failed_gate_aborts_before_install push_binary_aborts_the_deploy_when_the_config_check_fails

# The install substring is whitelisted in deploy/sudoers.goopy — any drift there
# (a different mode, path, or argument order) becomes a sudo denial on deploy.
assert_emits push_binary_pins_the_sudoers_install_command \
    "ssh -p 22 goopy@dev.example.com sudo install -m 755 /tmp/gl-serv /opt/goopy-life/bin/gl-serv && chmod 644 $REMOTE_CFG.new && mv $REMOTE_CFG.new $REMOTE_CFG && rm /tmp/gl-serv" \
    goopy@dev.example.com "$BIN" "$CFG"

# A ';' in that chain would report rm's exit status instead of install's, so a
# sudo denial would leave the deploy green while the old binary kept running.
assert_install_failure_propagates push_binary_propagates_a_failed_install "$BIN" "$CFG"

# The config is staged beside its destination so the swap is a same-directory
# rename. Staging in /tmp instead would make it a cross-filesystem copy, and
# gl-serv restarts moments later — it must never read a half-written file.
assert_config_swap_is_atomic push_binary_swaps_the_config_atomically "$BIN" "$CFG"

# A deploy that does not verify the restart reports success while the API is down.
assert_emits push_binary_verifies_the_service_is_active_after_restart \
    "ssh -p 22 goopy@dev.example.com sleep 8; systemctl is-active --quiet gl-serv" \
    goopy@dev.example.com "$BIN" "$CFG"

# All three positional arguments are mandatory — a missing one must not half-deploy.
assert_fails push_binary_requires_a_target goopy@dev.example.com
assert_fails push_binary_requires_a_binary_path
assert_fails push_binary_requires_a_config_path goopy@dev.example.com "$BIN"

# Outside dry-run both files must exist, so a failed build cannot ship the
# previous artifact (or nothing at all) to the droplet, and a mistyped
# environment cannot overwrite a working config with an empty file.
assert_missing_file_rejected push_binary_rejects_a_missing_binary \
    /nonexistent/gl-serv "$SCRIPT_UNDER_TEST"
assert_missing_file_rejected push_binary_rejects_a_missing_config \
    "$SCRIPT_UNDER_TEST" /nonexistent/config.toml

echo
if [[ "$FAILURES" -eq 0 ]]; then
    echo "$CASES passed, 0 failed"
else
    echo "$((CASES - FAILURES)) passed, $FAILURES failed"
    exit 1
fi
