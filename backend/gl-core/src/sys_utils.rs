use std::io::Write;
use std::path::Path;
use std::process::Command;
#[cfg(any(test, feature = "test-utils"))]
use std::sync::Mutex;

use tracing::{debug, error, info};

use crate::shared_types::Error;

/// Abstraction over privileged system operations.
///
/// Implementations range from `RealSysRunner` (executes commands for real) to
/// `DryRunSysRunner` (prints what would run) and `MockSysRunner` (records calls
/// for use in unit tests).
pub trait SysRunner: Send + Sync {
    /// Run a program and wait for it to exit successfully.
    fn run(&self, program: &str, args: &[&str]) -> Result<(), Error>;
    /// Write `content` to a privileged `path` via `sudo tee`.
    fn sudo_write(&self, path: &str, content: &str) -> Result<(), Error>;
    /// Spawn `program args` in `working_dir` as a detached background process,
    /// redirecting stderr to `log_path`. Waits ~200 ms and checks liveness;
    /// returns the PID on success or an error (with log contents) if the process
    /// exited immediately.
    fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        log_path: &Path,
    ) -> Result<u32, Error>;
    /// Send SIGTERM to the process with the given string PID.
    /// Returns `Ok` if the process was signalled or was already gone (ESRCH).
    /// Returns `Err` if the `kill` binary itself could not be executed.
    fn kill_pid(&self, pid: &str) -> Result<(), Error>;
}

// ── RealSysRunner ─────────────────────────────────────────────────────────────

/// Executes commands for real using [`std::process::Command`].
pub struct RealSysRunner;

impl SysRunner for RealSysRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<(), Error> {
        info!(program, ?args, "running command");
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| Error::Other(format!("failed to run {program}: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!(program, %stderr, "command failed");
            return Err(Error::Other(format!(
                "{program} failed (exit {}): {}",
                output.status,
                stderr.trim()
            )));
        }
        Ok(())
    }

    fn sudo_write(&self, path: &str, content: &str) -> Result<(), Error> {
        info!(path, "writing privileged file via sudo tee");
        let mut child = Command::new("sudo")
            .args(["tee", path])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .map_err(|e| Error::Other(format!("failed to spawn sudo tee: {e}")))?;

        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(content.as_bytes())
            .map_err(|e| Error::Other(format!("failed to write to sudo tee stdin: {e}")))?;

        let status = child
            .wait()
            .map_err(|e| Error::Other(format!("sudo tee wait failed: {e}")))?;

        if !status.success() {
            return Err(Error::Other(format!(
                "sudo tee {path} exited with status {status}"
            )));
        }
        Ok(())
    }

    fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        log_path: &Path,
    ) -> Result<u32, Error> {
        #[cfg(unix)]
        use std::os::unix::process::CommandExt as _;

        let log_file = std::fs::File::create(log_path)
            .map_err(|e| Error::Other(format!("failed to create log file: {e}")))?;

        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::null())
            .stderr(log_file);

        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Other(format!("failed to spawn {program}: {e}")))?;

        // Give the process a moment to start up (or crash).
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Check for an immediate exit — startup errors surface well within 200 ms.
        match child.try_wait() {
            Ok(Some(status)) => {
                let log = std::fs::read_to_string(log_path).unwrap_or_default();
                return Err(Error::Other(format!(
                    "{program} exited immediately (status {status})\n{log}"
                )));
            }
            Ok(None) => {} // still running — good
            Err(e) => {
                return Err(Error::Other(format!("failed to check {program} status: {e}")));
            }
        }

        let pid = child.id();
        // Detach: forget the Child so that Drop does not wait on the process.
        std::mem::forget(child);
        Ok(pid)
    }

    fn kill_pid(&self, pid: &str) -> Result<(), Error> {
        match Command::new("kill").args([pid]).output() {
            Ok(out) if !out.status.success() => {
                debug!(%pid, status = %out.status, "kill returned non-zero (process may have already exited)");
                Ok(())
            }
            Err(e) => Err(Error::Other(format!("kill command failed to execute: {e}"))),
            _ => Ok(()),
        }
    }
}

// ── DryRunSysRunner ───────────────────────────────────────────────────────────

/// Prints what would be executed without running anything.
///
/// Useful with the `--dry-run` CLI flag to preview privileged operations.
pub struct DryRunSysRunner;

impl SysRunner for DryRunSysRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<(), Error> {
        println!("[dry-run] would run: {program} {}", args.join(" "));
        Ok(())
    }

    fn sudo_write(&self, path: &str, content: &str) -> Result<(), Error> {
        println!("[dry-run] would sudo_write to: {path} ({} bytes)", content.len());
        Ok(())
    }

    fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        log_path: &Path,
    ) -> Result<u32, Error> {
        println!(
            "[dry-run] would spawn: {program} {} in {} (log: {})",
            args.join(" "),
            working_dir.display(),
            log_path.display()
        );
        Ok(0)
    }

    fn kill_pid(&self, pid: &str) -> Result<(), Error> {
        println!("[dry-run] would kill PID {pid}");
        Ok(())
    }
}

// ── MockSysRunner ─────────────────────────────────────────────────────────────

/// Records all calls so tests can assert on the exact sequence of commands.
#[cfg(any(test, feature = "test-utils"))]
pub struct MockSysRunner {
    calls: Mutex<Vec<MockCall>>,
}

/// A single recorded call to [`MockSysRunner`].
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone)]
pub enum MockCall {
    Run {
        program: String,
        args: Vec<String>,
    },
    SudoWrite {
        path: String,
        content: String,
    },
    SpawnDetached {
        program: String,
        args: Vec<String>,
        working_dir: std::path::PathBuf,
        log_path: std::path::PathBuf,
    },
    KillPid {
        pid: String,
    },
}

#[cfg(any(test, feature = "test-utils"))]
impl MockSysRunner {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(vec![]),
        }
    }

    /// Returns all recorded calls in order.
    pub fn recorded_calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl Default for MockSysRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl SysRunner for MockSysRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<(), Error> {
        self.calls.lock().unwrap().push(MockCall::Run {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        });
        Ok(())
    }

    fn sudo_write(&self, path: &str, content: &str) -> Result<(), Error> {
        self.calls.lock().unwrap().push(MockCall::SudoWrite {
            path: path.to_string(),
            content: content.to_string(),
        });
        Ok(())
    }

    fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        log_path: &Path,
    ) -> Result<u32, Error> {
        self.calls.lock().unwrap().push(MockCall::SpawnDetached {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            working_dir: working_dir.to_path_buf(),
            log_path: log_path.to_path_buf(),
        });
        Ok(0)
    }

    fn kill_pid(&self, pid: &str) -> Result<(), Error> {
        self.calls
            .lock()
            .unwrap()
            .push(MockCall::KillPid { pid: pid.to_string() });
        Ok(())
    }
}
