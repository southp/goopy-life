#!/bin/bash
# Host half of deploy/ghost-install.sh (#210): prepares, as root, each Ghost
# base install it is handed, and the pinned Node runtime that runs it.
#
#   ghost-install.remote.sh <version> <source_dir> <node_bin> [<version> <source_dir> <node_bin>]...
#
# One triple per Ghost version, as the environment's config names it:
#
#   version     the Ghost release, e.g. 6.71.0
#   source_dir  must be /opt/goopy-life/ghost-<version>. Instances link into it
#               as written, so the path is what pins them (docs/GHOST_PROVISIONER.md)
#   node_bin    /opt/goopy-node/<node-version>/bin/node, which this installs
#               from nodejs.org, or any other Node already on the host
#               (/usr/bin/node from NodeSource), which it only checks
#
# Every triple is checked before anything is downloaded. Then, for each:
#
#   Node    the official tarball, verified against SHASUMS256.txt, whose
#           signature is verified against the Node release team's keyring at
#           a pinned commit, whose own checksum is pinned below
#   Ghost   npm pack + the release's own package manager, run as GHOST_USER
#           with that Node first on PATH: native addons (sqlite3, sharp) are
#           built for the Node that runs the install, so it has to be the one
#           that will run the instances
#
# Both are built under a .partial name and renamed when complete, so the
# directory the config names exists only once it is whole. A failed run leaves
# the .partial behind, and the next run clears it and starts over.
#
# Idempotent: what is already there is checked, not rebuilt, and reported
# `unchanged`. The check is the same one a fresh build ends with: the release
# is the version named, the Node satisfies its engines.node, the layout is the
# one the provisioner links against, its SQLite driver loads, and root owns
# every file in it and is the only one who can write any of them.
#
# ROOT prefixes every host path. It is empty on a host; the tests set it to a
# scratch directory, and source this with GOOPY_GHOST_INSTALL_SOURCED=1 so that
# nothing runs.
set -euo pipefail

# --- Settings -----------------------------------------------------------------

# deploy/host-bootstrap.sh's APP_DIR. Not where Node goes: every instance unit
# hides this directory behind an empty tmpfs and binds back only its own
# source_dir (render_service_file in gl-core's ghost_provisioner.rs), so a Node
# under it would not exist for the instance that runs it.
APP_DIR=/opt/goopy-life
NODE_DIR=/opt/goopy-node
# The account the instances run as (#187). It builds the install too: the
# install runs the dependencies' own build scripts, third-party code with no
# business running as root.
GHOST_USER=goopy-ghost
# Who owns every install, and the only account that may write to one: an
# instance that could write the code every other instance runs would reach all
# of them (#187).
INSTALL_OWNER=root
INSTALL_GROUP=root

NODE_DIST=https://nodejs.org/dist
# Every key that has signed a Node release, retired ones included, so that an
# older Node still verifies. A new release manager's key needs a newer commit;
# the signature check then fails and says so, and the fix is to move both
# lines below together.
NODE_KEYRING_URL=https://raw.githubusercontent.com/nodejs/release-keys/481637f813e912c4aa3622d7964ab426c97b8e8d/gpg/pubring.kbx
NODE_KEYRING_SHA256=610b8d249da3d5733f5a128def2dd0294dbbf5b5713e6ca2529db8db419dee00

# Ghost 5 has no `packageManager` field and ships a yarn.lock: yarn classic.
# Named, because corepack would otherwise pick a yarn of its own choosing.
YARN_CLASSIC=1.22.22

# What a Ghost build needs on the disk holding APP_DIR: ~631 MB for the install
# and as much again for npm's and corepack's caches while it runs.
MIN_FREE_MB=2048

# What the provisioner links each instance against: gl-core's SHARED_ENTRIES,
# plus content/themes, whose every entry is linked.
LAYOUT=(index.js core node_modules package.json loggingrc.js content/themes)

# Loads the SQLite driver the package.json at argv[1] declares, run from the
# install, and prints its name. See verify_ghost.
SQLITE_DRIVER_CHECK='
const pkg = require(process.argv[1]);
const declared = { ...pkg.dependencies, ...pkg.optionalDependencies };
const driver = ["better-sqlite3", "sqlite3"].find((name) => name in declared);
if (!driver) {
    console.error("declares neither better-sqlite3 nor sqlite3");
    process.exit(1);
}
require(driver);
console.log(driver);
'

ROOT=${ROOT:-}

# --- Helpers ------------------------------------------------------------------

say() {
    printf '%-10s %s\n' "$1" "$2"
}

step() {
    printf '\n== %s\n' "$1"
}

die() {
    printf 'ghost-install: %s\n' "$1" >&2
    exit 1
}

sha256() {
    sha256sum "$1" | cut -d' ' -f1
}

