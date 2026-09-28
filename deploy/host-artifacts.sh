# Sourced, not run: the host-resident artifacts the deploy tracks, and the
# comparison that tells whether a host still matches them (#139).
#
# Shared by deploy/push-binary.sh (which installs what it can and refuses to
# touch a host whose sudoers drop-in has drifted), deploy/check-host.sh (which
# installs nothing and reports every difference) and deploy/admin-apply.sh
# (which applies what no deploy may). One table, so they cannot disagree about
# where a file lives or which files there are.
#
# Every staged path below is pinned verbatim in deploy/sudoers.goopy, so none
# of them can change here alone: a rule that no longer matches is a bare sudo
# denial on the host, not an error that explains itself.

# The shared unit. Identical on every host; nothing environment-specific in it.
UNIT_STAGED=/tmp/gl-serv.service
UNIT_HOST=/etc/systemd/system/gl-serv.service

# The per-environment drop-in, deploy/config/<env>.gl-serv.conf. One fixed name
# on every host rather than <env>.conf: a sudo rule can only be pinned to a
# literal path — a wildcard there would also match extra arguments — and a host
# that changed environment would otherwise keep the old one beside the new.
DROPIN_STAGED=/tmp/gl-serv.deploy.conf
DROPIN_DIR=/etc/systemd/system/gl-serv.service.d
DROPIN_HOST=$DROPIN_DIR/deploy.conf

# The api nginx site, deploy/config/<env>.api.nginx. Not `goopy-api`: gl-serv
# may `rm -f /etc/nginx/sites-*/goopy-*` for its per-instance sites, and this
# must never be one of those.
SITE_STAGED=/tmp/gl-serv-api.nginx
SITE_HOST=/etc/nginx/sites-available/gl-serv-api
SITE_ENABLED=/etc/nginx/sites-enabled/gl-serv-api

# What the api site was installed as before #139, by hand. Left enabled beside
# gl-serv-api it claims the same server_name and, sorting first, wins: nginx
# warns and serves the stale one. No deploy removes it — granting the deploy a
# permanent rule for a one-time migration would outlive the migration — so its
# presence is reported instead.
LEGACY_SITE_HOST=/etc/nginx/sites-available/api.goopy.life
LEGACY_SITE_ENABLED=/etc/nginx/sites-enabled/api.goopy.life

# The sudoers drop-in. Verify-only: it is the file that grants the deploy its
# own rights, so a deploy that installed it could revoke its own ability to
# deploy with one bad push, recoverable only from a console. It is compared,
# never written — the comparison itself runs through a pinned `cmp`, because
# /etc/sudoers.d/goopy is 0440 root and the deploy account cannot read it.
SUDOERS_STAGED=/tmp/sudoers.goopy
# Where check-host.sh stages it instead. Its own path, so a check run while a
# deploy is in flight can neither overwrite nor delete the deploy's copy, and
# pinned like the other because `cmp` runs as root and the rule must be literal.
SUDOERS_CHECK_STAGED=/tmp/sudoers.goopy.check
SUDOERS_HOST=/etc/sudoers.d/goopy

# The nginx cache zone, deploy/nginx/goopy-cache.conf. Like sudoers, applied by
# an admin (admin-apply.sh) rather than a deploy: it lives in nginx's http {}
# block, which no pinned rule should be able to write.
CACHE_CONF_HOST=/etc/nginx/conf.d/goopy-cache.conf

# The environment's config. The deploy stages and swaps it itself (beside its
# destination, for an atomic rename), so it is not in the staged list below.
# Must match the --config path in deploy/gl-serv.service's ExecStart.
CONFIG_HOST=/opt/goopy-life/config.toml

# Resolves the tracked source of each artifact for the environment whose
# config is $1 (deploy/config/<env>.toml), into UNIT_SOURCE, DROPIN_SOURCE,
# SITE_SOURCE, SUDOERS_SOURCE and CACHE_CONF_SOURCE. The per-environment ones sit
# beside the config under the same stem; the shared ones sit beside this file.
#
# Also fills HOST_ARTIFACT_SOURCES and HOST_ARTIFACT_STAGED, in step: every
# artifact a deploy or a check uploads, and the fixed path it is staged at.
# Adding an artifact to both lists here is what makes both scripts ship it.
resolve_host_artifacts() {
    local config=$1
    local here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    UNIT_SOURCE="$here/gl-serv.service"
    SUDOERS_SOURCE="$here/sudoers.goopy"
    CACHE_CONF_SOURCE="$here/nginx/goopy-cache.conf"
    DROPIN_SOURCE="${config%.toml}.gl-serv.conf"
    SITE_SOURCE="${config%.toml}.api.nginx"
    HOST_ARTIFACT_SOURCES=("$UNIT_SOURCE" "$DROPIN_SOURCE" "$SITE_SOURCE" "$SUDOERS_SOURCE")
    HOST_ARTIFACT_STAGED=("$UNIT_STAGED" "$DROPIN_STAGED" "$SITE_STAGED" "$SUDOERS_STAGED")
}

# Fails with `<caller>: no such <what>: <path>` for the first of the remaining
# arguments that is not a file. $1 names the calling script, $2 the kind of file.
require_files() {
    local caller=$1 what=$2 path
    shift 2
    for path in "$@"; do
        if [[ ! -f "$path" ]]; then
            echo "$caller: no such $what: $path" >&2
            return 1
        fi
    done
}

