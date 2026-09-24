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

The workflow ships **both binaries and the config** — `gl-serv` and `gl-cli`,
see [The maintenance CLI on the host](#the-maintenance-cli-on-the-host). The
systemd unit, the nginx configs and the ZFS pool are still one-time manual setup (see the
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

That check is honest but late: by the time it fires, the old binary has already
been stopped and the outage has happened. So one class of failure is caught
earlier. After both files are uploaded and **before** anything is installed,
`push-binary.sh` runs the new binary against the staged config:

```bash
/tmp/gl-serv --check-config --config /opt/goopy-life/config.toml.new
```

`--check-config` parses the file, prints a summary of it and exits 0/1. It binds
no port, opens no registry and starts no background task, so it is safe to run
beside the live service — running the binary bare to test a config is not, as it
collides on `Address in use`. A non-zero exit aborts the deploy with the
installed binary and config untouched and still serving.

It matters most on the **manual production path**: `deploy.sh` builds from
whatever local tree the operator has, which need not be the commit CI validated,
and nothing watches production. On the automated dev deploy it is close to
tautological — binary and config come from the same trunk commit, and
`committed_configs.rs` has already parsed that config with the same `gl-core` — but
it costs one ssh round trip and it is the cheap half of the guarantee.

To run it by hand against a config before deploying it (from `backend/`):

```bash
cargo run -q -p gl-serv -- --check-config --config ../deploy/config/prod.toml
```

### What commit is running right now

`GET /version` on gl-serv answers it directly, so nobody has to ssh in and guess
from file mtimes:

```bash
curl -sS https://<dev-api-host>/version
{"sha":"c50c932","sha_full":"c50c932…","built_at":"2026-09-04T11:57:00Z","version":"0.1.0"}
```

The commit id is compiled into the binary by `gl-core/build.rs` from
`GL_GIT_SHA`, which both deploy paths set — `deploy.sh` from the checkout it is
building, the workflow from `github.sha`. Three answers are possible and each
means something specific:

| `sha_full` | What it means |
|---|---|
| a commit id | that commit is serving |
| `<commit>-dirty` | built by `deploy.sh` from a tree with uncommitted changes — what is running is *not* that commit |
| `unknown` | built by neither deploy path (a local `cargo build`), so nothing can be said about which commit it is |

On the host itself, both binaries print the same stamp, so a gl-serv and a
gl-cli from one build print matching lines:

```bash
/opt/goopy-life/bin/gl-serv --version   # gl-serv 0.1.0 (c50c932, built 2026-09-04T11:57:00Z)
/opt/goopy-life/bin/gl-cli --version    # gl-cli 0.1.0 (c50c932, built 2026-09-04T11:57:00Z)
```

Nothing has to be checked by hand on a normal deploy: `push-binary.sh` makes the
comparison itself. After the restart and the `is-active` check it asks the host
for `/version` and fails the run unless the sha matches what it just built. That
turns the post-deploy check from "something is running" into "the thing I merged
is running" — the gap that let #117's fix sit undeployed for three weeks.

A failure there prints the sha it built and the body `/version` returned, and
means the restart did not produce the binary the install put down. Check in this
order: that the install actually replaced `/opt/goopy-life/bin/gl-serv`, that
`systemctl restart gl-serv` took, and that no other gl-serv is bound to the port.

### What frontend commit is running, and the footer

The landing page's footer answers the question for both halves at once:

```
web a1b2c3d · api c50c932
```

Each sha links to its commit on GitHub. The two come from two places on purpose:

| | Source | When |
|---|---|---|
| `web` | `VERCEL_GIT_COMMIT_SHA` → `NEXT_PUBLIC_GL_BUILD_SHA`, in `frontend/next.config.ts` | build time — correct, the bundle *is* the build |
| `api` | `GET /version`, fetched by the browser | runtime |

The backend's sha is deliberately **not** served from `GET /config`: the frontend
fetches that once at Vercel build time into a `force-static` page, and
`ignoreCommand` skips the Vercel build for backend-only changes, so a sha carried
there would go stale and stay stale while looking authoritative.

What the footer shows, and what it means:

- **`api` missing** — `/version` did not answer (backend down, CORS, network). It
  is omitted rather than replaced by a placeholder that would imply a version.
- **`unknown`, unlinked** — the build was stamped by neither path: a local build
  of either half.
- **`<sha>-dirty`, unlinked** — the backend was hand-deployed from a tree with
  uncommitted changes; linking the commit would claim code that is not running.

Vercel exposes `VERCEL_GIT_COMMIT_SHA` to the build only while *Automatically
expose System Environment Variables* is on (the default). If the footer reads
`web unknown` on a Vercel deployment, check that setting first. Without the
footer, the Vercel dashboard's *Deployments* tab shows the commit each deployment
was built from.

## The maintenance CLI on the host

`gl-serv` exposes no despawn route, so tearing down one instance by hand,
listing what exists and running `alloc`/`dealloc` need `gl-cli`. The deploy
installs it at `/opt/goopy-life/bin/gl-cli` from the same `cargo build` as
`gl-serv`.

**From the same build, on purpose.** `gl-cli` links `gl-core`, so it shares the
registry schema and the provisioner with `gl-serv`. Shipping it on a path of its
own is how a host ends up with a `gl-cli` older than the `gl-serv` it shares a
database with — the moment a maintenance tool is most dangerous. It is also why
a failed `gl-cli` install aborts the deploy rather than being skipped.

```bash
sudo -u goopy /opt/goopy-life/bin/gl-cli \
    --config /opt/goopy-life/config.toml --prod list
```

Two arguments, both load-bearing:

- **`--config /opt/goopy-life/config.toml`** — the default is `./config.toml`,
  which does not exist in the directory an operator is likely standing in.
- **`--prod`** — without it the CLI runs in **dev mode whatever the config
  says**, and a dev-mode despawn kills a detached process instead of removing
  the systemd unit and the nginx sites. The instance disappears from the
  registry and its real resources stay behind. `gl-cli --help` repeats this.

Run it as `goopy`: that account owns the registry, the working directories and
the `sudoers` rules the provisioner needs. As any other user it either cannot
write the database or cannot tear an instance down.

### Running it beside a live gl-serv

Safe by design, not by luck. `SqliteRegistry::new` opens every connection with
`PRAGMA busy_timeout = 5000` and requires `journal_mode=WAL` — it returns an
error rather than falling back if WAL is unavailable. In WAL mode a reader never
blocks the writer, and a second writer waits out the busy timeout instead of
failing with `SQLITE_BUSY`. Both processes also run the same schema migration,
each step inside an `IMMEDIATE` transaction that re-reads `user_version` under
the write lock, so two of them starting at once cannot double-apply a step.

Racing the two on one *instance* is refused rather than corrupted: `despawn` on
a slug the sweeper has already claimed sees status `Despawning` and fails as
`Invalid`.

The exception is `alloc` and `dealloc`. They take a raw path, consult no
registry and check nothing — never aim them at a live instance's working
directory.

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

Cross-compiles `gl-serv` and `gl-cli` to static musl binaries with
`cargo-zigbuild`, uploads them with `deploy/config/prod.toml`, and restarts the
service. Requires the
one-time local toolchain setup in the
[README](../README.md#cross-compilation-setup-one-time-on-macos).

The environment argument is required. It has no default because the config
reaches the host: a default would let an omitted argument reconfigure one
environment with another's settings.

It builds from whatever is in the local checkout, so it stamps the binary with
`git rev-parse HEAD` and appends `-dirty` when tracked files differ from it.
Deploying dirty is allowed — it is sometimes the point of the manual path — but
`/version` will say so, and nothing will later claim that commit is what is
running. Untracked files are not counted: a build's inputs cannot change without
some tracked file changing too, and counting scratch files would mark every
manual deploy dirty, which is the same as not marking any.

Outside a git checkout it refuses to deploy at all rather than stamping
`unknown`: the identity check would then compare `unknown` against `unknown`
and pass without having verified anything. `push-binary.sh` rejects
`GL_GIT_SHA=unknown` for the same reason, whoever calls it.

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
- **The schema is checked in CI.** `backend/gl-core/tests/committed_configs.rs`
  parses every file in `deploy/config/` with `Config::from_file` on each run, so
  a newly required field fails the PR that introduces it rather than the deploy
  that follows. Adding an environment means adding a file; the test picks it up
  with no edit.
- **And again on the host, before the swap.** The deploy runs the uploaded
  binary with `--check-config` against the staged config while the previous pair
  is still installed — see [Verifying a deploy](#verifying-a-deploy). CI covers
  the automated path; this covers the manual one, where the binary was built
  from an operator's local tree.

To roll a config change back, revert the commit and deploy again.

`prod.toml` is currently a placeholder copied from the dev values — there is no
production host yet. Every line that still names a dev-only value is marked
`REVIEW`; work through them before the first production deploy.

### `bind_address` and `api_address`

gl-serv binds `127.0.0.1:3000`, so **nginx is the only path to it**. That is
what makes the `X-Real-IP` the rate limiter keys on trustworthy: a caller able
to reach port 3000 directly would skip TLS and set the header itself, which does
not weaken the limiter so much as remove it — a fresh forged IP per request
empties every bucket (#21, #105). CI pins it —
`no_deployed_config_binds_a_wildcard` fails any file in `deploy/config/` that
listens on a wildcard.

`api_address` is the other half of the same fix (#149). It is where nginx
*connects* to reach gl-serv for each instance's `auth_request` alive-check, and
it is a separate key because a wildcard is a sensible thing to listen on and a
meaningless thing to connect to. Left unset — as all three committed configs
leave it — it takes `bind_address`'s port on loopback. `gl-serv --check-config`
prints both, so a host can be checked without reading its config file.

Instance sites rendered before #149 keep `proxy_pass http://0.0.0.0:3000/...`
until that instance is reprovisioned. This is harmless: the connect lands on
loopback regardless, which is why nobody noticed the wildcard in the first
place. No migration is needed — it is worth knowing only when reading
`sites-available` mid-transition and finding both forms.

## Testing the deploy scripts

`deploy/push-binary.sh` supports `DRY_RUN=1`, which prints the `scp`/`ssh`
commands instead of running them. The test suite drives it that way — no
droplet, network or key needed:

```bash
./deploy/tests/push-binary.test.sh
```
