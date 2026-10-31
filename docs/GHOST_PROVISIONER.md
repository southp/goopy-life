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
instances fit on the host. Instead each instance directory is assembled from
the shared install in two parts.

### The soft-link boundary

The rule is **symlink what Ghost reads, materialise what Ghost writes**.

| Path in the instance dir | How it is created | Why |
| --- | --- | --- |
| `index.js`, `core/`, `node_modules/`, `package.json` | symlink → `source_dir` | Application code. Identical for every instance and never written to. |
| `loggingrc.js` | symlink → `source_dir` | Ghost's logging config, read from the process root (the instance dir). Without it Ghost silently logs to stdout only and `content/logs/` stays empty (#197). |
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

Do this once per Ghost version, per host, with
[`deploy/ghost-install.sh`](../deploy/ghost-install.sh). It runs as an admin
account with password sudo (not `goopy`) and asks for the password once:

```bash
# Everything a config names: one run makes the host ready for that config.
./deploy/ghost-install.sh southp@<host> <env>

# One version, ahead of the config that will name it.
./deploy/ghost-install.sh southp@<host> 6.71.0 22.23.4
```

For each Ghost version it produces two root-owned directories:

| Path | What | How it is checked |
| --- | --- | --- |
| `/opt/goopy-node/<node-version>/` | The official Node release | `SHASUMS256.txt` must be signed by a Node release key (the keyring is pinned to a commit of `nodejs/release-keys`, and the keyring's checksum is pinned too), and the tarball must match it |
| `/opt/goopy-life/ghost-<version>/` | The shared install instances link to | The release's `engines.node` must accept that Node, the layout must be the one the provisioner links against, and the release's SQLite driver must load |

Both must also be root's alone: owned by root, and writable by nobody else
(group write is allowed only for root's own group, which node-gyp's build
directories need). Every instance runs that code, so anything else that could
write it could reach all of them (#187).

It is idempotent. A version that is already there is checked the same way and
reported `unchanged`, so a re-run doubles as a health check. An install built
by hand as `goopy` is refused, not passed. A failed build
leaves a `.partial` directory, which the next run clears.

**Install before you configure.** `gl-serv --check-config` refuses a deploy
whose config names an install the host does not have, and dev deploys on every
merge to trunk. So for a new version the order is:

1. `./deploy/ghost-install.sh southp@<dev-host> <ghost-version> <node-version>`.
   It prints the `[[provisioner.versions]]` entry to add.
2. Add it to `deploy/config/dev.toml`, and merge.
3. Verify on dev (see [Upgrading Ghost](#upgrading-ghost)), then repeat for
   production.

**Why Node lives in `/opt/goopy-node` and not under `/opt/goopy-life`:** every
instance unit hides `/opt/goopy-life` behind an empty tmpfs and binds back only
its own `source_dir` (see [The sandbox](#the-sandbox-what-an-instance-can-reach)).
A Node under it would not exist for the instance that runs it. A `node_bin`
anywhere else (the NodeSource `/usr/bin/node`, say) is fine. The script checks
it but does not install it.

### What the build does, and why

The script follows what this section used to do by hand. Three choices in it are
easy to get wrong:

> **Use `npm pack` + the release's own package manager, not `npm install
> ghost@<version>`.** Ghost declares 21 of its dependencies as
> `file:components/*.tgz` — tarballs that live *inside* the Ghost package.
> Installing Ghost as a dependency of an empty project cannot work: npm resolves
> those paths against the project root, where `components/` does not exist, and
> fails with `ENOENT`. Unpacking first and installing from inside the package
> root is what makes them resolve.

> **6.x is pnpm; 5.x is yarn classic.** Ghost 6 ships a `pnpm-lock.yaml` and a
> `pnpm-workspace.yaml` that lean on pnpm-only features (`catalog:` version
> references, `overrides`, `allowBuilds`), none of which yarn or npm understand.
> corepack fetches the exact pnpm the `packageManager` field names. Ghost 5 has
> no `packageManager` field, and gets yarn 1.22.22. The script picks one by the
> lockfile the release ships.

> **Build with the Node that will run it.** Native addons (the SQLite driver,
> `sharp`, `re2`) are built for the Node ABI that runs the install. The script
> puts `node_bin`'s directory first on `PATH` for the whole build. An install
> built with one Node and run with another fails at its first boot. The check
> that the SQLite driver loads catches it, on every run.

Two version-specific findings are behind the rest of the script:

- **Ghost 6 declares its SQLite driver, `better-sqlite3`, optional.** A failed
  native build of it does not fail the install, so the script's check that the
  driver loads is what catches it. The `client: "sqlite3"` that our config
  renders is still right: Ghost 6 aliases it to `better-sqlite3`.
- **Ghost 5 compiles its `sqlite3` on Node 22**, so the host needs
  `build-essential` and `python3-setuptools` (`deploy/host-bootstrap.sh`
  installs both).
  - The `napi-build-utils` it locks compares N-API versions as strings, so
    Node 22's `10` sorts below sqlite3's `6`, and no prebuilt binary is ever
    chosen.
  - The `node-gyp` it locks imports `distutils`, which Python 3.12 removed and
    `python3-setuptools` puts back.

  Its published `yarn.lock` is also the whole monorepo's, which a production
  install prunes to under half. So the script runs yarn without
  `--frozen-lockfile`, then refuses the install if yarn resolved anything the
  lockfile did not pin.

### Pointing a config at it

Name the install in the host's `deploy/config/<env>.toml`, as a
`[[provisioner.versions]]` entry, and deploy. Settings shared by every version
sit on `[provisioner]` itself, ahead of the entries: in TOML, keys after an
entry's header belong to that entry.

```toml
[provisioner]
kind = "Ghost"
default_version = "6.63.0"     # what new instances get; must be an entry below
service_user = "goopy-ghost"

[[provisioner.versions]]       # one per Ghost the host offers (#210)
version = "6.63.0"             # both keys name the same release, so they never disagree
source_dir = "/opt/goopy-life/ghost-6.63.0"
node_bin = "/usr/bin/node"     # per version: each release names its own engines.node
```

The parser refuses a config with no entry, a version listed twice, or a
`default_version` that is not one of the entries. A config in the shape from
before #210 (`version`, `source_dir` and `node_bin` on `[provisioner]`) is
refused with a message saying where they moved. `--check-config` checks
**every** entry's install on the host, not just the default's.

```bash
./deploy/deploy.sh goopy@<host> <env>
```

The config is version-controlled and installed by the deploy, so editing
`/opt/goopy-life/config.toml` over ssh does not survive the next run.

### Requirements

- **Node.js** at `node_bin`. It must be an absolute path: systemd does not search
  `PATH` for `ExecStart`. It must also satisfy Ghost's `engines` range — for
  6.63.0 that is `^22.23.1 || ^24.20.0`.
- **`ghost-install.sh` is the only thing that checks Node against that range.**
  gl-serv never inspects it, so on a host prepared any other way the first
  symptom of a wrong Node is a spawned instance whose unit fails. Re-running the
  script against the host's config checks every configured version.
- **`source_dir` must be the version-stamped directory**, never a symlink that
  later moves. The provisioner stores the path as given, so a moving symlink
  would silently pull running instances onto a different Ghost — see
  [Upgrading Ghost](#upgrading-ghost).
- **`service_user`** must exist, and must not be the account gl-serv runs as
  (see [The sandbox](#the-sandbox-what-an-instance-can-reach)). The deployed
  configs name `goopy-ghost`, which `deploy/host-bootstrap.sh` creates; a host
  built by hand needs it added the same way:
  ```bash
  sudo useradd --system --no-create-home --home-dir /nonexistent \
      --shell /usr/sbin/nologin --comment "goopy Ghost instances" goopy-ghost
  ```
  It must be able to read `source_dir`. It needs no write access anywhere: the
  provisioner hands each instance's `content/` to it. Omitted, the key
  defaults to `goopy`, so that a config written before #187 still parses.
- **systemd 257 or newer** for `PrivatePIDs=`, which keeps instances of that one
  account apart (Debian 13 ships 257). An older systemd ignores the line with a
  warning, and the instances still boot, with that isolation gone.
- **`tini` at `/usr/bin/tini`** (Debian's `tini` package), PID 1 of every
  instance unit; see below. Without it no unit starts.
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
sudo tail -f {base_dir}/{slug}/content/logs/*.log   # Ghost writes them as goopy-ghost, mode 640
```

`deprovision` reverses all of it: stop and disable the unit, remove the unit
file, remove the nginx site and reload nginx, then release the working
directory. Releasing removes the instance's symlinks — never the base install
they point at.

---

## The sandbox: what an instance can reach

Visitors are anonymous Ghost Owners: theme zip upload, content import,
`routes.yaml`, image processing. The design assumes Ghost will one day have a
bug in that surface, and decides what the bug reaches: **that instance, and
nothing else** (#187). Not root, not gl-serv, not the registry, not other
visitors.

**Ownership.** gl-serv writes the instance tree as itself, then hands over
`content/` alone (`sudo chown -R -h`, through the existing `chown` rule in
`deploy/sudoers.goopy`):

| Path | Owner | The instance can |
|---|---|---|
| `{base_dir}/{slug}/` (the dataset root) | `goopy` | read |
| `config.production.json`, the symlinks into `source_dir` | `goopy` | read |
| `content/` and everything under it | `service_user` | read and write |
| `source_dir` | `root` | read |

**The unit.** `render_service_file` in
`backend/gl-core/src/goopy_provisioner/ghost_provisioner.rs` documents each
line; in short:

- `/opt/goopy-life` is replaced by an empty read-only tmpfs. Bound back:
  `source_dir` read-only, the instance directory read-only, and its `content/`
  writable on top. The registry, gl-serv's config and binaries, and every other
  instance are not there at all.
- `NoNewPrivileges`, `RestrictSUIDSGID` and an empty capability set: sudo
  refuses to run, whatever the sudoers say.
- `PrivatePIDs` and `PrivateIPC`: every instance runs as the same account.
  Without a PID namespace one instance could read another's files through
  `/proc/<pid>/root` and signal its processes; without an IPC namespace it could
  attach to another's shared memory. `RemoveIPC` comes only with `PrivateIPC`:
  on its own, stopping one instance would remove every instance's IPC objects.
- `SystemCallFilter=@system-service` minus `@privileged` and `@resources`, with
  `EPERM` for anything else, and `RestrictAddressFamilies` to TCP, Unix and
  netlink sockets (`os.networkInterfaces()` needs netlink). With sudo dead, the
  kernel is what stands between `goopy-ghost` and root; these cut the
  interfaces an exploit would go through.
- `ProtectSystem=strict`, `PrivateTmp`, `PrivateDevices`, the kernel
  `Protect*` lines and `IPAddressDeny=169.254.169.254`.
- **Not** `MemoryDenyWriteExecute` (breaks V8's JIT), `MemoryMax` (the #113
  caps were measured without one), `PrivateNetwork` (nginx reaches Ghost over
  loopback, and Ghost fetches embed and bookmark cards), `PrivateUsers` (the
  base install and the config would map to `nobody`) or `ProcSubset=pid`
  (`os.cpus()` reads `/proc/stat`).

Outside the unit, the host firewall (`deploy/host-bootstrap.sh`) refuses
`goopy-ghost`'s connections to gl-serv's loopback port. From there an instance
would skip nginx and set its own `X-Real-IP`, the header the rate limiter keys
on. Instances still reach each other's loopback ports, which gives them nothing
the public sites don't.

`systemd-analyze security` on the unit: **1.3 OK**, from 9.2 UNSAFE before #187.

Two consequences an operator meets:

- **A stop is `tini` + `KillMode=control-group`** (#195). Under
  `PrivatePIDs`, `KillMode=mixed` SIGKILLed the unit on about half of all
  stops, with no wait after SIGTERM, so Ghost's shutdown never ran (6 of 12 on
  the dev host, 0 of 12 without `PrivatePIDs`). Switching the kill mode alone
  still lost 3 of 12, and Node as PID 1 ignored a SIGTERM that came before
  Ghost had installed its handler. The unit runs `tini -e 143 -- node …` with
  `KillMode=control-group`: systemd signals the whole cgroup and waits for it
  to empty, tini is PID 1 and passes the signal on, and `-e 143` counts "ended
  by SIGTERM" as a clean exit. Measured: 12 of 12 stops clean, each logging
  "Ghost has shut down", and a stop during boot finishes in ~100 ms.
- **Ghost's files are not `goopy`'s to read.** `UMask=0027` makes what Ghost
  writes readable by `goopy-ghost` alone, so reading an instance's logs needs
  `sudo`. `zfs destroy` removes the dataset whatever owns the files.

Dev mode has none of this: Ghost runs as your own user, with no unit.

---

## Outbound requests: the phone-home keys

A stock Ghost talks to third parties on boot and on a schedule. That is a
reasonable default for a real, long-lived site and the wrong one here: every
instance is a throwaway sandbox handed to a stranger, so the traffic scales with
visitors rather than with deployments, and what it carries — the instance's
public URL, the email typed at `/ghost/` setup — belongs to whoever is holding
the sandbox.

`render_ghost_config` therefore writes five keys into each instance's
`config.production.json` whose only job is to keep that traffic from happening.
They look like clutter, which is exactly the risk: **an empty string that
silently disables a feature is the kind of line that gets tidied up.** Each one
carries a comment in `ghost_provisioner.rs` saying why it is there.

| Key | Stops | Off-switch is clean? |
| --- | --- | --- |
| `explore.update_url: ""` | Boot-time POST of the instance URL, site UUID, theme and post counts to `explore.ghost.org` | **Yes** — `ping()` returns early on a falsy URL |
| `updateCheck.url: ""` | Daily GET to `updates.ghost.org` | **No** — see below |
| `privacy.useGravatar: false` | Hashing the setup email and fetching it from Gravatar | Yes |
| `privacy.useIndexNow: false` | Announcing each published post's URL to search engines (Ghost 6) | Yes |
| `privacy.useRpcPing: false` | POSTing each published post's URL to `rpc.pingomatic.com` (Ghost 5) | Yes — `ping()` returns early on the flag |

Every key goes to every version (#210). Ghost 6 has no RPC ping and Ghost 5 has
no IndexNow, and each version simply never reads the other's key, so one
rendered config covers the whole curated list with no branch on the version.
Verified against 5.130.6, 6.63.0 and 6.71.0: Ghost 5's explore ping guards an
empty URL and its update check fails on one exactly as 6's does (below), and
the privacy flags above are the only ones that reach a third party.

Two things deliberately left alone: `explore.testimonials_url` (a GET the admin
UI makes, not a POST of our data) and `useStructuredData` (emits schema.org and
OG tags, makes no outbound request — a sandbox should render like a real Ghost).
This is also why the config names the privacy flags individually instead of
using `privacy.useTinfoil: true`, which would also disable structured data.

`tinybird` needs no entry: it defaults to `null` upstream, so Ghost's stats
pipeline is already off.

### The update check is a trade, not a fix

Unlike the explore ping, Ghost has **no guard on an empty `updateCheck.url`**.
Verified against 6.63.0: `update-check/index.js` returns early only on the
environment, and `update-check-service.js` goes from `checkEndpoint` straight to
`await this.request(...)`. So the daily job still runs, the request throws on the
empty URL, and `updateCheckError` logs `Update check failed` and records the next
timestamp. `rethrowErrors` defaults to false, so nothing propagates.

The cost is **one error line per instance per day**, in that instance's own
`content/logs` — no outbound request, no effect on boot, nothing a visitor sees.
The job is scheduled at a random time in a 24-hour window and instances live
8 hours (`life_in_hours`), so most never reach it. A `Update check failed` line in an
instance's log is expected, not a symptom.

### These keys are version-sensitive — re-check them on every upgrade

The names above are **not stable across Ghost majors**, and a key that no longer
bites fails silently: the config still parses, the instance still boots, and the
traffic quietly comes back. What already changed between 5 and 6:

- `privacy.useUpdateCheck` **is gone in 6**. In 5 it never disabled the update
  check anyway — it only downgraded the request from POST to GET.
- The update check itself narrowed: 5 POSTed site stats, 6 sends a GET carrying
  only `ghost_version`.
- `services/indexnow-ping` is **new in 6** — a phone-home that did not exist when
  this was first looked at.
- The explore ping service moved from `.js` to `.ts`.

So on a Ghost upgrade, re-derive the list rather than assuming it still applies.
In the new install directory:

```bash
cd /opt/goopy-life/ghost-<new-version>

# Which privacy flags does this version actually read?
grep -rn "isPrivacyDisabled(" core/ --include=*.js --include=*.ts

# Which services gate themselves on a config URL, and do they guard an empty one?
grep -rln "phone home\|update_url\|updateCheck" core/server/services/
```

Then confirm on dev, not just in a unit test — a unit test proves only that we
emitted a key, never that Ghost honoured it. Spawn an instance and check its log:

```bash
tail -f {base_dir}/{slug}/content/logs/*.log | grep -i "pinging explore\|update-check\|gravatar"
```

No `Pinging Explore with Payload` line should ever appear.

---

## Readiness: why a spawn waits

A spawn does not report `Done` when the provisioning steps return. It reports
`Done` when the instance actually serves, and that is a deliberate extra wait of
several seconds.

Nothing in the start-up path knows whether Ghost works:

* the unit is `Type=simple`, so systemd calls it active the instant `ExecStart`
  has forked — `systemctl start` returns within milliseconds of `node` existing;
* Ghost binds its port immediately and then answers **its own maintenance page**
  for the whole of first boot: migrations against a brand-new SQLite file,
  fixture seeding, theme compilation.

Because Ghost is listening the entire time, a TCP-connect check would pass at
once and prove nothing. So after starting the unit — and *before* publishing the
nginx site — the provisioner polls `GET /` on `127.0.0.1:{port}` and accepts
**only a real 200**. Anything else, including the maintenance response, is "not
yet". The instance's own port is probed rather than `{slug}.{domain}`, because
the public route goes through the `auth_request` gate, which denies anything not
yet `Done` — probing it would deadlock.

The visible effect is that the frontend keeps showing `Spawning…` for a few
seconds longer, and the URL it eventually reveals renders Ghost on the first
request. Before this existed, the URL was handed over roughly 13 s early and the
only recovery on offer was the reload button (#151).

### Why the probe forges two nginx headers

Taking the shortcut around nginx costs the probe the request context nginx would
have added, and Ghost does not treat that as cosmetic. It enforces its
configured canonical `url`: a request that looks like it arrived on the wrong
scheme is answered with a `301` to the right one, **however healthy the instance
is**. Measured against Ghost 6.63.0 on the dev host, probing `GET /` on the
instance's own port:

| what the probe sends | booting | booted |
|---|---|---|
| nothing (bare loopback request) | `503` | **`301`** |
| `Host:` alone | — | `301` |
| `X-Forwarded-Proto: http` | — | `301` |
| **`X-Forwarded-Proto: https`** | `503` | **`200`** |

So a bare probe against a fully booted instance never sees a `200` and the wait
can only end in a timeout — every spawn `Failed` after the full budget. The
probe therefore sends the same `Host` and `X-Forwarded-Proto` that
`nginx.rs` sets on the real route.

The scheme is read from the instance's own configuration, not fixed at `https`:
a dev instance is configured for `http://127.0.0.1:{port}` and would be
redirected just as firmly the other way. `GhostProvisioner::instance_origin`
is the single source of both the probe's origin and the `url` written into
`config.production.json`, so the two cannot drift apart.

This is the same wall `goopy_provisioner::nginx` documents for the production
route; the probe hits it because it deliberately bypasses that route.

### Tuning it

Two keys under `[provisioner]`, when `kind = "Ghost"`:

| key | default | what it does |
|---|---|---|
| `ready_timeout_secs` | `120` | how long an instance may take to serve before the spawn is given up on |
| `ready_poll_ms` | `500` | gap between probes |

One Ghost booting alone on the dev host took ~13 s, so the default timeout is
an order of magnitude of headroom. Raise it if the host runs several instances
at once, or if `zpool` IO is contended — a loaded host is slow, not broken.
Lowering it below a measured boot time throws away sandboxes seconds before they
would have worked.

`ready_timeout_secs` is a real ceiling, not a floor: each probe is given no more
than the wait has left, so a connection that hangs cannot carry a spawn past the
configured budget.

**When the budget runs out** the spawn fails: the instance ends `Failed`, its
port is released, and `sweep()` reaps its working directory. The log line
carries the last thing the instance said (`last probe: status 503`), which is
what to look at first:

```bash
sudo journalctl -u gl-serv | grep 'never became ready'
sudo journalctl -u goopy-{slug}          # what Ghost itself was doing
```

A run of these usually means the host is slower than the budget assumes, not
that Ghost is broken.

---

## Upgrading Ghost

Instances are **pinned to the version they were created with**: each one is
stamped with a `service_version` at spawn time, and provisioned from that
version's entry, so its symlinks name that version-stamped install directly.
An instance created against `ghost-6.63.0` keeps pointing there whatever the
config says afterwards. A spawn may ask for any entry (`POST /goopies` with
`{"ghost_version": "…"}`, or `gl-cli spawn --ghost …`); one that names none
gets `default_version`, and one that names a version with no entry is refused
with `400 unknown_version` before anything is allocated.

To move the default to a new version:

1. Prepare the new version in its own directory, on the host, before any config
   names it:
   ```bash
   ./deploy/ghost-install.sh southp@<host> 6.64.0 22.23.4
   ```
2. Add its entry, and point `default_version` at it:
   ```toml
   [provisioner]
   default_version = "6.64.0"

   [[provisioner.versions]]
   version = "6.64.0"
   source_dir = "/opt/goopy-life/ghost-6.64.0"
   node_bin = "/opt/goopy-node/22.23.4/bin/node"

   [[provisioner.versions]]
   version = "6.63.0"
   source_dir = "/opt/goopy-life/ghost-6.63.0"
   node_bin = "/usr/bin/node"
   ```
   Keeping the old entry is optional: deprovisioning never reads the config's
   installs, so an instance whose version has left the config still tears
   down cleanly.
3. Deploy, which installs the edited config and restarts `gl-serv`.
4. Instances spawned from now on use the new version. Existing instances keep
   running against the old one.
5. **Re-check the phone-home keys** against the new version — see
   [Outbound requests](#outbound-requests-the-phone-home-keys). Their names have
   already changed once between majors, and a key that stops biting fails
   silently: the config still parses and the instance still boots, but the
   traffic comes back. Spawn one instance on dev and confirm its log carries no
   `Pinging Explore` line before letting the upgrade reach production.

> Do not introduce a stable `/opt/goopy-life/ghost` symlink and point
> `source_dir` at it. The provisioner links each instance at
> `{source_dir}/index.js` and friends as written, so repointing such a symlink
> would redirect **every existing instance** the next time its Ghost process
> restarts — including when #96 suspends and resumes it. Adding an entry is a
> few lines, and the version-stamped path in it is what makes pinning real.

### Retiring the old install

Instances created before the switch hold links into the **old** directory. Take
its entry out of the config whenever you like, but do not delete the directory
until every instance that references it is gone:

```bash
# Which versions are still in use?
gl-cli list | grep service_version | sort | uniq -c
```

Once no instance reports the old version — instances are ephemeral, so this
happens within `life_in_hours` — remove it:

```bash
rm -rf /opt/goopy-life/ghost-6.63.0
```

---

## Dev mode

With `dev_mode` on, the provisioner skips systemd and nginx entirely: it
assembles the same instance directory, writes the same
`config.production.json` — with `url` pointing straight at `http://127.0.0.1:{port}`
since there is no proxy in front — spawns Ghost as a detached background process,
and records the PID in `{working_dir}/server.pid`. `deprovision` kills that PID
and removes the directory.

Ghost is always run with `NODE_ENV=production` so it reads
`config.production.json`; "dev mode" refers to goopy.life's mode, not Ghost's.

The readiness wait applies here too — same Ghost, same first boot — so a local
spawn also sits in `Spawning` until the instance serves.

On macOS, set `node_bin` to the output of `which node` — the `/usr/bin/node`
default is Linux-specific.
