#!/usr/bin/env bash
# Usage: ./deploy/deploy.sh user@droplet [ssh-port]
#
# Manual deploy path — this is how production gets updated. The dev droplet is
# deployed automatically on every merge to trunk by
# .github/workflows/backend-deploy.yml; see docs/DEPLOYMENT.md.
#
# Cross-compiles gl-serv for x86_64 Linux and hands the binary to
# deploy/push-binary.sh, which installs it and restarts the systemd service.
#
# One-time setup on your local machine (macOS):
#   rustup target add x86_64-unknown-linux-musl
#   cargo install cargo-zigbuild
#   brew install zig
#
# See README.md for full cross-compilation setup instructions.
set -euo pipefail

TARGET=${1:?"Usage: deploy.sh user@droplet [ssh-port]"}
PORT=${2:-22}

HERE="$(cd "$(dirname "$0")" && pwd)"

cd "$HERE/../backend"
cargo zigbuild --release --target x86_64-unknown-linux-musl -p gl-serv
"$HERE/push-binary.sh" "$TARGET" target/x86_64-unknown-linux-musl/release/gl-serv "$PORT"
