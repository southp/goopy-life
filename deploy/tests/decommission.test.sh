#!/usr/bin/env bash
# Tests for deploy/decommission.sh and its host half, decommission.remote.sh.
#
# Run: ./deploy/tests/decommission.test.sh
#
# The host half runs for real against a scratch directory standing in for the
# host's filesystem (its ROOT), with stub hostname, nginx, systemctl, zfs,
# zpool, id, getent, userdel, pgrep and ss whose answers each case chooses.
# No host, no root, no network required.
set -uo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LOCAL_HALF="$DEPLOY_DIR/decommission.sh"
REMOTE_HALF="$DEPLOY_DIR/decommission.remote.sh"
FAILURES=0
CASES=0

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

# Reads `NAME=value` from decommission.sh, the single place the host half's
# non-artifact paths are set.
constant() {
    sed -n "s|^$1=\([^ ]*\).*|\1|p" "$LOCAL_HALF" | sed "s|\$APP_DIR|$(sed -n 's|^APP_DIR=\([^ ]*\).*|\1|p' "$LOCAL_HALF")|"
}

# Writes an executable stub at <root>/bin/<name> with the given sh body.
stub() {
    local root=$1 name=$2 body=$3
    printf '#!/bin/sh\n%s\n' "$body" >"$root/bin/$name"
    chmod +x "$root/bin/$name"
}

# A scratch host with Goopy fully on it, beside things that are not Goopy's
# and must survive: another unit, site, conf.d file and sudoers drop-in.
#
# The stubs' state lives in <root>/state: `hostname`, the accounts (one file
# each, holding its home), the pool (`pool` present = it exists, `datasets` its
# `zfs list`), processes still running per account (`running/<user>`), what
# listens on gl-serv's port (`listening`), and when nginx -t fails
# (`nginx-broken` always, `nginx-needs-cache` while the cache zone is gone).
# Every systemctl, zfs, zpool and userdel call is appended to state/calls.
make_host() {
    local root f
    root=$(mktemp -d)
    mkdir -p "$root/bin" "$root/staging" "$root/state/accounts" "$root/state/running" \
        "$root/etc/systemd/system/gl-serv.service.d" "$root/etc/sudoers.d" \
        "$root/etc/nginx/conf.d" "$root/etc/nginx/sites-available" "$root/etc/nginx/sites-enabled" \
        "$root/var/cache/nginx/goopy_alive/0" "$root/var/lib" \
        "$root/opt/goopy-life/bin" "$root/opt/goopy-node/22.23.4/bin" "$root/home/goopy/.ssh"

    printf 'unit\n' >"$root/etc/systemd/system/gl-serv.service"
    printf 'drop-in\n' >"$root/etc/systemd/system/gl-serv.service.d/deploy.conf"
    printf 'other unit\n' >"$root/etc/systemd/system/other.service"
    for f in goopy-tasty-lucky-clover goopy-brave-quiet-otter; do
        printf 'instance unit\n' >"$root/etc/systemd/system/$f.service"
    done
    for f in gl-serv-api goopy-tasty-lucky-clover goopy-brave-quiet-otter other-site; do
        printf 'site %s\n' "$f" >"$root/etc/nginx/sites-available/$f"
        ln -s "$root/etc/nginx/sites-available/$f" "$root/etc/nginx/sites-enabled/$f"
    done
    printf 'cache zone\n' >"$root/etc/nginx/conf.d/goopy-cache.conf"
    printf 'other conf\n' >"$root/etc/nginx/conf.d/other.conf"
    printf 'deploy rules\n' >"$root/etc/sudoers.d/goopy"
    printf 'admin rule\n' >"$root/etc/sudoers.d/90-bootstrap-southp"
    printf 'cached\n' >"$root/var/cache/nginx/goopy_alive/0/x"
    printf 'binary\n' >"$root/opt/goopy-life/bin/gl-serv"
    printf 'db\n' >"$root/opt/goopy-life/goopy.db"
    printf 'wal\n' >"$root/opt/goopy-life/goopy.db-wal"
    printf 'node\n' >"$root/opt/goopy-node/22.23.4/bin/node"
    printf 'pool file\n' >"$root/var/lib/zfs-ghost.img"
    printf 'key\n' >"$root/home/goopy/.ssh/authorized_keys"

    printf 'spdev\n' >"$root/state/hostname"
    printf '/home/goopy\n' >"$root/state/accounts/goopy"
    printf '/nonexistent\n' >"$root/state/accounts/goopy-ghost"
    : >"$root/state/pool"
    printf 'zpool_ghost\nzpool_ghost/tasty-lucky-clover\nzpool_ghost/brave-quiet-otter\n' >"$root/state/datasets"
    : >"$root/state/listening"
    : >"$root/state/calls"

    local s="$root/state"
    stub "$root" hostname "cat '$s/hostname'"
    stub "$root" nginx "
if [ -e '$s/nginx-broken' ]; then exit 1; fi
if [ -e '$s/nginx-needs-cache' ] && [ ! -e '$root/etc/nginx/conf.d/goopy-cache.conf' ]; then exit 1; fi
exit 0"
    stub "$root" systemctl "
echo \"systemctl \$*\" >>'$s/calls'
if [ \"\$1\" = is-active ]; then
    if [ -e '$s/gl-serv-active' ]; then exit 0; fi
    exit 3
fi
exit 0"
    stub "$root" zfs "
echo \"zfs \$*\" >>'$s/calls'
case \"\$1\" in
    list) cat '$s/datasets' ;;
    destroy) grep -vx \"\$3\" '$s/datasets' >'$s/datasets.new'; mv '$s/datasets.new' '$s/datasets' ;;
