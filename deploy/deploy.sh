#!/usr/bin/env bash
# Usage: ./deploy/deploy.sh user@droplet <env> [ssh-port]
#
#   env   the environment to deploy, naming a file in deploy/config/
#         (e.g. `dev` -> deploy/config/dev.toml)
#
# Manual deploy path — this is how production gets updated. The dev droplet is
# deployed automatically on every merge to trunk by
# .github/workflows/backend-deploy.yml; see docs/DEPLOYMENT.md.
#
# Cross-compiles gl-serv and gl-cli for x86_64 Linux and hands both binaries and
# that environment's config to deploy/push-binary.sh, which installs all three
# and restarts the systemd service.
#
# gl-cli is built from the same invocation as gl-serv rather than separately:
# the two link the same gl-core, so a droplet must never hold a pair built from
# different commits.
#
# The environment is a required argument with no default: the config is shipped
# to the host, so a default would quietly reconfigure one environment with
# another's settings the first time someone omitted it.
#
# One-time setup on your local machine (macOS):
#   rustup target add x86_64-unknown-linux-musl
#   cargo install cargo-zigbuild
#   brew install zig
#
# See README.md for full cross-compilation setup instructions.
set -euo pipefail

TARGET=${1:?"Usage: deploy.sh user@droplet <env> [ssh-port]"}
ENVIRONMENT=${2:?"Usage: deploy.sh user@droplet <env> [ssh-port]"}
PORT=${3:-22}

HERE="$(cd "$(dirname "$0")" && pwd)"
CONFIG="$HERE/config/$ENVIRONMENT.toml"

if [[ ! -f "$CONFIG" ]]; then
    echo "deploy.sh: no configuration for environment '$ENVIRONMENT'" >&2
    echo "deploy.sh: available environments:" >&2
    for candidate in "$HERE"/config/*.toml; do
        echo "  $(basename "$candidate" .toml)" >&2
    done
    exit 1
fi

# Stamp the build with the commit it is made from, so `GET /version` on the
# droplet can be compared against what this run built (see push-binary.sh) and
# so "which commit is serving?" has an answer that is not a guess at file
# mtimes.
#
# `--untracked-files=no` on purpose: a build's inputs cannot change without some
# tracked file changing too (a new module has to be declared in one), while an
# untracked scratch file in the checkout is not a difference in what ships.
# Counting those would mark every manual deploy dirty, which makes the suffix
# mean nothing precisely when it needs to mean something.
#
# The suffix is not cosmetic: a hand-deployed working copy that claims to be a
# clean commit is the exact failure this stamp exists to eliminate, reproduced
# inside the fix.
if GIT_SHA=$(git -C "$HERE" rev-parse HEAD 2>/dev/null); then
    if [[ -n "$(git -C "$HERE" status --porcelain --untracked-files=no)" ]]; then
        GIT_SHA="$GIT_SHA-dirty"
        echo "deploy.sh: working tree is dirty; deploying as $GIT_SHA" >&2
    fi
else
    echo "deploy.sh: not a git checkout; the deploy will report an unknown commit" >&2
    GIT_SHA=unknown
fi

# Exported rather than passed per-command: the same value has to reach both the
# build below and push-binary.sh's identity assertion, and a second derivation
# there could disagree with this one.
BUILT_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
export GL_GIT_SHA="$GIT_SHA"
export GL_BUILT_AT="$BUILT_AT"

cd "$HERE/../backend"
cargo zigbuild --release --target x86_64-unknown-linux-musl -p gl-serv -p gl-cli
"$HERE/push-binary.sh" "$TARGET" \
    target/x86_64-unknown-linux-musl/release/gl-serv \
    target/x86_64-unknown-linux-musl/release/gl-cli \
    "$CONFIG" "$PORT"