fetch() {
    curl -fsSL --retry 3 -o "$2" "$1"
}

# The Node version a node_bin names when it is one of ours, or nothing.
managed_node_version() {
    if [[ $1 =~ ^$NODE_DIR/([0-9]+\.[0-9]+\.[0-9]+)/bin/node$ ]]; then
        printf '%s' "${BASH_REMATCH[1]}"
    fi
}

# Node's name for this machine, as its release tarballs spell it.
node_arch() {
    case $(uname -m) in
        x86_64) printf x64 ;;
        aarch64 | arm64) printf arm64 ;;
        *) die "no Node release for $(uname -m)" ;;
    esac
}

check_triple() {
    local version=$1 source_dir=$2 node_bin=$3
    if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        die "'$version' is not a Ghost release (expected e.g. 6.71.0)"
    fi
    if [[ $source_dir != "$APP_DIR/ghost-$version" ]]; then
        die "Ghost $version: source_dir is $source_dir, must be $APP_DIR/ghost-$version. Instances link into it as written, so the path is what pins them to $version"
    fi
    if [[ -z $(managed_node_version "$node_bin") && ! -x $ROOT$node_bin ]]; then
        die "Ghost $version: node_bin $node_bin does not exist, and is not under $NODE_DIR/<version>/bin/node, so this cannot install it"
    fi
}

check_free_space() {
    local avail_mb
    avail_mb=$(($(df -Pk "$ROOT$APP_DIR" | awk 'NR == 2 { print $4 }') / 1024))
    if ((avail_mb < MIN_FREE_MB)); then
        die "$APP_DIR has ${avail_mb} MB free; a Ghost build needs ${MIN_FREE_MB} MB"
    fi
}

# Fails unless everything in the install at $1 is INSTALL_OWNER's and writable
# by no one else. A re-run calls an install `unchanged` only after this: one
# built by hand as the deploy account (or left group-writable by a stray chmod)
# passes every other check and is still writable by something that is not root.
# Group write is fine when the group is INSTALL_GROUP: node-gyp leaves its
# build directories 775, and only root is in root's group.
check_locked_down() {
    local dir=$1 loose
    loose=$(find "$dir" ! -type l \( ! -user "$INSTALL_OWNER" -o \( -perm -020 ! -group "$INSTALL_GROUP" \) -o -perm -002 \) -print -quit)
    if [[ -n $loose ]]; then
        die "${loose#"$ROOT"} is not $INSTALL_OWNER's alone (owner $(stat -c %U "$loose" 2>/dev/null || stat -f %Su "$loose"), mode $(stat -c %a "$loose" 2>/dev/null || stat -f %Lp "$loose")). Every instance runs this code: fix it with chown -R $INSTALL_OWNER:$INSTALL_OWNER and chmod -R go-w, or remove ${dir#"$ROOT"} and re-run"
    fi
}

# Runs a command as GHOST_USER, in the build's own HOME, with $1's directory
# first on PATH so that npm, corepack and the build scripts all run the Node
# the instances will.
as_ghost() {
    local node=$1 home=$2
    shift 2
    runuser -u "$GHOST_USER" -- env HOME="$home" PATH="$(dirname "$node"):$PATH" \
        COREPACK_ENABLE_DOWNLOAD_PROMPT=0 "$@"
}

# --- Node ---------------------------------------------------------------------

ensure_node() {
    local version=$1
    local dir=$NODE_DIR/$version
    local real=$ROOT$dir partial=$ROOT$dir.partial
    step "Node $version"
    if [[ -e $real ]]; then
        # Asking it its version runs it as root, so it has to be root's alone
        # first.
        check_locked_down "$real"
        local have
        if ! have=$("$real/bin/node" --version 2>/dev/null); then
            have="nothing it can run"
        fi
        if [[ $have != "v$version" ]]; then
            die "$dir/bin/node reports $have, not v$version. Remove $dir and re-run"
        fi
        say unchanged "$dir"
        return
    fi

    local arch tarball expected
    arch=$(node_arch)
    tarball=node-v$version-linux-$arch.tar.xz
    rm -rf "$partial"
    mkdir -p "$partial/.download"
    local dl=$partial/.download

    fetch "$NODE_KEYRING_URL" "$dl/keyring.kbx"
    if [[ $(sha256 "$dl/keyring.kbx") != "$NODE_KEYRING_SHA256" ]]; then
        die "the Node release keyring from $NODE_KEYRING_URL does not match its pinned checksum"
    fi
    fetch "$NODE_DIST/v$version/SHASUMS256.txt.asc" "$dl/SHASUMS256.txt.asc"
    # The checksums used are gpgv's output, the text the signature covers, and
    # not a second download that nothing signed.
    if ! gpgv --quiet --keyring "$dl/keyring.kbx" --output "$dl/SHASUMS256.txt" "$dl/SHASUMS256.txt.asc" 2>"$dl/gpgv.err"; then
        cat "$dl/gpgv.err" >&2
        die "Node $version: SHASUMS256.txt is not signed by a Node release key"
    fi
    expected=$(awk -v f="$tarball" '$2 == f { print $1 }' "$dl/SHASUMS256.txt")
    if [[ -z $expected ]]; then
        die "Node $version: no $tarball in its SHASUMS256.txt"
    fi
    fetch "$NODE_DIST/v$version/$tarball" "$dl/$tarball"
    if [[ $(sha256 "$dl/$tarball") != "$expected" ]]; then
        die "Node $version: $tarball does not match its signed checksum"
    fi

    tar -xJf "$dl/$tarball" --strip-components=1 -C "$partial"
    rm -rf "$dl"
    chown -R root:root "$partial"
    chmod -R a+rX,go-w "$partial"
    check_locked_down "$partial"
    mv "$partial" "$real"
    say created "$dir"
}

