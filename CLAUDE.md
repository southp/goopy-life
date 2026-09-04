# Goopy.Life

Accountless Ghost sandboxing service — ephemeral Ghost instances with one click.

## Tech stack

- **Backend:** Rust workspace (`backend/`) — `gl-core` (library), `gl-cli` (CLI), `gl-serv` (HTTP API)
- **Frontend:** Next.js (`frontend/`) on Vercel
- **Database:** SQLite via `rusqlite` (bundled); `:memory:` for tests
- **Logging:** `tracing` crate throughout

## Build & test

All commands run from `backend/`:

```bash
cargo build            # build all crates
cargo test             # run all tests
cargo clippy           # lint
cargo fmt --check      # format check
```

Per-crate: `cargo test -p gl-core`, `cargo build -p gl-serv`, etc.

## Project structure

```
backend/
  gl-core/   — GoopyManager, GoopyRegistry trait, GoopyProvisioner trait, StorageAllocator trait
  gl-cli/    — CLI (clap): spawn, despawn, list, alloc
  gl-serv/   — HTTP API server (thin layer over gl-core)
frontend/    — Next.js app
```

## Key conventions

- **Identifier:** `slug` (adjective-adjective-noun format, no separate gid)
- **Registry:** `GoopyRegistry` trait (sync); `SqliteRegistry` is the sole impl
- **Provisioner:** `GoopyProvisioner` trait with `kind() -> ProvisionerKind`
- **Storage:** `StorageAllocator` trait — `ZfsAllocator` (prod) / `PlainDirAllocator` (dev)
- **Error handling:** custom `Error` enum in `shared_types.rs`
- **Async boundary:** `gl-serv` uses `spawn_blocking` for registry calls
- **Config:** TOML-based. `backend/config.local.toml` is committed and runs
  locally as-is; `deploy/config/*.toml` are what the droplets run. All are
  parsed by `gl-core/tests/committed_configs.rs` in CI
- **Rust edition:** 2024

## Branching

- One PR per issue
- Branch naming: `issue/<number>-<short-slug>` or `shared/<short-slug>`
- Never commit directly to `trunk`
