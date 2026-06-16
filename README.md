# Goopy.Life

## What is it?

Goopy.life is an accountless service for creating ephemeral Ghost sites; just like poopy.life for WordPress.

Users can create an ephemeral Ghost instance on Goopy.Life by just one click. The created instance will live for a limited of time with minimal resource that should be just enough for exploring Ghost but inadequate for any production usage. 

## Everything is WIP 🚧

So nothing good to see here 🙈

## Deployment

The `deploy/` directory contains all artifacts needed to run the service on the droplet.

### Cross-compilation setup (one-time, on macOS)

The droplet runs x86_64 Linux. Before running `deploy/deploy.sh` for the first time, add the Linux target:

```bash
rustup target add x86_64-unknown-linux-gnu
```

You will also need a cross-compilation linker. The easiest option on macOS is the `musl-cross` toolchain via Homebrew:

```bash
brew install FiloSottile/musl-cross/musl-cross
```

Then add the following to `backend/.cargo/config.toml` (create it if it does not exist):

```toml
[target.x86_64-unknown-linux-gnu]
linker = "x86_64-linux-musl-gcc"
```

### Deploying

```bash
./deploy/deploy.sh user@droplet
```

This cross-compiles `gl-serv`, uploads the binary and static pages to the droplet, and restarts the `gl-serv` systemd service.
