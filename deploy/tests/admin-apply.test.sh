#!/usr/bin/env bash
# Tests for deploy/admin-apply.sh and its host half, admin-apply.remote.sh.
#
# Run: ./deploy/tests/admin-apply.test.sh
#
# The host half runs for real against a scratch directory standing in for the
# host's filesystem (its ROOT), with stub visudo, nginx and systemctl
# whose verdicts each case chooses. No droplet, no root, no network required.
set -uo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LOCAL_HALF="$DEPLOY_DIR/admin-apply.sh"
REMOTE_HALF="$DEPLOY_DIR/admin-apply.remote.sh"
FAILURES=0
CASES=0

# The host half is handed its paths from this table by the local half; the cases
# below hand it the same ones, and spell the expected results out literally, so
# a path changed in the table alone fails here rather than on a host.
# shellcheck source=../host-artifacts.sh
source "$DEPLOY_DIR/host-artifacts.sh"

pass() {
    CASES=$((CASES + 1))
    echo "ok   — $1"
}

fail() {
    CASES=$((CASES + 1))
    echo "FAIL — $1"
    shift
    printf '       %s\n' "$@"
    FAILURES=$((FAILURES + 1))
}

# A scratch host that already matches the repo: the drop-in and the cache zone
# in place, gl-serv-api enabled, no legacy site. Staging holds the repo's copies.
# The stubs accept everything until a case says otherwise.
make_host() {
    local root
    root=$(mktemp -d)
    mkdir -p "$root/bin" "$root/staging" "$root/etc/sudoers.d" "$root/etc/nginx/conf.d" \
        "$root/etc/nginx/sites-available" "$root/etc/nginx/sites-enabled"
    printf 'sudoers from the repo\n' >"$root/staging/sudoers.goopy"
    printf 'cache zone from the repo\n' >"$root/staging/goopy-cache.conf"
    printf 'api site from the repo\n' >"$root/staging/gl-serv-api"
    cp "$root/staging/sudoers.goopy" "$root/etc/sudoers.d/goopy"
    cp "$root/staging/goopy-cache.conf" "$root/etc/nginx/conf.d/goopy-cache.conf"
    cp "$root/staging/gl-serv-api" "$root/etc/nginx/sites-available/gl-serv-api"
    ln -s "$root/etc/nginx/sites-available/gl-serv-api" "$root/etc/nginx/sites-enabled/gl-serv-api"
    verdicts "$root" 0 0 0
    printf '#!/bin/sh\necho "$*" >>"%s/reloads"\n' "$root" >"$root/bin/systemctl"
    chmod +x "$root/bin/systemctl"
    printf '%s' "$root"
}

# Sets what the stubs answer: `visudo -cf <file>`, `visudo -c`, and `nginx -t`.
verdicts() {
    local root=$1 file=$2 whole=$3 nginx=$4
    cat >"$root/bin/visudo" <<STUB
#!/bin/sh
case "\$1" in
    -cf) exit $file ;;
    *) exit $whole ;;
esac
STUB
    printf '#!/bin/sh\nexit %s\n' "$nginx" >"$root/bin/nginx"
    chmod +x "$root/bin/visudo" "$root/bin/nginx"
}

# A host set up before #139: the api site enabled as api.goopy.life, and no
# gl-serv-api. Otherwise up to date.
make_legacy_host() {
    local root
    root=$(make_host)
    rm "$root/etc/nginx/sites-enabled/gl-serv-api" "$root/etc/nginx/sites-available/gl-serv-api"
    printf 'legacy site\n' >"$root/etc/nginx/sites-available/api.goopy.life"
    ln -s "$root/etc/nginx/sites-available/api.goopy.life" "$root/etc/nginx/sites-enabled/api.goopy.life"
    printf '%s' "$root"
}

# Runs the host half against `root`, leaving its exit status in STATUS.
apply() {
    local root=$1
    PATH="$root/bin:$PATH" ROOT="$root" \
        SUDOERS_HOST="$SUDOERS_HOST" CACHE_CONF_HOST="$CACHE_CONF_HOST" \
        SITE_HOST="$SITE_HOST" SITE_ENABLED="$SITE_ENABLED" \
        LEGACY_SITE_HOST="$LEGACY_SITE_HOST" LEGACY_SITE_ENABLED="$LEGACY_SITE_ENABLED" \
        sh "$REMOTE_HALF" "$root/staging" >/dev/null 2>&1
    STATUS=$?
}

content() {
    cat "$1" 2>/dev/null || printf '<absent>'
}