esac"
    stub "$root" zpool "
echo \"zpool \$*\" >>'$s/calls'
case \"\$1\" in
    list)
        if [ ! -e '$s/pool' ]; then exit 1; fi
        if [ \"\$3\" = -v ]; then printf 'zpool_ghost\t7.5G\n\t/var/lib/zfs-ghost.img\t7.5G\n'; fi
        ;;
    destroy) rm -f '$s/pool' ;;
esac"
    stub "$root" id "if [ -e '$s/accounts/'\"\$2\" ]; then echo 1000; else exit 1; fi"
    stub "$root" getent "printf '%s:x:1000:1000::%s:/bin/sh\n' \"\$2\" \"\$(cat '$s/accounts/'\"\$2\")\""
    stub "$root" userdel "echo \"userdel \$*\" >>'$s/calls'; rm -f '$s/accounts/'\"\$1\""
    stub "$root" pgrep "if [ -e '$s/running/'\"\$2\" ]; then echo 4242; else exit 1; fi"
    stub "$root" ss "cat '$s/listening'"
    printf '%s' "$root"
}

# Runs the host half against `root`, leaving its exit status in STATUS and its
# output in OUT. $2 is PURGE, $3 the name to confirm.
decommission() {
    local root=$1 purge=${2:-0} confirm=${3:-spdev}
    OUT=$(PATH="$root/bin:$PATH" ROOT="$root" CONFIRM="$confirm" PURGE="$purge" \
        UNIT_HOST="$UNIT_HOST" DROPIN_DIR="$DROPIN_DIR" SITE_HOST="$SITE_HOST" SITE_ENABLED="$SITE_ENABLED" \
        LEGACY_SITE_HOST="$LEGACY_SITE_HOST" LEGACY_SITE_ENABLED="$LEGACY_SITE_ENABLED" \
        SUDOERS_HOST="$SUDOERS_HOST" CACHE_CONF_HOST="$CACHE_CONF_HOST" \
        CACHE_DIR="$(constant CACHE_DIR)" APP_DIR="$(constant APP_DIR)" NODE_DIR="$(constant NODE_DIR)" \
        REGISTRY="$(constant REGISTRY)" POOL="$(constant POOL)" DEPLOY_USER="$(constant DEPLOY_USER)" \
        GHOST_USER="$(constant GHOST_USER)" GL_SERV_PORT="$(constant GL_SERV_PORT)" \
        sh "$REMOTE_HALF" "$root/staging" 2>&1)
    STATUS=$?
}

calls() {
    grep -c -- "$2" "$1/state/calls" 2>/dev/null || true
}

# Every path that is not Goopy's and must outlive any run, in one check.
others_survive() {
    local root=$1
    [[ -e "$root/etc/systemd/system/other.service" \
        && -e "$root/etc/nginx/sites-available/other-site" \
        && -L "$root/etc/nginx/sites-enabled/other-site" \
        && -e "$root/etc/nginx/conf.d/other.conf" \
        && -e "$root/etc/sudoers.d/90-bootstrap-southp" ]]
}

