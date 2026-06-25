#!/usr/bin/env bash
# Usage: ./deploy/deploy.sh user@droplet
#
# Cross-compiles gl-serv for x86_64 Linux and uploads the binary to the
# droplet, then restarts the gl-serv systemd service.
#
# One-time setup on your local machine (macOS):
#   rustup target add x86_64-unknown-linux-musl
#   cargo install cargo-zigbuild
#   brew install zig
#
# See README.md for full cross-compilation setup instructions.
set -euo pipefail

TARGET=${1:?"Usage: deploy.sh user@droplet"}

cd "$(dirname "$0")/../backend"
cargo zigbuild --release --target x86_64-unknown-linux-musl -p gl-serv
scp target/x86_64-unknown-linux-musl/release/gl-serv "$TARGET:/tmp/gl-serv"
ssh "$TARGET" "sudo install -m 755 /tmp/gl-serv /opt/goopy-life/bin/gl-serv && rm /tmp/gl-serv"
ssh "$TARGET" sudo systemctl restart gl-serv
