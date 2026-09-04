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
- **Frontend** — the Vercel project's Git integration (project `goopy-life-frontend`).

## Backend — automated dev deploys

`.github/workflows/backend-deploy.yml` runs on every push to `trunk` under
`backend/**`. It tests, lints, cross-compiles a static musl binary, then hands
it to `deploy/push-binary.sh` — the same script the manual production deploy
uses, so the two paths cannot drift apart.

The workflow only ships the **binary**. `config.toml`, the systemd unit, the
nginx configs and the ZFS pool are still one-time manual setup (see the
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
success as soon as the process is spawned, so a binary that panics at startup
would leave the run green while the API is down. It waits past
`RestartSec=5` and asserts `systemctl is-active gl-serv`, failing the job
otherwise. To check by hand:

```bash
ssh goopy@<dev-host> 'systemctl status gl-serv --no-pager'
curl -sS https://<dev-api-host>/config | head
```

## Frontend — Vercel Git integration

The frontend deploys through Vercel's native GitHub integration, not a workflow:
Vercel builds `trunk` to production and every PR to a preview URL.

Project settings that matter:

- **Root Directory:** `frontend`
- **Production Branch:** `trunk`
- **Environment Variables:** `NEXT_PUBLIC_GL_API_URL` and `GL_CONFIG_API_URL`
  (see `frontend/.env.local.example`). `GL_CONFIG_API_URL` is read at build time
  to fetch `GET /config`, so changing it needs a redeploy, not just a restart.

`frontend/vercel.json` carries the repo-side half of that configuration. Its
`ignoreCommand` skips the build when a push changed nothing under `frontend/`,
so backend-only merges don't burn a Vercel build. The command exits non-zero —
i.e. builds — if it cannot determine the diff (i.e. a clone too shallow for
`HEAD^`), which is the safe direction to fail.

## Manual production deploy

```bash
./deploy/deploy.sh user@droplet [ssh-port]
```

Cross-compiles `gl-serv` to a static musl binary with `cargo-zigbuild`, uploads
it, and restarts the service. Requires the one-time local toolchain setup in the
[README](../README.md#cross-compilation-setup-one-time-on-macos).

## Testing the deploy scripts

`deploy/push-binary.sh` supports `DRY_RUN=1`, which prints the `scp`/`ssh`
commands instead of running them. The test suite drives it that way — no
droplet, network or key needed:

```bash
./deploy/tests/push-binary.test.sh
```
