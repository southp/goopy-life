#!/bin/sh
# The host half of deploy/decommission.sh. Runs as root, from a staging
# directory the local half has just made:
#
#   sh decommission.remote.sh <staging-dir>
#
# Takes Goopy off this host, in the order that keeps each step safe:
#
#   1. gl-serv stops, so nothing spawns or sweeps while the rest goes
#   2. the deploy account's sudo rights go, so no deploy can bring it back
#   3. every instance's unit stops and goes
#   4. every nginx file Goopy put there goes, in one checked reload
#   5. every instance's dataset goes
#   6. gl-serv's own unit and drop-in go
#   7. the registry is moved aside -- or, with PURGE=1, the pool, its file,
#      /opt/goopy-life, /opt/goopy-node and the two service accounts go
#
# and then checks that nothing of it is still running or listening.
#
# Instances are found by name on the host -- `goopy-*` units and sites,
# datasets under the pool -- not read from the registry. That catches orphans
# the registry has lost track of, and it does not depend on the gl-cli and
# config being in working order on a host that is being taken apart.
#
# Idempotent: what is already gone is reported `absent`, so a run that stopped
# partway is finished by running it again.
#
# Never touched: certificates, DNS, the firewall, sshd, swap, sysctl, apt
# packages, any other account or site, and the machine itself.
#
# Inputs, from the local half:
#   CONFIRM   the host's name, which `hostname` must print, or nothing happens
#   PURGE     1 to also remove the pool, the install and the accounts
#   the paths of deploy/host-artifacts.sh (UNIT_HOST, DROPIN_DIR, SITE_HOST,
#   SITE_ENABLED, LEGACY_SITE_HOST, LEGACY_SITE_ENABLED, SUDOERS_HOST,
#   CACHE_CONF_HOST) and of the host state decommission.sh lists (APP_DIR,
#   NODE_DIR, REGISTRY, CACHE_DIR, POOL, DEPLOY_USER, GHOST_USER, GL_SERV_PORT).
#   This runs under plain sh on the host and cannot source that table itself.
#
# ROOT prefixes every absolute path. It is empty on a host; the tests set it to
# a scratch directory, which is the only reason it exists.
set -eu

STAGING=${1:?"usage: decommission.remote.sh <staging-dir>"}
ROOT=${ROOT:-}
CONFIRM=${CONFIRM:?}
PURGE=${PURGE:-0}

UNIT=$ROOT${UNIT_HOST:?}
DROPIN_DIR=$ROOT${DROPIN_DIR:?}
SITE=$ROOT${SITE_HOST:?}
SITE_ENABLED=$ROOT${SITE_ENABLED:?}
LEGACY=$ROOT${LEGACY_SITE_HOST:?}
LEGACY_ENABLED=$ROOT${LEGACY_SITE_ENABLED:?}
SUDOERS=$ROOT${SUDOERS_HOST:?}
CACHE_CONF=$ROOT${CACHE_CONF_HOST:?}
CACHE_DIR=$ROOT${CACHE_DIR:?}
APP_DIR=$ROOT${APP_DIR:?}
NODE_DIR=$ROOT${NODE_DIR:?}
REGISTRY=$ROOT${REGISTRY:?}
POOL=${POOL:?}
DEPLOY_USER=${DEPLOY_USER:?}
GHOST_USER=${GHOST_USER:?}
GL_SERV_PORT=${GL_SERV_PORT:?}

UNIT_DIR=$ROOT/etc/systemd/system
SITES_AVAILABLE=$ROOT/etc/nginx/sites-available
SITES_ENABLED=$ROOT/etc/nginx/sites-enabled

say() {
    printf '%-10s %s\n' "$1" "$2"
}

# The path as it is on the host, for messages: without the tests' ROOT.
shown() {
    printf '%s' "${1#"$ROOT"}"
}

has_account() {
    id -u "$1" >/dev/null 2>&1
}

has_pool() {
    zpool list -H -o name "$POOL" >/dev/null 2>&1
}

