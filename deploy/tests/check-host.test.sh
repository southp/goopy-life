#!/usr/bin/env bash
# Tests for deploy/check-host.sh and the drift comparison it shares with
# deploy/push-binary.sh (deploy/host-artifacts.sh).
#
# Run: ./deploy/tests/check-host.test.sh
#
# The comparison is a remote shell command, so the cases here run it locally
# against a scratch directory standing in for the host's filesystem: every
# /etc, /tmp and /opt path is rewritten under it, and `sudo` is a stub. No
# host, no network, no ssh key required.
set -uo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT_UNDER_TEST="$DEPLOY_DIR/check-host.sh"
FAILURES=0
CASES=0

# shellcheck source=../host-artifacts.sh
source "$DEPLOY_DIR/host-artifacts.sh"
resolve_host_artifacts "$DEPLOY_DIR/config/dev.toml"

# A scratch host on which every tracked artifact matches the repo: staged and
# installed copies identical, the site linked, the config in place. Each case
# then breaks exactly one thing.
make_matching_host() {
    local root
    root=$(mktemp -d)
    mkdir -p "$root/bin" "$root/tmp" "$root/etc/sudoers.d" "$root$DROPIN_DIR" \
        "$root/etc/nginx/sites-available" "$root/etc/nginx/sites-enabled" "$root/opt/goopy-life"
    local pair
    for pair in "unit $UNIT_STAGED $UNIT_HOST" "dropin $DROPIN_STAGED $DROPIN_HOST" \
        "site $SITE_STAGED $SITE_HOST" "sudoers $SUDOERS_STAGED $SUDOERS_HOST" \
        "config /tmp/config.toml /opt/goopy-life/config.toml"; do
        set -- $pair
        printf '%s from the repo\n' "$1" >"$root$2"
        printf '%s from the repo\n' "$1" >"$root$3"
    done
    ln -s "$root$SITE_HOST" "$root$SITE_ENABLED"
    # Runs its command, as the pinned rule would allow.
    # -n is sudo's own option (never prompt); the command follows it.
    printf '#!/bin/sh\nif [ "$1" = -n ]; then\n    shift\nfi\nexec "$@"\n' >"$root/bin/sudo"
    chmod +x "$root/bin/sudo"
    printf '%s' "$root"
}

# Runs the comparison in `mode` against the scratch host and asserts its exit
# status. `status` is 0 (host accepted) or 1 (drift that stops the caller).
assert_drift_verdict() {
    local name=$1 mode=$2 root=$3 want=$4 shell=${5:-sh}
    CASES=$((CASES + 1))
    local remote status output
    remote="$(drift_check_command "$mode" /tmp/config.toml "${HOST_ARTIFACT_STAGED[@]}"); exit \$rc"
    remote=$(printf '%s' "$remote" | sed -e "s|/etc/|$root/etc/|g" -e "s|/tmp/|$root/tmp/|g" -e "s|/opt/|$root/opt/|g")
    output=$(PATH="$root/bin:$PATH" "$shell" -c "$remote" 2>&1)
    status=$?
    /bin/rm -rf "$root"

    if [[ "$status" -eq "$want" ]]; then
        echo "ok   — $name"
    else
        echo "FAIL — $name"
        echo "       exit status: $status (expected $want)"
        printf '%s\n' "$output" | sed 's/^/         /'
        FAILURES=$((FAILURES + 1))
    fi
}

# Runs check-host.sh in dry-run mode and asserts the output contains a line.
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
        printf '%s\n' "$output" | sed 's/^/         /'
        FAILURES=$((FAILURES + 1))
    fi
}

echo "== drift comparison (host-artifacts.sh) =="

# The baseline every other case breaks one thing of. Without it, a comparison
# that always failed would pass every "drift is caught" case below.
root=$(make_matching_host)
assert_drift_verdict drift_accepts_a_host_that_matches_in_check_mode check "$root" 0
root=$(make_matching_host)
assert_drift_verdict drift_accepts_a_host_that_matches_in_deploy_mode deploy "$root" 0

