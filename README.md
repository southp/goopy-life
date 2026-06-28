# Goopy.Life

## What is it?

Goopy.life is an accountless service for creating ephemeral Ghost sites; just like poopy.life for WordPress.

Users can create an ephemeral Ghost instance on Goopy.Life by just one click. The created instance will live for a limited of time with minimal resource that should be just enough for exploring Ghost but inadequate for any production usage. 

## Everything is WIP 🚧

I'm continuously rolling out updates in the dev instance at https://southp.dev. Currently, it has the full flow connected, but only provisions a [placeholder python server](https://github.com/southp/goopy-life/blob/trunk/backend/gl-core/src/goopy_provisioner/hello_provisioner.rs) that does nothing more than greeting you. Play at your own risk 💩

## Deployment

The `deploy/` directory contains all artifacts needed to run the service on the droplet.

### Cross-compilation setup (one-time, on macOS)

The droplet runs x86_64 Linux. Install the musl target and `cargo-zigbuild` (uses [Zig](https://ziglang.org/) as the cross-linker — no extra toolchain taps required):

```bash
rustup target add x86_64-unknown-linux-musl
cargo install cargo-zigbuild
brew install zig
```

### Droplet setup (one-time)

```bash
# 1. Install the systemd unit and sudoers drop-in
sudo cp deploy/gl-serv.service /etc/systemd/system/gl-serv.service
sudo cp deploy/sudoers.goopy /etc/sudoers.d/goopy
sudo chmod 0440 /etc/sudoers.d/goopy
sudo systemctl daemon-reload
sudo systemctl enable gl-serv

# 2. Install the nginx reverse-proxy config
sudo cp deploy/nginx.api.goopy.life /etc/nginx/sites-available/api.goopy.life
sudo ln -s /etc/nginx/sites-available/api.goopy.life /etc/nginx/sites-enabled/api.goopy.life
sudo nginx -t && sudo systemctl reload nginx

# 3. Set the ZFS pool mountpoint to match base_dir in config.toml (default: /opt/goopy-life/data).
#    gl-serv creates/destroys child datasets via sudo (sudoers rules restrict to zpool_ghost/*).
#    NoNewPrivileges is intentionally omitted from the unit to allow this; see issue #90 for
#    the long-term fix (privilege-separated ZFS helper).
sudo zfs set mountpoint=/opt/goopy-life/data zpool_ghost
```

### Deploying

```bash
./deploy/deploy.sh user@droplet
```

This cross-compiles `gl-serv` to a fully static musl binary, uploads it to the droplet, and restarts the `gl-serv` systemd service.
