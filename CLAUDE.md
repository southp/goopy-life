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

- Every PR is associated with an issue. An issue may span several PRs when that
  makes each one reviewable on its own — split where a reviewer's question
  changes (a refactor, a new module, a config cutover), not by line count.
- Exception: `shared/<short-slug>` — ad-hoc work too small to warrant an issue,
  or a foundation genuinely shared between two open issues. A refactor extracted
  from one issue belongs to that issue, not to `shared/`.
- Branch naming: `issue/<number>/<short-slug>` or `shared/<short-slug>`
- Worktrees mirror the branch: `.worktrees/issue-<number>/<slug>`
- When an issue spans several PRs, prefer branching them all off `trunk` over
  stacking them, so each can merge without rebasing the others. Stack only on a
  real dependency.
- Never commit directly to `trunk`
