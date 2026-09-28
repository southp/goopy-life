#!/bin/sh
# The host half of deploy/admin-apply.sh. Runs as root, from a staging
# directory the local half has just filled:
#
#   sh admin-apply.remote.sh <staging-dir>
#
#   <staging-dir>/sudoers.goopy        deploy/sudoers.goopy
#   <staging-dir>/goopy-cache.conf     deploy/nginx/goopy-cache.conf
#   <staging-dir>/gl-serv-api          deploy/config/<env>.api.nginx
#
# Each artifact is compared first and left alone when it already matches, so
# running this on a host that is up to date changes nothing and reloads nothing.
# Every change is validated before it can take effect, and put back if the
# validation that can only run afterwards rejects it.
#
# ROOT prefixes every absolute path. It is empty on a host; the tests set it to
# a scratch directory, which is the only reason it exists.
set -eu

STAGING=${1:?"usage: admin-apply.remote.sh <staging-dir>"}
ROOT=${ROOT:-}

SUDOERS=$ROOT/etc/sudoers.d/goopy
CACHE_CONF=$ROOT/etc/nginx/conf.d/goopy-cache.conf
CACHE_DIR=$ROOT/var/cache/nginx
SITE=$ROOT/etc/nginx/sites-available/gl-serv-api
SITE_ENABLED=$ROOT/etc/nginx/sites-enabled/gl-serv-api
LEGACY=$ROOT/etc/nginx/sites-available/api.goopy.life
LEGACY_ENABLED=$ROOT/etc/nginx/sites-enabled/api.goopy.life

say() {
    printf '%-10s %s\n' "$1" "$2"
}

# --- 1. The sudoers drop-in ---------------------------------------------------
#
# The one file here that can lock everyone out: a syntax error in *any* file
# under /etc/sudoers.d makes sudo refuse to run at all, for every account,
# including the one that would fix it. So the staged copy is checked with
# `visudo -cf` before it goes anywhere near that directory, and it arrives by
# rename: staged as `goopy.new` beside its destination, which sudo's
# `@includedir` skips because the name contains a dot, then moved over the old
# one in a single step. There is no moment at which sudo reads half a file.
#
# `visudo -c` afterwards checks the whole configuration as sudo will read it,
# and puts the previous copy back if that fails. It should never fire given the
# check above; it is here because the price of being wrong is a console session.
if cmp -s "$STAGING/sudoers.goopy" "$SUDOERS"; then
    say unchanged /etc/sudoers.d/goopy
else
    if ! visudo -cf "$STAGING/sudoers.goopy" >/dev/null; then
        echo "admin-apply: deploy/sudoers.goopy does not parse; /etc/sudoers.d/goopy left as it was" >&2
        exit 1
    fi
    had_sudoers=0
    if [ -e "$SUDOERS" ]; then
        cp -p "$SUDOERS" "$STAGING/sudoers.previous"
        had_sudoers=1
    fi
    install -m 0440 "$STAGING/sudoers.goopy" "$SUDOERS.new"
    chown root:root "$SUDOERS.new"
    mv -f "$SUDOERS.new" "$SUDOERS"
    if ! visudo -c >/dev/null; then
        if [ "$had_sudoers" = 1 ]; then
            mv -f "$STAGING/sudoers.previous" "$SUDOERS"
        else
            rm -f "$SUDOERS"
        fi
        echo "admin-apply: sudo rejected the configuration with the new drop-in; the previous one is back" >&2
        exit 1
    fi
    say updated /etc/sudoers.d/goopy
fi

# --- 2. The nginx cache zone --------------------------------------------------
#
# Lives in the http {} block, which no site file can reach, so the deploy
# cannot ship it (see the file's own header). nginx creates only the last
# component of a proxy_cache_path, hence the mkdir. A rejected zone is taken
# back out before nginx ever reloads: every per-instance site references it,
# so a broken one fails `nginx -t` host-wide.
if cmp -s "$STAGING/goopy-cache.conf" "$CACHE_CONF"; then
    say unchanged /etc/nginx/conf.d/goopy-cache.conf
else
    had_cache=0
    if [ -e "$CACHE_CONF" ]; then
        cp -p "$CACHE_CONF" "$STAGING/goopy-cache.previous"
        had_cache=1
    fi
    mkdir -p "$CACHE_DIR"
    install -m 644 "$STAGING/goopy-cache.conf" "$CACHE_CONF"
    if ! nginx -t >/dev/null 2>&1; then
        if [ "$had_cache" = 1 ]; then
            mv -f "$STAGING/goopy-cache.previous" "$CACHE_CONF"
        else
            rm -f "$CACHE_CONF"
        fi
        echo "admin-apply: nginx rejected goopy-cache.conf; the previous one is back (run \`nginx -t\` to see why)" >&2
        exit 1
    fi
    systemctl reload nginx
    say updated /etc/nginx/conf.d/goopy-cache.conf
fi

# --- 3. One-time: retire the pre-#139 api site --------------------------------
#
# Hosts set up before #139 serve the api from a hand-installed
# sites-enabled/api.goopy.life, which the deploy refuses to run beside. This
# swaps it for gl-serv-api in a single reload, so the API never goes down. It is
# a no-op everywhere else, and from then on the deploy owns gl-serv-api.
if [ -e "$LEGACY_ENABLED" ]; then
    install -m 644 "$STAGING/gl-serv-api" "$SITE"
    ln -sf "$SITE" "$SITE_ENABLED"
    rm -f "$LEGACY_ENABLED"
    if ! nginx -t >/dev/null 2>&1; then
        ln -sf "$LEGACY" "$LEGACY_ENABLED"
        rm -f "$SITE_ENABLED"
        echo "admin-apply: nginx rejected gl-serv-api; api.goopy.life is enabled again (run \`nginx -t\` to see why)" >&2
        exit 1
    fi
    systemctl reload nginx
    rm -f "$LEGACY"
    say migrated "sites-enabled/api.goopy.life -> sites-enabled/gl-serv-api"
fi
