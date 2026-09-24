# Sourced, not run: the host-resident artifacts the deploy tracks, and the
# comparison that tells whether a host still matches them (#139).
#
# Shared by deploy/push-binary.sh (which installs what it can and refuses to
# touch a host whose sudoers drop-in has drifted) and deploy/check-host.sh
# (which installs nothing and reports every difference). One table, so the two
# cannot disagree about where a file lives.
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
SITE_PREVIOUS=/tmp/gl-serv-api.nginx.prev
SITE_HOST=/etc/nginx/sites-available/gl-serv-api
SITE_ENABLED=/etc/nginx/sites-enabled/gl-serv-api

# What the api site was installed as before #139, by hand. Left enabled beside
# gl-serv-api it claims the same server_name and, sorting first, wins: nginx
# warns and serves the stale one. No deploy removes it — granting the deploy a
# permanent rule for a one-time migration would outlive the migration — so its
# presence is reported instead.
LEGACY_SITE_ENABLED=/etc/nginx/sites-enabled/api.goopy.life

# The sudoers drop-in. Verify-only: it is the file that grants the deploy its
# own rights, so a deploy that installed it could revoke its own ability to
# deploy with one bad push, recoverable only from a console. It is compared,
# never written — the comparison itself runs through a pinned `cmp`, because
# /etc/sudoers.d/goopy is 0440 root and the deploy account cannot read it.
SUDOERS_STAGED=/tmp/sudoers.goopy
SUDOERS_HOST=/etc/sudoers.d/goopy

# Resolves the tracked source of each artifact for the environment whose
# config is $1 (deploy/config/<env>.toml), into UNIT_SOURCE, DROPIN_SOURCE,
# SITE_SOURCE and SUDOERS_SOURCE. The per-environment ones sit beside the
# config under the same stem; the shared ones sit beside this file.
resolve_host_artifacts() {
    local config=$1
    local here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    UNIT_SOURCE="$here/gl-serv.service"
    SUDOERS_SOURCE="$here/sudoers.goopy"
    DROPIN_SOURCE="${config%.toml}.gl-serv.conf"
    SITE_SOURCE="${config%.toml}.api.nginx"
}

# Emits a remote sh command comparing every staged artifact with the host's
# copy. It prints one line per artifact and leaves `status` at 1 if anything
# must stop the caller; the caller appends its own `exit $status`, so it can
# clean up first.
#
#   $1  `deploy` — the deploy is about to install the unit, drop-in and site,
#       so a difference in those is reported (the log should say what it
#       overwrote) but does not stop it. `check` — nothing will be installed,
#       so every difference is drift.
#   $2  optional: where the environment's config was staged, to compare with
#       the installed one. The deploy gates and swaps the config itself, so
#       only `check` passes it.
#
# Regardless of mode, three things always fail, because no deploy can fix them:
# a sudoers drop-in that does not match, a legacy api site still enabled, and
# any file in the unit's drop-in directory other than the one the deploy
# ships — a stray override.conf changes the unit without appearing in either
# tracked file, and the deploy would leave it in place.
drift_check_command() {
    local mode=$1 staged_config=${2:-}
    local replaced=":" differs="differs from the repo" missing="is missing"
    if [[ "$mode" == check ]]; then
        replaced="status=1"
    else
        differs="differs from the repo; this deploy replaces it"
        missing="is missing; this deploy installs it"
    fi

    local c="status=0; "

    # sudo's own complaint is dropped: on a host whose drop-in predates the
    # `cmp` rule it is a password prompt that cannot be answered, and the line
    # below already says what that means and what to do.
    c+="if sudo cmp -s $SUDOERS_STAGED $SUDOERS_HOST 2>/dev/null; then echo 'ok     $SUDOERS_HOST'; "
    c+="else echo 'DRIFT  $SUDOERS_HOST does not match deploy/sudoers.goopy, or predates the rule that lets a deploy compare it. No deploy installs it: apply it by hand (docs/DEPLOYMENT.md), then re-run.' >&2; status=1; fi; "

    c+="if [ -e $LEGACY_SITE_ENABLED ]; then echo 'DRIFT  $LEGACY_SITE_ENABLED is still enabled and shadows $SITE_ENABLED. Remove it by hand (docs/DEPLOYMENT.md).' >&2; status=1; fi; "

    c+="for f in $DROPIN_DIR/*; do if [ -e \"\$f\" ] && [ \"\$f\" != $DROPIN_HOST ]; then echo \"DRIFT  \$f overrides gl-serv.service and is not shipped by any deploy. Fold it into deploy/config/<env>.gl-serv.conf or remove it.\" >&2; status=1; fi; done; "

    local pair staged host
    for pair in "$UNIT_STAGED $UNIT_HOST" "$DROPIN_STAGED $DROPIN_HOST" "$SITE_STAGED $SITE_HOST" ${staged_config:+"$staged_config /opt/goopy-life/config.toml"}; do
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
