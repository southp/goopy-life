# Deployment

Two environments, two deployment models:

| | Backend (`gl-serv`) | Frontend (Next.js) |
|---|---|---|
| **Dev** | Automatic — every merge to `trunk` that touches `backend/**` | Automatic — every merge to `trunk` that touches `frontend/**` |
| **Prod** | Manual — `./deploy/deploy.sh user@droplet` | Vercel production branch (see below) |

Dev is deliberately hands-off so `trunk` is always live somewhere; production
stays a deliberate act.

## Where the servers are configured

Neither host is hardcoded in the repo. Both are settings you can change without
a code change:

- **Backend dev droplet** — the `dev` [GitHub Environment](https://github.com/southp/goopy-life/settings/environments).
- **Frontend** — the Vercel project's Git integration (project `goopy-life-frontend-dev`).

## Backend — automated dev deploys

`.github/workflows/backend-deploy.yml` runs on every push to `trunk` under
`backend/**`. It tests, lints, cross-compiles a static musl binary, then hands
it to `deploy/push-binary.sh` — the same script the manual production deploy
uses, so the two paths cannot drift apart.

The workflow ships the **binary and the config**. The systemd unit, the nginx
configs and the ZFS pool are still one-time manual setup (see the
[droplet setup](../README.md#droplet-setup-one-time) steps); change one of those
and you still have to apply it by hand.

### One-time setup

**1. Create a deploy key on your machine.**

```bash
ssh-keygen -t ed25519 -N '' -f ~/.ssh/goopy-dev-deploy -C 'github-actions dev deploy'
```

**2. Authorise it on the dev droplet** for the `goopy` service account — the
same account `deploy/sudoers.goopy` grants the `install` and
`systemctl restart gl-serv` rules to:

```bash
ssh-copy-id -i ~/.ssh/goopy-dev-deploy.pub goopy@<dev-host>
```

**3. Capture the droplet's host key** so the runner can verify what it connects
to (the workflow pins `StrictHostKeyChecking yes` — an unverified key would let
anyone winning a DNS race collect a `sudo install` on the droplet):

```bash
ssh-keyscan -t ed25519 <dev-host>
```

**4. Create a `dev` environment** under *Settings → Environments* and add:

| Kind | Name | Value |
|---|---|---|
| Secret | `DEV_SSH_PRIVATE_KEY` | contents of `~/.ssh/goopy-dev-deploy` (the private half, including the BEGIN/END lines) |
| Secret | `DEV_SSH_KNOWN_HOSTS` | the `ssh-keyscan` output from step 3 |
| Variable | `DEV_DEPLOY_HOST` | dev droplet hostname or IP |
| Variable | `DEV_DEPLOY_USER` | `goopy` |
| Variable | `DEV_SSH_PORT` | optional; defaults to `22` |

The workflow fails fast with a named-variable error if any of these is missing,
so a half-configured environment is obvious rather than an opaque ssh failure.

Adding a `prod` environment later is the same shape — the workflow reads its
target from environment settings, not from the YAML.

### Re-running a deploy

*Actions → Deploy backend to dev → Run workflow*. Useful after rotating the
deploy key or rebuilding the droplet, and avoids an empty commit.

### Verifying a deploy

`push-binary.sh` does not trust `systemctl restart` on its own: restart reports
success as soon as the process is spawned, so a binary that panics at startup —
or a config it cannot parse — would leave the run green while the API is down. It waits past
`RestartSec=5` and asserts `systemctl is-active gl-serv`, failing the job
otherwise. To check by hand:

```bash
ssh goopy@<dev-host> 'systemctl status gl-serv --no-pager'
curl -sS https://<dev-api-host>/config | head
```

## Frontend — Vercel Git integration

The frontend deploys through Vercel's native GitHub integration, not a workflow:
Vercel builds `trunk` to production and every PR to a preview URL.

A project created by `vercel deploy` from a laptop has no Git integration —
*Connect Git* on the project overview adds it. Until that is done nothing here
applies, `vercel.json` is inert, and `trunk` does not deploy itself.

Project settings that matter:

- **Root Directory:** `frontend` — required, and load-bearing beyond the build:
  see the `ignoreCommand` note below.
- **Production Branch:** `trunk` — Vercel assumes `main`, so this needs setting
  explicitly even though `trunk` is the repository default.
- **Environment Variables:** `NEXT_PUBLIC_GL_API_URL` and `GL_CONFIG_API_URL`
  (see `frontend/.env.local.example`), both scoped to Production —
  `https://api.southp.dev` for the dev environment. `GL_CONFIG_API_URL` is read
  at build time to fetch `GET /config`, so changing it needs a redeploy, not
  just a restart; `frontend/lib/config.ts` throws when it is unset, so a missing
  value fails the build rather than shipping a broken page.

`frontend/vercel.json` carries the repo-side half of that configuration:

```json
"ignoreCommand": "git diff --quiet HEAD^ HEAD -- ."
```

It skips the build when a push changed nothing under `frontend/`, so backend-only
merges don't burn a Vercel build.

**The `.` is relative to the Root Directory**, which is what makes it mean
`frontend/`. With Root Directory unset, `.` is the repository root, every commit
looks like a change, and the command silently never skips anything — no error,
just the build cost it was meant to avoid. If skipping appears not to work, check
that setting first.

The command exits non-zero — i.e. builds — if it cannot determine the diff (e.g.
a clone too shallow for `HEAD^`), which is the safe direction to fail. The first
build after connecting Git always runs, since there is no previous Git deployment
to diff against.

## Manual production deploy

```bash
./deploy/deploy.sh goopy@droplet prod [ssh-port]
```

Cross-compiles `gl-serv` to a static musl binary with `cargo-zigbuild`, uploads
it with `deploy/config/prod.toml`, and restarts the service. Requires the
one-time local toolchain setup in the
[README](../README.md#cross-compilation-setup-one-time-on-macos).

The environment argument is required. It has no default because the config
reaches the host: a default would let an omitted argument reconfigure one
environment with another's settings.

## Configuration

Each environment's configuration is version-controlled in
[`deploy/config/`](../deploy/config/) and installed at
`/opt/goopy-life/config.toml` by the deploy. **The deploy is the only writer of
that file** — a hand-edit on the droplet is overwritten by the next run, so
changes go through a commit like any other.

| | |
|---|---|
| `deploy/config/dev.toml` | shipped automatically on every merge to `trunk` |
| `deploy/config/prod.toml` | shipped by the manual production deploy |

This exists because the file used to live only on the droplet. It drifted:
#63 replaced the flat `provisioner_kind` key with a `[provisioner]` table, the
droplet's copy kept the old spelling, and the next deploy to read it crash-looped
gl-serv with `missing field provisioner` while nginx served 502.

Two rules keep it that way:

- **No secrets in these files.** They are tracked in git and world-readable on
  the host. When gl-serv needs a credential, add it to a root-owned
  `EnvironmentFile` referenced from `deploy/gl-serv.service`.
- **The schema is checked in CI.** `backend/gl-core/tests/deploy_configs.rs`
  parses every file in `deploy/config/` with `Config::from_file` on each run, so
  a newly required field fails the PR that introduces it rather than the deploy
  that follows. Adding an environment means adding a file; the test picks it up
  with no edit.

To roll a config change back, revert the commit and deploy again.

`prod.toml` is currently a placeholder copied from the dev values — there is no
production host yet. Every line that still names a dev-only value is marked
`REVIEW`; work through them before the first production deploy.

A note on `bind_address`: it is `0.0.0.0:3000`, which exposes gl-serv directly
alongside nginx. A caller reaching it that way bypasses TLS and can set the
`X-Real-IP` header the rate limiter keys on. `127.0.0.1:3000` satisfies both
nginx's `proxy_pass` and its `auth_request` subrequests; the value is left as-is
here only because changing it is a behaviour change, not a cleanup.

## Testing the deploy scripts

`deploy/push-binary.sh` supports `DRY_RUN=1`, which prints the `scp`/`ssh`
commands instead of running them. The test suite drives it that way — no
droplet, network or key needed:

```bash
./deploy/tests/push-binary.test.sh
```
