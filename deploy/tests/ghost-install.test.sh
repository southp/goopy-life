#!/usr/bin/env bash
# Tests for deploy/ghost-install.sh and its host half, ghost-install.remote.sh.
#
# Run: ./deploy/tests/ghost-install.test.sh
#
# The local half is driven with DRY_RUN=1, and its config reader is run against
# fixtures and the committed configs. The host half is sourced with
# GOOPY_GHOST_INSTALL_SOURCED=1 and its steps run for real against a scratch
# directory standing in for the host (its ROOT), with stub curl, gpgv, npm,
# corepack, runuser and chown in front of PATH, serving fixture releases. No
# host, root or network required.
#
# The Ghost cases run the checks the host half makes with Node itself, so they
# need a `node` on PATH, and are skipped without one. What only the real
# registry and nodejs.org can answer -- does this Ghost actually build -- is
# answered by a run against the dev host; see docs/GHOST_PROVISIONER.md.
set -uo pipefail

DEPLOY_DIR="$(cd "$(dirname "$0")/.." && pwd)"
LOCAL_HALF="$DEPLOY_DIR/ghost-install.sh"
REMOTE_HALF="$DEPLOY_DIR/ghost-install.remote.sh"
FAILURES=0
CASES=0
REAL_NODE=$(command -v node || true)
# Who owns what the tests build: the host half's INSTALL_OWNER is root, which
# nothing in a scratch directory is.
ME=$(whoami)

pass() {
    CASES=$((CASES + 1))
    echo "ok   — $1"
}

fail() {
    CASES=$((CASES + 1))
    echo "FAIL — $1"
    shift
    printf '       %s\n' "$@"
    FAILURES=$((FAILURES + 1))
}

skip() {
    echo "skip — $1 ($2)"
}

sha256_of() {
    sha256sum "$1" | cut -d' ' -f1
}

for script in "$LOCAL_HALF" "$REMOTE_HALF"; do
    name=$(basename "$script" .sh)
    if bash -n "$script"; then
        pass "${name}_parses"
    else
        fail "${name}_parses" "bash -n rejected it"
    fi
    # Control flow takes explicit blocks: `if ...; then die ...; fi`, never a
    # `[[ ... ]] || die` shorthand that a second statement could later slip
    # out of.
    shorthand=$(grep -nE '(\|\||&&)[[:space:]]*(die|exit|have=|return)\b' "$script")
    if [[ -z $shorthand ]]; then
        pass "${name}_uses_explicit_blocks"
    else
        fail "${name}_uses_explicit_blocks" "$shorthand"
    fi
done

# --- Local half ---------------------------------------------------------------

echo
echo "== ghost-install.sh =="

# The local half's helpers, without running it.
# shellcheck source=../ghost-install.sh
GOOPY_GHOST_INSTALL_SOURCED=1 source "$LOCAL_HALF"
set +e

FIXTURES=$(mktemp -d)

# The shape every config has today: one Ghost, in [provisioner] itself.
cat >"$FIXTURES/flat.toml" <<'EOF'
domain = "example.test"
version = "not-this-one"

[provisioner]
kind = "Ghost"
# The prepared base install.
source_dir = "/opt/goopy-life/ghost-6.63.0"
version = "6.63.0"   # stamped onto every instance
node_bin = "/usr/bin/node"
service_user = "goopy-ghost"

[ratelimit]
version = "not-this-one-either"
EOF
got=$(config_versions "$FIXTURES/flat.toml")
if [[ $got == "6.63.0 /opt/goopy-life/ghost-6.63.0 /usr/bin/node" ]]; then
    pass config_versions_reads_the_provisioner_section
else
    fail config_versions_reads_the_provisioner_section "got: $got"
fi

# The shape #210 moves to: one [[provisioner.versions]] entry per Ghost, in
# config order. An entry without node_bin gets gl-core's default, as gl-serv
# would give it.
cat >"$FIXTURES/versions.toml" <<'EOF'
[provisioner]
kind = "Ghost"
default_version = "6.71.0"
service_user = "goopy-ghost"

[[provisioner.versions]]
version = "6.71.0"
source_dir = "/opt/goopy-life/ghost-6.71.0"
node_bin = "/opt/goopy-node/22.23.2/bin/node"

[[provisioner.versions]]
  version    = "6.63.0"
  source_dir = "/opt/goopy-life/ghost-6.63.0"

