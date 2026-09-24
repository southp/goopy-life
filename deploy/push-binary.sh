#!/usr/bin/env bash
# Usage: push-binary.sh <user@host> <gl-serv> <gl-cli> <config> [ssh-port]
#
# Uploads the already-built gl-serv and gl-cli binaries together with the
# configuration they should run with, installs all three over the ones on the
# host along with the host-resident artifacts that configure how gl-serv runs
# and is reached (see "Host artifacts" below), restarts the systemd service,
# verifies it came back up -- and then
# verifies that what came back up is the commit this run built, by asking
# `GET /version` on the host. Set GL_GIT_SHA to that commit; it is required,
# because a defaulted one would turn the assertion into one that always passes.
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
# Host artifacts (#139). Beside the config, each environment ships
#   deploy/gl-serv.service              -> the unit, shared by every environment
#   deploy/config/<env>.gl-serv.conf    -> its systemd drop-in
#   deploy/config/<env>.api.nginx       -> the api nginx site
# found next to <config> by name, and all three are installed like the config:
# the deploy is their only writer. deploy/sudoers.goopy is the exception -- it is
# the file that grants this script its rights, so it is compared with the host's
# copy and never installed; a mismatch stops the deploy before anything changes.
# deploy/host-artifacts.sh holds the table and the comparison, shared with
# deploy/check-host.sh, which runs the same comparison without installing.
#
# Set DRY_RUN=1 to print the scp/ssh commands instead of running them.
set -euo pipefail

# shellcheck source=host-artifacts.sh
source "$(cd "$(dirname "$0")" && pwd)/host-artifacts.sh"

USAGE="Usage: push-binary.sh <user@host> <gl-serv> <gl-cli> <config> [ssh-port]"

TARGET=${1:?"$USAGE"}
SERV_BINARY=${2:?"$USAGE"}
CLI_BINARY=${3:?"$USAGE"}
CONFIG=${4:?"$USAGE"}
PORT=${5:-22}
DRY_RUN=${DRY_RUN:-0}

# The commit the binaries being pushed were built from, stamped into them via
# gl-core/build.rs. Required rather than defaulted: it is the value the identity
# check at the end compares against, and a default would turn a real assertion
# into one that always passes. Both callers export it.
SHA_HINT="push-binary.sh: GL_GIT_SHA must name the commit the binaries were built from"
GIT_SHA=${GL_GIT_SHA:?"$SHA_HINT (deploy/deploy.sh and .github/workflows/backend-deploy.yml set it)"}

# `unknown` is what gl-core/build.rs stamps into a build handed no commit, so a
# caller passing it through would build a binary that reports `unknown` and then
# compare that against `unknown` -- the always-passing check again, arrived at
# by a sentinel rather than a default.
if [[ "$GIT_SHA" == unknown ]]; then
    echo "$SHA_HINT; 'unknown' is the unstamped sentinel, not a commit" >&2
    exit 1
fi

# Must match the --config path in deploy/gl-serv.service's ExecStart.
REMOTE_CONFIG=/opt/goopy-life/config.toml

# Must match the install destination below, and gl-serv.service's ExecStart.
REMOTE_SERV=/opt/goopy-life/bin/gl-serv

resolve_host_artifacts "$CONFIG"

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
    for artifact in "$UNIT_SOURCE" "$DROPIN_SOURCE" "$SITE_SOURCE" "$SUDOERS_SOURCE"; do
        if [[ ! -f "$artifact" ]]; then
            echo "push-binary.sh: no such host artifact: $artifact" >&2
            echo "push-binary.sh: every deploy/config/<env>.toml needs a <env>.gl-serv.conf and a <env>.api.nginx beside it." >&2
            exit 1
        fi
    done
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

