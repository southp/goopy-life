#!/usr/bin/env bash
# Usage: push-binary.sh <user@host> <path-to-gl-serv> [ssh-port]
#
# Uploads an already-built gl-serv binary to a droplet, installs it over the
# running one, restarts the systemd service and verifies it came back up.
#
# This is the single source of truth for the remote half of a deploy. Both
# callers share it on purpose:
#   - deploy/deploy.sh                        (manual, production)
#   - .github/workflows/backend-deploy.yml    (automated, dev droplet)
# The install command below is pinned verbatim in deploy/sudoers.goopy, so a
# copy that drifted in one caller would fail with a sudo denial on the droplet
# rather than anything self-explanatory.
#
# Set DRY_RUN=1 to print the scp/ssh commands instead of running them.
set -euo pipefail

TARGET=${1:?"Usage: push-binary.sh <user@host> <path-to-gl-serv> [ssh-port]"}
BINARY=${2:?"Usage: push-binary.sh <user@host> <path-to-gl-serv> [ssh-port]"}
PORT=${3:-22}
DRY_RUN=${DRY_RUN:-0}

if [[ "$DRY_RUN" != "1" && ! -f "$BINARY" ]]; then
    echo "push-binary.sh: no such binary: $BINARY" >&2
    echo "push-binary.sh: build it first, or check the --target path." >&2
    exit 1
fi

run() {
    if [[ "$DRY_RUN" == "1" ]]; then
        printf '%s\n' "$*"
    else
        "$@"
    fi
}

# scp spells the port -P, ssh spells it -p.
run scp -P "$PORT" "$BINARY" "$TARGET:/tmp/gl-serv"
run ssh -p "$PORT" "$TARGET" "sudo install -m 755 /tmp/gl-serv /opt/goopy-life/bin/gl-serv; rm /tmp/gl-serv"
run ssh -p "$PORT" "$TARGET" sudo systemctl restart gl-serv

# Restart is fire-and-forget: systemd reports success as soon as the process is
# spawned, so a binary that panics on startup would leave the deploy green while
# the API is down. RestartSec=5 in gl-serv.service means a crash-looping unit
# reads as "activating", which --quiet rejects; sleep past the first restart
# window before asking so a genuinely healthy unit is not caught mid-start.
run ssh -p "$PORT" "$TARGET" "sleep 8; systemctl is-active --quiet gl-serv"
