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
use gl_core::config::ProvisionerConfig;

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

/// On a deployed host nginx must be the *only* path to gl-serv.
///
/// A wildcard bind puts gl-serv on the public interface beside nginx, where a
/// caller reaching it directly skips TLS and supplies its own `X-Real-IP` — the
/// header the per-IP rate limiter keys on (#21, #105). The limiter is then not
/// weakened but defeated, since a fresh forged IP per request empties every
/// bucket. Asserted on the committed files rather than left to review because
/// the exposure is invisible from the outside: everything keeps working.
///
/// Deployed configs only. A local run binding every interface is a reasonable
/// thing to want (reaching the dev server from a phone) and risks nothing.
#[test]
fn no_deployed_config_binds_a_wildcard() {
    for path in deployed_configs() {
        let cfg = Config::from_file(&path).expect("deployed configs parse");
        let addr: std::net::SocketAddr = cfg
            .bind_address
            .parse()
            .expect("from_file has already rejected an unparseable bind_address");
        assert!(
            !addr.ip().is_unspecified(),
            "{} binds {} — a deployed host must keep gl-serv on loopback, or the \
             rate limiter's X-Real-IP can be forged by anyone who talks to port \
             {} directly",
            path.display(),
            cfg.bind_address,
            addr.port(),
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

/// `source_dir` and `version` are two hand-edited keys that must agree, and
/// nothing at runtime cross-checks them: the provisioner links instances into
/// `source_dir` and separately stamps `version` onto each one, so a stale
/// `version` mislabels every instance a host creates and a stale `source_dir`
/// silently serves the previous Ghost. `docs/GHOST_PROVISIONER.md` therefore
/// requires the install directory to be version-stamped — this asserts the
/// committed configs actually are, which is the only place the two can be
/// compared without the host in front of you.
#[test]
fn a_ghost_deploy_config_names_a_version_stamped_source_dir() {
    for path in deployed_configs() {
        let cfg = Config::from_file(&path).expect("deployed configs parse");
        let ProvisionerConfig::Ghost(ghost) = &cfg.provisioner else {
            continue;
        };

        // Absolute: the provisioner symlinks instances at this path as written,
        // and each instance's working directory is somewhere else entirely.
        assert!(
            ghost.source_dir.is_absolute(),
            "{}: source_dir {} must be absolute",
            path.display(),
            ghost.source_dir.display()
        );

        let dir_name = ghost
            .source_dir
            .file_name()
            .expect("an absolute source_dir has a final component")
            .to_string_lossy();
        // A bare suffix match is not enough: `ghost-6.63.0` ends with `3.0`,
        // so a truncated or otherwise mistyped version would sail through the
        // very check meant to catch it. Require the version to start at a
        // boundary — either the whole component, or preceded by something that
        // could not itself be part of a version number.
        let stamped = dir_name
            .strip_suffix(ghost.version.as_str())
            .is_some_and(|prefix| {
                prefix.is_empty()
                    || !prefix.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '.')
            });
        assert!(
            stamped,
            "{}: source_dir {dir_name} is not stamped with version {} — the \
             version must be the final component or follow a separator, so \
             that a partial match like `ghost-6.63.0` against `3.0` does not \
             pass. The two keys disagree, and instances would be stamped with \
             a version the install does not hold",
            path.display(),
            ghost.version
        );
    }
}