# Nothing of Goopy's was touched: what a refused run must leave.
untouched() {
    local root=$1
    [[ -e "$root/etc/sudoers.d/goopy" && -e "$root/etc/systemd/system/gl-serv.service" \
        && -e "$root/etc/nginx/sites-available/gl-serv-api" \
        && -e "$root/etc/nginx/conf.d/goopy-cache.conf" \
        && ! -s "$root/state/calls" ]]
}

echo "== decommission.remote.sh =="

# The name check comes before anything else: an ssh alias pointed at the wrong
# box must not cost that box anything.
root=$(make_host)
decommission "$root" 0 spdev-old
if [[ "$STATUS" -ne 0 ]] && untouched "$root" && [[ "$OUT" == *"not 'spdev-old'"* ]]; then
    pass decommission_refuses_a_host_whose_name_does_not_match
else
    fail decommission_refuses_a_host_whose_name_does_not_match "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# Whether removing Goopy's nginx files is safe is judged by `nginx -t` after;
# a configuration that fails already would make that judgement meaningless.
root=$(make_host)
: >"$root/state/nginx-broken"
decommission "$root"
if [[ "$STATUS" -ne 0 ]] && untouched "$root"; then
    pass decommission_refuses_a_host_whose_nginx_already_fails
else
    fail decommission_refuses_a_host_whose_nginx_already_fails "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# The everyday run: every trace of the service and its instances goes, the
# pool, install and accounts stay, and nothing that is not Goopy's is touched.
root=$(make_host)
decommission "$root"
moved_db=$(find "$root/opt/goopy-life" -name 'goopy.db.decommissioned-*' ! -name '*-wal' | wc -l | tr -d ' ')
moved_wal=$(find "$root/opt/goopy-life" -name 'goopy.db.decommissioned-*-wal' | wc -l | tr -d ' ')
leftovers=$(find "$root/etc" -name 'goopy-*' -o -name 'gl-serv*' -o -path '*/sudoers.d/goopy' | sed "s|^$root||")
if [[ "$STATUS" -eq 0 && -z "$leftovers" ]] && others_survive "$root" \
    && [[ "$(cat "$root/state/datasets")" == zpool_ghost && -e "$root/state/pool" \
    && ! -e "$root/var/cache/nginx/goopy_alive" \
    && ! -e "$root/opt/goopy-life/goopy.db" && "$moved_db" -eq 1 && "$moved_wal" -eq 1 \
    && -e "$root/opt/goopy-life/bin/gl-serv" && -e "$root/opt/goopy-node" \
    && -e "$root/state/accounts/goopy" && -e "$root/state/accounts/goopy-ghost" \
    && "$(calls "$root" 'disable --now gl-serv$')" -eq 1 \
    && "$(calls "$root" 'disable --now goopy-')" -eq 2 \
    && "$(calls "$root" 'reload nginx')" -eq 1 \
    && "$(calls "$root" 'zpool destroy')" -eq 0 && "$(calls "$root" userdel)" -eq 0 ]]; then
    pass decommission_removes_the_service_and_its_instances_and_keeps_the_rest
else
    fail decommission_removes_the_service_and_its_instances_and_keeps_the_rest "exit $STATUS" \
        "left in /etc: ${leftovers:-none}" "datasets: $(tr '\n' ' ' <"$root/state/datasets")" \
        "db moved: $moved_db, wal moved: $moved_wal" "$OUT"
fi

# Run again on the same host, as after a run that stopped partway: nothing is
# left to stop, and nginx has no reason to reload.
: >"$root/state/calls"
decommission "$root"
if [[ "$STATUS" -eq 0 && "$(calls "$root" 'disable')" -eq 0 && "$(calls "$root" 'reload nginx')" -eq 0 \
    && "$(calls "$root" 'zfs destroy')" -eq 0 ]] && others_survive "$root"; then
    pass decommission_is_idempotent
else
    fail decommission_is_idempotent "exit $STATUS" "calls: $(tr '\n' ';' <"$root/state/calls")" "$OUT"
fi
rm -rf "$root"

# Something else on the host turns out to need a file being removed (here, the
# cache zone): every nginx file goes back exactly as it was, links included,
# and nginx is never reloaded into the broken state.
root=$(make_host)
: >"$root/state/nginx-needs-cache"
decommission "$root"
if [[ "$STATUS" -ne 0 && -e "$root/etc/nginx/conf.d/goopy-cache.conf" \
    && "$(readlink "$root/etc/nginx/sites-enabled/gl-serv-api")" == "$root/etc/nginx/sites-available/gl-serv-api" \
    && "$(readlink "$root/etc/nginx/sites-enabled/goopy-tasty-lucky-clover")" == "$root/etc/nginx/sites-available/goopy-tasty-lucky-clover" \
    && -e "$root/etc/nginx/sites-available/goopy-brave-quiet-otter" \
    && "$(calls "$root" 'reload nginx')" -eq 0 ]]; then
    pass decommission_puts_every_nginx_file_back_when_nginx_rejects_the_result