# The host artifacts go to the fixed /tmp paths deploy/sudoers.goopy pins, not
# beside their destinations: those are root-owned directories.
run scp -P "$PORT" "$UNIT_SOURCE" "$TARGET:$UNIT_STAGED"
run scp -P "$PORT" "$DROPIN_SOURCE" "$TARGET:$DROPIN_STAGED"
run scp -P "$PORT" "$SITE_SOURCE" "$TARGET:$SITE_STAGED"
run scp -P "$PORT" "$SUDOERS_SOURCE" "$TARGET:$SUDOERS_STAGED"

# Before anything else touches the host: does it still match what this deploy
# assumes about it? Three answers stop the deploy here, while nothing has
# changed -- a sudoers drop-in that differs from deploy/sudoers.goopy, the
# pre-#139 api site still enabled, or a stray drop-in overriding gl-serv.service.
# None of them is something this deploy can put right, and each makes the steps
# below either fail halfway (a missing sudo rule) or succeed while changing
# nothing (a shadowed site, an override that wins). The unit, drop-in and site
# are only reported: this deploy is about to replace them, and the log should
# say what it replaced.
#
# The sudoers comparison is also what makes the rules below safe to add: a host
# whose drop-in predates them fails here, by name, rather than as a bare sudo
# denial partway through an install.
run ssh -p "$PORT" "$TARGET" "$(drift_check_command deploy); exit \$status"

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

# The api site, on its own and ahead of every install below, because it is the
# one artifact that can break something other than gl-serv. `nginx -t` checks
# the whole configuration, not one file, and it is the same check every
# per-instance provision runs before its reload: a site that fails it and stays
# in place would make each later spawn fail too, host-wide, until someone
# removed it by hand. So a rejected site is taken back out -- the previous copy
# restored, or on a first install the link removed -- and the deploy stops.
# nginx itself never reloaded, so it goes on serving what it had.
#
# The previous copy is saved without sudo: sites-available is world-readable.
NGINX_SITE="had=0; if [ -e $SITE_HOST ]; then cp $SITE_HOST $SITE_PREVIOUS || exit 1; had=1; fi; "
NGINX_SITE+="if sudo install -m 644 $SITE_STAGED $SITE_HOST && sudo ln -sf $SITE_HOST $SITE_ENABLED && sudo nginx -t && sudo systemctl reload nginx; then rm -f $SITE_STAGED $SITE_PREVIOUS; exit 0; fi; "
NGINX_SITE+="echo 'push-binary.sh: nginx rejected the api site; putting the previous one back' >&2; "
NGINX_SITE+="if [ \$had = 1 ]; then sudo install -m 644 $SITE_PREVIOUS $SITE_HOST; else sudo rm -f $SITE_ENABLED; fi; exit 1"
run ssh -p "$PORT" "$TARGET" "$NGINX_SITE"

# The statements are joined with && rather than ';' on purpose: the exit status
# of a ';' sequence is the LAST command's, so a failed install would be reported
# as rm's success. set -e would not fire, and the deploy would go on to restart
# a service whose binary it never replaced -- green run, stale API. A sudo
# denial (the usual cause: /etc/sudoers.d/goopy missing, or the deploy running
# as an account the drop-in does not name) has to stop the deploy here.
#
# Each install is pinned as its own line in deploy/sudoers.goopy, and gl-serv's
# binary and config go last on purpose: everything ahead of them is something
# the running process does not read until the restart below, so a host whose
# drop-in predates one of those rules is denied while gl-serv's binary and
# config are still untouched and still serving. Installing gl-serv first would
# leave a half-applied deploy -- new binary and config on disk, old process
# running -- to fail on the same missing rule. Anything added later belongs
# ahead of gl-serv for the same reason. (The drift check above already refuses a
# host whose drop-in differs; this ordering is what still holds if it is ever
# skipped.)
#
# The unit and its drop-in are followed by daemon-reload, so the restart below
# starts gl-serv under the unit this deploy installed rather than the one
# systemd had cached, and by enable, so a host whose unit this deploy just put
# down also starts it on boot -- idempotent on every later run. `install -D`
# creates the drop-in directory on a host that has never had one.
INSTALL="sudo install -m 644 $UNIT_STAGED $UNIT_HOST && sudo install -D -m 644 $DROPIN_STAGED $DROPIN_HOST && sudo systemctl daemon-reload && sudo systemctl enable gl-serv && "
INSTALL+="sudo install -m 755 /tmp/gl-cli /opt/goopy-life/bin/gl-cli && sudo install -m 755 /tmp/gl-serv /opt/goopy-life/bin/gl-serv && "
INSTALL+="chmod 644 $REMOTE_CONFIG.new && mv $REMOTE_CONFIG.new $REMOTE_CONFIG && rm /tmp/gl-serv /tmp/gl-cli $UNIT_STAGED $DROPIN_STAGED $SUDOERS_STAGED"
run ssh -p "$PORT" "$TARGET" "$INSTALL"