[[provisioner.versions]]  # the last Ghost 5
version = "5.130.6"
source_dir = "/opt/goopy-life/ghost-5.130.6"
node_bin = "/opt/goopy-node/22.23.2/bin/node"

[allocator]
kind = "Zfs"
EOF
got=$(config_versions "$FIXTURES/versions.toml")
want="6.71.0 /opt/goopy-life/ghost-6.71.0 /opt/goopy-node/22.23.2/bin/node
6.63.0 /opt/goopy-life/ghost-6.63.0 /usr/bin/node
5.130.6 /opt/goopy-life/ghost-5.130.6 /opt/goopy-node/22.23.2/bin/node"
if [[ $got == "$want" ]]; then
    pass config_versions_reads_every_versions_entry_in_order
else
    fail config_versions_reads_every_versions_entry_in_order "got:" "$got"
fi

# config.local.toml runs Hello and carries the Ghost keys commented out, as a
# recipe. Neither is a version to install.
got=$(config_versions "$DEPLOY_DIR/../backend/config.local.toml")
if [[ -z $got ]]; then
    pass config_versions_names_nothing_for_hello_or_commented_out_keys
else
    fail config_versions_names_nothing_for_hello_or_commented_out_keys "got: $got"
fi

# Whatever shape the deployed configs are in, the reader has to find their
# Ghost, and each has to be the version-stamped install the host half accepts.
for env in dev prod; do
    got=$(config_versions "$DEPLOY_DIR/config/$env.toml")
    bad=""
    while read -r version source_dir node_bin; do
        if [[ $source_dir != "/opt/goopy-life/ghost-$version" || $node_bin != /* ]]; then
            bad+="$version $source_dir $node_bin; "
        fi
    done <<<"$got"
    if [[ -n $got && -z $bad ]]; then
        pass "config_versions_reads_${env}_toml"
    else
        fail "config_versions_reads_${env}_toml" "got: ${got:-nothing}" "${bad:+not version-stamped: $bad}"
    fi
done

# Env mode hands the host every version the config names, as one argument list.
expected_args=$(config_versions "$DEPLOY_DIR/config/dev.toml" | tr '\n' ' ' | sed 's/ $//')
out=$(DRY_RUN=1 bash "$LOCAL_HALF" southp@host.test dev 2>&1)
status=$?
if [[ $status -eq 0 && $out == *"sudo bash <staging>/ghost-install.remote.sh $expected_args;"* ]]; then
    pass env_mode_hands_the_host_the_configs_versions
else
    fail env_mode_hands_the_host_the_configs_versions "exit $status" "$out"
fi

# Ad-hoc mode, for staging a version before the config names it, builds the
# paths the host half then accepts: the two halves spell them separately.
out=$(DRY_RUN=1 bash "$LOCAL_HALF" -p 2222 southp@host.test 6.71.0 22.23.2 2>&1)
status=$?
triple="6.71.0 /opt/goopy-life/ghost-6.71.0 /opt/goopy-node/22.23.2/bin/node"
accepted=$(
    GOOPY_GHOST_INSTALL_SOURCED=1 source "$REMOTE_HALF"
    # shellcheck disable=SC2086 # the triple is three words on purpose
    check_triple $triple && echo yes
)
if [[ $status -eq 0 && $out == *"ghost-install.remote.sh $triple;"* && $accepted == yes ]]; then
    pass adhoc_mode_names_paths_the_host_half_accepts
else
    fail adhoc_mode_names_paths_the_host_half_accepts "exit $status, host half accepts: ${accepted:-no}" "$out"
fi

if [[ $out == *"scp -P 2222 "* && $out == *"ssh -t -p 2222 "* && $out == *"ssh -p 2222 "* ]]; then
    pass port_reaches_every_scp_and_ssh
else
    fail port_reaches_every_scp_and_ssh "$out"
fi

out=$(DRY_RUN=1 bash "$LOCAL_HALF" southp@host.test latest 22.23.2 2>&1)
status=$?
if [[ $status -ne 0 && $out != *ssh* ]]; then
    pass adhoc_mode_refuses_a_version_that_is_not_a_release
else
    fail adhoc_mode_refuses_a_version_that_is_not_a_release "exit $status" "$out"
fi

out=$(DRY_RUN=1 bash "$LOCAL_HALF" goopy@host.test dev 2>&1)
status=$?
if [[ $status -ne 0 && $out != *ssh* && $out == *"deploy account"* ]]; then
    pass refuses_the_deploy_account_before_connecting
else
    fail refuses_the_deploy_account_before_connecting "exit $status" "$out"
fi

out=$(DRY_RUN=1 bash "$LOCAL_HALF" southp@host.test nope 2>&1)
status=$?
if [[ $status -ne 0 && $out != *ssh* && $out == *"available environments"* ]]; then
    pass refuses_an_unknown_environment
else
    fail refuses_an_unknown_environment "exit $status" "$out"
fi

rm -rf "$FIXTURES"

# What the host half needs and does not install itself, the bootstrap must:
# a host built by it can then run this with nothing added by hand.
missing=$(
    GOOPY_BOOTSTRAP_SOURCED=1 ROOT=/nonexistent source "$DEPLOY_DIR/host-bootstrap.sh"
    for package in curl xz-utils build-essential python3-setuptools; do
        if [[ " ${BASE_PACKAGES[*]} " != *" $package "* ]]; then
            printf '%s ' "$package"
        fi
    done
)
if [[ -z $missing ]]; then
    pass bootstrap_installs_what_the_host_half_needs
else
    fail bootstrap_installs_what_the_host_half_needs "missing from BASE_PACKAGES: $missing"
fi

# --- Host half ----------------------------------------------------------------

echo
echo "== ghost-install.remote.sh =="

NODE_VERSION=22.23.2
NODE_TARBALL=node-v$NODE_VERSION-linux-x64.tar.xz

# A scratch host with what host-bootstrap.sh leaves: APP_DIR, and GHOST_USER
# (the stub `id` answers for it). `served/` is what the stub curl answers with,
# by the URL's last component: a keyring, a signed checksum list and a Node
# tarball, all consistent until a case breaks one.
make_host() {
    local root
    root=$(mktemp -d)
    mkdir -p "$root/bin" "$root/served" "$root/releases" "$root/opt/goopy-life"
    : >"$root/calls"

    cat >"$root/bin/curl" <<STUB
#!/bin/sh
# curl -fsSL --retry 3 -o <dest> <url>
echo "curl \$6" >>"$root/calls"
src="$root/served/\$(basename "\$6")"
[ -f "\$src" ] || exit 22
cp "\$src" "\$5"
STUB
    cat >"$root/bin/gpgv" <<STUB
#!/bin/sh
# gpgv --quiet --keyring <kbx> --output <out> <in>: a fixture signature is
# the signed text itself. A rejection still writes it out, as the worst case:
# only the exit status may say the signature was bad.
cp "\$6" "\$5"
if [ -e "$root/gpgv-rejects" ]; then
    echo "gpgv: BAD signature" >&2
    exit 1
fi
STUB
    printf '#!/bin/sh\nshift 3\nexec "$@"\n' >"$root/bin/runuser"
    printf '#!/bin/sh\necho "chown $*" >>"%s/calls"\n' "$root" >"$root/bin/chown"
    printf '#!/bin/sh\n[ -e "%s/no-ghost-user" ] && exit 1\necho 994\n' "$root" >"$root/bin/id"
    printf '#!/bin/sh\necho x86_64\n' >"$root/bin/uname"
    # npm pack --silent ghost@<version>: packs releases/<version>/ as npm does,
    # under package/.
    cat >"$root/bin/npm" <<STUB
#!/bin/sh
echo "npm \$*" >>"$root/calls"
version=\${3#ghost@}
tar czf "ghost-\$version.tgz" -C "$root/releases/\$version" package
echo "ghost-\$version.tgz"
STUB
    # The dependency install: lays down whatever SQLite driver the case says
    # the build produced, or fails if it says so.
    cat >"$root/bin/corepack" <<STUB
#!/bin/sh
echo "corepack \$*" >>"$root/calls"
[ -e "$root/install-fails" ] && exit 1
if [ -e "$root/resolves-afresh" ]; then
    printf 'left-pad@^1.0.0:\n  version "1.3.0"\n' >>yarn.lock
fi
mkdir -p node_modules
for driver in \$(cat "$root/builds" 2>/dev/null); do
    mkdir -p "node_modules/\$driver"
    echo "module.exports = {};" >"node_modules/\$driver/index.js"
done
STUB
    # The toolchain Ghost 5 compiles sqlite3 with: present, and Python with
    # distutils, until a case takes it away.
    printf '#!/bin/sh\nexit 0\n' >"$root/bin/make"
    printf '#!/bin/sh\nexit 0\n' >"$root/bin/g++"
    printf '#!/bin/sh\n[ -e "%s/no-distutils" ] && exit 1\nexit 0\n' "$root" >"$root/bin/python3"
    chmod +x "$root/bin/"*

    # Node: a fake that reports its own version and hands everything else to
    # the real one, beside the bundled-npm semver the host half reads. This
    # fake's semver refuses a range of `unsatisfiable` and accepts anything
    # else.
    local tree=$root/node-tree/node-v$NODE_VERSION-linux-x64
    mkdir -p "$tree/bin" "$tree/lib/node_modules/npm/node_modules/semver"
    printf '#!/bin/sh\nif [ "$1" = --version ]; then echo v%s; exit 0; fi\nexec "%s" "$@"\n' \
        "$NODE_VERSION" "${REAL_NODE:-false}" >"$tree/bin/node"
    chmod +x "$tree/bin/node"
    echo "exports.satisfies = (version, range) => range !== 'unsatisfiable';" \
        >"$tree/lib/node_modules/npm/node_modules/semver/index.js"
    tar -cJf "$root/served/$NODE_TARBALL" -C "$root/node-tree" "node-v$NODE_VERSION-linux-x64"

    printf 'the Node release keyring\n' >"$root/served/pubring.kbx"
    printf '%s  %s\n%s  %s\n' \
        "$(sha256_of "$root/served/$NODE_TARBALL")" "$NODE_TARBALL" \
        0000000000000000000000000000000000000000000000000000000000000000 "node-v$NODE_VERSION-linux-arm64.tar.xz" \
        >"$root/served/SHASUMS256.txt.asc"
    printf '%s' "$root"
}

# A Ghost release in releases/<version>/package, as `npm pack` would fetch it.
# $3 is its lockfile, $4 its engines.node, $5 the driver it declares.
make_release() {
    local root=$1 version=$2 lockfile=$3 engines=$4 driver=$5
    local pkg=$root/releases/$version/package
    mkdir -p "$pkg/core" "$pkg/content/themes/casper"
    printf '{"name":"ghost","version":"%s","engines":{"node":"%s"},"optionalDependencies":{"%s":"1.0.0"}}\n' \
        "$version" "$engines" "$driver" >"$pkg/package.json"
    : >"$pkg/index.js"
    : >"$pkg/loggingrc.js"
    : >"$pkg/$lockfile"
    printf '%s\n' "$driver" >"$root/builds"
}

# Runs `<function> <args>` from the sourced host half against `root`, leaving
# its exit status in STATUS and its output, stderr included, in OUT. The keyring pin is the
# fixture's: the real one only ever matches the real file.
host() {
    local root=$1
    shift
    OUT=$(
        exec 2>&1
        export PATH="$root/bin:$PATH" ROOT="$root"
        GOOPY_GHOST_INSTALL_SOURCED=1 source "$REMOTE_HALF"
        NODE_KEYRING_SHA256=$(sha256_of "$root/served/pubring.kbx")
        INSTALL_OWNER=${OWNER:-$ME}
        # A gid no scratch file has, so group write is never excused here.
        INSTALL_GROUP=${GROUP_OK:-4242424}
        "$@"
    )
    STATUS=$?
}

calls() {
    grep -c "$2" "$1/calls"
}

NODE_HOME=/opt/goopy-node/$NODE_VERSION

# ---- Node ----

root=$(make_host)
host "$root" ensure_node "$NODE_VERSION"
if [[ $STATUS -eq 0 && -x $root$NODE_HOME/bin/node && ! -e $root$NODE_HOME.partial \
    && -z $(ls -A "$root$NODE_HOME" | grep -x .download) && $(calls "$root" "chown -R root:root") -eq 1 ]]; then
    pass installs_node_from_its_signed_checksum
else
    fail installs_node_from_its_signed_checksum "exit $STATUS" "$OUT" "$(cat "$root/calls")"
fi

# A second run checks the one it has and downloads nothing.
: >"$root/calls"
host "$root" ensure_node "$NODE_VERSION"
if [[ $STATUS -eq 0 && $OUT == *unchanged* && $(calls "$root" curl) -eq 0 ]]; then
    pass leaves_an_installed_node_alone
else
    fail leaves_an_installed_node_alone "exit $STATUS" "$OUT" "$(cat "$root/calls")"
fi

# Kept only while nobody but its owner can write it.
chmod o+w "$root$NODE_HOME/bin/node"
host "$root" ensure_node "$NODE_VERSION"
if [[ $STATUS -ne 0 && $OUT == *"bin/node is not $ME's alone"* ]]; then
    pass refuses_an_installed_node_others_can_write
else
    fail refuses_an_installed_node_others_can_write "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# A directory of that name with some other Node in it is not silently kept.
root=$(make_host)
mkdir -p "$root$NODE_HOME/bin"
printf '#!/bin/sh\necho v20.0.0\n' >"$root$NODE_HOME/bin/node"
chmod +x "$root$NODE_HOME/bin/node"
host "$root" ensure_node "$NODE_VERSION"
if [[ $STATUS -ne 0 ]]; then
    pass refuses_a_node_directory_holding_another_version
else
    fail refuses_a_node_directory_holding_another_version "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# What a failed run left is cleared, not built on.
root=$(make_host)
mkdir -p "$root$NODE_HOME.partial/bin"
printf 'half a file' >"$root$NODE_HOME.partial/bin/stale"
host "$root" ensure_node "$NODE_VERSION"
if [[ $STATUS -eq 0 && ! -e $root$NODE_HOME/bin/stale && -x $root$NODE_HOME/bin/node ]]; then
    pass clears_what_a_failed_node_install_left
else
    fail clears_what_a_failed_node_install_left "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# Every link in the chain refuses on its own, and leaves no Node behind.
refuses_node() {
    local name=$1 root=$2
    host "$root" ensure_node "$NODE_VERSION"
    if [[ $STATUS -ne 0 && ! -e $root$NODE_HOME ]]; then
        pass "$name"
    else
        fail "$name" "exit $STATUS" "$OUT"
    fi
    rm -rf "$root"
}

root=$(make_host)
OUT=$(
    export PATH="$root/bin:$PATH" ROOT="$root"
    GOOPY_GHOST_INSTALL_SOURCED=1 source "$REMOTE_HALF"
    # The real pin, against the fixture keyring.
    ensure_node "$NODE_VERSION" 2>&1
)
STATUS=$?
if [[ $STATUS -ne 0 && ! -e $root$NODE_HOME && $OUT == *"pinned checksum"* ]]; then
    pass refuses_a_keyring_that_does_not_match_its_pin
else
    fail refuses_a_keyring_that_does_not_match_its_pin "exit $STATUS" "$OUT"
fi
rm -rf "$root"

root=$(make_host)
touch "$root/gpgv-rejects"
refuses_node refuses_checksums_no_release_key_signed "$root"

# A well-formed tarball, just not the one that was signed: appending garbage
# would fail in xz instead, and prove nothing about the checksum.
root=$(make_host)
printf 'not what was released\n' >"$root/node-tree/node-v$NODE_VERSION-linux-x64/bin/extra"
tar -cJf "$root/served/$NODE_TARBALL" -C "$root/node-tree" "node-v$NODE_VERSION-linux-x64"
host "$root" ensure_node "$NODE_VERSION"
if [[ $STATUS -ne 0 && ! -e $root$NODE_HOME && $OUT == *"does not match its signed checksum"* ]]; then
    pass refuses_a_tarball_that_does_not_match_its_signed_checksum
else
    fail refuses_a_tarball_that_does_not_match_its_signed_checksum "exit $STATUS" "$OUT"
fi
rm -rf "$root"

root=$(make_host)
printf '%s  %s\n' 1111111111111111111111111111111111111111111111111111111111111111 "node-v$NODE_VERSION-darwin-arm64.tar.gz" \
    >"$root/served/SHASUMS256.txt.asc"
host "$root" ensure_node "$NODE_VERSION"
if [[ $STATUS -ne 0 && ! -e $root$NODE_HOME && $OUT == *"no $NODE_TARBALL in its SHASUMS256.txt"* ]]; then
    pass refuses_a_release_with_no_tarball_for_this_machine
else
    fail refuses_a_release_with_no_tarball_for_this_machine "exit $STATUS" "$OUT"
fi
rm -rf "$root"

# ---- yarn.lock ----

lockfile=$(mktemp)
cat >"$lockfile" <<'EOF'
# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.
# yarn lockfile v1


"@babel/core@^7.0.0", "@babel/core@^7.12.3":
  version "7.24.0"
  resolved "https://registry.yarnpkg.com/@babel/core/-/core-7.24.0.tgz"

"@tryghost/i18n@file:components/tryghost-i18n-5.130.6.tgz":
  version "5.130.6"
  resolved "file:components/tryghost-i18n-5.130.6.tgz"

abbrev@1, abbrev@^1.0.0:
  version "1.1.1"

sqlite3@5.1.7:
  version "5.1.7"
EOF
got=$(
    GOOPY_GHOST_INSTALL_SOURCED=1 source "$REMOTE_HALF"
    yarn_lock_pins "$lockfile"
)
want="@babel/core@7.24.0
abbrev@1.1.1
sqlite3@5.1.7"
if [[ $got == "$want" ]]; then
    pass yarn_lock_pins_reads_scoped_unquoted_and_bundled_entries
else
    fail yarn_lock_pins_reads_scoped_unquoted_and_bundled_entries "got:" "$got"
fi
rm -f "$lockfile"

# ---- Validation ----

# Nothing is fetched until every triple has been checked.
root=$(make_host)
host "$root" main 6.71.0 /opt/goopy-life/ghost-6.71.0 "$NODE_HOME/bin/node" \
    6.63.0 /opt/goopy-life/ghost "$NODE_HOME/bin/node"
if [[ $STATUS -ne 0 && $(calls "$root" .) -eq 0 ]]; then
    pass refuses_a_source_dir_not_named_for_its_version_before_fetching
else
    fail refuses_a_source_dir_not_named_for_its_version_before_fetching "exit $STATUS" "$(cat "$root/calls")"
fi

host "$root" main latest /opt/goopy-life/ghost-latest "$NODE_HOME/bin/node"
if [[ $STATUS -ne 0 && $(calls "$root" .) -eq 0 ]]; then
    pass refuses_a_version_that_is_not_a_release
else
    fail refuses_a_version_that_is_not_a_release "exit $STATUS"
fi

# A node_bin outside /opt/goopy-node is the host's own: checked, never installed.
host "$root" main 6.71.0 /opt/goopy-life/ghost-6.71.0 /usr/local/bin/node
if [[ $STATUS -ne 0 && $(calls "$root" .) -eq 0 ]]; then
    pass refuses_a_missing_node_bin_it_cannot_install
else
    fail refuses_a_missing_node_bin_it_cannot_install "exit $STATUS"
fi

host "$root" main 6.71.0 /opt/goopy-life/ghost-6.71.0
if [[ $STATUS -ne 0 && $(calls "$root" .) -eq 0 ]]; then
    pass refuses_an_incomplete_triple
else
    fail refuses_an_incomplete_triple "exit $STATUS"
fi

touch "$root/no-ghost-user"
host "$root" main 6.71.0 /opt/goopy-life/ghost-6.71.0 "$NODE_HOME/bin/node"
if [[ $STATUS -ne 0 && $(calls "$root" .) -eq 0 ]]; then
    pass refuses_a_host_without_the_ghost_account
else
    fail refuses_a_host_without_the_ghost_account "exit $STATUS"
fi
rm -rf "$root"

# ---- Ghost ----

if [[ -z $REAL_NODE ]]; then
    skip "the Ghost cases" "no node on PATH"
else
    GHOST_671=/opt/goopy-life/ghost-6.71.0
    GHOST_5=/opt/goopy-life/ghost-5.130.6

    root=$(make_host)
    make_release "$root" 6.71.0 pnpm-lock.yaml "^22.23.1 || ^24.20.0" better-sqlite3
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -eq 0 && -f $root$GHOST_671/index.js && ! -e $root$GHOST_671.partial && ! -e $root$GHOST_671.home \
        && $(calls "$root" "corepack pnpm install --prod --frozen-lockfile") -eq 1 \
        && $(calls "$root" "chown -R root:root $root$GHOST_671.partial") -eq 1 && $OUT == *"better-sqlite3 loads"* ]]; then
        pass builds_a_pnpm_release_with_pnpm_and_hands_it_to_root
    else
        fail builds_a_pnpm_release_with_pnpm_and_hands_it_to_root "exit $STATUS" "$OUT" "$(cat "$root/calls")"
    fi

    # The re-run verifies the install it has and builds nothing.
    : >"$root/calls"
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -eq 0 && $OUT == *"unchanged  $GHOST_671"* && $(calls "$root" "npm\|corepack\|curl") -eq 0 ]]; then
        pass leaves_an_installed_release_alone
    else
        fail leaves_an_installed_release_alone "exit $STATUS" "$OUT" "$(cat "$root/calls")"
    fi

    # The install dev was found with (#212): built by hand as the deploy
    # account, group-writable. Every other check passes; this one must not.
    chmod g+w "$root$GHOST_671/package.json"
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -ne 0 && $OUT == *"package.json is not $ME's alone"* && $OUT == *"chmod -R go-w"* ]]; then
        pass refuses_an_installed_release_its_group_can_write
    else
        fail refuses_an_installed_release_its_group_can_write "exit $STATUS" "$OUT"
    fi

    # ...unless the group is the owner's own: node-gyp leaves its build
    # directories 775, and on a host that group is root's.
    file_group=$(stat -c %G "$root$GHOST_671/package.json" 2>/dev/null || stat -f %Sg "$root$GHOST_671/package.json")
    GROUP_OK=$file_group host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -eq 0 && $OUT == *"unchanged  $GHOST_671"* ]]; then
        pass allows_group_write_by_the_owners_own_group
    else
        fail allows_group_write_by_the_owners_own_group "exit $STATUS" "$OUT"
    fi
    chmod g-w "$root$GHOST_671/package.json"

    # Owned by anyone but INSTALL_OWNER is refused the same way. The Ghost
    # step alone: through main, the Node it shares would be refused first.
    OWNER=nobody host "$root" ensure_ghost 6.71.0 "$NODE_HOME/bin/node"
    if [[ $STATUS -ne 0 && $OUT == *"$GHOST_671"*" is not nobody's alone (owner $ME"* ]]; then
        pass refuses_an_installed_release_someone_else_owns
    else
        fail refuses_an_installed_release_someone_else_owns "exit $STATUS" "$OUT"
    fi

    # An install whose driver went missing since, or was built for another
    # Node and so cannot load, is reported rather than reported unchanged.
    rm -rf "$root$GHOST_671/node_modules/better-sqlite3"
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -ne 0 ]]; then
        pass refuses_an_installed_release_whose_driver_does_not_load
    else
        fail refuses_an_installed_release_whose_driver_does_not_load "exit $STATUS" "$OUT"
    fi
    rm -rf "$root"

    # Ghost 5: yarn classic, sqlite3. Its yarn.lock is the monorepo's, which
    # a production install prunes, so it is not --frozen-lockfile.
    root=$(make_host)
    make_release "$root" 5.130.6 yarn.lock "^18.12.1 || ^20.11.1 || ^22.13.1" sqlite3
    printf 'left-pad@1.0.0:\n  version "1.0.0"\n' >"$root/releases/5.130.6/package/yarn.lock"
    host "$root" main 5.130.6 "$GHOST_5" "$NODE_HOME/bin/node"
    if [[ $STATUS -eq 0 && -f $root$GHOST_5/index.js && $(calls "$root" "corepack yarn@1.22.22 install --production") -eq 1 \
        && $(calls "$root" "pnpm") -eq 0 && $OUT == *"sqlite3 loads"* ]]; then
        pass builds_a_yarn_release_with_yarn_classic
    else
        fail builds_a_yarn_release_with_yarn_classic "exit $STATUS" "$OUT" "$(cat "$root/calls")"
    fi
    rm -rf "$root"

    # It may drop pins, never add one: that would be a dependency nobody
    # tested, resolved on the day.
    root=$(make_host)
    make_release "$root" 5.130.6 yarn.lock "^22.13.1" sqlite3
    printf 'left-pad@1.0.0:\n  version "1.0.0"\n' >"$root/releases/5.130.6/package/yarn.lock"
    touch "$root/resolves-afresh"
    host "$root" main 5.130.6 "$GHOST_5" "$NODE_HOME/bin/node"
    if [[ $STATUS -ne 0 && ! -e $root$GHOST_5 && $OUT == *"left-pad@1.3.0"* ]]; then
        pass refuses_a_yarn_install_that_resolves_what_the_lockfile_does_not_pin
    else
        fail refuses_a_yarn_install_that_resolves_what_the_lockfile_does_not_pin "exit $STATUS" "$OUT"
    fi
    rm -rf "$root"

    # Without distutils its sqlite3 cannot compile, and yarn would call the
    # install a success anyway: refused before it starts.
    root=$(make_host)
    make_release "$root" 5.130.6 yarn.lock "^22.13.1" sqlite3
    touch "$root/no-distutils"
    host "$root" main 5.130.6 "$GHOST_5" "$NODE_HOME/bin/node"
    if [[ $STATUS -ne 0 && $(calls "$root" corepack) -eq 0 && $OUT == *python3-setuptools* ]]; then
        pass refuses_a_yarn_release_without_the_toolchain_to_compile_sqlite3
    else
        fail refuses_a_yarn_release_without_the_toolchain_to_compile_sqlite3 "exit $STATUS" "$OUT" "$(cat "$root/calls")"
    fi
    rm -rf "$root"

    # Two releases on one Node fetch it once.
    root=$(make_host)
    make_release "$root" 5.130.6 yarn.lock "^22.13.1" sqlite3
    make_release "$root" 6.71.0 pnpm-lock.yaml "^22.23.1" better-sqlite3
    printf 'sqlite3\nbetter-sqlite3\n' >"$root/builds"
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node" 5.130.6 "$GHOST_5" "$NODE_HOME/bin/node"
    if [[ $STATUS -eq 0 && -f $root$GHOST_671/index.js && -f $root$GHOST_5/index.js \
        && $(calls "$root" "curl .*$NODE_TARBALL") -eq 1 && $(grep -c "^== Node" <<<"$OUT") -eq 1 ]]; then
        pass fetches_a_node_shared_by_two_releases_once
    else
        fail fetches_a_node_shared_by_two_releases_once "exit $STATUS" "$OUT" "$(cat "$root/calls")"
    fi
    rm -rf "$root"

    # A Node the release refuses costs seconds, not the install.
    root=$(make_host)
    make_release "$root" 6.71.0 pnpm-lock.yaml unsatisfiable better-sqlite3
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -ne 0 && ! -e $root$GHOST_671 && $(calls "$root" corepack) -eq 0 ]]; then
        pass refuses_a_node_outside_engines_before_installing
    else
        fail refuses_a_node_outside_engines_before_installing "exit $STATUS" "$OUT" "$(cat "$root/calls")"
    fi
    rm -rf "$root"

    # Ghost 6 declares its driver optional: a failed native build does not fail
    # the install, so the check after it is what catches it.
    root=$(make_host)
    make_release "$root" 6.71.0 pnpm-lock.yaml "^22.23.1" better-sqlite3
    : >"$root/builds"
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -ne 0 && ! -e $root$GHOST_671 ]]; then
        pass refuses_a_build_whose_optional_driver_was_skipped
    else
        fail refuses_a_build_whose_optional_driver_was_skipped "exit $STATUS" "$OUT"
    fi
    rm -rf "$root"

    root=$(make_host)
    make_release "$root" 6.71.0 pnpm-lock.yaml "^22.23.1" better-sqlite3
    rm "$root/releases/6.71.0/package/loggingrc.js"
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -ne 0 && ! -e $root$GHOST_671 && $OUT == *"no loggingrc.js"* ]]; then
        pass refuses_a_release_missing_what_instances_link_against
    else
        fail refuses_a_release_missing_what_instances_link_against "exit $STATUS" "$OUT"
    fi
    rm -rf "$root"

    root=$(make_host)
    make_release "$root" 6.71.0 pnpm-lock.yaml "^22.23.1" better-sqlite3
    touch "$root/install-fails"
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    rm "$root/install-fails"
    # Whatever the failed run left must not ride into the finished install.
    printf 'left by a failed run' >"$root$GHOST_671.partial/stale"
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -eq 0 && -f $root$GHOST_671/index.js && ! -e $root$GHOST_671.partial && ! -e $root$GHOST_671/stale ]]; then
        pass a_rerun_finishes_what_a_failed_install_left
    else
        fail a_rerun_finishes_what_a_failed_install_left "exit $STATUS" "$OUT"
    fi
    rm -rf "$root"

    # An install under the right name holding another release.
    root=$(make_host)
    make_release "$root" 6.71.0 pnpm-lock.yaml "^22.23.1" better-sqlite3
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    sed -i.bak 's/"version":"6.71.0"/"version":"6.70.0"/' "$root$GHOST_671/package.json"
    host "$root" main 6.71.0 "$GHOST_671" "$NODE_HOME/bin/node"
    if [[ $STATUS -ne 0 ]]; then
        pass refuses_an_install_holding_another_release
    else
        fail refuses_an_install_holding_another_release "exit $STATUS" "$OUT"
    fi
    rm -rf "$root"
fi

echo
if [[ "$FAILURES" -eq 0 ]]; then
    echo "$CASES passed, 0 failed"
else
    echo "$((CASES - FAILURES)) passed, $FAILURES failed"
    exit 1
fi
