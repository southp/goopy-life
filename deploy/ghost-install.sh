#!/usr/bin/env bash
# Usage: ./deploy/ghost-install.sh [-p ssh-port] <admin@host> <env>
#        ./deploy/ghost-install.sh [-p ssh-port] <admin@host> <ghost-version> <node-version>
#
#   admin  an account with password sudo on the host -- NOT the deploy account
#   env    the environment the host runs, naming a file in deploy/config/
#
# Prepares, in one run and one sudo password prompt, every Ghost base install a
# host needs, each with the Node it runs on (#210):
#
#   /opt/goopy-node/<node-version>/       the official Node release, verified
#   /opt/goopy-life/ghost-<ghost-version>/  the shared install instances link to
#
# With <env>, it installs every Ghost version deploy/config/<env>.toml names,
# with the node_bin each entry names: one run makes the host ready for that
# config. A node_bin under /opt/goopy-node is installed; any other (the
# NodeSource /usr/bin/node) has to exist already, and is only checked.
#
# With <ghost-version> <node-version>, it installs that one pair, ahead of the
# config that names it. That is the order a new version has to go in:
# `gl-serv --check-config` refuses a deploy whose config names an install the
# host does not have, and dev deploys on every merge to trunk. So stage the
# version on dev, then merge the config. The run ends by printing the keys to
# add.
#
# Idempotent: what is already there is verified, not rebuilt, and reported
# `unchanged`. The work on the host is deploy/ghost-install.remote.sh, which
# says what is verified and how. A new Ghost takes a few minutes, most of it
# its dependency install.
#
# Set DRY_RUN=1 to print the scp/ssh commands instead of running them.
set -euo pipefail

USAGE="Usage: ghost-install.sh [-p ssh-port] <admin@host> <env>
       ghost-install.sh [-p ssh-port] <admin@host> <ghost-version> <node-version>"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=host-artifacts.sh
source "$HERE/host-artifacts.sh"

# Where the host half puts them, and refuses anything else for: spelled out
# again here only to build the ad-hoc pair's paths, which the host half then
# checks like any other.
APP_DIR=/opt/goopy-life
NODE_DIR=/opt/goopy-node

# What `node_bin` is when the config leaves it out: gl-core's
# default_node_bin().
DEFAULT_NODE_BIN=/usr/bin/node

die() {
    echo "ghost-install.sh: $1" >&2
    exit 1
}