# The sudoers drop-in is the one artifact no deploy may install, so a
# difference always stops the caller — in a deploy too, before anything changes.
root=$(make_matching_host)
printf 'hand-edited\n' >"$root$SUDOERS_HOST"
assert_drift_verdict drift_stops_a_deploy_when_sudoers_differs deploy "$root" 1

# A drop-in that predates the `cmp` rule denies the comparison itself. That has
# to read as drift, not as a pass: it is exactly the host that needs the new
# drop-in applied before this deploy can install anything.
root=$(make_matching_host)
printf '#!/bin/sh\nexit 1\n' >"$root/bin/sudo"
assert_drift_verdict drift_stops_a_deploy_when_sudo_denies_the_comparison deploy "$root" 1

# The pre-#139 site claims the same server_name and sorts first, so nginx would
# serve it and ignore the one the deploy installs.
root=$(make_matching_host)
touch "$root$LEGACY_SITE_ENABLED"
assert_drift_verdict drift_stops_a_deploy_while_the_legacy_site_is_enabled deploy "$root" 1

# A stray override (what `systemctl edit` leaves behind) changes the unit
# without appearing in either tracked file, and no deploy removes it.
root=$(make_matching_host)
printf '[Service]\nEnvironment=RUST_LOG=trace\n' >"$root$DROPIN_DIR/override.conf"
assert_drift_verdict drift_stops_a_deploy_on_a_stray_unit_override deploy "$root" 1

# The artifacts a deploy installs are reported but do not stop it — it is about
# to replace them — while a check, which installs nothing, calls them drift.
root=$(make_matching_host)
printf 'hand-edited\n' >"$root$UNIT_HOST"
assert_drift_verdict drift_lets_a_deploy_replace_a_changed_unit deploy "$root" 0
root=$(make_matching_host)
printf 'hand-edited\n' >"$root$UNIT_HOST"
assert_drift_verdict drift_reports_a_changed_unit_in_check_mode check "$root" 1

root=$(make_matching_host)
/bin/rm "$root$DROPIN_HOST"
assert_drift_verdict drift_lets_a_deploy_install_a_missing_drop_in deploy "$root" 0
root=$(make_matching_host)
/bin/rm "$root$DROPIN_HOST"
assert_drift_verdict drift_reports_a_missing_drop_in_in_check_mode check "$root" 1

# The site that prompted #139: a host running a different api site from the
# repo's, under a name that suggested otherwise.
root=$(make_matching_host)
printf 'server_name api.southp.dev;\n' >"$root$SITE_HOST"
assert_drift_verdict drift_reports_a_changed_api_site_in_check_mode check "$root" 1

# Present but not enabled serves nothing.
root=$(make_matching_host)
/bin/rm "$root$SITE_ENABLED"
assert_drift_verdict drift_reports_an_unlinked_api_site_in_check_mode check "$root" 1

root=$(make_matching_host)
printf 'hand-edited\n' >"$root/opt/goopy-life/config.toml"
assert_drift_verdict drift_reports_a_changed_config_in_check_mode check "$root" 1

# The comparison runs in the deploy account's login shell, whatever that is.
# Under zsh, `status` is a read-only special and a glob matching nothing aborts
# the command, so both would turn a clean host into a failed deploy. Run there
# for real when zsh is installed: a clean host, and one with no drop-in
# directory at all, where a bare glob would have nothing to match.
if command -v zsh >/dev/null 2>&1; then
    root=$(make_matching_host)
    assert_drift_verdict drift_accepts_a_matching_host_under_zsh check "$root" 0 zsh
    root=$(make_matching_host)
    rm -r "$root$DROPIN_DIR"
    assert_drift_verdict drift_survives_a_missing_drop_in_dir_under_zsh deploy "$root" 0 zsh
fi

echo
echo "== check-host.sh =="

# Everything it compares is uploaded into its own private directory, and the
# sudoers copy to the one path its own `cmp` rule names.
assert_emits check_host_uploads_the_environments_api_site \
    "scp -P 22 $DEPLOY_DIR/config/dev.api.nginx goopy@dev.example.com:<staging>/gl-serv-api.nginx" \
    goopy@dev.example.com dev