else
    fail decommission_puts_every_nginx_file_back_when_nginx_rejects_the_result "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# A host set up before #139 still serves the api from its hand-installed name.
root=$(make_host)
rm "$root/etc/nginx/sites-enabled/gl-serv-api" "$root/etc/nginx/sites-available/gl-serv-api"
printf 'legacy\n' >"$root/etc/nginx/sites-available/api.goopy.life"
ln -s "$root/etc/nginx/sites-available/api.goopy.life" "$root/etc/nginx/sites-enabled/api.goopy.life"
decommission "$root"
if [[ "$STATUS" -eq 0 && ! -e "$root/etc/nginx/sites-available/api.goopy.life" \
    && ! -L "$root/etc/nginx/sites-enabled/api.goopy.life" ]]; then
    pass decommission_removes_the_pre_139_api_site
else
    fail decommission_removes_the_pre_139_api_site "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# --purge: the pool, the file it lives in, both install trees and both accounts
# go. goopy's home is /home's and goes with it; goopy-ghost's is /nonexistent,
# which is never anything to remove.
root=$(make_host)
mkdir -p "$root/nonexistent"
decommission "$root" 1
if [[ "$STATUS" -eq 0 && ! -e "$root/state/pool" && ! -e "$root/var/lib/zfs-ghost.img" \
    && ! -e "$root/opt/goopy-life" && ! -e "$root/opt/goopy-node" \
    && ! -e "$root/state/accounts/goopy" && ! -e "$root/state/accounts/goopy-ghost" \
    && ! -e "$root/home/goopy" && -d "$root/nonexistent" ]] && others_survive "$root"; then
    pass decommission_purge_removes_the_pool_the_install_and_the_accounts
else
    fail decommission_purge_removes_the_pool_the_install_and_the_accounts "exit $STATUS" "$OUT"
fi

# Purging an already-purged host finds everything absent.
decommission "$root" 1
if [[ "$STATUS" -eq 0 ]]; then
    pass decommission_purge_is_idempotent
else
    fail decommission_purge_is_idempotent "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# A process that outlived its unit would make userdel and zpool destroy fail
# partway; it is asked about before either runs.
root=$(make_host)
: >"$root/state/running/goopy-ghost"
decommission "$root" 1
if [[ "$STATUS" -ne 0 && -e "$root/state/pool" && -e "$root/opt/goopy-life" \
    && -e "$root/state/accounts/goopy" && "$OUT" == *"running as goopy-ghost"* ]]; then
    pass decommission_purge_stops_while_goopy_processes_still_run
else
    fail decommission_purge_stops_while_goopy_processes_still_run "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# The final check is on what runs, not on the files just removed: a listener on
# gl-serv's port means the host is not off Goopy, whatever was deleted.
root=$(make_host)
printf 'LISTEN 0 4096 127.0.0.1:3000 0.0.0.0:*\n' >"$root/state/listening"
decommission "$root"
if [[ "$STATUS" -ne 0 && "$OUT" == *"listens on port 3000"* ]]; then
    pass decommission_fails_while_something_still_listens_on_the_port
else
    fail decommission_fails_while_something_still_listens_on_the_port "exit $STATUS" "$OUT"
fi
rm -rf "$root"

echo "== decommission.sh =="

# The deploy account is refused by name, before any connection is made.
out=$(DRY_RUN=1 "$LOCAL_HALF" goopy@dev.example.com --confirm spdev 2>&1)
rc=$?
if [[ "$rc" -ne 0 && "$out" == *"deploy account"* && "$out" != *"ssh "* ]]; then
    pass decommission_refuses_the_deploy_account
else
    fail decommission_refuses_the_deploy_account "exit $rc" "$out"
fi

# --confirm is required: without it the script cannot tell the host it reaches
# is the one meant.
out=$(DRY_RUN=1 "$LOCAL_HALF" southp@dev.example.com 2>&1)
rc=$?
if [[ "$rc" -ne 0 && "$out" == *Usage* ]]; then
    pass decommission_requires_confirm