reloads() {
    if [[ -s "$1/reloads" ]]; then
        wc -l <"$1/reloads" | tr -d ' '
    else
        echo 0
    fi
}

echo "== admin-apply.remote.sh =="

# An up-to-date host is left exactly as it was: nothing installed, and above
# all nginx not reloaded for no reason.
root=$(make_host)
apply "$root"
if [[ "$STATUS" -eq 0 && "$(reloads "$root")" -eq 0 ]]; then
    pass admin_apply_leaves_an_up_to_date_host_alone
else
    fail admin_apply_leaves_an_up_to_date_host_alone "exit $STATUS, reloads $(reloads "$root")"
fi
rm -rf "$root"

# The everyday case: sudoers.goopy changed in the repo. Staged as goopy.new —
# skipped by sudo's includedir for its dot — and renamed into place, so nothing
# may be left behind under that name.
root=$(make_host)
printf 'sudoers before this change\n' >"$root/etc/sudoers.d/goopy"
apply "$root"
mode=$(stat -f '%Lp' "$root/etc/sudoers.d/goopy" 2>/dev/null || stat -c '%a' "$root/etc/sudoers.d/goopy")
if [[ "$STATUS" -eq 0 && "$(content "$root/etc/sudoers.d/goopy")" == "sudoers from the repo" && "$mode" == 440 \
    && ! -e "$root/etc/sudoers.d/goopy.new" ]]; then
    pass admin_apply_installs_a_changed_sudoers_drop_in
else
    fail admin_apply_installs_a_changed_sudoers_drop_in "exit $STATUS, mode $mode" \
        "drop-in: $(content "$root/etc/sudoers.d/goopy")"
fi
rm -rf "$root"

# A drop-in that does not parse must never reach /etc/sudoers.d: one broken
# file there disables sudo for every account on the host.
root=$(make_host)
printf 'sudoers before this change\n' >"$root/etc/sudoers.d/goopy"
verdicts "$root" 1 0 0
apply "$root"
if [[ "$STATUS" -ne 0 && "$(content "$root/etc/sudoers.d/goopy")" == "sudoers before this change" ]]; then
    pass admin_apply_refuses_a_sudoers_drop_in_that_does_not_parse
else
    fail admin_apply_refuses_a_sudoers_drop_in_that_does_not_parse "exit $STATUS" \
        "drop-in: $(content "$root/etc/sudoers.d/goopy")"
fi
rm -rf "$root"

# And if sudo's whole-configuration check rejects it after the rename, the
# previous drop-in comes back.
root=$(make_host)
printf 'sudoers before this change\n' >"$root/etc/sudoers.d/goopy"
verdicts "$root" 0 1 0
apply "$root"
if [[ "$STATUS" -ne 0 && "$(content "$root/etc/sudoers.d/goopy")" == "sudoers before this change" ]]; then
    pass admin_apply_restores_the_previous_sudoers_when_sudo_rejects_the_whole
else
    fail admin_apply_restores_the_previous_sudoers_when_sudo_rejects_the_whole "exit $STATUS" \
        "drop-in: $(content "$root/etc/sudoers.d/goopy")"
fi
rm -rf "$root"

# A new host: no drop-in yet, and none left behind when sudo rejects it.
root=$(make_host)
rm "$root/etc/sudoers.d/goopy"
verdicts "$root" 0 1 0
apply "$root"
if [[ "$STATUS" -ne 0 && ! -e "$root/etc/sudoers.d/goopy" ]]; then
    pass admin_apply_removes_a_first_sudoers_drop_in_sudo_rejects
else
    fail admin_apply_removes_a_first_sudoers_drop_in_sudo_rejects "exit $STATUS" \
        "drop-in: $(content "$root/etc/sudoers.d/goopy")"
fi
rm -rf "$root"

# A changed cache zone is installed, the cache directory created, and nginx
# reloaded to pick it up.
root=$(make_host)
printf 'cache zone before this change\n' >"$root/etc/nginx/conf.d/goopy-cache.conf"
apply "$root"
if [[ "$STATUS" -eq 0 && "$(content "$root/etc/nginx/conf.d/goopy-cache.conf")" == "cache zone from the repo" \
    && -d "$root/var/cache/nginx" && "$(reloads "$root")" -eq 1 ]]; then
    pass admin_apply_installs_a_changed_cache_zone_and_reloads_nginx
else
    fail admin_apply_installs_a_changed_cache_zone_and_reloads_nginx "exit $STATUS, reloads $(reloads "$root")" \
        "zone: $(content "$root/etc/nginx/conf.d/goopy-cache.conf")"
