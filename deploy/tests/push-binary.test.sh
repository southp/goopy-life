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

# The script requires the commit its binaries were built from, so every case
# below has to supply one. A single case runs without it, to assert that the
# requirement is real: without it the identity check at the end of a deploy
# would have nothing to compare against.
BUILT_SHA=c50c932ab1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8
export GL_GIT_SHA="$BUILT_SHA"

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
    local name=$1 denied=$2 serv=$3 cli=$4 config=$5
    CASES=$((CASES + 1))
    local remote stub status
    remote=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$serv" "$cli" "$config" \
        | grep -F 'sudo install -m 755' \
        | sed 's|^ssh -p [0-9]* [^ ]* ||')
    if [[ -z "$remote" ]]; then
        echo "FAIL — $name (no install command found in the dry run)"
        FAILURES=$((FAILURES + 1))
        return
    fi

    stub=$(mktemp -d)
    # Only the named destination is denied, so each binary's install can be
    # failed on its own while the other still succeeds.
    cat >"$stub/sudo" <<STUB
#!/bin/sh
case "\$*" in
    *$denied*) exit 1 ;;
esac
exit 0
STUB
    printf '#!/bin/sh\nexit 0\n' >"$stub/chmod"
    printf '#!/bin/sh\nexit 0\n' >"$stub/install"
    printf '#!/bin/sh\nexit 0\n' >"$stub/rm"    # the cleanup would succeed
    # The config swap must not happen once an install has failed; record it.
    cat >"$stub/mv" <<STUB
