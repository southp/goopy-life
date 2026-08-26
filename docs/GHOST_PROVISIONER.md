# Ghost Provisioner — operator guide

`GhostProvisioner` is the production provisioner. It creates a real,
SQLite-backed Ghost instance at `{slug}.{domain}` by soft-linking every instance
against **one shared base Ghost install** instead of running `npm install` per
instance.

This document covers the two things an operator has to do: **prepare the base
install**, and **upgrade it**.

---

## Why a shared base install

A Ghost install is roughly 200 MB, almost all of it `node_modules`. Installing
one per sandbox would make provisioning slow and put a hard ceiling on how many
instances fit on the droplet. Instead each instance directory is assembled from
the shared install in two parts.

### The soft-link boundary

The rule is **symlink what Ghost reads, materialise what Ghost writes**.

| Path in the instance dir | How it is created | Why |
| --- | --- | --- |
| `index.js`, `core/`, `node_modules/`, `package.json` | symlink → `ghost_source_dir` | Application code. Identical for every instance and never written to. |
| `content/themes/casper` | symlink → `ghost_source_dir` | The stock theme ships with Ghost and is read-only. |
| `content/data/` | real directory | Holds `ghost.db`, this instance's SQLite database. |
| `content/images/`, `content/media/`, `content/files/` | real directory | User uploads. |
| `content/themes/` | real directory | So a user-uploaded theme lands here, next to the `casper` symlink. |
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

Do this once per Ghost version, as `root` on the droplet.

```bash
# 1. Pick a version-stamped directory. Keeping the version in the path lets a
#    new install be prepared while the current one is still serving instances.
GHOST_VERSION=5.87.1
INSTALL_DIR=/opt/goopy-life/ghost-${GHOST_VERSION}

mkdir -p "${INSTALL_DIR}"
cd "${INSTALL_DIR}"

# 2. Install Ghost itself — NOT ghost-cli. We provision instances ourselves.
npm install ghost@${GHOST_VERSION} --production

# 3. Ghost lands under node_modules/ghost; hoist it so the install directory is
#    a Ghost root with index.js, core/, node_modules/ and package.json at the top.
mv node_modules/ghost/* .
mv node_modules/ghost/.[!.]* . 2>/dev/null || true
rmdir node_modules/ghost

# 4. Confirm the layout the provisioner expects.
ls index.js core node_modules package.json content/themes/casper

# 5. Make it read-only to the service account — instances only ever read it.
chown -R root:root "${INSTALL_DIR}"
chmod -R a+rX "${INSTALL_DIR}"

# 6. Point the stable path at this version.
ln -sfn "${INSTALL_DIR}" /opt/goopy-life/ghost
```

Then set the provisioner section in `/opt/goopy-life/config.toml`:

```toml
[provisioner]
kind = "Ghost"
ghost_source_dir = "/opt/goopy-life/ghost"
ghost_version = "5.87.1"
node_bin = "/usr/bin/node"
service_user = "goopy"
```

and restart the API server:

```bash
sudo systemctl restart gl-serv
```

### Requirements

- **Node.js** at `node_bin`. It must be an absolute path: systemd does not search
  `PATH` for `ExecStart`. Use the major version Ghost supports for the version
  you installed.
- **`service_user`** (`goopy` by default) must be able to read `ghost_source_dir`
  and write the instance working directories under `base_dir`. Instances run as
  this user, never as root.
- **A wildcard TLS certificate** for the domain at
  `/etc/letsencrypt/live/<domain>/` — the same prerequisite the Hello
  provisioner has.

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

Instances are **pinned to the version they were created with**. `ghost_version`
is recorded on every instance as `service_version` at spawn time, and the
instance keeps running against the base install it was linked to. Beta runs a
single Ghost version at a time; supporting several coexisting versions is a
follow-up.

To upgrade:

1. Prepare the new version in its own directory, following the steps above with
   a new `GHOST_VERSION` (e.g. `/opt/goopy-life/ghost-5.90.0`).
2. Repoint the stable symlink:
   ```bash
   ln -sfn /opt/goopy-life/ghost-5.90.0 /opt/goopy-life/ghost
   ```
3. Bump `ghost_version` in `config.toml` to match, and restart `gl-serv`.
4. Instances spawned from now on use the new version. Existing instances keep
   running against the old one.

### Retiring the old install

Because the stable symlink is resolved at provision time, instances created
before the switch hold links into the **old** directory. Do not delete it until
every instance that references it is gone:

```bash
# Which versions are still in use?
gl-cli list | grep service_version | sort | uniq -c
```

Once no instance reports the old version — instances are ephemeral, so this
happens within `life_in_days` — remove it:

```bash
rm -rf /opt/goopy-life/ghost-5.87.1
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
