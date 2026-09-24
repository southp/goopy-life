#!/usr/bin/env bash
# Usage: ./deploy/check-host.sh <user@host> <env> [ssh-port]
#
#   env   the environment the host is meant to run, naming a file in
#         deploy/config/ (e.g. `dev` -> deploy/config/dev.toml)
#
# Read-only drift check (#139). Compares every host-resident artifact the repo
# tracks for <env> with the host's copy, prints one line per artifact, and exits
# non-zero if any of them differs:
#
#   /etc/sudoers.d/goopy                             deploy/sudoers.goopy
#   /etc/systemd/system/gl-serv.service              deploy/gl-serv.service
#   /etc/systemd/system/gl-serv.service.d/deploy.conf  deploy/config/<env>.gl-serv.conf
#     ...and no other file in that directory
#   /etc/nginx/sites-available/gl-serv-api           deploy/config/<env>.api.nginx
#     ...linked from sites-enabled, with the pre-#139 api.goopy.life site gone
#   /opt/goopy-life/config.toml                      deploy/config/<env>.toml
#
# It installs nothing and needs no rule beyond the `cmp` a deploy already uses:
# the only file the deploy account cannot read is the sudoers drop-in. Run it
# before a manual production deploy to see what the deploy is about to replace,
# and after any hand-edit on a host to see what the next deploy will undo.
#
# A deploy (push-binary.sh) runs the same comparison first, but only the
# sudoers drop-in, a stray unit override and the legacy site stop it -- the rest
# it is about to overwrite anyway. The binaries are not compared here; the
# deploy's own `GET /version` assertion covers those.
#
# Set DRY_RUN=1 to print the scp/ssh commands instead of running them.
set -euo pipefail

USAGE="Usage: check-host.sh <user@host> <env> [ssh-port]"

TARGET=${1:?"$USAGE"}
ENVIRONMENT=${2:?"$USAGE"}
PORT=${3:-22}
DRY_RUN=${DRY_RUN:-0}

HERE="$(cd "$(dirname "$0")" && pwd)"
CONFIG="$HERE/config/$ENVIRONMENT.toml"

# shellcheck source=host-artifacts.sh
source "$HERE/host-artifacts.sh"
resolve_host_artifacts "$CONFIG"

# Staged apart from the deploy's $REMOTE_CONFIG.new, which a running deploy
# would be about to rename into place.
CONFIG_STAGED=/tmp/gl-serv.check.toml

# Checked in dry-run too, unlike push-binary.sh's inputs: these are tracked
# files, so a missing one is a mistyped environment rather than a build that
# has not run yet.
for artifact in "$CONFIG" "$UNIT_SOURCE" "$DROPIN_SOURCE" "$SITE_SOURCE" "$SUDOERS_SOURCE"; do
    if [[ ! -f "$artifact" ]]; then
        echo "check-host.sh: no such artifact for environment '$ENVIRONMENT': $artifact" >&2
        echo "check-host.sh: available environments:" >&2
        for candidate in "$HERE"/config/*.toml; do
            echo "  $(basename "$candidate" .toml)" >&2
        done
        exit 1
    fi
done

run() {
    if [[ "$DRY_RUN" == "1" ]]; then
        printf '%s\n' "$*"
    else
        "$@"
    fi
}

run scp -P "$PORT" "$UNIT_SOURCE" "$TARGET:$UNIT_STAGED"
run scp -P "$PORT" "$DROPIN_SOURCE" "$TARGET:$DROPIN_STAGED"
run scp -P "$PORT" "$SITE_SOURCE" "$TARGET:$SITE_STAGED"
run scp -P "$PORT" "$SUDOERS_SOURCE" "$TARGET:$SUDOERS_STAGED"
run scp -P "$PORT" "$CONFIG" "$TARGET:$CONFIG_STAGED"

# One command, so the staged copies are removed whatever the verdict and the
# check leaves nothing behind on the host.
CHECK="$(drift_check_command check "$CONFIG_STAGED"); "
CHECK+="rm -f $UNIT_STAGED $DROPIN_STAGED $SITE_STAGED $SUDOERS_STAGED $CONFIG_STAGED; "
CHECK+="if [ \$status -eq 0 ]; then echo 'check-host.sh: $TARGET matches $ENVIRONMENT'; else echo 'check-host.sh: $TARGET has drifted from $ENVIRONMENT' >&2; fi; exit \$status"
run ssh -p "$PORT" "$TARGET" "$CHECK"