# --- 0. Before anything changes ----------------------------------------------
#
# The name check is the last line of defence against a run aimed at the wrong
# host: an ssh alias can point anywhere, and what it points at is only known
# here. Everything below is destructive, so it is the first thing done.
actual=$(hostname)
if [ "$actual" != "$CONFIRM" ]; then
    echo "decommission: this host is '$actual', not '$CONFIRM'; nothing was changed" >&2
    exit 1
fi

# Step 4 decides whether removing Goopy's nginx files is safe by asking
# `nginx -t` afterwards. A configuration that already fails would make that
# answer meaningless, so it is asked first, while nothing has changed.
if ! nginx -t >/dev/null 2>&1; then
    echo "decommission: \`nginx -t\` already fails on this host; fix that first. Nothing was changed" >&2
    exit 1
fi

# --- 1. gl-serv stops ---------------------------------------------------------
#
# First, so that neither a spawn nor the sweeper is halfway through an instance
# while the steps below take instances apart. `stop` waits for it to exit.
if [ -e "$UNIT" ]; then
    systemctl disable --now gl-serv
    say stopped gl-serv
else
    say absent "$(shown "$UNIT")"
fi

# --- 2. The deploy account loses its rights -----------------------------------
#
# Every deploy installs and restarts through these rules, so with them gone no
# deploy can put gl-serv back while this run is taking it apart. Removing a
# drop-in cannot break sudo's parse, unlike installing one.
if [ -e "$SUDOERS" ]; then
    rm -f "$SUDOERS"
    say removed "$(shown "$SUDOERS")"
else
    say absent "$(shown "$SUDOERS")"
fi

# --- 3. Every instance's unit -------------------------------------------------
#
# `disable --now` stops it and removes its wants symlink. Stopped before its
# dataset is destroyed in step 5, which would otherwise be busy.
for f in "$UNIT_DIR"/goopy-*.service; do
    if [ -e "$f" ]; then
        systemctl disable --now "$(basename "$f")"
        rm -f "$f"
        say removed "$(shown "$f")"
    fi
done

# --- 4. Every nginx file Goopy put there, in one reload -----------------------
#
# Each instance's site, the api site (and its pre-#139 name, if a host still
# has it) and the cache zone. They are moved into the staging directory rather
# than deleted, so that if `nginx -t` rejects the configuration without them --
# something else on the host used the cache zone, say -- every one of them goes
# back and nginx is never reloaded into a broken state. -L as well as -e, so a
# link whose target is already gone is still found.
removed=$STAGING/nginx-removed
mkdir -p "$removed"
moved=0
for f in "$SITES_ENABLED"/goopy-* "$SITES_AVAILABLE"/goopy-* \
    "$SITE_ENABLED" "$SITE" "$LEGACY_ENABLED" "$LEGACY" "$CACHE_CONF"; do
    if [ -e "$f" ] || [ -L "$f" ]; then
        mkdir -p "$removed$(dirname "$f")"
        mv "$f" "$removed$f"
        moved=$((moved + 1))
    fi
done

