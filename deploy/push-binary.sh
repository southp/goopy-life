#!/usr/bin/env bash
# Usage: push-binary.sh <user@host> <gl-serv> <gl-cli> <config> [ssh-port]
#
# Uploads the already-built gl-serv and gl-cli binaries together with the
# configuration they should run with, installs all three over the ones on the
# host, restarts the systemd service and verifies it came back up.
#
# gl-cli is the droplet's maintenance CLI: despawning one instance by hand,
# listing what exists and driving alloc/dealloc have no route on gl-serv. It
# ships from this same deploy rather than a path of its own because it links
# gl-core, so it shares the registry schema and the provisioner with gl-serv --
# a separate path is how a host ends up with a gl-cli older than the gl-serv it
# shares a database with, which is when a maintenance tool is most dangerous.
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
# gl-core's tests/committed_configs.rs catches that at review time, and the
# --check-config gate below catches it on the host before the swap, which is
# what the manual production path has instead of CI.
#
# Set DRY_RUN=1 to print the scp/ssh commands instead of running them.
set -euo pipefail

USAGE="Usage: push-binary.sh <user@host> <gl-serv> <gl-cli> <config> [ssh-port]"

TARGET=${1:?"$USAGE"}
SERV_BINARY=${2:?"$USAGE"}
CLI_BINARY=${3:?"$USAGE"}
CONFIG=${4:?"$USAGE"}
PORT=${5:-22}
DRY_RUN=${DRY_RUN:-0}

# Must match the --config path in deploy/gl-serv.service's ExecStart.
REMOTE_CONFIG=/opt/goopy-life/config.toml

if [[ "$DRY_RUN" != "1" ]]; then
    for binary in "$SERV_BINARY" "$CLI_BINARY"; do
        if [[ ! -f "$binary" ]]; then
            echo "push-binary.sh: no such binary: $binary" >&2
            echo "push-binary.sh: build it first, or check the --target path." >&2
            exit 1
        fi
    done
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
run scp -P "$PORT" "$SERV_BINARY" "$TARGET:/tmp/gl-serv"
run scp -P "$PORT" "$CLI_BINARY" "$TARGET:/tmp/gl-cli"

# Staged next to its destination rather than in /tmp so the swap below is a
# rename within one directory, which is atomic: gl-serv is restarted moments
# later and must never read a half-written file.
run scp -P "$PORT" "$CONFIG" "$TARGET:$REMOTE_CONFIG.new"

# The gate: the binary that was just uploaded has to be able to read the config
# that was just staged, while the pair currently on the host is still installed
# and still serving. A config the new binary cannot parse is a crash loop --
# `systemctl is-active` at the end of this script catches the same class of
# failure, but only after the old binary has been stopped, i.e. after the outage.
#
# This runs as the deploy account with no sudo: both files were just written by
# that account, so deploy/sudoers.goopy needs no entry for it.
#
# On failure `set -e` stops here, leaving /tmp/gl-serv and $REMOTE_CONFIG.new
# behind. Neither is read by anything -- the rename below is what makes the
# staged config live -- and the next deploy overwrites both.
#
# chmod first because scp's handling of the executable bit varies with the
# transfer backend, and a gate that failed on a permission bit would block a
# deploy for a reason that has nothing to do with the config.
#
# The other half of that: this execs from /tmp, so a host that mounts /tmp
# noexec fails here with `Permission denied` -- a message that reads like a
# config failure and is not one. The install below only *reads* /tmp/gl-serv,
# so it never had this dependency. /tmp is a plain tmpfs on the droplets today
# (rw,nosuid,nodev); if one is ever hardened, stage the binary somewhere
# executable and update the install path pinned in deploy/sudoers.goopy to
# match -- the two have to move together.
run ssh -p "$PORT" "$TARGET" "chmod +x /tmp/gl-serv && /tmp/gl-serv --check-config --config $REMOTE_CONFIG.new"

# The statements are joined with && rather than ';' on purpose: the exit status
# of a ';' sequence is the LAST command's, so a failed install would be reported
# as rm's success. set -e would not fire, and the deploy would go on to restart
# a service whose binary it never replaced -- green run, stale API. A sudo
# denial (the usual cause: /etc/sudoers.d/goopy missing, or the deploy running
# as an account the drop-in does not name) has to stop the deploy here.
#
# Each install is pinned as its own line in deploy/sudoers.goopy, and gl-cli
# goes first on purpose: it is the artifact nothing else in the deploy depends
# on, so a host whose drop-in predates it is denied while gl-serv's binary and
# config are still untouched and still serving. Installing it last would leave
# a half-applied deploy -- new binary and config on disk, old process running --
# to fail on the same missing rule. Anything added later belongs ahead of
# gl-serv for the same reason.
run ssh -p "$PORT" "$TARGET" "sudo install -m 755 /tmp/gl-cli /opt/goopy-life/bin/gl-cli && sudo install -m 755 /tmp/gl-serv /opt/goopy-life/bin/gl-serv && chmod 644 $REMOTE_CONFIG.new && mv $REMOTE_CONFIG.new $REMOTE_CONFIG && rm /tmp/gl-serv /tmp/gl-cli"

run ssh -p "$PORT" "$TARGET" sudo systemctl restart gl-serv

# Restart is fire-and-forget: systemd reports success as soon as the process is
# spawned, so a binary that panics on startup -- or a config it cannot parse --
# would leave the deploy green while the API is down. RestartSec=5 in
# gl-serv.service means a crash-looping unit reads as "activating", which
# --quiet rejects; sleep past the first restart window before asking so a
# genuinely healthy unit is not caught mid-start.
run ssh -p "$PORT" "$TARGET" "sleep 8; systemctl is-active --quiet gl-serv"