assert_emits check_host_uploads_sudoers_to_its_own_pinned_path \
    "scp -P 22 $DEPLOY_DIR/sudoers.goopy goopy@dev.example.com:$SUDOERS_CHECK_STAGED" \
    goopy@dev.example.com dev
assert_emits check_host_uploads_the_environments_config \
    "scp -P 22 $DEPLOY_DIR/config/prod.toml goopy@dev.example.com:<staging>/config.toml" \
    goopy@dev.example.com prod
assert_emits check_host_honours_a_custom_ssh_port \
    "scp -P 2222 $DEPLOY_DIR/gl-serv.service goopy@dev.example.com:<staging>/gl-serv.service" \
    goopy@dev.example.com dev 2222

# A check can run while a deploy is between its upload and its install — CI
# deploys on every merge. Nothing it uploads or removes may be a path the deploy
# stages at, or the deploy installs this environment's files or loses its own.
CASES=$((CASES + 1))
dry_run=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com prod)
shared=""
for path in "${HOST_ARTIFACT_STAGED[@]}" "$CONFIG_HOST.new"; do
    if grep -Eq -- "(:| )$path( |;|\$)" <<<"$dry_run"; then
        shared+="$path "
    fi
done
if [[ -z "$shared" ]]; then
    echo "ok   — check_host_stays_off_the_deploys_staging_paths"
else
    echo "FAIL — check_host_stays_off_the_deploys_staging_paths"
    echo "       also used by the deploy: $shared"
    FAILURES=$((FAILURES + 1))
fi

# Its sudoers comparison is the only thing it runs under sudo, so it has to be
# a rule the drop-in actually grants, or every check reports drift.
CASES=$((CASES + 1))
dry_run=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com dev)
compare=$(grep -o 'sudo [^;&|>]*' <<<"$dry_run" | sed -e 's/ *2*$//' -e 's/^sudo //' -e 's/^-n //' | sort -u)
# Every rule in the drop-in ends in a comma, so matching `<rule>,` rejects a
# rule that merely starts with the command and permits more.
if [[ -n "$compare" ]] && grep -Fq -- "/usr/bin/$compare," "$DEPLOY_DIR/sudoers.goopy"; then
    echo "ok   — check_host_compares_sudoers_with_a_pinned_rule"
else
    echo "FAIL — check_host_compares_sudoers_with_a_pinned_rule"
    echo "       not in sudoers.goopy: /usr/bin/$compare"
    FAILURES=$((FAILURES + 1))
fi

# Read-only: the deploy account can write only /tmp and /opt/goopy-life on its
# own, so anything else would have to go through sudo — and the one command it
# may run there is the sudoers comparison.
CASES=$((CASES + 1))
dry_run=$(DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com dev)
privileged=$(grep -o 'sudo \(-n \)\?[a-z]*' <<<"$dry_run" | sed 's/-n //' | sort -u)
if [[ "$privileged" == "sudo cmp" ]] && ! grep -q '/opt/goopy-life/config.toml.new' <<<"$dry_run"; then
    echo "ok   — check_host_changes_nothing_on_the_host"
else
    echo "FAIL — check_host_changes_nothing_on_the_host"
    echo "       privileged commands: $(tr '\n' ' ' <<<"$privileged")(expected only: sudo cmp)"
    FAILURES=$((FAILURES + 1))
fi

# An environment with no config is a typo, and is refused before any upload.
CASES=$((CASES + 1))
if DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com staging >/dev/null 2>&1; then
    echo "FAIL — check_host_rejects_an_unknown_environment (expected non-zero exit, got 0)"
    FAILURES=$((FAILURES + 1))
else
    echo "ok   — check_host_rejects_an_unknown_environment"
fi

CASES=$((CASES + 1))
if DRY_RUN=1 "$SCRIPT_UNDER_TEST" goopy@dev.example.com >/dev/null 2>&1; then
    echo "FAIL — check_host_requires_an_environment (expected non-zero exit, got 0)"
    FAILURES=$((FAILURES + 1))
else
    echo "ok   — check_host_requires_an_environment"
fi

echo
if [[ "$FAILURES" -eq 0 ]]; then
    echo "$CASES passed, 0 failed"
else
    echo "$((CASES - FAILURES)) passed, $FAILURES failed"
    exit 1
fi
