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
# It installs nothing, and stages nowhere a deploy does, so it is safe to run
# while one is in flight. Its only sudo rule is a `cmp` of its own: the one
# file the deploy account cannot read is the sudoers drop-in. Run it
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

# Checked in dry-run too, unlike push-binary.sh's inputs: these are tracked
# files, so a missing one is a mistyped environment rather than a build that
# has not run yet.
if ! require_files check-host.sh "artifact for environment '$ENVIRONMENT'" "$CONFIG" "${HOST_ARTIFACT_SOURCES[@]}"; then
    list_environments
    exit 1
fi

# Nothing is staged at the deploy's paths. A deploy -- CI's, on every merge --
# may be between its upload and its install at any moment; a check sharing its
# /tmp names would hand it this environment's files, or delete them under it.
# So everything goes to a private directory (mktemp -d is 0700), except the
# sudoers copy: the root-run `cmp` can only be pinned to a literal path, so it
# has one of its own, SUDOERS_CHECK_STAGED.
if [[ "$DRY_RUN" == "1" ]]; then
    STAGING="<staging>"
    printf '%s\n' "ssh -p $PORT $TARGET mktemp -d"
else
    STAGING=$(ssh -p "$PORT" "$TARGET" mktemp -d)
fi
CHECK_STAGED=()
for staged in "${HOST_ARTIFACT_STAGED[@]}"; do
    if [[ "$staged" == "$SUDOERS_STAGED" ]]; then
        CHECK_STAGED+=("$SUDOERS_CHECK_STAGED")
    else
        CHECK_STAGED+=("$STAGING/$(basename "$staged")")
    fi
done
CONFIG_STAGED=$STAGING/config.toml

stage_host_artifacts "$TARGET" "$PORT" "${CHECK_STAGED[@]}"
run scp -P "$PORT" "$CONFIG" "$TARGET:$CONFIG_STAGED"

# One command, so the staged copies are removed whatever the verdict and the
# check leaves nothing behind on the host.
CHECK="$(drift_check_command check "$CONFIG_STAGED" "${CHECK_STAGED[@]}"); "
CHECK+="rm -rf $STAGING $SUDOERS_CHECK_STAGED; "
CHECK+="if [ \$status -eq 0 ]; then echo 'check-host.sh: $TARGET matches $ENVIRONMENT'; else echo 'check-host.sh: $TARGET has drifted from $ENVIRONMENT' >&2; fi; exit \$status"
run ssh -p "$PORT" "$TARGET" "$CHECK"