run ssh -p "$PORT" "$TARGET" sudo systemctl restart gl-serv

# Restart is fire-and-forget: systemd reports success as soon as the process is
# spawned, so a binary that panics on startup -- or a config it cannot parse --
# would leave the deploy green while the API is down. RestartSec=5 in
# gl-serv.service means a crash-looping unit reads as "activating", which
# --quiet rejects; sleep past the first restart window before asking so a
# genuinely healthy unit is not caught mid-start.
run ssh -p "$PORT" "$TARGET" "sleep 8; systemctl is-active --quiet gl-serv"

# And then the question `is-active` cannot answer: is the process that came back
# the one this run built? `is-active` says *something* is running, which stays
# green through an install that did not replace the binary, a restart that
# raced, or a rollback that silently did not take -- the class of failure that
# left #117's fix undeployed for three weeks with nothing surfacing the gap.
#
# Three steps, as one remote command so the tests can extract and drive it:
#
#   1. Ask the *installed* binary where gl-serv can be reached, rather than
#      parsing the TOML here. `api_address` is the field that already answers
#      "an address a client can connect to" -- it resolves a wildcard
#      bind_address to loopback (#149) -- and re-deriving that in awk is how the
#      two drift. This re-reads the config that is actually installed, not the
#      staged copy the gate upstream checked.
#   2. `GET /version`, which gl-serv answers with the commit it was compiled
#      from. Loopback, so it bypasses nginx and reaches the process directly;
#      `--max-time` keeps a wedged socket from hanging the deploy forever. curl
#      is the one host tool this step assumes.
#   3. Compare. The match is on the full sha inside the JSON body rather than
#      via a parser, so the check needs no jq on the droplet.
#
# There is no `set -e` in the remote shell, so every step ends its own failure
# explicitly, and the mismatch branch prints what /version actually said -- a
# deploy that failed here is one where knowing the served sha is the whole
# diagnosis.
#
# A dirty build reports `<sha>-dirty` on both sides and matches; that is the
# point of the suffix, not an exception to it.
VERIFY_IDENTITY="api=\$($REMOTE_SERV --check-config --config $REMOTE_CONFIG | awk '\$1 == \"api_address\" { print \$2 }'); "
VERIFY_IDENTITY+="[ -n \"\$api\" ] || { echo 'push-binary.sh: could not read api_address from the installed config' >&2; exit 1; }; "
VERIFY_IDENTITY+="serving=\$(curl -fsS --max-time 10 \"http://\$api/version\") || { echo \"push-binary.sh: GET /version failed on \$api\" >&2; exit 1; }; "
VERIFY_IDENTITY+="case \"\$serving\" in *'\"sha_full\":\"$GIT_SHA\"'*) echo \"push-binary.sh: verified $GIT_SHA is serving\" ;; "
VERIFY_IDENTITY+="*) echo \"push-binary.sh: deployed the wrong commit -- built $GIT_SHA, /version says: \$serving\" >&2; exit 1 ;; esac"

run ssh -p "$PORT" "$TARGET" "$VERIFY_IDENTITY"
