# Ghost Provisioner — operator guide

`GhostProvisioner` is the production provisioner. It creates a real,
SQLite-backed Ghost instance at `{slug}.{domain}` by soft-linking every instance
against **one shared base Ghost install** instead of running `npm install` per
instance.

This document covers the two things an operator has to do: **prepare the base
install**, and **upgrade it**.

---

## Why a shared base install

A Ghost install is around 540 MB, almost all of it `node_modules`. Installing
one per sandbox would make provisioning slow and put a hard ceiling on how many
instances fit on the droplet. Instead each instance directory is assembled from
the shared install in two parts.

### The soft-link boundary

The rule is **symlink what Ghost reads, materialise what Ghost writes**.

| Path in the instance dir | How it is created | Why |
| --- | --- | --- |
| `index.js`, `core/`, `node_modules/`, `package.json` | symlink → `source_dir` | Application code. Identical for every instance and never written to. |
| `content/themes/<each stock theme>` | symlink → `source_dir` | The themes that ship with Ghost are read-only. Every entry in the base install's `content/themes/` is linked, so the instance gets whichever theme a fresh site activates. |
| `content/data/` | real directory | Holds `ghost.db`, this instance's SQLite database. |
| `content/images/`, `content/media/`, `content/files/` | real directory | User uploads. |
| `content/themes/` | real directory | So a user-uploaded theme lands here, next to the stock-theme symlinks. |
| `content/settings/`, `content/adapters/`, `content/public/`, `content/logs/` | real directory | Ghost writes generated routes, assets and logs here. |
| `config.production.json` | real file | Per-instance URL, port, and database path. |

Sharing anything from the writable column would leak one sandbox's state into
another, so the split is a correctness boundary, not just an optimisation.
Provisioning an instance therefore costs a handful of symlinks and empty
directories.

Ghost is started with its working directory set to the instance directory —
which is where it looks for `config.production.json` — and creates and migrates
its own SQLite database on first boot. There is no separate migration step.

---

## Preparing the base install

Do this once per Ghost version. Only the last step needs `root`: `/opt/goopy-life`
is owned by the service account, so it can build the tree itself.

```bash
# 1. Pick a version-stamped directory. Keeping the version in the path lets a
#    new install be prepared while the current one is still serving instances,
#    and is what pins existing instances to the version they were created with.
GHOST_VERSION=6.63.0
INSTALL_DIR=/opt/goopy-life/ghost-${GHOST_VERSION}

mkdir -p "${INSTALL_DIR}"

# 2. Unpack the Ghost release into it. The published tarball is already a Ghost
#    root, so this is the install directory — there is nothing to hoist.
cd /tmp
npm pack ghost@${GHOST_VERSION}
tar xzf ghost-${GHOST_VERSION}.tgz --strip-components=1 -C "${INSTALL_DIR}"

# 3. Install Ghost's dependencies, with pnpm, from inside the install dir.
#    corepack reads the `packageManager` field and fetches the exact pnpm the
#    release was built with, so there is nothing to install globally.
cd "${INSTALL_DIR}"
corepack pnpm install --prod

# 4. Confirm the layout the provisioner expects.
ls index.js core node_modules package.json content/themes

# 5. Make it read-only to the service account — instances only ever read it,
#    and they run as that account, so anything it can write it can corrupt for
#    every other instance sharing the install. Needs root.
sudo chown -R root:root "${INSTALL_DIR}"
sudo chmod -R a+rX "${INSTALL_DIR}"
```

