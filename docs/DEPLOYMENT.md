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

## Before the first deploy

The `deploy/` directory contains all artifacts needed to run the service on the droplet.
Two things are set up by hand, once: the cross-compile toolchain on your machine,
and the droplet itself.

### Cross-compilation setup (one-time, on macOS)

The droplet runs x86_64 Linux. The musl target comes with the pinned toolchain (`backend/rust-toolchain.toml`); install `cargo-zigbuild` (uses [Zig](https://ziglang.org/) as the cross-linker — no extra toolchain taps required). Both link the production binary, so they are pinned, and `deploy/deploy.sh` refuses to run with other versions:

```bash
cargo install --locked cargo-zigbuild@0.22.3
brew install zig@0.16
```

### Droplet setup (one-time)

The service runs as a dedicated `goopy` account, which is also the account you deploy as — the sudoers drop-in below names it explicitly, so deploying as any other user fails with a password prompt.

```bash
# 0. Create the service account and authorise your deploy key for it
sudo useradd --system --create-home --shell /bin/bash goopy
ssh-copy-id goopy@<droplet>

# 1. From your machine, as an admin with password sudo: install the sudoers
#    drop-in and the nginx cache zone. No deploy installs these — the drop-in
#    grants the deploy its rights — and every deploy checks the drop-in.
./deploy/admin-apply.sh <admin>@<droplet> <env>

# 2. Set the ZFS pool mountpoint to match base_dir in config.toml (default: /opt/goopy-life/data).
#    gl-serv creates/destroys child datasets via sudo (sudoers rules restrict to zpool_ghost/*).
#    NoNewPrivileges is intentionally omitted from the unit to allow this; see issue #90 for
#    the long-term fix (privilege-separated ZFS helper).
sudo zfs set mountpoint=/opt/goopy-life/data zpool_ghost

# 3. Give the deploy account ownership of the service directory. The deploy
#    writes /opt/goopy-life/config.toml directly, so this must not be root-owned.
sudo install -d -o goopy -g goopy /opt/goopy-life /opt/goopy-life/bin
```

There is no step for `config.toml`, the systemd unit or the api nginx site: they are version-controlled — the unit at [`deploy/gl-serv.service`](../deploy/gl-serv.service), the rest per environment under [`deploy/config/`](../deploy/config/) — and installed by the deploy itself. After the first deploy, `./deploy/check-host.sh goopy@<droplet> <env>` confirms the host matches the repo. See [Host artifacts](#host-artifacts).

## Backend — automated dev deploys

`.github/workflows/backend-deploy.yml` runs on every push to `trunk` under
`backend/**`. It tests, lints, cross-compiles a static musl binary, then hands
it to `deploy/push-binary.sh` — the same script the manual production deploy
uses, so the two paths cannot drift apart.

The workflow ships **both binaries, the config and the host artifacts** —
`gl-serv` and `gl-cli` (see [The maintenance CLI on the
host](#the-maintenance-cli-on-the-host)), and the systemd unit, its drop-in and
the api nginx site (see [Host artifacts](#host-artifacts)). The sudoers drop-in
and the nginx cache zone are applied by an admin with `deploy/admin-apply.sh`
(see [Applying the root-owned artifacts](#applying-the-root-owned-artifacts)) —
the deploy checks the sudoers drop-in and fails until that has run. The ZFS pool
is still one-time manual setup (see the
[droplet setup](#droplet-setup-one-time) steps).

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

Each instance also records the commit of the binary that provisioned it, which
outlives any number of later deploys. `gl-cli list` (invoked as in
[the maintenance CLI](#the-maintenance-cli-on-the-host)) prints it per instance
as `build_sha`, next to `service_version` (the Ghost version, a separate fact).
`(not recorded)` means the row predates #119, not that the build was unstamped;
an unstamped build records `unknown`.

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

### Usage stats: `GET /stats`

gl-serv counts provisions and answers with the totals, publicly and on the read
rate limiter:

```bash
curl -sS https://<dev-api-host>/stats
{"all_time":{"provisioned":1234,"failed":56},"last_7_days":{"provisioned":140,"failed":3},"today":{"provisioned":21,"failed":0},"window_days":90,"daily":[{"day":"2026-09-29","provisioned":21,"failed":0}]}
```

What counts:

| Counter | Counted when | Notes |
|---|---|---|
| `provisioned` | the spawn sets the instance `Done` | since #151 `Done` means it answered HTTP, so this counts instances a visitor could use |
| `failed` | a spawn fails and sets the instance `Failed` | once per instance; a failed despawn or sweep is a cleanup problem and is not counted |

- **Days are UTC.** `today` is the current UTC day so far. `last_7_days` is the
  last seven UTC days, today included.
- **`daily`** lists only days that have a row, newest first. A day with no
  activity is left out, not reported as zeros.
- **`window_days`** is `stats_retention_days` (default 90, minimum 7). The sweep
  drops daily rows older than that; `all_time` is a separate row and is never
  pruned.
- **`gl-cli spawn` counts too.** It runs the same `GoopyManager` code, so a load
  test run on the host (like #113's) inflates every figure it touches. Nothing
  filters it out; subtract it by hand if it matters.
- **Counting starts at deploy.** Migration 4 creates the counters at zero, so
  nothing provisioned before the first deploy that carries them is counted.

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

Two arguments:

- **`--config /opt/goopy-life/config.toml`** — the default is `./config.toml`,
  which does not exist in the directory an operator is likely standing in.
- **`--prod`** — an assertion, not a mode switch. The mode comes from
  `dev_mode` in the config, the same rule `gl-serv` follows. With `--prod`,
  a config that sets `dev_mode = true` is refused — exit 1, naming the file —
  before the registry is opened or a provisioner is built, so a `--config`
  pointed at the wrong file fails loudly instead of running a dev-mode
  teardown on a real host (#163).

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
`cargo-zigbuild`, uploads them with `deploy/config/prod.toml` and production's
[host artifacts](#host-artifacts), and restarts the service. Run
`./deploy/check-host.sh goopy@droplet prod` first to see what it is about to
replace.

A deploy makes about a dozen `scp`/`ssh` calls. CI shares one connection
between them (`ControlMaster` in the workflow's ssh config); to do the same on
your machine, add to the droplet's entry in `~/.ssh/config`:

```
ControlMaster auto
ControlPath ~/.ssh/cm-%C
ControlPersist 60
```

Requires the one-time local toolchain setup in
[Cross-compilation setup](#cross-compilation-setup-one-time-on-macos).

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

## Production

Production is one droplet serving `goopy.life`: the api at `api.goopy.life`,
instances at `{slug}.goopy.life`. Nothing deploys it automatically. Its files
are `deploy/config/prod.*`.

### First deploy

Run in this order. Steps 1–3 run from your machine, at the repo root.

| # | Command | Run as | What it does |
|---|---|---|---|
| 0 | the [#147 host checklist](https://github.com/southp/goopy-life/issues/147#issuecomment-5885857718) | admin | host state nothing below creates (see [What lives on the host](#what-lives-on-the-host)) |
| 1 | `./deploy/admin-apply.sh <admin@host> prod` | admin, password sudo | installs `/etc/sudoers.d/goopy` and `goopy-cache.conf` |
| 2 | `./deploy/deploy.sh goopy@<host> prod` | `goopy` | builds, ships, restarts, checks `/version` |
| 3 | `./deploy/check-host.sh goopy@<host> prod` | either | read-only; expect `matches prod` |

- **Step 1 comes before step 2, always.** The deploy compares the host's sudoers
  drop-in with `deploy/sudoers.goopy` and stops, before changing anything, until
  they match. A deploy never installs that file itself.
- **Step 2 as `goopy`, never as the admin account.** The sudoers rules name
  `goopy` only.
- **Step 2 checks itself.** It fails unless `GET /version` on the host reports
  the commit it just built (see [Verifying a deploy](#verifying-a-deploy)).

Then check the public side by hand:

```bash
curl -sS https://api.goopy.life/version    # sha_full = the commit you deployed
curl -sS https://api.goopy.life/capacity   # total = 20
```

### Later deploys

- `check-host.sh` first shows what the deploy is about to replace.
- If `deploy/sudoers.goopy` or `deploy/nginx/goopy-cache.conf` changed since the
  last deploy, re-run `admin-apply.sh` first. Otherwise the sudoers comparison
  stops the deploy.
- Deploy from a clean checkout. A dirty tree deploys as `<sha>-dirty`, which
  `/version` then reports.

### Rollback

Re-run the deploy from the previous commit:

```bash
git worktree add .worktrees/rollback <previous-commit>
.worktrees/rollback/deploy/deploy.sh goopy@<host> prod
curl -sS https://api.goopy.life/version   # sha_full = <previous-commit>
```

- **Binary and config roll back together.** `deploy.sh` ships that commit's
  `prod.toml` and host artifacts beside its binaries.
- **`/version` confirms it.** The deploy fails unless the host reports the commit
  it built, so a rollback that did not take is a red run, not a silent one.
- **A registry migration blocks rollback.** An older gl-serv refuses a database
  a newer one has migrated (`SchemaVersionTooNew`). `--check-config` does not
  open the registry, so that deploy passes the gate, restarts into a crash loop,
  and fails at `is-active` with the API down. Check whether `MIGRATIONS` in
  `backend/gl-core/src/goopy_registry/sqlite_registry.rs` grew between the two
  commits. If it did, roll forward with a fix instead.
- **A sudoers change blocks rollback too.** If `deploy/sudoers.goopy` differs
  between the two commits, the deploy stops at the drift check. Run
  `admin-apply.sh` from the rollback checkout first.

### What lives on the host

The deploy ships binaries, config and the [host artifacts](#host-artifacts).
`admin-apply.sh` ships the root-owned pair. **Everything else is set up once by
hand**, following the
[#147 host checklist](https://github.com/southp/goopy-life/issues/147#issuecomment-5885857718).
`check-host.sh` compares files only, so it cannot tell if any of this is missing:

| Host state | Why it matters |
|---|---|
| Swap (4 GB) + zswap | the caps of 20 assume it. Without it the box serves ~10, and nothing errors |
| DNS for `api.` and `*.goopy.life`, wildcard cert at `/etc/letsencrypt/live/goopy.life/` | the api site and every instance site hardcode that path |
| Cloud firewall: 80/443 open, 3000 closed | defence in depth for the loopback bind |
| `goopy` account, ZFS pool, `/var/cache/nginx`, `/opt/goopy-life/` | the deploy and the provisioner write into these |
| Node 22.23.2 + Ghost 6.63.0 at `/opt/goopy-life/ghost-6.63.0` | `prod.toml`'s `source_dir` and `node_bin` |
| Vercel production env: `NEXT_PUBLIC_GL_API_URL`, `GL_CONFIG_API_URL` = `https://api.goopy.life` | the frontend; see [Frontend](#frontend--vercel-git-integration) |

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
  `EnvironmentFile` referenced from the environment's drop-in,
  `deploy/config/<env>.gl-serv.conf`, and create that file on the host by hand.
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

A line marked `REVIEW` in `prod.toml` names a value not yet settled for
production; settle it before the first production deploy.

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

## Host artifacts

Beside the config, every deploy ships the files that decide how gl-serv runs and
how it is reached, and it is the only writer of each (#139):

| Tracked | Installed at | |
|---|---|---|
| `deploy/gl-serv.service` | `/etc/systemd/system/gl-serv.service` | shared by every environment |
| `deploy/config/<env>.gl-serv.conf` | `/etc/systemd/system/gl-serv.service.d/deploy.conf` | systemd drop-in: what differs per environment (`RUST_LOG`) |
| `deploy/config/<env>.api.nginx` | `/etc/nginx/sites-available/gl-serv-api`, linked from `sites-enabled` | the api site |
| `deploy/sudoers.goopy` | `/etc/sudoers.d/goopy` | **compared, never installed** — see below |

They used to be tracked and never installed, and the dev droplet ran an api
site named `api.goopy.life` that served `api.southp.dev` from a different
certificate — a file with the right name and the wrong contents, which nothing
noticed.

**Per environment, tied to the config.** `server_name`, the certificate path and
`proxy_pass` in `<env>.api.nginx` must agree with `domain` and `api_address` in
`<env>.toml`; `committed_configs.rs` fails a PR that changes one side only. The
shared unit may carry no `Environment=` line — anything that varies goes in the
drop-in.

**The api site is checked before anything else is installed.** `nginx -t` checks
the whole configuration, and every per-instance provision runs it too, so a
rejected site left in place would fail every spawn after it. The deploy puts
the previous site back (or, on a first install, unlinks the new one) and stops;
nginx never reloaded, so it keeps serving what it had.

**The sudoers drop-in is verify-only, and must stay that way.** It grants the
deploy its own rights, so a deploy that installed it could revoke them with one
bad push, recoverable only from a console. Every deploy instead compares the
host's copy with `deploy/sudoers.goopy` — through a pinned `sudo cmp`, since the
file is `0440 root` — and stops **before changing anything** if they differ.
Changing it is an admin's act, done with
[`admin-apply.sh`](#applying-the-root-owned-artifacts) before the deploy that
needs it.

This is also why a host's *first* deploy never creates the drop-in: the deploy
needs its rules before it can install anything. A new host gets it from
`admin-apply.sh` during the [droplet setup](#droplet-setup-one-time),
and the first deploy is what verifies it.

Two more things stop a deploy, because it cannot fix either: a file in
`gl-serv.service.d/` other than `deploy.conf` (a `systemctl edit` override
changes the unit without appearing in any tracked file), and the pre-#139
`sites-enabled/api.goopy.life` still enabled (it claims the same `server_name`,
sorts first, and wins).

### Checking a host for drift

```bash
./deploy/check-host.sh goopy@<host> <env> [ssh-port]
```

Read-only. Compares each artifact above, plus the installed config, with what
the repo holds for `<env>`, prints `ok` or `DRIFT` per file with a diff, and
exits non-zero on any difference. Run it before a manual production deploy to
see what the deploy is about to replace, and after any hand-edit on a host to
see what the next deploy will undo. It stages into a private directory, never
the deploy's fixed `/tmp` paths, so it is safe to run while a deploy is in
flight; its only sudo rule is its own pinned `cmp` of the sudoers drop-in.

### Applying the root-owned artifacts

```bash
./deploy/admin-apply.sh <admin@host> <env> [ssh-port]   # e.g. spdev dev
```

Run from your machine, as an account with **password sudo** on the host — not
`goopy`, whose rights are exactly the pinned list. One run, one password prompt,
and it applies everything a deploy may not:

| Tracked | Installed at | Validated by |
|---|---|---|
| `deploy/sudoers.goopy` | `/etc/sudoers.d/goopy` | `visudo -cf` before, `visudo -c` after |
| `deploy/nginx/goopy-cache.conf` | `/etc/nginx/conf.d/goopy-cache.conf` | `nginx -t` |

Each file is compared first and left alone if it already matches, so it is safe
to run whenever in doubt — an up-to-date host sees no change and no reload. A
change that fails its check is never left in place: the sudoers drop-in is
checked **before** it reaches `/etc/sudoers.d` (one broken file there disables
sudo for every account) and arrives by an atomic rename; anything rejected after
the fact is put back to the previous copy.

When to run it: after merging a change to either file, **before** the deploy
that follows — for the dev droplet that deploy starts on merge, so run it just
before merging. Then `check-host.sh` confirms the host matches.

### Migrating a host from before #139

A host set up before #139 fails its first deploy, by design: its sudoers drop-in
lacks the new rules, and its api site is enabled as `api.goopy.life`.
`admin-apply.sh` handles both. Alongside the new drop-in it swaps
`sites-enabled/api.goopy.life` for `gl-serv-api` (from
`deploy/config/<env>.api.nginx`) in a single reload, so the API stays up, and
re-enables the old site if nginx rejects the new one. On any other host that
step is a no-op.

Then deploy. It installs the unit without `RUST_LOG` and the drop-in that now
carries it, and `check-host.sh` should report the host clean.

## Testing the deploy scripts

`deploy/push-binary.sh` supports `DRY_RUN=1`, which prints the `scp`/`ssh`
commands instead of running them. The test suite drives it that way — no
droplet, network or key needed:

```bash
./deploy/tests/push-binary.test.sh
./deploy/tests/check-host.test.sh
./deploy/tests/admin-apply.test.sh
```

`check-host.test.sh` also covers the drift comparison both scripts share
(`deploy/host-artifacts.sh`), by running it against a scratch directory that
stands in for the host's filesystem.
