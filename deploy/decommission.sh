#!/usr/bin/env bash
# Usage: ./deploy/decommission.sh <admin@host> --confirm <hostname> [--purge] [ssh-port]
#
#   admin      an account with password sudo on the host -- NOT the deploy
#              account, whose rights this removes
#   --confirm  the host's name, as `hostname` prints it there. Required: the
#              run stops before changing anything if the host says otherwise,
#              which is what catches an ssh alias pointing somewhere else
#   --purge    also remove the ZFS pool and its file, /opt/goopy-life (the
#              binaries, the config, the registry, the Ghost installs),
#              /opt/goopy-node and the goopy and goopy-ghost accounts
#
# Takes Goopy off a host: the reverse of what deploy.sh and admin-apply.sh put
# there, and with --purge of the Goopy half of host-bootstrap.sh. In one run and
# one sudo password prompt it stops gl-serv, revokes the deploy account's sudo
# rule, destroys every instance (unit, nginx site, dataset) and removes
# gl-serv's unit, drop-in, api site and cache zone. Without --purge the
# registry is kept, moved aside where no later deploy would open it.
#
# It leaves everything that is the host's rather than Goopy's: certificates,
# DNS, the provider and host firewalls, sshd, fail2ban, swap, sysctl, apt
# packages and the machine. Destroying the machine is the provider's job.
#
# Nothing is backed up. Idempotent: what is already gone is reported `absent`,
# so a run that stopped partway is finished by running it again. The work on the
# host is deploy/decommission.remote.sh.
#
# Set DRY_RUN=1 to print the scp/ssh commands instead of running them.
set -euo pipefail

USAGE="Usage: decommission.sh <admin@host> --confirm <hostname> [--purge] [ssh-port]"

TARGET=""
CONFIRM=""
PURGE=0
PORT=22
positional=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --confirm)
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "$USAGE" >&2
                exit 1
            fi
            CONFIRM=$2
            shift 2
            ;;
        --purge)
            PURGE=1
            shift
            ;;
        -*)
            echo "decommission.sh: unknown option $1" >&2
            echo "$USAGE" >&2
            exit 1
            ;;
        *)
            positional+=("$1")
            shift
            ;;
    esac
done
if [[ ${#positional[@]} -lt 1 || ${#positional[@]} -gt 2 || -z "$CONFIRM" ]]; then
    echo "$USAGE" >&2
    exit 1
fi
TARGET=${positional[0]}
if [[ ${#positional[@]} -eq 2 ]]; then
    PORT=${positional[1]}
fi
DRY_RUN=${DRY_RUN:-0}

HERE="$(cd "$(dirname "$0")" && pwd)"

# shellcheck source=host-artifacts.sh
source "$HERE/host-artifacts.sh"

# Host state that is not a tracked artifact, and so is not in host-artifacts.sh.
# Each names where it comes from; deploy/tests/decommission.test.sh reads those
# sources and fails if one of these no longer matches.
APP_DIR=/opt/goopy-life                     # host-bootstrap.sh APP_DIR
NODE_DIR=/opt/goopy-node                    # ghost-install.remote.sh NODE_DIR
REGISTRY=$APP_DIR/goopy.db                  # deploy/config/*.toml [registry] path
CACHE_DIR=/var/cache/nginx/goopy_alive      # deploy/nginx/goopy-cache.conf proxy_cache_path
POOL=zpool_ghost                            # host-bootstrap.sh POOL; deploy/config/*.toml pool
DEPLOY_USER=goopy                           # host-bootstrap.sh DEPLOY_USER
GHOST_USER=goopy-ghost                      # host-bootstrap.sh GHOST_USER
GL_SERV_PORT=3000                           # host-bootstrap.sh GL_SERV_PORT; *.toml bind_address

# The deploy account has no password sudo, and this removes its rules -- so it
# would fail at the prompt anyway, but only after asking for a password it
# cannot have. Say why up front instead. An ssh alias for the account is caught
# below, by asking the host.
if [[ "${TARGET%%@*}" == "$DEPLOY_USER" ]]; then
    echo "decommission.sh: $TARGET is the deploy account; run this as an admin with password sudo" >&2
    exit 1
fi

# A private directory made by the admin account (mktemp -d is 0700), as
# admin-apply.sh does: nothing the deploy account can write should be what root
# runs. The same connection says which account the target really is.
MAKE_STAGING="if [ \"\$(id -un)\" = $DEPLOY_USER ]; then echo $DEPLOY_USER; else mktemp -d; fi"
if [[ "$DRY_RUN" == "1" ]]; then
    STAGING="<staging>"
    printf '%s\n' "ssh -p $PORT $TARGET $MAKE_STAGING"
else
    STAGING=$(ssh -p "$PORT" "$TARGET" "$MAKE_STAGING")
fi
if [[ "$STAGING" == "$DEPLOY_USER" ]]; then
    echo "decommission.sh: $TARGET logs in as $DEPLOY_USER, the deploy account; run this as an admin with password sudo" >&2
    exit 1
fi

if [[ "$DRY_RUN" != "1" ]]; then
    if [[ "$PURGE" == 1 ]]; then
        echo "decommission.sh: taking Goopy off $CONFIRM, with --purge: its pool, install and accounts go too."
    else
        echo "decommission.sh: taking Goopy off $CONFIRM; the pool, install and accounts stay."
    fi
fi

run scp -P "$PORT" "$HERE/decommission.remote.sh" "$TARGET:$STAGING/decommission.remote.sh"

REMOTE_ENV="CONFIRM=$CONFIRM PURGE=$PURGE"
REMOTE_ENV+=" UNIT_HOST=$UNIT_HOST DROPIN_DIR=$DROPIN_DIR SITE_HOST=$SITE_HOST SITE_ENABLED=$SITE_ENABLED"
REMOTE_ENV+=" LEGACY_SITE_HOST=$LEGACY_SITE_HOST LEGACY_SITE_ENABLED=$LEGACY_SITE_ENABLED"
REMOTE_ENV+=" SUDOERS_HOST=$SUDOERS_HOST CACHE_CONF_HOST=$CACHE_CONF_HOST CACHE_DIR=$CACHE_DIR"
REMOTE_ENV+=" APP_DIR=$APP_DIR NODE_DIR=$NODE_DIR REGISTRY=$REGISTRY POOL=$POOL"
REMOTE_ENV+=" DEPLOY_USER=$DEPLOY_USER GHOST_USER=$GHOST_USER GL_SERV_PORT=$GL_SERV_PORT"

# -t so sudo can ask for the password; one sudo for the whole run, so it asks
# once. The staging directory goes whatever the outcome. `rc`, not `status`:
# this line runs in the admin's login shell, and zsh treats `status` as
# read-only.
run ssh -t -p "$PORT" "$TARGET" "sudo env $REMOTE_ENV sh $STAGING/decommission.remote.sh $STAGING; rc=\$?; rm -rf $STAGING; exit \$rc"