# --- Ghost --------------------------------------------------------------------

# Ghost 5 cannot use sqlite3's prebuilt binary on Node 22, and compiles it.
# The napi-build-utils it locks (1.0.2) compares N-API versions as strings, so
# Node 22's "10" is below sqlite3's "6", no prebuild is ever chosen, and the
# build falls back to node-gyp. The node-gyp it locks (8.4.1) imports
# distutils, which Python 3.12 removed and python3-setuptools puts back. Both
# packages are in host-bootstrap.sh's BASE_PACKAGES. Checked before the
# install because yarn treats sqlite3 as optional: without these it would
# finish "successfully" with no database driver, and only the check after it
# would say so, less clearly.
check_build_toolchain() {
    local version=$1 tool
    for tool in make g++ python3; do
        if ! command -v "$tool" >/dev/null; then
            die "Ghost $version compiles sqlite3 and needs $tool: apt-get install build-essential python3-setuptools"
        fi
    done
    if ! python3 -c 'import distutils.version' 2>/dev/null; then
        die "Ghost $version compiles sqlite3 with a node-gyp that needs Python's distutils: apt-get install python3-setuptools"
    fi
}

# Prints every `<name>@<version>` a yarn classic lockfile at $1 resolves to,
# sorted, leaving out the tarballs the release bundles (`file:components/...`),
# which no registry resolves.
yarn_lock_pins() {
    awk '
        /^[^ #].*:$/ {
            spec = $0
            sub(/^"/, "", spec)
            sub(/[",]?[,:] .*$|"?:$/, "", spec)
            bundled = (spec ~ /@file:/)
            name = spec
            sub(/@[^@]*$/, "", name)
        }
        /^  version "/ && !bundled {
            v = $2
            gsub(/"/, "", v)
            print name "@" v
        }
    ' "$1" | sort -u
}

# Fails unless the Node at $2 satisfies the engines.node of the release in $1,
# using the semver that the Node's own npm bundles: the release's own
# node_modules do not exist yet when this first runs.
check_engines() {
    local dir=$1 node=$2 range semver
    range=$("$node" -p "require(process.argv[1]).engines.node" "$dir/package.json")
    semver=$(dirname "$node")/../lib/node_modules/npm/node_modules/semver
    if ! "$node" -e "process.exit(require(process.argv[1]).satisfies(process.version, process.argv[2]) ? 0 : 1)" \
        "$semver" "$range"; then
        die "Node $("$node" --version) does not satisfy Ghost's engines.node ($range)"
    fi
    printf '%s' "$range"
}

# The check a build ends with, and all that a re-run does to an install that is
# already there. $1 is the install, $2 the version it must be, $3 the Node.
verify_ghost() {
    local dir=$1 version=$2 node=$3 have entry range
    have=$("$node" -p "require(process.argv[1]).version" "$dir/package.json")
    if [[ $have != "$version" ]]; then
        die "${dir#"$ROOT"} holds Ghost $have, not $version"
    fi
    range=$(check_engines "$dir" "$node")
    for entry in "${LAYOUT[@]}"; do
        if [[ ! -e $dir/$entry ]]; then
            die "Ghost $version has no $entry, which every instance links against"
        fi
    done
    # The SQLite driver is native: built for one Node ABI, it fails to load
    # under another, and an instance would find out on its first boot. Ghost 6
    # uses better-sqlite3 (and aliases the `sqlite3` client our config names to
    # it) and declares it optional, so a failed build of it does not even fail
    # the install. Ghost 5 uses sqlite3. Whichever the release declares is the
    # one that has to load.
    local driver
    if ! driver=$(cd "$dir" && runuser -u "$GHOST_USER" -- "$node" -e "$SQLITE_DRIVER_CHECK" "$dir/package.json"); then
        die "Ghost $version: its SQLite driver does not load under Node $("$node" --version). Was it built with another Node?"
    fi
    say node "$("$node" --version) satisfies $range"
    say sqlite "$driver loads"
}

ensure_ghost() {
    local version=$1 node_bin=$2
    local dir=$APP_DIR/ghost-$version
    local real=$ROOT$dir partial=$ROOT$dir.partial home=$ROOT$dir.home node=$ROOT$node_bin
    step "Ghost $version"
    if [[ -e $real ]]; then
        verify_ghost "$real" "$version" "$node"
        check_locked_down "$real"
        say unchanged "$dir"
        return
    fi

    check_free_space
    # Both at fixed names beside the release, so whatever a failed run leaves
    # (npm's and corepack's caches run to hundreds of MB) is cleared here, by
    # the next run, instead of piling up in /tmp one run at a time.
    rm -rf "$partial" "$home"
    mkdir -p "$partial" "$home"
    chown "$GHOST_USER:$GHOST_USER" "$partial" "$home"
    (
        cd "$partial"
        as_ghost "$node" "$home" npm pack --silent "ghost@$version" >/dev/null
        as_ghost "$node" "$home" tar xzf "ghost-$version.tgz" --strip-components=1
        rm "ghost-$version.tgz"
        # Before the install, which takes minutes: a Node the release refuses
        # should cost seconds.
        check_engines "$partial" "$node" >/dev/null
        if [[ -f pnpm-lock.yaml ]]; then
            # Exactly the dependency versions the release was tested with: a
            # lockfile that does not match fails the run instead of being
            # resolved afresh on the day.
            say install "pnpm, as the release's packageManager names"
            as_ghost "$node" "$home" corepack pnpm install --prod --frozen-lockfile
        elif [[ -f yarn.lock ]]; then
            # Not --frozen-lockfile, which refuses Ghost 5's: the yarn.lock it
            # publishes is the whole monorepo's, and a production install of
            # the one package prunes it to under half. Checked after instead:
            # the install may drop entries, never resolve one afresh.
            say install "yarn $YARN_CLASSIC"
            check_build_toolchain "$version"
            cp yarn.lock "$home/yarn.lock.release"
            as_ghost "$node" "$home" corepack "yarn@$YARN_CLASSIC" install --production --non-interactive
            local fresh
            fresh=$(comm -13 <(yarn_lock_pins "$home/yarn.lock.release") <(yarn_lock_pins yarn.lock))
            if [[ -n $fresh ]]; then
                die "Ghost $version: yarn resolved what its yarn.lock does not pin: $(tr '\n' ' ' <<<"$fresh")"
            fi
        else
            die "Ghost $version ships neither pnpm-lock.yaml nor yarn.lock"
        fi
    )
    rm -rf "$home"
    verify_ghost "$partial" "$version" "$node"
    # Root-owned so no instance can write here and change the code every other
    # instance runs (#187), the same as host-bootstrap.sh's install_ghost.
    chown -R root:root "$partial"
    chmod -R a+rX,go-w "$partial"
    check_locked_down "$partial"
    mv "$partial" "$real"
    say created "$dir"
}

# --- Main ---------------------------------------------------------------------

main() {
    if (($# == 0 || $# % 3 != 0)); then
        die "usage: ghost-install.remote.sh <version> <source_dir> <node_bin> [...]"
    fi
    if [[ -z $ROOT && $EUID -ne 0 ]]; then
        die "must run as root"
    fi
    if ! id -u "$GHOST_USER" >/dev/null 2>&1; then
        die "no $GHOST_USER account; run deploy/host-bootstrap.sh first"
    fi
    if [[ ! -d $ROOT$APP_DIR ]]; then
        die "no $APP_DIR; run deploy/host-bootstrap.sh first"
    fi

    local args=("$@") i
    for ((i = 0; i < ${#args[@]}; i += 3)); do
        check_triple "${args[i]}" "${args[i + 1]}" "${args[i + 2]}"
    done

    mkdir -p "$ROOT$NODE_DIR"
    chown root:root "$ROOT$NODE_DIR"
    chmod 0755 "$ROOT$NODE_DIR"

    # Each Node once, however many releases share it, and before any Ghost:
    # a Ghost build needs its Node in place.
    local seen=" " node_version
    for ((i = 0; i < ${#args[@]}; i += 3)); do
        node_version=$(managed_node_version "${args[i + 2]}")
        if [[ -n $node_version && $seen != *" $node_version "* ]]; then
            ensure_node "$node_version"
            seen+="$node_version "
        fi
    done
    for ((i = 0; i < ${#args[@]}; i += 3)); do
        ensure_ghost "${args[i]}" "${args[i + 2]}"
    done
}

if [[ ${GOOPY_GHOST_INSTALL_SOURCED:-0} != 1 ]]; then
    main "$@"
fi