fi
rm -rf "$root"

# One nginx rejects is taken back out before any reload: every instance site
# references the zone, so leaving it would fail `nginx -t` for all of them.
root=$(make_host)
printf 'cache zone before this change\n' >"$root/etc/nginx/conf.d/goopy-cache.conf"
verdicts "$root" 0 0 1
apply "$root"
if [[ "$STATUS" -ne 0 && "$(content "$root/etc/nginx/conf.d/goopy-cache.conf")" == "cache zone before this change" \
    && "$(reloads "$root")" -eq 0 ]]; then
    pass admin_apply_restores_the_previous_cache_zone_when_nginx_rejects_it
else
    fail admin_apply_restores_the_previous_cache_zone_when_nginx_rejects_it "exit $STATUS, reloads $(reloads "$root")" \
        "zone: $(content "$root/etc/nginx/conf.d/goopy-cache.conf")"
fi
rm -rf "$root"

# The pre-#139 host: api.goopy.life enabled, no gl-serv-api. Swapped in one
# reload, and the old site file gone so nothing can re-enable it by accident.
root=$(make_legacy_host)
apply "$root"
if [[ "$STATUS" -eq 0 && -L "$root/etc/nginx/sites-enabled/gl-serv-api" \
    && "$(content "$root/etc/nginx/sites-enabled/gl-serv-api")" == "api site from the repo" \
    && ! -e "$root/etc/nginx/sites-enabled/api.goopy.life" && ! -e "$root/etc/nginx/sites-available/api.goopy.life" \
    && "$(reloads "$root")" -eq 1 ]]; then
    pass admin_apply_migrates_the_legacy_api_site_in_one_reload
else
    fail admin_apply_migrates_the_legacy_api_site_in_one_reload "exit $STATUS, reloads $(reloads "$root")" \
        "gl-serv-api: $(content "$root/etc/nginx/sites-enabled/gl-serv-api")" \
        "legacy enabled: $([[ -e "$root/etc/nginx/sites-enabled/api.goopy.life" ]] && echo yes || echo no)"
fi
rm -rf "$root"

# If nginx rejects the new site, the old one is enabled again and the new one
# unlinked, so the API keeps being served exactly as before.
root=$(make_legacy_host)
verdicts "$root" 0 0 1
apply "$root"
if [[ "$STATUS" -ne 0 && "$(content "$root/etc/nginx/sites-enabled/api.goopy.life")" == "legacy site" \
    && ! -e "$root/etc/nginx/sites-enabled/gl-serv-api" && "$(reloads "$root")" -eq 0 ]]; then
    pass admin_apply_keeps_the_legacy_api_site_when_nginx_rejects_the_new_one
else
    fail admin_apply_keeps_the_legacy_api_site_when_nginx_rejects_the_new_one "exit $STATUS, reloads $(reloads "$root")"
fi
rm -rf "$root"

# The legacy entry was placed by hand, so it need not be the symlink it usually
# is. A plain file, rejected alongside the new site, must come back as that same
# file — not as a guessed symlink to a target that may not exist.
root=$(make_host)
rm "$root/etc/nginx/sites-enabled/gl-serv-api" "$root/etc/nginx/sites-available/gl-serv-api"
printf 'hand-placed site\n' >"$root/etc/nginx/sites-enabled/api.goopy.life"
verdicts "$root" 0 0 1
apply "$root"
if [[ "$STATUS" -ne 0 && -f "$root/etc/nginx/sites-enabled/api.goopy.life" && ! -L "$root/etc/nginx/sites-enabled/api.goopy.life" \
    && "$(content "$root/etc/nginx/sites-enabled/api.goopy.life")" == "hand-placed site" \
    && ! -e "$root/etc/nginx/sites-enabled/gl-serv-api" ]]; then
    pass admin_apply_restores_a_legacy_site_that_was_a_plain_file
else
    fail admin_apply_restores_a_legacy_site_that_was_a_plain_file "exit $STATUS" \
        "legacy: $(ls -l "$root/etc/nginx/sites-enabled/api.goopy.life" 2>&1)"
fi
rm -rf "$root"

echo
echo "== admin-apply.sh =="