# Prints `<version> <source_dir> <node_bin>` for every Ghost version the config
# at $1 names, in order: the [provisioner] section itself, and each
# [[provisioner.versions]] entry. Nothing for a provisioner that is not Ghost,
# which names none.
#
# A line reader, not a TOML parser: it reads `key = "value"` at the start of a
# line, which is how every committed config is written, and skips anything
# commented out. CI parses the same files with the real parser
# (gl-core/tests/committed_configs.rs), and the tests run this against them.
config_versions() {
    awk -v default_node="$DEFAULT_NODE_BIN" '
        function value(line) {
            match(line, /"[^"]*"/)
            return substr(line, RSTART + 1, RLENGTH - 2)
        }
        function flush() {
            if (in_provisioner && version != "") {
                print version, source_dir, (node_bin == "" ? default_node : node_bin)
            }
            version = source_dir = node_bin = ""
        }
        /^[[:space:]]*\[/ {
            flush()
            in_provisioner = ($0 ~ /^[[:space:]]*\[provisioner\][[:space:]]*(#.*)?$/ ||
                $0 ~ /^[[:space:]]*\[\[provisioner\.versions\]\][[:space:]]*(#.*)?$/)
            next
        }
        in_provisioner && /^[[:space:]]*version[[:space:]]*=/ { version = value($0) }
        in_provisioner && /^[[:space:]]*source_dir[[:space:]]*=/ { source_dir = value($0) }
        in_provisioner && /^[[:space:]]*node_bin[[:space:]]*=/ { node_bin = value($0) }
        END { flush() }
    ' "$1"
}

# Prints the [[provisioner.versions]] entry that offers the install named by
# `<version> <source_dir> <node_bin>`, ready to paste into a config.
config_entry() {
    echo "[[provisioner.versions]]"
    echo "version = \"$1\""
    echo "source_dir = \"$2\""
    echo "node_bin = \"$3\""
}

is_release() {
    [[ $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
}

main() {
    local port=22
    if [[ ${1:-} == -p ]]; then
        port=${2:?"$USAGE"}
        shift 2
    fi
    if (($# != 2 && $# != 3)); then
        die "$USAGE"
    fi
    local target=$1 environment="" config=""
    local triples=()

    if (($# == 2)); then
        environment=$2
        config="$HERE/config/$environment.toml"
        if ! require_files ghost-install.sh "config for environment '$environment'" "$config"; then
            list_environments
            exit 1
        fi
        local version source_dir node_bin
        while read -r version source_dir node_bin; do
            triples+=("$version" "$source_dir" "$node_bin")
        done < <(config_versions "$config")
        if ((${#triples[@]} == 0)); then
            echo "ghost-install.sh: deploy/config/$environment.toml names no Ghost version; nothing to install"
            exit 0
        fi
    else
        local ghost_version=$2 node_version=$3
        if ! is_release "$ghost_version"; then
            die "'$ghost_version' is not a Ghost release (expected e.g. 6.71.0)"
        fi
        if ! is_release "$node_version"; then
            die "'$node_version' is not a Node release (expected e.g. 22.23.2)"
        fi
        triples=("$ghost_version" "$APP_DIR/ghost-$ghost_version" "$NODE_DIR/$node_version/bin/node")
    fi

    # They go into a remote command line, in whatever the admin's login shell
    # is: nothing in them may be something that shell would interpret.
    local arg
    for arg in "${triples[@]}"; do
        if [[ ! $arg =~ ^[A-Za-z0-9._/-]+$ ]]; then
            die "refusing '$arg' from ${config:-the command line}: only letters, digits and ._/- are passed to the host"
        fi
    done

    # Refuses the deploy account before asking it for a password it cannot have:
    # `goopy` can only run the pinned commands in deploy/sudoers.goopy.
    if [[ "${target%%@*}" == goopy ]]; then
        die "$target is the deploy account; run this as an admin with password sudo"
    fi

    # A private staging directory made by the admin account, as admin-apply.sh
    # does and for the same reason: nothing the deploy account can write should
    # be what root runs. The same connection catches an ssh alias for the
    # deploy account.
    local make_staging="if [ \"\$(id -un)\" = goopy ]; then echo goopy; else mktemp -d; fi"
    local staging
    if [[ "${DRY_RUN:-0}" == "1" ]]; then
        staging="<staging>"
        printf '%s\n' "ssh -p $port $target $make_staging"
    else
        staging=$(ssh -p "$port" "$target" "$make_staging")
    fi
    if [[ "$staging" == goopy ]]; then
        die "$target logs in as goopy, the deploy account; run this as an admin with password sudo"
    fi

    run scp -P "$port" "$HERE/ghost-install.remote.sh" "$target:$staging/ghost-install.remote.sh"

    # -t so sudo can ask for the password, once. The exit code is kept in `rc`,
    # not `status`, which zsh treats as read-only.
    run ssh -t -p "$port" "$target" "sudo bash $staging/ghost-install.remote.sh ${triples[*]}; rc=\$?; rm -rf $staging; exit \$rc"

    if [[ "${DRY_RUN:-0}" != "1" && -z $environment ]]; then
        echo
        echo "ghost-install.sh: done. To offer Ghost ${triples[0]}, add this entry to deploy/config/<env>.toml,"
        echo "after the [provisioner] keys (point default_version at it to make it the default):"
        echo
        config_entry "${triples[@]}"
    fi
}

if [[ ${GOOPY_GHOST_INSTALL_SOURCED:-0} != 1 ]]; then
    main "$@"
fi
