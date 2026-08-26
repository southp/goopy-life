//! Shared dev-mode process supervision for provisioners.
//!
//! Dev mode has no systemd: the instance is a detached background process whose
//! PID is recorded in `server.pid` inside its working directory. That filename
//! is a contract between spawning and killing, so both halves live here rather
//! than being restated by each provisioner.

use std::path::Path;

use tracing::info;

use crate::shared_types::Error;
use crate::sys_utils::SysRunner;

const PID_FILE: &str = "server.pid";

/// Spawns `program` detached in `working_dir` and records its PID.
pub(crate) fn spawn(
    sys: &dyn SysRunner,
    working_dir: &Path,
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    log_name: &str,
) -> Result<(), Error> {
    info!(working_dir = %working_dir.display(), program, "spawning dev server");
    let log_path = working_dir.join(log_name);
    let pid = sys.spawn_detached(program, args, working_dir, envs, &log_path)?;

    let pid_path = working_dir.join(PID_FILE);
    info!(%pid, pid_path = %pid_path.display(), "writing PID file");
    std::fs::write(&pid_path, pid.to_string()).map_err(Error::Io)
}

/// Kills the recorded process and removes the PID file.
///
/// Succeeds when no PID file is present: a `Failed` instance may never have got
/// as far as spawning anything.
pub(crate) fn kill(sys: &dyn SysRunner, working_dir: &Path) -> Result<(), Error> {
    let pid_path = working_dir.join(PID_FILE);
    let pid_str = match std::fs::read_to_string(&pid_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!(pid_path = %pid_path.display(), "no PID file found, nothing to kill");
            return Ok(());
        }
        Err(e) => return Err(Error::Io(e)),
    };
    let pid = pid_str.trim();

    if pid.is_empty() {
        return Err(Error::Invalid);
    }

    if !pid.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::Subprocess(format!("invalid PID in file: {pid:?}")));
    }

    info!(%pid, "killing dev server");
    sys.kill_pid(pid)?;
    std::fs::remove_file(&pid_path).map_err(Error::Io)
}
