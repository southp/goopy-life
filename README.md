# Goopy.Life

## What is it?

Goopy.life is an accountless service for creating ephemeral Ghost sites; just like poopy.life for WordPress.

Users can create an ephemeral Ghost instance on Goopy.Life by just one click. The created instance will live for a limited of time with minimal resource that should be just enough for exploring Ghost but inadequate for any production usage. 

## Everything is WIP 🚧

I'm continuously rolling out updates in the dev instance at https://southp.dev. Currently, it has the full flow connected, but only provisions a [placeholder python server](https://github.com/southp/goopy-life/blob/trunk/backend/gl-core/src/goopy_provisioner/hello_provisioner.rs) that does nothing more than greeting you. Play at your own risk 💩

## Deployment

The `deploy/` directory contains all artifacts needed to run the service on the droplet.

The dev droplet and the Vercel frontend deploy themselves on every merge to
`trunk`; production is deployed by hand. See [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md)
for the full picture and the one-time GitHub setup.

### Cross-compilation setup (one-time, on macOS)

The droplet runs x86_64 Linux. Install the musl target and `cargo-zigbuild` (uses [Zig](https://ziglang.org/) as the cross-linker — no extra toolchain taps required):

```bash
rustup target add x86_64-unknown-linux-musl
cargo install cargo-zigbuild
brew install zig
```

### Droplet setup (one-time)

The service runs as a dedicated `goopy` account, which is also the account you deploy as — the sudoers drop-in below names it explicitly, so deploying as any other user fails with a password prompt.

```bash
# 0. Create the service account and authorise your deploy key for it
sudo useradd --system --create-home --shell /bin/bash goopy
ssh-copy-id goopy@<droplet>

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

# 4. Give the deploy account ownership of the service directory. The deploy
#    writes /opt/goopy-life/config.toml directly, so this must not be root-owned.
sudo install -d -o goopy -g goopy /opt/goopy-life /opt/goopy-life/bin
```

There is no step for `config.toml`: it is version-controlled under [`deploy/config/`](deploy/config/) and installed by the deploy itself.

### Deploying

```bash
./deploy/deploy.sh goopy@droplet <env> [ssh-port]   # e.g. goopy@droplet dev
```

This cross-compiles `gl-serv` to a fully static musl binary, uploads it to the droplet along with `deploy/config/<env>.toml`, restarts the `gl-serv` systemd service, and verifies the service came back up.

The environment is required and has no default: the config is shipped to the host, so a default would quietly reconfigure one environment with another's settings. Edit `deploy/config/<env>.toml` and deploy — a hand-edit on the droplet is overwritten by the next run.

This is the manual path, used for production. The dev droplet is deployed automatically by [`.github/workflows/backend-deploy.yml`](.github/workflows/backend-deploy.yml) on every merge to `trunk` — both share `deploy/push-binary.sh` for the remote half, so they cannot drift apart.
