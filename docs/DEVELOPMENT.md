# Development

## Project structure

```
goopy-life
    |- backend/
        |- gl-core/    — core library (GoopyManager, traits, types)
        |- gl-cli/     — CLI tool (clap)
        |- gl-serv/    — HTTP API server
    |- frontend/       — Next.js app (deployed on Vercel)
    |- docs/
    |- deploy/
```

In the following, we will refer to the ephemeral Ghost instance created on our service as "goopy". Each goopy is identified by a **slug** in adjective-adjective-noun format (e.g. `brave-quiet-walrus`).

## Backend

The backend is a Rust workspace (edition 2024) consisting of three crates. All commands run from `backend/`:

```bash
cargo build            # build all crates
cargo test             # run all tests
cargo clippy           # lint
cargo fmt --check      # format check
```

### gl-core — Core library

The core library provides the trait-based architecture that all other crates build on.

#### GoopyManager

The orchestrator. Coordinates registry, provisioner, and storage allocator to spawn and despawn goopies.

**Spawning a goopy:**

1. Generate a unique slug via `generate_slug()`.
2. Attempt `INSERT OR FAIL` into SQLite (slug is the primary key). If a collision occurs, retry up to 10 times.
3. Allocate storage via the `StorageAllocator`.
4. Provision the instance via the `GoopyProvisioner`.
5. Allocate a port from the configured range and persist it in the `allocated_ports` table.

**Despawning a goopy:**

1. Deprovision the instance via the `GoopyProvisioner`.
2. Deallocate storage via the `StorageAllocator`.
3. Remove the registry entry and release the port.

#### GoopyRegistry trait (sync)

Defines the persistence interface for goopy metadata and port allocation. Sole implementation: **SqliteRegistry** (SQLite via `rusqlite`, bundled). Tests use `:memory:` databases.

Key responsibilities:
- CRUD operations on goopy records
- Port allocation/deallocation via the `allocated_ports` table
- Slug uniqueness enforcement at the database level

#### GoopyProvisioner trait

Abstraction for instance provisioning. Each implementation returns a `ProvisionerKind` via `kind()`.

Implementations:
- **GhostLocalProvisioner** — uses Ghost-CLI (`ghost install --local` / `ghost uninstall`). This is the current provisioner.
- **HelloProvisioner** — lightweight scaffolding provisioner (stub, planned).

#### StorageAllocator trait

Abstraction for disk allocation per goopy.

Implementations:
- **ZfsAllocator** — ZFS dataset creation with configurable pool and quota (production).
- **PlainDirAllocator** — simple mkdir/rmdir (development).

### gl-cli — CLI tool

A `clap`-based CLI for manual operations and maintenance.

| Command | Description |
|---------|-------------|
| `spawn [count]` | Create one or more goopy instances (default: 1) |
| `despawn <slugs...>` | Destroy one or more instances by slug |
| `list` | Show all instances with metadata |
| `alloc --path <PATH>` | Smoke-test storage allocation (PlainDirAllocator) |
| `dealloc --path <PATH>` | Smoke-test storage deallocation (PlainDirAllocator) |

### gl-serv — HTTP API server

A thin layer over `gl-core`. Since `GoopyRegistry` is sync, the server uses `spawn_blocking` at the handler boundary to avoid blocking the async runtime.

Planned endpoints (full API is in progress — see #6):

- `POST /goopies` — create a goopy
- `GET /goopies/:slug` — query goopy info
- `GET /goopies/:slug/alive` — expiration check (used by nginx `auth_request`)
- `GET /config` — public configuration for the frontend

CORS is configured via `tower-http` `CorsLayer` to allow requests from the Vercel-hosted frontend.

## Goopy router

Nginx acts as the reverse proxy for goopy instances. In production, a goopy URL is `{slug}.goopy.life`.

Expiration is enforced via nginx `auth_request`: every request to a goopy subdomain triggers `GET /goopies/:slug/alive` on gl-serv, which returns the appropriate status.

Statuses:

- **Live** — current datetime is within creation datetime + days to live.
- **Expired** — current datetime is past the TTL. The user is redirected to an expiration page.
- **In-Progress** — the instance is still being provisioned.
- **Error** — provisioning failed.
- **Non-existent** — no goopy with that slug exists. In production, the reverse proxy filters these out, but the router handles this case too.

## Maintenance jobs

- **Sweeper:** removes expired goopies on a configurable interval (`sweep_interval_secs` in config). Runs inside `gl-serv`.

## Frontend

Next.js application deployed on Vercel.

- The landing page lets users create a goopy with one click.
- `GET /config` is fetched at Vercel build time (App Router Server Component with `force-static`) to obtain runtime settings from gl-serv. The server-only `GL_CONFIG_API_URL` env var (no `NEXT_PUBLIC_` prefix) points to `api.goopy.life` and is required — the build fails if it is unset. Browser-side calls use `NEXT_PUBLIC_GL_API_URL`.
- CORS on gl-serv is configured to accept requests from the Vercel origin.

## Configuration

Configuration is TOML-based. See `backend/gl-cli/config.toml.example` for a local
development template, and [`deploy/config/`](../deploy/config/) for the files the
droplets actually run — those are version-controlled and installed by the deploy,
not edited on the host. See [DEPLOYMENT.md](DEPLOYMENT.md#configuration).

Key settings:

| Setting | Description |
|---------|-------------|
| `base_dir` | Working directory for instance data |
| `domain` | Public domain (e.g. `goopy.life`) |
| `life_in_days` | Instance TTL |
| `provisioner.kind` | `"Hello"` or `"Ghost"` |
| `port_range_start` / `port_range_end` | Port allocation range |
| `dev_mode` | Skips systemd/nginx/ZFS operations |
| `sweep_interval_secs` | Sweeper frequency |
| `cors_origin` | Allowed CORS origin |
| `bind_address` | gl-serv HTTP bind address |
| `registry.path` | SQLite database file path |
| `allocator.pool` | ZFS pool name |
| `allocator.quota_mb` | Disk quota per instance |

## Deployment

- **Infrastructure:** DigitalOcean droplet with ZFS.
- **Backend:** The droplet runs gl-serv. The dev droplet is deployed automatically on every merge to `trunk`; production is a manual cross-compile + scp.
- **Frontend:** Deployed on Vercel via its GitHub integration. The droplet serves only gl-serv and local static pages (e.g. `/expired`).
- **API domain:** `api.goopy.life`.

Full details, including the one-time GitHub setup: [DEPLOYMENT.md](DEPLOYMENT.md).

## Roadmap

- **"Connect the Dot"** (active) — wiring all components end-to-end: registry, provisioner, storage, CLI, API server, frontend, and nginx routing.
- **"Public Beta"** (next) — Ghost provisioner (#18), API safety (#21), instance cap (#24).
