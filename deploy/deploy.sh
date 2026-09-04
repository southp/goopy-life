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
# Cross-compiles gl-serv for x86_64 Linux and hands the binary and that
# environment's config to deploy/push-binary.sh, which installs both and
# restarts the systemd service.
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

cd "$HERE/../backend"
cargo zigbuild --release --target x86_64-unknown-linux-musl -p gl-serv
"$HERE/push-binary.sh" "$TARGET" target/x86_64-unknown-linux-musl/release/gl-serv "$CONFIG" "$PORT"
