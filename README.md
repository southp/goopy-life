# Goopy.Life

## What is it?

Goopy.life is an accountless service for creating ephemeral Ghost sites; just like poopy.life for WordPress.

Users can create an ephemeral Ghost instance on Goopy.Life by just one click. The created instance will live for a limited of time with minimal resource that should be just enough for exploring Ghost but inadequate for any production usage. 

## Everything is WIP 🚧

I'm continuously rolling out updates in the dev instance at https://southp.dev. Play at your own risk 💩

## Quick start

Run the whole thing locally. This uses the **Hello** provisioner: each instance is a
one-line "Hello, I am <slug>" page instead of a Ghost site, so there is no Ghost install,
no root and no ZFS.

**You need:** [rustup](https://rustup.rs) (the Rust version is pinned and installs itself),
Node 22 with Yarn 1, and `python3` (each Hello instance is a tiny Python server).

**1. Backend.** The API, on `127.0.0.1:3001`:

```bash
cd backend
cargo run -p gl-serv -- --config config.local.toml
```

**2. Frontend.** In a second terminal:

```bash
cd frontend
cp .env.local.example .env.local   # already points at the backend above
yarn install
yarn dev
```

Open http://localhost:3000 and click **Ghost now!**

**3. gl-cli.** The same instances from the command line, run from `backend/`:

```bash
alias gl-cli='cargo run -q -p gl-cli -- --config config.local.toml'
gl-cli spawn        # create one
gl-cli list         # what exists, and on which port
gl-cli events       # why a spawn failed
gl-cli despawn <slug>
```

Everything a run creates lands in `backend/.local/`. To start over, despawn your
instances and then delete that directory. Deleting it first leaves their processes running.

**Real Ghost instead of Hello:** prepare a base Ghost install following
[docs/GHOST_PROVISIONER.md](docs/GHOST_PROVISIONER.md), then switch `[provisioner]` in
`backend/config.local.toml` to `"Ghost"`. The commented block there lists the keys.

## Docs

- [Development](docs/DEVELOPMENT.md) — architecture, crates, configuration reference
- [Deployment](docs/DEPLOYMENT.md) — the hosts, Vercel, and production
- [Ghost provisioner](docs/GHOST_PROVISIONER.md) — preparing and upgrading the base Ghost install
