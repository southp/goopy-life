//! Parses the configuration files that are actually deployed.
//!
//! `deploy/config/*.toml` are installed verbatim on the droplets, so a field
//! this crate starts requiring without a matching edit there is not a stale
//! fixture — it is a service that will not boot. Checking them here moves that
//! failure from deploy time on a live host to review time on a branch.
//!
//! This is the check that would have caught #63, which replaced the flat
//! `provisioner_kind` key with a `[provisioner]` table. The dev droplet's
//! config kept the old spelling and stayed unparseable until a deploy months
//! later crash-looped gl-serv with `missing field provisioner`.

use std::path::PathBuf;

use gl_core::Config;

/// Every `.toml` under `deploy/config/`, sorted for a stable failure order.
fn deployed_configs() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../deploy/config")
        .canonicalize()
        .expect("deploy/config should exist — did the directory move?");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("deploy/config should be readable")
        .map(|entry| entry.expect("readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    files
}

#[test]
fn every_deployed_config_parses() {
    let files = deployed_configs();
    // An empty directory would make this test vacuously green, which is the
    // exact silence it exists to break.
    assert!(
        !files.is_empty(),
        "no .toml files found in deploy/config — this test would pass by default"
    );
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
