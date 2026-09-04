//! Parses every configuration file the repository commits.
//!
//! Two kinds are covered, for the same reason and with different rules:
//!
//! * `deploy/config/*.toml` are installed verbatim on the droplets, so a field
//!   gl-core starts requiring without a matching edit there is not a stale
//!   fixture — it is a service that will not boot.
//! * `backend/config.local.toml` is what a developer runs locally. Committing
//!   it is only an improvement over copying an example if it cannot rot, and
//!   this is what stops it rotting.
//!
//! Checking both here moves that failure from deploy time on a live host, or
//! from a confusing first afternoon on a new checkout, to review time on a
//! branch. This is the check that would have caught #63, which replaced the
//! flat `provisioner_kind` key with a `[provisioner]` table: the dev droplet's
//! config kept the old spelling and stayed unparseable until a deploy months
//! later crash-looped gl-serv with `missing field provisioner`.

use std::path::PathBuf;

use gl_core::Config;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root should be two levels above gl-core")
}

/// Every `.toml` under `deploy/config/`, sorted for a stable failure order.
/// These are exactly the configurations `deploy.sh` can ship to a host.
fn deployed_configs() -> Vec<PathBuf> {
    let dir = repo_root().join("deploy/config");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("deploy/config should be readable — did the directory move?")
        .map(|entry| entry.expect("readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    files
}

/// The committed local development configuration. It lives outside
/// `deploy/config/` deliberately: that directory is the deployable set, and
/// `dev_mode = true` on a real droplet would skip systemd, nginx and ZFS.
fn local_config() -> PathBuf {
    repo_root().join("backend/config.local.toml")
}

#[test]
fn every_committed_config_parses() {
    let mut files = deployed_configs();
    // An empty deploy/config would make this vacuously green, which is the
    // exact silence it exists to break.
    assert!(
        !files.is_empty(),
        "no .toml files found in deploy/config — this test would pass by default"
    );
    files.push(local_config());

    for path in files {
        if let Err(e) = Config::from_file(&path) {
            panic!("{} does not parse: {e:?}", path.display());
        }
    }
}

#[test]
fn no_deployed_config_enables_dev_mode() {
    for path in deployed_configs() {
        let cfg = Config::from_file(&path).expect("deployed configs parse");
        assert!(
            !cfg.dev_mode,
            "{} sets dev_mode = true, which skips systemd, nginx and ZFS on a real host",
            path.display()
        );
    }
}

#[test]
fn the_local_config_enables_dev_mode() {
    let cfg = Config::from_file(&local_config()).expect("the local config parses");
    assert!(
        cfg.dev_mode,
        "backend/config.local.toml must keep dev_mode = true — without it a local \
         run tries to drive systemd, nginx and ZFS"
    );
    // Together with the test above this pins the split: a local-style config
    // moved into deploy/config/ fails there, and this one stops the local
    // config quietly acquiring host-shaped settings.
}