# One ssh -t, one sudo: one password prompt for the whole run, the host half
# handed every path from the table, and the staging directory removed whatever
# the outcome.
dry_run=$(DRY_RUN=1 "$LOCAL_HALF" admin@dev.example.com dev 2>&1)
expected="ssh -t -p 22 admin@dev.example.com sudo env SUDOERS_HOST=$SUDOERS_HOST CACHE_CONF_HOST=$CACHE_CONF_HOST"
expected+=" SITE_HOST=$SITE_HOST SITE_ENABLED=$SITE_ENABLED LEGACY_SITE_HOST=$LEGACY_SITE_HOST LEGACY_SITE_ENABLED=$LEGACY_SITE_ENABLED"
expected+=' sh <staging>/admin-apply.remote.sh <staging>; rc=$?; rm -rf <staging>; exit $rc'
if grep -Fqx -- "$expected" <<<"$dry_run" \
    && [[ "$(grep -c ' sudo ' <<<"$dry_run")" -eq 1 ]]; then
    pass admin_apply_asks_for_the_password_once
else
    fail admin_apply_asks_for_the_password_once "$dry_run"
fi

# The ssh line runs in the admin's login shell, which may be zsh — where
# `status` is read-only, so a successful run used to report failure and leave
# the staging directory behind. Run the line for real under zsh, with a stub
# sudo that succeeds, and require a clean exit and the directory gone.
if command -v zsh >/dev/null 2>&1; then
    root=$(mktemp -d)
    mkdir -p "$root/bin" "$root/staging"
    printf '#!/bin/sh\nexit 0\n' >"$root/bin/sudo"
    chmod +x "$root/bin/sudo"
    remote=$(DRY_RUN=1 "$LOCAL_HALF" admin@dev.example.com dev | grep '^ssh -t ' | sed 's|^ssh -t -p [0-9]* [^ ]* ||')
    remote=${remote//<staging>/$root/staging}
    PATH="$root/bin:$PATH" zsh -c "$remote" >/dev/null 2>&1
    STATUS=$?
    if [[ "$STATUS" -eq 0 && ! -e "$root/staging" ]]; then
        pass admin_apply_reports_success_under_a_zsh_login_shell
    else
        fail admin_apply_reports_success_under_a_zsh_login_shell "exit $STATUS" \
            "staging left behind: $([[ -e "$root/staging" ]] && echo yes || echo no)"
    fi
    rm -rf "$root"
fi

# An ssh alias hides the account (`spdev-goopy`), so the name check alone misses
# it. The host is asked instead, on the connection that would make the staging
# directory, and a goopy login stops there: nothing uploaded, nothing left.
root=$(mktemp -d)
mkdir -p "$root/bin"
cat >"$root/bin/ssh" <<STUB
#!/bin/sh
echo "\$*" >>"$root/calls"
echo goopy
STUB
printf '#!/bin/sh\necho "scp $*" >>"%s/calls"\n' "$root" >"$root/bin/scp"
chmod +x "$root/bin/ssh" "$root/bin/scp"
PATH="$root/bin:$PATH" DRY_RUN=0 "$LOCAL_HALF" spdev-goopy dev >/dev/null 2>&1
STATUS=$?
calls=$(wc -l <"$root/calls" | tr -d ' ')
if [[ "$STATUS" -ne 0 && "$calls" -eq 1 ]]; then
    pass admin_apply_refuses_an_alias_for_the_deploy_account
else
    fail admin_apply_refuses_an_alias_for_the_deploy_account "exit $STATUS, remote calls $calls (expected 1)" \
        "$(cat "$root/calls")"
fi
rm -rf "$root"

# Each environment migrates to its own api site, never another's.
dry_run=$(DRY_RUN=1 "$LOCAL_HALF" admin@prod.example.com prod 2>&1)
if grep -Fqx "scp -P 22 $DEPLOY_DIR/config/prod.api.nginx admin@prod.example.com:<staging>/gl-serv-api" <<<"$dry_run"; then
    pass admin_apply_stages_the_environments_api_site
else
    fail admin_apply_stages_the_environments_api_site "$dry_run"
fi

# The deploy account cannot do this, and should not be asked for a password
# it does not have.
if DRY_RUN=1 "$LOCAL_HALF" goopy@dev.example.com dev >/dev/null 2>&1; then
    fail admin_apply_refuses_the_deploy_account "expected non-zero exit, got 0"
else
    pass admin_apply_refuses_the_deploy_account
fi

if DRY_RUN=1 "$LOCAL_HALF" admin@dev.example.com staging >/dev/null 2>&1; then
    fail admin_apply_rejects_an_unknown_environment "expected non-zero exit, got 0"
else
    pass admin_apply_rejects_an_unknown_environment
fi

echo
if [[ "$FAILURES" -eq 0 ]]; then
    echo "$CASES passed, 0 failed"
else
    echo "$((CASES - FAILURES)) passed, $FAILURES failed"
    exit 1
fi