#!/bin/sh
echo swapped >>"$stub/swapped"
STUB
    chmod +x "$stub/sudo" "$stub/chmod" "$stub/install" "$stub/rm" "$stub/mv"
    PATH="$stub:$PATH" bash -c "$remote" >/dev/null 2>&1
    status=$?
    local swapped=0
    if [[ -e "$stub/swapped" ]]; then
        swapped=1
    fi
    /bin/rm -rf "$stub"

    if [[ "$status" -ne 0 && "$swapped" -eq 0 ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        echo "       exit status: $status (expected non-zero)"
        echo "       config swapped: $swapped (expected 0)"
        echo "       remote command: $remote"
        FAILURES=$((FAILURES + 1))
    fi
}


# Asserts every `sudo install` the script would run is pinned verbatim in
# deploy/sudoers.goopy. That drop-in whitelists exact command lines, so a mode,
# path or argument-order change on one side alone is a bare sudo denial on the
# droplet rather than anything self-explanatory. The commands are read out of
# the script's own dry run instead of being restated here, so an artifact added
# to the deploy later is covered without editing this test.
assert_sudoers_pins_every_install() {
    local name=$1 serv=$2 cli=$3 config=$4
    CASES=$((CASES + 1))
    local sudoers output installs unpinned=""
    sudoers="$(cd "$(dirname "$0")/.." && pwd)/sudoers.goopy"
    output=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$serv" "$cli" "$config")

    # Each install is one link of an && chain, so stop at the next operator.
    installs=$(printf '%s\n' "$output" | grep -o 'sudo install [^&]*' | sed 's/ *$//')
    if [[ -z "$installs" ]]; then
        echo "FAIL — $name (no install command found in the dry run)"
        FAILURES=$((FAILURES + 1))
        return
    fi

    while IFS= read -r command; do
        # sudo resolves a bare `install` through PATH to /usr/bin/install,
        # which is the absolute path the drop-in has to spell.
        local pinned="/usr/bin/${command#sudo }"
        if ! grep -Fq -- "$pinned" "$sudoers"; then
            unpinned+="       missing from sudoers.goopy: $pinned"$'\n'
        fi
    done <<<"$installs"

    if [[ -z "$unpinned" ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        printf '%s' "$unpinned"
        FAILURES=$((FAILURES + 1))
    fi
}

# Asserts the config is staged in the same directory it is renamed into, which
# is what makes the swap atomic. Compares the two paths the script actually
# emits rather than restating them, so a change to either one is caught here.
assert_config_swap_is_atomic() {
    local name=$1 serv=$2 cli=$3 config=$4
    CASES=$((CASES + 1))
    local output staged destination
    output=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$serv" "$cli" "$config")
    staged=$(printf '%s\n' "$output" | grep '^scp ' | grep -v -e ':/tmp/gl-serv$' -e ':/tmp/gl-cli$' | sed 's/.*://')
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
    local name=$1 serv=$2 cli=$3 config=$4
    CASES=$((CASES + 1))
    local output last_upload gate install
    output=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$serv" "$cli" "$config")
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

    # Any three existing files stand in for the binaries and the config: the
    # transfer is stubbed, only the existence check ahead of it is real.
    PATH="$stub:$PATH" DRY_RUN=0 "$SCRIPT_UNDER_TEST" goopy@dev.example.com \
        "$SCRIPT_UNDER_TEST" "$SCRIPT_UNDER_TEST" "$SCRIPT_UNDER_TEST" >/dev/null 2>&1
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

# Drives the script for real against stub scp/ssh, with the stub failing the
# remote install step. Asserts the deploy stops there rather than restarting
# gl-serv: with either binary un-replaced, a restart would report a green deploy
# while the host went on running exactly what it ran before.
assert_failed_install_aborts_before_restart() {
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
    *"sudo install"*) exit 1 ;;
esac
exit 0
STUB
    chmod +x "$stub/scp" "$stub/ssh"

    PATH="$stub:$PATH" DRY_RUN=0 "$SCRIPT_UNDER_TEST" goopy@dev.example.com \
        "$SCRIPT_UNDER_TEST" "$SCRIPT_UNDER_TEST" "$SCRIPT_UNDER_TEST" >/dev/null 2>&1
    status=$?

    local restarted=""
    restarted=$(grep -c 'systemctl restart' "$log" 2>/dev/null || true)
    /bin/rm -rf "$stub"

    if [[ "$status" -ne 0 && "${restarted:-0}" -eq 0 ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        echo "       exit status: $status (expected non-zero)"
        echo "       restart calls: ${restarted:-0} (expected 0)"
        FAILURES=$((FAILURES + 1))
    fi
}

# Asserts the identity check sits after the restart and after `is-active`:
# asking a process that has not been replaced yet, or one that is still
# starting, answers a question about the wrong binary.
assert_identity_check_is_last() {
    local name=$1 serv=$2 cli=$3 config=$4
    CASES=$((CASES + 1))
    local output restart is_active version
    output=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$serv" "$cli" "$config")
    restart=$(printf '%s\n' "$output" | grep -n 'systemctl restart' | head -1 | cut -d: -f1)
    is_active=$(printf '%s\n' "$output" | grep -n 'is-active' | head -1 | cut -d: -f1)
    version=$(printf '%s\n' "$output" | grep -n '/version' | head -1 | cut -d: -f1)

    if [[ -n "$restart" && -n "$is_active" && -n "$version" \
        && "$restart" -lt "$is_active" && "$is_active" -lt "$version" ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        echo "       restart:   ${restart:-<none>}"
        echo "       is-active: ${is_active:-<none>}"
        echo "       /version:  ${version:-<none>}"
        FAILURES=$((FAILURES + 1))
    fi
}

# Extracts the identity check the script would run on the host and drives it
# locally against a stub gl-serv and a stub curl. The comparison runs in the
# remote shell, so asserting only on the ssh line would leave the part that
# decides whether a deploy passes or fails entirely untested.
#
# `built` is the commit the deploy claims to have built, `expect` is `pass` or
# `fail`, and `curl_status` lets the /version fetch itself be failed — which is
# a different outcome from a fetch that returns the wrong sha.
assert_identity_check() {
    local name=$1 built=$2 expect=$3 served_body=$4 curl_status=$5 serv=$6 cli=$7 config=$8
    CASES=$((CASES + 1))
    local remote stub local_cmd status
    remote=$(GL_GIT_SHA="$built" DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$serv" "$cli" "$config" \
        | grep -F '/version' \
        | sed 's|^ssh -p [0-9]* [^ ]* ||')
    if [[ -z "$remote" ]]; then
        echo "FAIL — $name (no /version check found in the dry run)"
        FAILURES=$((FAILURES + 1))
        return
    fi

    stub=$(mktemp -d)
    mkdir -p "$stub/bin"
    # Stands in for the installed gl-serv, printing the one line of the
    # --check-config summary the check reads.
    cat >"$stub/bin/gl-serv" <<'STUB'
#!/bin/sh
echo "  bind_address  127.0.0.1:3000"
echo "  api_address   127.0.0.1:3000"
STUB
    cat >"$stub/curl" <<STUB
#!/bin/sh
printf '%s' '$served_body'
exit $curl_status
STUB
    chmod +x "$stub/bin/gl-serv" "$stub/curl"

    # The installed binary is named by absolute path, which no PATH entry can
    # stand in for, so it is rewritten to the stub.
    local_cmd=$(printf '%s' "$remote" | sed "s|/opt/goopy-life/bin/gl-serv|$stub/bin/gl-serv|")
    PATH="$stub:$PATH" sh -c "$local_cmd" >/dev/null 2>&1
    status=$?
    /bin/rm -rf "$stub"

    local ok=0
    if [[ "$expect" == "pass" && "$status" -eq 0 ]]; then
        ok=1
    fi
    if [[ "$expect" == "fail" && "$status" -ne 0 ]]; then
        ok=1
    fi

    if [[ "$ok" -eq 1 ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        echo "       expected: $expect"
        echo "       exit status: $status"
        echo "       built:       $built"
        echo "       served body: $served_body"
        FAILURES=$((FAILURES + 1))
    fi
}

# Asserts that outside dry-run a non-existent input file aborts before any
# command runs. Takes the binary and config paths so either can be the missing
# one.
assert_missing_file_rejected() {
    local name=$1 serv=$2 cli=$3 config=$4
    CASES=$((CASES + 1))
    if DRY_RUN=0 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$serv" "$cli" "$config" >/dev/null 2>&1; then
        echo "FAIL — $name (expected non-zero exit, got 0)"
        FAILURES=$((FAILURES + 1))
    else
        echo "ok   — $name"
    fi
}

SERV=target/x86_64-unknown-linux-musl/release/gl-serv
CLI=target/x86_64-unknown-linux-musl/release/gl-cli
CFG=deploy/config/dev.toml
REMOTE_CFG=/opt/goopy-life/config.toml
SERV_DEST=/opt/goopy-life/bin/gl-serv
CLI_DEST=/opt/goopy-life/bin/gl-cli

echo "== push-binary.sh =="

# Port 22 is implied when the caller omits it, so deploy.sh's four-arg form works.
assert_emits push_binary_defaults_to_ssh_port_22 \
    "scp -P 22 $SERV goopy@dev.example.com:/tmp/gl-serv" \
    goopy@dev.example.com "$SERV" "$CLI" "$CFG"

# scp takes -P and ssh takes -p; a custom port has to reach both spellings.
assert_emits push_binary_honours_custom_ssh_port_for_scp \
    "scp -P 2222 $SERV goopy@dev.example.com:/tmp/gl-serv" \
    goopy@dev.example.com "$SERV" "$CLI" "$CFG" 2222
assert_emits push_binary_honours_custom_ssh_port_for_ssh \
    "ssh -p 2222 goopy@dev.example.com sudo systemctl restart gl-serv" \
    goopy@dev.example.com "$SERV" "$CLI" "$CFG" 2222

# gl-cli is the droplet's maintenance CLI — despawning one instance by hand,
# listing what exists and alloc/dealloc have no route on gl-serv. It ships from
# this deploy so it can never be older than the gl-serv it shares a database
# with.
assert_emits push_binary_uploads_the_cli_binary \
    "scp -P 22 $CLI goopy@dev.example.com:/tmp/gl-cli" \
    goopy@dev.example.com "$SERV" "$CLI" "$CFG"

# The config travels with the binaries rather than being hand-maintained on the
# droplet, which is what stopped it drifting out of sync with the schema.
assert_emits push_binary_ships_the_config_alongside_the_binary \
    "scp -P 22 $CFG goopy@dev.example.com:$REMOTE_CFG.new" \
    goopy@dev.example.com "$SERV" "$CLI" "$CFG"

# The gate runs the uploaded binary against the STAGED config, not the one
# currently installed: the installed file is about to be replaced, so checking
# it would pass on a broken incoming pair and fail on a fine one.
assert_emits push_binary_checks_the_staged_config_with_the_new_binary \
    "ssh -p 22 goopy@dev.example.com chmod +x /tmp/gl-serv && /tmp/gl-serv --check-config --config $REMOTE_CFG.new" \
    goopy@dev.example.com "$SERV" "$CLI" "$CFG"

assert_config_gate_is_ordered push_binary_checks_the_config_before_installing "$SERV" "$CLI" "$CFG"

# The point of checking early: a bad config must cost a failed deploy, not an
# outage. `systemctl is-active` at the end catches the same failure, but only
# once the old binary has already been stopped.
assert_failed_gate_aborts_before_install push_binary_aborts_the_deploy_when_the_config_check_fails

# The install substrings are whitelisted in deploy/sudoers.goopy — any drift
# there (a different mode, path, or argument order) becomes a sudo denial on
# deploy. gl-cli is installed first so a host whose drop-in predates it is
# denied while gl-serv's binary and config are still untouched and serving.
assert_emits push_binary_pins_the_sudoers_install_command \
    "ssh -p 22 goopy@dev.example.com sudo install -m 755 /tmp/gl-cli $CLI_DEST && sudo install -m 755 /tmp/gl-serv $SERV_DEST && chmod 644 $REMOTE_CFG.new && mv $REMOTE_CFG.new $REMOTE_CFG && rm /tmp/gl-serv /tmp/gl-cli" \
    goopy@dev.example.com "$SERV" "$CLI" "$CFG"

# The same pinning, checked against the drop-in itself rather than a literal
# repeated here: the assertion above catches a change to the script, this one
# catches a change to either side that the other did not follow.
assert_sudoers_pins_every_install push_binary_installs_only_commands_sudoers_allows "$SERV" "$CLI" "$CFG"

# A ';' in that chain would report rm's exit status instead of install's, so a
# sudo denial would leave the deploy green while the old binary kept running.
# Either install failing has to stop the chain before the config is swapped —
# a config swapped for a binary that was never replaced is the mismatch
# shipping the config from the repo exists to prevent.
assert_install_failure_propagates push_binary_propagates_a_failed_serv_install \
    "$SERV_DEST" "$SERV" "$CLI" "$CFG"
assert_install_failure_propagates push_binary_propagates_a_failed_cli_install \
    "$CLI_DEST" "$SERV" "$CLI" "$CFG"

# And the whole script has to stop there too: restarting gl-serv after a failed
# install reports a green deploy while the host runs what it ran before.
assert_failed_install_aborts_before_restart push_binary_aborts_the_deploy_when_an_install_fails

# The config is staged beside its destination so the swap is a same-directory
# rename. Staging in /tmp instead would make it a cross-filesystem copy, and
# gl-serv restarts moments later — it must never read a half-written file.
assert_config_swap_is_atomic push_binary_swaps_the_config_atomically "$SERV" "$CLI" "$CFG"

# A deploy that does not verify the restart reports success while the API is down.
assert_emits push_binary_verifies_the_service_is_active_after_restart \
    "ssh -p 22 goopy@dev.example.com sleep 8; systemctl is-active --quiet gl-serv" \
    goopy@dev.example.com "$SERV" "$CLI" "$CFG"

# `is-active` says *something* is running, which stays green through an install
# that did not replace the binary, a restart that raced, or a rollback that
# silently did not take. The identity check closes that gap — but only if it
# runs after the process it is asking about has actually been replaced.
assert_identity_check_is_last push_binary_verifies_identity_after_the_restart "$SERV" "$CLI" "$CFG"

# The commit is compared against the full sha in the /version body, so the check
# needs no JSON parser on the droplet.
assert_emits push_binary_asks_the_host_which_commit_is_serving \
    "ssh -p 22 goopy@dev.example.com api=\$($SERV_DEST --check-config --config $REMOTE_CFG | awk '\$1 == \"api_address\" { print \$2 }'); [ -n \"\$api\" ] || { echo 'push-binary.sh: could not read api_address from the installed config' >&2; exit 1; }; serving=\$(curl -fsS --max-time 10 \"http://\$api/version\") || { echo \"push-binary.sh: GET /version failed on \$api\" >&2; exit 1; }; case \"\$serving\" in *'\"sha_full\":\"$BUILT_SHA\"'*) echo \"push-binary.sh: verified $BUILT_SHA is serving\" ;; *) echo \"push-binary.sh: deployed the wrong commit -- built $BUILT_SHA, /version says: \$serving\" >&2; exit 1 ;; esac" \
    goopy@dev.example.com "$SERV" "$CLI" "$CFG"

# The deploy passes when the host reports the commit this run built.
assert_identity_check push_binary_accepts_the_commit_it_built "$BUILT_SHA" pass \
    "{\"sha\":\"c50c932\",\"sha_full\":\"$BUILT_SHA\",\"built_at\":\"2026-09-23T10:00:00Z\",\"version\":\"0.1.0\"}" 0 \
    "$SERV" "$CLI" "$CFG"

# And fails when it reports a different one — an install that did not take, or
# a restart that raced. This is the whole reason the endpoint is worth building.
assert_identity_check push_binary_fails_when_a_different_commit_is_serving "$BUILT_SHA" fail \
    '{"sha":"3db4f20","sha_full":"3db4f20aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","built_at":"2026-09-01T10:00:00Z","version":"0.1.0"}' 0 \
    "$SERV" "$CLI" "$CFG"

# A build made by neither deploy path reports `unknown`. Reaching a host that
# says so means the binary that is serving was not built by this deploy.
assert_identity_check push_binary_fails_when_the_host_reports_an_unknown_commit "$BUILT_SHA" fail \
    '{"sha":"unknown","sha_full":"unknown","built_at":"unknown","version":"0.1.0"}' 0 \
    "$SERV" "$CLI" "$CFG"

# A prefix match would accept any commit whose id merely starts with the built
# one, so the comparison includes the closing quote.
assert_identity_check push_binary_rejects_a_merely_prefixed_commit "$BUILT_SHA" fail \
    "{\"sha\":\"c50c932\",\"sha_full\":\"${BUILT_SHA}99\",\"version\":\"0.1.0\"}" 0 \
    "$SERV" "$CLI" "$CFG"

# A dirty build stamps `-dirty` on both sides, so it matches. The suffix says
# what is running; it does not forbid deploying it.
assert_identity_check push_binary_accepts_a_dirty_build_that_matches "$BUILT_SHA-dirty" pass \
    "{\"sha\":\"c50c932-dirty\",\"sha_full\":\"$BUILT_SHA-dirty\",\"version\":\"0.1.0\"}" 0 \
    "$SERV" "$CLI" "$CFG"

# ...and a dirty binary must not pass as the clean commit it was made from. That
# is the failure the suffix exists to catch, so the check has to act on it.
assert_identity_check push_binary_rejects_a_dirty_host_against_a_clean_build "$BUILT_SHA" fail \
    "{\"sha_full\":\"$BUILT_SHA-dirty\",\"version\":\"0.1.0\"}" 0 \
    "$SERV" "$CLI" "$CFG"

# An unreachable /version fails the deploy rather than being read as agreement.
# A check that passes when it could not ask is not a check.
assert_identity_check push_binary_fails_when_version_is_unreachable "$BUILT_SHA" fail \
    '' 7 \
    "$SERV" "$CLI" "$CFG"

# The commit is required, not defaulted: a default would make the check above
# compare a value against itself and pass on every deploy.
CASES=$((CASES + 1))
if env -u GL_GIT_SHA DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$SERV" "$CLI" "$CFG" >/dev/null 2>&1; then
    echo "FAIL — push_binary_requires_the_built_commit_id (expected non-zero exit, got 0)"
    FAILURES=$((FAILURES + 1))
else
    echo "ok   — push_binary_requires_the_built_commit_id"
fi

# Nor may the sentinel stand in for one: a binary stamped `unknown` reports
# `unknown`, so accepting it would make the check pass against itself.
CASES=$((CASES + 1))
if GL_GIT_SHA=unknown DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com "$SERV" "$CLI" "$CFG" >/dev/null 2>&1; then
    echo "FAIL — push_binary_rejects_the_unknown_sentinel (expected non-zero exit, got 0)"
    FAILURES=$((FAILURES + 1))
else
    echo "ok   — push_binary_rejects_the_unknown_sentinel"
fi

# All four positional arguments are mandatory — a missing one must not
# half-deploy, and must not shift the config into a binary's position.
assert_fails push_binary_requires_a_target goopy@dev.example.com
assert_fails push_binary_requires_a_serv_binary_path
assert_fails push_binary_requires_a_cli_binary_path goopy@dev.example.com "$SERV"
assert_fails push_binary_requires_a_config_path goopy@dev.example.com "$SERV" "$CLI"

# Outside dry-run all three files must exist, so a failed build cannot ship the
# previous artifact (or nothing at all) to the droplet, and a mistyped
# environment cannot overwrite a working config with an empty file.
assert_missing_file_rejected push_binary_rejects_a_missing_serv_binary \
    /nonexistent/gl-serv "$SCRIPT_UNDER_TEST" "$SCRIPT_UNDER_TEST"
assert_missing_file_rejected push_binary_rejects_a_missing_cli_binary \
    "$SCRIPT_UNDER_TEST" /nonexistent/gl-cli "$SCRIPT_UNDER_TEST"
assert_missing_file_rejected push_binary_rejects_a_missing_config \
    "$SCRIPT_UNDER_TEST" "$SCRIPT_UNDER_TEST" /nonexistent/config.toml

echo
if [[ "$FAILURES" -eq 0 ]]; then
    echo "$CASES passed, 0 failed"
else
    echo "$((CASES - FAILURES)) passed, $FAILURES failed"
    exit 1
fi
