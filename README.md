# Goopy.Life

## What is it?

Goopy.life is an accountless service for creating ephemeral Ghost sites; just like poopy.life for WordPress.

Users can create an ephemeral Ghost instance on Goopy.Life by just one click. The created instance will live for a limited of time with minimal resource that should be just enough for exploring Ghost but inadequate for any production usage. 

## Everything is WIP 🚧

So nothing good to see here 🙈

## Deployment

The `deploy/` directory contains all artifacts needed to run the service on the droplet.

### Cross-compilation setup (one-time, on macOS)

The droplet runs x86_64 Linux. Install the musl target and `cargo-zigbuild` (uses [Zig](https://ziglang.org/) as the cross-linker — no extra toolchain taps required):

```bash
rustup target add x86_64-unknown-linux-musl
cargo install cargo-zigbuild
brew install zig
```

### Deploying

```bash
./deploy/deploy.sh user@droplet
```

This cross-compiles `gl-serv` to a fully static musl binary, uploads it to the droplet, and restarts the `gl-serv` systemd service.