if [ "$moved" -gt 0 ]; then
    if ! nginx -t >/dev/null 2>&1; then
        (cd "$removed" && find . -type f -o -type l) | while read -r f; do
            mv "$removed/${f#./}" "/${f#./}"
        done
        echo "decommission: nginx rejects the configuration without Goopy's files; they are all back (run \`nginx -t\` to see why)" >&2
        exit 1
    fi
    systemctl reload nginx
    (cd "$removed" && find . -type f -o -type l) | while read -r f; do
        say removed "$(shown "/${f#./}")"
    done
    rm -rf "$removed"
else
    say absent "nginx sites and cache zone"
fi

if [ -d "$CACHE_DIR" ]; then
    rm -rf "$CACHE_DIR"
    say removed "$(shown "$CACHE_DIR")"
fi

# --- 5. Every instance's dataset ----------------------------------------------
#
# -d 1: the pool's direct children, one per instance. -r takes any snapshot
# with it. The pool itself stays unless PURGE asks for it.
if has_pool; then
    for ds in $(zfs list -H -o name -d 1 "$POOL"); do
        if [ "$ds" != "$POOL" ]; then
            zfs destroy -r "$ds"
            say removed "dataset $ds"
        fi
    done
else
    say absent "pool $POOL"
fi

# --- 6. gl-serv's unit and drop-in --------------------------------------------
#
# The whole drop-in directory: the deploy's deploy.conf, and any stray override
# check-host.sh would have reported as drift.
if [ -e "$UNIT" ]; then
    rm -f "$UNIT"
    say removed "$(shown "$UNIT")"
fi
if [ -d "$DROPIN_DIR" ]; then
    rm -rf "$DROPIN_DIR"
    say removed "$(shown "$DROPIN_DIR")"
fi
systemctl daemon-reload

# --- 7. The registry, or everything -------------------------------------------
if [ "$PURGE" = 1 ]; then
    # userdel refuses an account with a running process and zpool destroy a
    # busy pool, but only after the steps before them have run. Every process
    # should have stopped with its unit; one that did not is asked about here,
    # before anything else is removed.
    for user in "$DEPLOY_USER" "$GHOST_USER"; do
        if has_account "$user" && pgrep -u "$user" >/dev/null 2>&1; then
            echo "decommission: processes are still running as $user (pgrep -u $user); stop them and run this again" >&2
            exit 1
        fi
    done

    # The pool's files are read before it is destroyed, since afterwards
    # nothing records them. Only a regular file is removed: a pool on a real
    # disk keeps the disk.
    if has_pool; then
        vdevs=$(zpool list -H -v -P "$POOL" | awk 'NR > 1 && $1 ~ /^\// { print $1 }')
        zpool destroy "$POOL"
        say removed "pool $POOL"
        for vdev in $vdevs; do
            if [ -f "$ROOT$vdev" ]; then
                rm -f "$ROOT$vdev"
                say removed "$vdev"
            fi
        done
    fi

    for dir in "$APP_DIR" "$NODE_DIR"; do
        if [ -e "$dir" ]; then
            rm -rf "$dir"
            say removed "$(shown "$dir")"
        else
            say absent "$(shown "$dir")"
        fi
    done

    # The home directory only where it is one of /home's, which is where the
    # bootstrap puts goopy's; goopy-ghost's is /nonexistent.
    for user in "$DEPLOY_USER" "$GHOST_USER"; do
        if has_account "$user"; then
            home=$(getent passwd "$user" | cut -d: -f6)
            userdel "$user"
            case "$home" in
                /home/?*)
                    rm -rf "$ROOT$home"
                    ;;
            esac
            say removed "account $user"
        else
            say absent "account $user"
        fi
    done
else
    # Kept, but not where a later deploy would open it: its rows name instances
    # that no longer exist, and a gl-serv started on them would count them
    # against capacity until they expired.
    if [ -e "$REGISTRY" ]; then
        stamp=$(date -u +%Y%m%dT%H%M%SZ)
        for suffix in "" -wal -shm; do
            if [ -e "$REGISTRY$suffix" ]; then
                mv "$REGISTRY$suffix" "$REGISTRY.decommissioned-$stamp$suffix"
            fi
        done
        say moved "$(shown "$REGISTRY") -> $(shown "$REGISTRY").decommissioned-$stamp"
    fi
fi

# --- 8. Nothing of it is left running ------------------------------------------
#
# The live state, not the files just removed: a process that survived its unit,
# or something else bound to gl-serv's port, is what would make "decommissioned"
# untrue.
left=0
if systemctl is-active --quiet gl-serv; then
    echo "LEFT       gl-serv is still active" >&2
    left=1
fi
if [ -n "$(ss -ltnH "sport = :$GL_SERV_PORT")" ]; then
    echo "LEFT       something still listens on port $GL_SERV_PORT" >&2
    left=1
fi
for user in "$DEPLOY_USER" "$GHOST_USER"; do
    if has_account "$user" && pgrep -u "$user" >/dev/null 2>&1; then
        echo "LEFT       processes still run as $user" >&2
        left=1
    fi
done
if [ "$left" -ne 0 ]; then
    exit 1
fi
say done "Goopy is off $CONFIRM"