# Prints each environment deploy/config/ defines, for a caller rejecting a name
# that is not one of them.
list_environments() {
    local here candidate
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    echo "available environments:" >&2
    for candidate in "$here"/config/*.toml; do
        echo "  $(basename "$candidate" .toml)" >&2
    done
}

# Runs its arguments, or with DRY_RUN=1 prints them instead. Every scp/ssh the
# deploy scripts issue goes through this, which is what their tests assert on.
run() {
    if [[ "${DRY_RUN:-0}" == "1" ]]; then
        printf '%s\n' "$*"
    else
        "$@"
    fi
}

# Uploads every host artifact to $1 over ssh port $2. By default each goes to
# its fixed /tmp path from HOST_ARTIFACT_STAGED, which deploy/sudoers.goopy pins
# -- not beside its destination, as those are root-owned directories. A caller
# that installs nothing passes its own destinations instead, one per artifact in
# the same order, to stay out of a concurrent deploy's way. scp spells the
# port -P.
stage_host_artifacts() {
    local target=$1 port=$2 i
    shift 2
    local destinations=("${HOST_ARTIFACT_STAGED[@]}")
    if [[ $# -gt 0 ]]; then
        destinations=("$@")
    fi
    for i in "${!HOST_ARTIFACT_SOURCES[@]}"; do
        run scp -P "$port" "${HOST_ARTIFACT_SOURCES[$i]}" "$target:${destinations[$i]}"
    done
}

# Emits a remote sh command comparing every staged artifact with the host's
# copy. It prints one line per artifact and leaves `rc` at 1 if anything
# must stop the caller; the caller appends its own `exit $rc`, so it can
# clean up first.
#
#   $1  `deploy` — the deploy is about to install the unit, drop-in and site,
#       so a difference in those is reported (the log should say what it
#       overwrote) but does not stop it. `check` — nothing will be installed,
#       so every difference is drift.
#   $2  where the environment's config was staged, to compare with the
#       installed one, or empty. The deploy gates and swaps the config itself,
#       so only `check` passes it.
#   $3… where the unit, drop-in, site and sudoers drop-in were staged, in
#       HOST_ARTIFACT_STAGED's order -- the deploy passes that array, the check
#       its own paths.
#
# Regardless of mode, three things always fail, because no deploy can fix them:
# a sudoers drop-in that does not match, a legacy api site still enabled, and
# any file in the unit's drop-in directory other than the one the deploy
# ships — a stray override.conf changes the unit without appearing in either
# tracked file, and the deploy would leave it in place.
drift_check_command() {
    local mode=$1 staged_config=$2
    local unit_staged=$3 dropin_staged=$4 site_staged=$5 sudoers_staged=$6
    local replaced=":" differs="differs from the repo" missing="is missing"
    if [[ "$mode" == check ]]; then
        replaced="rc=1"
    else
        differs="differs from the repo; this deploy replaces it"
        missing="is missing; this deploy installs it"
    fi

    # The result is `rc`, not `status`: this runs in the remote login shell, and
    # zsh treats `status` as a read-only special. Nothing below may rely on a
    # bash- or sh-only behaviour for the same reason -- hence no bare glob,
    # which zsh aborts on when it matches nothing.
    local c="rc=0; "

    # -n, so a host whose drop-in predates the `cmp` rule fails at once instead
    # of prompting for a password nobody can type -- over a tty that prompt
    # would sit on the discarded stderr and look like a hang. sudo's own
    # complaint is dropped: the line below says what it means and what to do.
    c+="if sudo -n cmp -s $sudoers_staged $SUDOERS_HOST 2>/dev/null; then echo 'ok     $SUDOERS_HOST'; "
    c+="else echo 'DRIFT  $SUDOERS_HOST does not match deploy/sudoers.goopy, or predates the rule that lets a deploy compare it. No deploy installs it: run deploy/admin-apply.sh as an admin, then re-run.' >&2; rc=1; fi; "

    c+="if [ -e $LEGACY_SITE_ENABLED ]; then echo 'DRIFT  $LEGACY_SITE_ENABLED is still enabled and shadows $SITE_ENABLED. deploy/admin-apply.sh migrates it.' >&2; rc=1; fi; "

    c+="for f in \$(ls -A $DROPIN_DIR 2>/dev/null); do if [ $DROPIN_DIR/\$f != $DROPIN_HOST ]; then echo \"DRIFT  $DROPIN_DIR/\$f overrides gl-serv.service and is not shipped by any deploy. Fold it into deploy/config/<env>.gl-serv.conf or remove it.\" >&2; rc=1; fi; done; "

    local pair staged host
    for pair in "$unit_staged $UNIT_HOST" "$dropin_staged $DROPIN_HOST" "$site_staged $SITE_HOST" ${staged_config:+"$staged_config $CONFIG_HOST"}; do
        staged=${pair% *}
        host=${pair#* }
        c+="if cmp -s $staged $host; then echo 'ok     $host'; "
        c+="elif [ -e $host ]; then echo 'DRIFT  $host $differs:'; diff -u $host $staged; $replaced; "
        c+="else echo 'DRIFT  $host $missing'; $replaced; fi; "
    done

    c+="if [ \"\$(readlink $SITE_ENABLED)\" = $SITE_HOST ]; then echo 'ok     $SITE_ENABLED'; "
    c+="else echo 'DRIFT  $SITE_ENABLED does not link to $SITE_HOST'; $replaced; fi"

    printf '%s' "$c"
}