> **Use `npm pack` + the release's own package manager, not `npm install
> ghost@<version>`.** Ghost declares 21 of its dependencies as
> `file:components/*.tgz` — tarballs that live *inside* the Ghost package.
> Installing Ghost as a dependency of an empty project cannot work: npm resolves
> those paths against the project root, where `components/` does not exist, and
> fails with `ENOENT`. Unpacking first and installing from inside the package
> root is what makes them resolve.

> **6.x is pnpm; 5.x was yarn.** Ghost 6 ships a `pnpm-lock.yaml` and a
> `pnpm-workspace.yaml` that leans on pnpm-only features — `catalog:` version
> references, `overrides`, `allowBuilds` — none of which yarn or npm understand.
> Follow the `packageManager` field in the release's `package.json` rather than
> this paragraph if you move to a version that changes it again.

Then point that host's `deploy/config/<env>.toml` at it — **both** keys, so they
never disagree — and deploy:

```toml
[provisioner]
kind = "Ghost"
source_dir = "/opt/goopy-life/ghost-6.63.0"
version = "6.63.0"
node_bin = "/usr/bin/node"
service_user = "goopy"
```

```bash
./deploy/deploy.sh goopy@<host> <env>
```

The config is version-controlled and installed by the deploy, so editing
`/opt/goopy-life/config.toml` over ssh does not survive the next run.

### Requirements

- **Node.js** at `node_bin`. It must be an absolute path: systemd does not search
  `PATH` for `ExecStart`. It must also satisfy Ghost's `engines` range — for
  6.63.0 that is `^22.23.1 || ^24.20.0`.
- **Nothing checks that Node satisfies the range before an instance boots.**
  `corepack pnpm install --prod` completes happily on an out-of-range Node, and
  gl-serv never inspects it, so the first symptom of a wrong Node is a spawned
  instance whose unit fails. Check it by hand while preparing:
  ```bash
  node --version
  node -p "require('/opt/goopy-life/ghost-<version>/package.json').engines.node"
  ```
- **corepack**, to fetch pnpm for the once-per-version preparation. It ships with
  Node and is not needed at runtime.
- **`source_dir` must be the version-stamped directory**, never a symlink that
  later moves. The provisioner stores the path as given, so a moving symlink
  would silently pull running instances onto a different Ghost — see
  [Upgrading Ghost](#upgrading-ghost).
- **`service_user`** (`goopy` by default) must be able to read `source_dir`
  and write the instance working directories under `base_dir`. Instances run as
  this user, never as root.
- **A wildcard TLS certificate** for the domain at
  `/etc/letsencrypt/live/<domain>/` — the same prerequisite the Hello
  provisioner has.
- **The `goopy_alive` nginx cache zone** — see below. Unlike the others this
  one is not optional and not Ghost-specific, but Ghost is what makes it
  matter.

---

## The `goopy_alive` cache zone

Install this once per host, **before provisioning any instance**:

```bash
sudo mkdir -p /var/cache/nginx
sudo install -m 644 deploy/nginx/goopy-cache.conf /etc/nginx/conf.d/
sudo nginx -t && sudo systemctl reload nginx
```

> The `mkdir` is load-bearing. nginx creates only the **final** component of a
> `proxy_cache_path` — here `goopy_alive` — and never its parents, and the
> nginx package does not ship `/var/cache/nginx` unless something on the host
> already caches. Skip it and `nginx -t` fails with
> `mkdir() "/var/cache/nginx/goopy_alive" failed (2: No such file or directory)`,
> which is exactly the host-wide breakage the next paragraph warns about —
> arriving through the install step rather than through forgetting it.
> nginx creates `goopy_alive` itself, owned by its worker user, on first reload.

> ⚠️ **Order matters, and getting it wrong is host-wide.**
> Every generated site references the `goopy_alive` zone. A site referencing an
> undeclared zone fails `nginx -t`, and nginx then refuses to reload *at all* —
> so no instance can be provisioned or torn down until the zone exists. Install
> the snippet first; it is inert on its own.

### Why it exists

Each instance's site runs an `auth_request` against gl-serv to check the
instance has not expired. `auth_request` fires once per **request**, not once
per page. A Ghost admin screen pulls around 45 subresources, so without caching
that single page load asks gl-serv 45 times whether the instance is alive.

That is not merely wasteful. The liveness endpoint is rate limited, and nginx's
`auth_request` renders a refusal as a **500** — so exhausting the budget does
not slow the page down, it blanks it. The cache removes the burst; the separate
`ratelimit.alive_*` budget covers what remains.

### How long anything is cached

nginx does not decide — gl-serv does, per response:

| Instance state | Response | `Cache-Control` |
| --- | --- | --- |
| alive | `200` | `max-age=<ratelimit.alive_cache_secs>` (default 5s) |
| expired, not ready, unknown | `403` | `no-store` |

Keeping the decision at the origin is what makes the cache safe. **A denial is
never cached**, so an expired instance stops being reachable promptly, and once
#96 (scale-to-zero) lands, a suspended instance's check always reaches gl-serv
and the wake still fires.

The tradeoff to know about: an instance that expires *mid-window* stays
reachable for up to `alive_cache_secs`. Set it to `0` for exact expiry timing
at the cost of one subrequest per asset; it is capped at 60.

---

## What gets created per instance

```
{base_dir}/{slug}/                     ← from the StorageAllocator (ZFS dataset in prod)
/etc/systemd/system/goopy-{slug}.service
/etc/nginx/sites-available/goopy-{slug}
/etc/nginx/sites-enabled/goopy-{slug}   ← symlink
```

The systemd unit is deliberately self-contained: it depends on no other goopy
unit, so a single instance can be stopped and started in isolation. Issue #96
(scale-to-zero) suspends and resumes instances by doing exactly that.

Handy commands:

```bash
sudo systemctl status goopy-{slug}      # is the instance running?
sudo journalctl -u goopy-{slug} -f      # follow its output
tail -f {base_dir}/{slug}/content/logs/*.log
```

`deprovision` reverses all of it: stop and disable the unit, remove the unit
file, remove the nginx site and reload nginx, then release the working
directory. Releasing removes the instance's symlinks — never the base install
they point at.

---

## Upgrading Ghost

Instances are **pinned to the version they were created with**, because their
symlinks name the version-stamped install directory directly: an instance
created against `ghost-6.63.0` keeps pointing there no matter what is prepared
afterwards. `version` is recorded alongside on every instance as
`service_version` at spawn time. Beta runs a single Ghost version at a time;
supporting several coexisting versions is a follow-up.

To upgrade:

1. Prepare the new version in its own directory, following the steps above with
   a new `GHOST_VERSION` (e.g. `/opt/goopy-life/ghost-6.64.0`).
2. Point the config at it — **both** keys, so they never disagree:
   ```toml
   [provisioner]
   source_dir = "/opt/goopy-life/ghost-6.64.0"
   version = "6.64.0"
   ```
3. Deploy, which installs the edited config and restarts `gl-serv`.
4. Instances spawned from now on use the new version. Existing instances keep
   running against the old one.

> Do not introduce a stable `/opt/goopy-life/ghost` symlink and point
> `source_dir` at it. The provisioner links each instance at
> `{source_dir}/index.js` and friends as written, so repointing such a symlink
> would redirect **every existing instance** the next time its Ghost process
> restarts — including when #96 suspends and resumes it. Editing `source_dir` is
> one line, and it is the line that makes pinning real.

### Retiring the old install

Instances created before the switch hold links into the **old** directory. Do
not delete it until every instance that references it is gone:

```bash
# Which versions are still in use?
gl-cli list | grep service_version | sort | uniq -c
```

Once no instance reports the old version — instances are ephemeral, so this
happens within `life_in_days` — remove it:

```bash
rm -rf /opt/goopy-life/ghost-6.63.0
```

---

## Dev mode

With `dev_mode` on (or `gl-cli` without `--prod`), the provisioner skips systemd
and nginx entirely: it assembles the same instance directory, writes the same
`config.production.json` — with `url` pointing straight at `http://127.0.0.1:{port}`
since there is no proxy in front — spawns Ghost as a detached background process,
and records the PID in `{working_dir}/server.pid`. `deprovision` kills that PID
and removes the directory.

Ghost is always run with `NODE_ENV=production` so it reads
`config.production.json`; "dev mode" refers to goopy.life's mode, not Ghost's.

On macOS, set `node_bin` to the output of `which node` — the `/usr/bin/node`
default is Linux-specific.
