#!/usr/bin/env bash
# Usage: ./deploy/deploy.sh user@droplet
#
# Cross-compiles gl-serv for x86_64 Linux, uploads the binary and static pages
# to the droplet, then restarts the gl-serv systemd service.
#
# One-time setup on your local machine (macOS):
#   rustup target add x86_64-unknown-linux-gnu
#   brew install FiloSottile/musl-cross/musl-cross   # or use cross / docker
#
# See README.md for full cross-compilation setup instructions.
set -euo pipefail

TARGET=${1:?"Usage: deploy.sh user@droplet"}

cargo build --release --target x86_64-unknown-linux-gnu -p gl-serv
scp target/x86_64-unknown-linux-gnu/release/gl-serv "$TARGET:/opt/goopy-life/bin/gl-serv"
scp -r deploy/pages/ "$TARGET:/opt/goopy-life/pages/"
ssh "$TARGET" sudo systemctl restart gl-serv