else
    fail decommission_requires_confirm "exit $rc" "$out"
fi

out=$(DRY_RUN=1 "$LOCAL_HALF" southp@dev.example.com --confirm spdev --force 2>&1)
rc=$?
if [[ "$rc" -ne 0 && "$out" == *"unknown option --force"* ]]; then
    pass decommission_rejects_an_unknown_option
else
    fail decommission_rejects_an_unknown_option "exit $rc" "$out"
fi

# The commands themselves: a private staging dir, the host half uploaded into
# it, one sudo carrying every path, and the staging dir removed whatever the
# outcome.
out=$(DRY_RUN=1 "$LOCAL_HALF" southp@dev.example.com --purge --confirm spdev 2202 2>&1)
rc=$?
expected_scp="scp -P 2202 $REMOTE_HALF southp@dev.example.com:<staging>/decommission.remote.sh"
if [[ "$rc" -eq 0 && "$out" == *"ssh -p 2202 southp@dev.example.com if [ \"\$(id -un)\" = goopy ]"* \
    && "$out" == *"$expected_scp"* \
    && "$out" == *"ssh -t -p 2202 southp@dev.example.com sudo env CONFIRM=spdev PURGE=1 "* \
    && "$out" == *"SUDOERS_HOST=$SUDOERS_HOST"* \
    && "$out" == *"sh <staging>/decommission.remote.sh <staging>; rc=\$?; rm -rf <staging>; exit \$rc"* ]]; then
    pass decommission_dry_run_prints_the_commands
else
    fail decommission_dry_run_prints_the_commands "exit $rc" "$out"
fi

# An ssh alias hides the account, so the name check alone misses it; the host
# is asked which account it logged in as, and nothing is uploaded.
root=$(mktemp -d)
mkdir -p "$root/bin"
stub "$root" ssh 'echo goopy'
stub "$root" scp "echo scp >>'$root/scp-calls'"
out=$(PATH="$root/bin:$PATH" DRY_RUN=0 "$LOCAL_HALF" spdev-goopy --confirm spdev 2>&1)
rc=$?
if [[ "$rc" -ne 0 && "$out" == *"logs in as goopy"* && ! -e "$root/scp-calls" ]]; then
    pass decommission_refuses_an_alias_for_the_deploy_account
else
    fail decommission_refuses_an_alias_for_the_deploy_account "exit $rc" "$out"
fi
rm -rf "$root"

echo "== decommission.sh constants =="

# The host state decommission.sh names is set elsewhere first; each is read
# from where it comes from, so moving one there fails here instead of leaving
# it behind on a host.
bootstrap() {
    sed -n "s|^$1=\(.*\)|\1|p" "$DEPLOY_DIR/host-bootstrap.sh"
}
prod_value() {
    sed -n "s|^$1 = \"\(.*\)\"|\1|p" "$DEPLOY_DIR/config/prod.toml"
}
checks=(
    "APP_DIR:$(bootstrap APP_DIR)"
    "POOL:$(bootstrap POOL)"
    "POOL:$(prod_value pool)"
    "DEPLOY_USER:$(bootstrap DEPLOY_USER)"
    "GHOST_USER:$(bootstrap GHOST_USER)"
    "GL_SERV_PORT:$(bootstrap GL_SERV_PORT)"
    "GL_SERV_PORT:$(prod_value bind_address | sed 's/.*://')"
    "REGISTRY:$(prod_value path)"
    "NODE_DIR:$(sed -n 's|^NODE_DIR=\(.*\)|\1|p' "$DEPLOY_DIR/ghost-install.remote.sh")"
    "CACHE_DIR:$(sed -n 's|^proxy_cache_path \([^ ]*\).*|\1|p' "$DEPLOY_DIR/nginx/goopy-cache.conf")"
)
mismatches=()
for check in "${checks[@]}"; do
    name=${check%%:*}
    want=${check#*:}
    got=$(constant "$name")
    if [[ -z "$want" || "$got" != "$want" ]]; then
        mismatches+=("$name: decommission.sh has '$got', its source has '$want'")
    fi
done
if [[ ${#mismatches[@]} -eq 0 ]]; then
    pass decommission_constants_match_their_sources
else
    fail decommission_constants_match_their_sources "${mismatches[@]}"
fi

echo
echo "$CASES cases, $FAILURES failed"
if [[ "$FAILURES" -ne 0 ]]; then
    exit 1
fi
