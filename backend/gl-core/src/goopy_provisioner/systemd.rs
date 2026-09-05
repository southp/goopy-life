//! Shared systemd unit management for provisioners.
//!
//! Each instance is supervised by its own `{unit}.service`. The unit *bodies*
//! genuinely differ per provisioner — Ghost needs `User=` and `NODE_ENV`, Hello
//! does not — so each provisioner renders its own. Everything about installing,
//! starting and removing that unit is identical, and lives here.

use crate::shared_types::Error;
use crate::sys_utils::SysRunner;

fn unit_path(unit: &str) -> String {
    format!("/etc/systemd/system/{unit}.service")
}

fn unit_file(unit: &str) -> String {
    format!("{unit}.service")
}

/// Writes the unit file, then reloads systemd and enables and starts the unit.
pub(crate) fn install_and_start(sys: &dyn SysRunner, unit: &str, body: &str) -> Result<(), Error> {
    sys.sudo_write(&unit_path(unit), body)?;
    let svc = unit_file(unit);
    sys.sudo_run(&["systemctl", "daemon-reload"])?;
    sys.sudo_run(&["systemctl", "enable", &svc])?;
    sys.sudo_run(&["systemctl", "start", &svc])
}

/// Stops and disables the unit, removes its file, and reloads systemd.
///
/// `stop`/`disable` tolerate a missing or never-installed unit: a `Failed`
/// instance may hold only partial state, and `sweep()` reaps those, so a
/// non-zero exit here must not block cleanup and strand the instance. The
/// `rm -f` + `daemon-reload` below is the authoritative removal.
pub(crate) fn stop_and_remove(sys: &dyn SysRunner, unit: &str) -> Result<(), Error> {
    let svc = unit_file(unit);

    if let Err(e) = sys.sudo_run(&["systemctl", "stop", &svc]) {
        tracing::warn!(error = %e, %svc, "systemctl stop failed (unit may not exist), continuing");
    }
    if let Err(e) = sys.sudo_run(&["systemctl", "disable", &svc]) {
        tracing::warn!(error = %e, %svc, "systemctl disable failed (unit may not exist), continuing");
    }

    sys.sudo_run(&["rm", "-f", &unit_path(unit)])?;
    sys.sudo_run(&["systemctl", "daemon-reload"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys_utils::MockSysRunner;

    #[test]
    fn install_and_start_writes_unit_then_enables_it() {
        let sys = MockSysRunner::new();
        install_and_start(&sys, "goopy-tasty-lucky-clover", "[Unit]\n").unwrap();

        assert_eq!(
            sys.sudo_write_paths(),
            ["/etc/systemd/system/goopy-tasty-lucky-clover.service"]
        );
        let args = sys.sudo_run_args();
        let verbs: Vec<&String> = args
            .iter()
            .filter(|a| ["daemon-reload", "enable", "start"].contains(&a.as_str()))
            .collect();
        assert_eq!(verbs, ["daemon-reload", "enable", "start"]);
    }

    /// A `Failed` instance may never have had its unit installed, and `sweep()`
    /// reaps those — so a failing stop/disable must not prevent the unit file
    /// from being removed, or the instance is stranded forever.
    #[test]
    fn stop_and_remove_continues_when_the_unit_does_not_exist() {
        let sys = MockSysRunner::failing_sudo_run(|args| {
            matches!(args.get(1), Some(&"stop") | Some(&"disable"))
        });

        stop_and_remove(&sys, "goopy-tasty-lucky-clover")
            .expect("removal must succeed even when the unit was never installed");

        let args = sys.sudo_run_args();
        assert!(
            args.contains(&"/etc/systemd/system/goopy-tasty-lucky-clover.service".to_string()),
            "unit file must still be removed"
        );
        assert_eq!(
            args.last().map(String::as_str),
            Some("daemon-reload"),
            "systemd must be reloaded after the unit file is gone"
        );
    }
}
