#!/usr/bin/env bash
# Usage: push-binary.sh <user@host> <path-to-gl-serv> <path-to-config> [ssh-port]
#
# Uploads an already-built gl-serv binary together with the configuration it
# should run with, installs both over the running ones, restarts the systemd
# service and verifies it came back up.
#
# This is the single source of truth for the remote half of a deploy. Both
# callers share it on purpose:
#   - deploy/deploy.sh                        (manual, production)
#   - .github/workflows/backend-deploy.yml    (automated, dev droplet)
# The install command below is pinned verbatim in deploy/sudoers.goopy, so a
# copy that drifted in one caller would fail with a sudo denial on the droplet
# rather than anything self-explanatory.
#
# The config is shipped rather than hand-maintained on the droplet, which makes
# this script the only writer of the remote file: a hand-edit there is
# overwritten by the next deploy. Edit deploy/config/<env>.toml instead. A
# config that drifts from the schema the binary expects is a crash loop —
# gl-core's tests/deploy_configs.rs catches that at review time.
#
# Set DRY_RUN=1 to print the scp/ssh commands instead of running them.
set -euo pipefail

TARGET=${1:?"Usage: push-binary.sh <user@host> <path-to-gl-serv> <path-to-config> [ssh-port]"}
BINARY=${2:?"Usage: push-binary.sh <user@host> <path-to-gl-serv> <path-to-config> [ssh-port]"}
CONFIG=${3:?"Usage: push-binary.sh <user@host> <path-to-gl-serv> <path-to-config> [ssh-port]"}
PORT=${4:-22}
DRY_RUN=${DRY_RUN:-0}

# Must match the --config path in deploy/gl-serv.service's ExecStart.
REMOTE_CONFIG=/opt/goopy-life/config.toml

if [[ "$DRY_RUN" != "1" ]]; then
    if [[ ! -f "$BINARY" ]]; then
        echo "push-binary.sh: no such binary: $BINARY" >&2
        echo "push-binary.sh: build it first, or check the --target path." >&2
        exit 1
    fi
    if [[ ! -f "$CONFIG" ]]; then
        echo "push-binary.sh: no such config: $CONFIG" >&2
        echo "push-binary.sh: expected one of deploy/config/*.toml." >&2
        exit 1
    fi
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

# Staged next to its destination rather than in /tmp so the swap below is a
# rename within one directory, which is atomic: gl-serv is restarted moments
# later and must never read a half-written file.
run scp -P "$PORT" "$CONFIG" "$TARGET:$REMOTE_CONFIG.new"

# The statements are joined with && rather than ';' on purpose: the exit status
# of a ';' sequence is the LAST command's, so a failed install would be reported
# as rm's success. set -e would not fire, and the deploy would go on to restart
# a service whose binary it never replaced -- green run, stale API. A sudo
# denial (the usual cause: /etc/sudoers.d/goopy missing, or the deploy running
# as an account the drop-in does not name) has to stop the deploy here.
run ssh -p "$PORT" "$TARGET" "sudo install -m 755 /tmp/gl-serv /opt/goopy-life/bin/gl-serv && chmod 644 $REMOTE_CONFIG.new && mv $REMOTE_CONFIG.new $REMOTE_CONFIG && rm /tmp/gl-serv"

run ssh -p "$PORT" "$TARGET" sudo systemctl restart gl-serv

# Restart is fire-and-forget: systemd reports success as soon as the process is
# spawned, so a binary that panics on startup -- or a config it cannot parse --
# would leave the deploy green while the API is down. RestartSec=5 in
# gl-serv.service means a crash-looping unit reads as "activating", which
# --quiet rejects; sleep past the first restart window before asking so a
# genuinely healthy unit is not caught mid-start.
run ssh -p "$PORT" "$TARGET" "sleep 8; systemctl is-active --quiet gl-serv"
