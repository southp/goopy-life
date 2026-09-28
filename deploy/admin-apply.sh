#!/usr/bin/env bash
# Usage: ./deploy/admin-apply.sh <admin@host> <env> [ssh-port]
#
#   admin  an account with password sudo on the host -- NOT the deploy account.
#          `goopy` can only run the pinned commands in deploy/sudoers.goopy,
#          which is the point of it.
#   env    the environment the host runs, naming a file in deploy/config/
#
# Applies, in one run and one sudo password prompt, the root-owned artifacts no
# deploy is allowed to install (#139):
#
#   deploy/sudoers.goopy           -> /etc/sudoers.d/goopy
#   deploy/nginx/goopy-cache.conf  -> /etc/nginx/conf.d/goopy-cache.conf
#
# and, on a host set up before #139, retires the hand-installed api.goopy.life
# site in favour of deploy/config/<env>.api.nginx as gl-serv-api.
#
# Everything else on the host -- the unit, its drop-in, the api site from then
# on, the binaries, the config -- belongs to the deploy. Run this *before* the
# deploy that needs it: the deploy compares /etc/sudoers.d/goopy with the repo
# and stops until they match.
#
# Idempotent: an artifact that already matches is reported `unchanged` and not
# touched, so running it against an up-to-date host changes nothing. The work
# on the host is deploy/admin-apply.remote.sh, which validates each change
# before it can take effect (`visudo -cf`, `nginx -t`) and puts back the
# previous copy if the check that can only run afterwards rejects it.
#
# Set DRY_RUN=1 to print the scp/ssh commands instead of running them.
set -euo pipefail

USAGE="Usage: admin-apply.sh <admin@host> <env> [ssh-port]"

TARGET=${1:?"$USAGE"}
ENVIRONMENT=${2:?"$USAGE"}
PORT=${3:-22}
DRY_RUN=${DRY_RUN:-0}

HERE="$(cd "$(dirname "$0")" && pwd)"
CONFIG="$HERE/config/$ENVIRONMENT.toml"

# shellcheck source=host-artifacts.sh
source "$HERE/host-artifacts.sh"
resolve_host_artifacts "$CONFIG"

# The deploy account has no password sudo and no rule for `sh`, so this would
# fail at the prompt anyway -- but only after asking for a password it cannot
# have. Say why up front instead.
if [[ "${TARGET%%@*}" == goopy ]]; then
    echo "admin-apply.sh: $TARGET is the deploy account; run this as an admin with password sudo" >&2
    exit 1
fi

if ! require_files admin-apply.sh "artifact for environment '$ENVIRONMENT'" "$CONFIG" "$SITE_SOURCE" "$SUDOERS_SOURCE" "$CACHE_CONF_SOURCE"; then
    list_environments
    exit 1
fi

# visudo is checked locally too when there is one: a drop-in that does not parse
# should fail here, before anything reaches the host. The host checks again with
# its own sudo, which is the check that counts.
if command -v visudo >/dev/null 2>&1; then
    if ! visudo -cf "$SUDOERS_SOURCE" >/dev/null; then
        echo "admin-apply.sh: deploy/sudoers.goopy does not parse; nothing was sent" >&2
        exit 1
    fi
fi

# A private directory made by the admin account (mktemp -d is 0700), rather than
# fixed /tmp names: the deploy account stages its own files in /tmp, and nothing
# it can write should be what root installs from here.
if [[ "$DRY_RUN" == "1" ]]; then
    STAGING="<staging>"
    printf '%s\n' "ssh -p $PORT $TARGET mktemp -d"
else
    STAGING=$(ssh -p "$PORT" "$TARGET" mktemp -d)
fi

run scp -P "$PORT" "$SUDOERS_SOURCE" "$TARGET:$STAGING/sudoers.goopy"
run scp -P "$PORT" "$CACHE_CONF_SOURCE" "$TARGET:$STAGING/goopy-cache.conf"
run scp -P "$PORT" "$SITE_SOURCE" "$TARGET:$STAGING/gl-serv-api"
run scp -P "$PORT" "$HERE/admin-apply.remote.sh" "$TARGET:$STAGING/admin-apply.remote.sh"

# Where each file goes comes from host-artifacts.sh, handed to the host half
# rather than restated there: it runs under plain sh on the host and cannot
# source the table itself.
REMOTE_ENV="SUDOERS_HOST=$SUDOERS_HOST CACHE_CONF_HOST=$CACHE_CONF_HOST SITE_HOST=$SITE_HOST SITE_ENABLED=$SITE_ENABLED"
REMOTE_ENV+=" LEGACY_SITE_HOST=$LEGACY_SITE_HOST LEGACY_SITE_ENABLED=$LEGACY_SITE_ENABLED"

# -t so sudo can ask for the password; a single sudo for the whole run, so it
# asks once. The staging directory goes whatever the outcome. The exit code is
# kept in `rc`, not `status`: this line runs in the admin's login shell, and
# zsh treats `status` as read-only.
run ssh -t -p "$PORT" "$TARGET" "sudo env $REMOTE_ENV sh $STAGING/admin-apply.remote.sh $STAGING; rc=\$?; rm -rf $STAGING; exit \$rc"

if [[ "$DRY_RUN" != "1" ]]; then
    echo
    echo "admin-apply.sh: done. To confirm the host now matches $ENVIRONMENT:"
    echo "  ./deploy/check-host.sh <deploy-account@host> $ENVIRONMENT"
fi
