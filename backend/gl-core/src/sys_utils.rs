use std::io::Write;
use std::path::Path;
#[cfg(any(test, feature = "test-utils"))]
use std::path::PathBuf;
use std::process::Command;
#[cfg(any(test, feature = "test-utils"))]
use std::sync::Mutex;

use tracing::{debug, error, info};

use crate::shared_types::Error;

/// Abstraction over privileged system operations.
///
/// Implementations: `RealSysRunner` (executes commands for real) and
/// `MockSysRunner` (records calls for use in unit tests).
pub trait SysRunner: Send + Sync {
    /// Run a program and wait for it to exit successfully.
    fn run(&self, program: &str, args: &[&str]) -> Result<(), Error>;
    /// Run a command via `sudo -n` (non-interactive, no password prompt).
    fn sudo_run(&self, args: &[&str]) -> Result<(), Error>;
    /// Write `content` to a privileged `path` via `sudo -n tee`.
    fn sudo_write(&self, path: &str, content: &str) -> Result<(), Error>;

    /// Spawn a long-running `program` in `working_dir` as a detached background
    /// process and return its PID.
    ///
    /// `envs` are extra environment variables for the child; stderr is redirected
    /// to `log_path`. Implementations must confirm the process survived startup
    /// rather than returning a PID that has already exited.
    fn spawn_detached(
        &self,
        program: &str,
        args: &[&str],
        working_dir: &Path,
        envs: &[(&str, &str)],
        log_path: &Path,
    ) -> Result<u32, Error>;

    /// Terminate the process with the given PID. Succeeds if it is already gone.
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
            .map_err(Error::Io)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!(program, %stderr, "command failed");
            return Err(Error::Subprocess(format!(
                "{program} failed (exit {}): {}",
                output.status,
                stderr.trim()
            )));
        }
        Ok(())
    }

    fn sudo_run(&self, args: &[&str]) -> Result<(), Error> {
        let mut full: Vec<&str> = vec!["-n"];
        full.extend_from_slice(args);
        self.run("sudo", &full)
    }

    fn sudo_write(&self, path: &str, content: &str) -> Result<(), Error> {
        info!(path, "writing privileged file via sudo tee");
        let mut child = Command::new("sudo")
            .args(["-n", "tee", path])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .map_err(Error::Io)?;

        child
            .stdin
            .take()
            .expect("stdin configured as piped")
            .write_all(content.as_bytes())
            .map_err(Error::Io)?;

        let status = child.wait().map_err(Error::Io)?;

        if !status.success() {
            return Err(Error::Subprocess(format!(
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
        envs: &[(&str, &str)],
        log_path: &Path,
    ) -> Result<u32, Error> {
        info!(program, ?args, working_dir = %working_dir.display(), "spawning detached process");
        let log_file = std::fs::File::create(log_path).map_err(Error::Io)?;

        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(working_dir)
            .stdout(std::process::Stdio::null())
            .stderr(log_file);
        for (key, value) in envs {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(Error::Io)?;

        // Give the process a moment to start up (or crash).
        // 200 ms gives the process time to crash on import/config errors; a
        // well-behaved server has not exited by then even if it is still booting.
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Check for an immediate exit — startup errors surface well within 200 ms.
        match child.try_wait() {
            Ok(Some(status)) => {
                let log = std::fs::read_to_string(log_path).unwrap_or_default();
                Err(Error::Subprocess(format!(
                    "{program} exited immediately (status {status})\n{log}"
                )))
            }
            Ok(None) => {
                let pid = child.id();
                // Detach: forget the Child so that Drop does not wait on the process.
                std::mem::forget(child);
                Ok(pid)
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn kill_pid(&self, pid: &str) -> Result<(), Error> {
        info!(%pid, "killing process");
        let out = Command::new("kill")
            .args([pid.trim()])
            .output()
            .map_err(Error::Io)?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("No such process") {
                debug!(%pid, "process already gone");
            } else {
                return Err(Error::Subprocess(format!("kill {pid}: {}", stderr.trim())));
            }
        }
        Ok(())
    }
}

// ── MockSysRunner ─────────────────────────────────────────────────────────────

/// Records all calls so tests can assert on the exact sequence of commands.
#[cfg(any(test, feature = "test-utils"))]
pub struct MockSysRunner {
    calls: Mutex<Vec<MockCall>>,
    #[allow(clippy::type_complexity)]
    sudo_run_fails_when: Option<Box<dyn Fn(&[&str]) -> bool + Send + Sync>>,
}

/// A single recorded call to [`MockSysRunner`].
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone)]
pub enum MockCall {
    Run {
        program: String,
        args: Vec<String>,
    },
    SudoRun {
        args: Vec<String>,
    },
    SudoWrite {
        path: String,
        content: String,
    },
    SpawnDetached {
        program: String,
        args: Vec<String>,
        working_dir: PathBuf,
        envs: Vec<(String, String)>,
        log_path: PathBuf,
    },
    KillPid {
        pid: String,
    },
}

/// PID handed back by [`MockSysRunner::spawn_detached`]. Tests that assert on a
/// written PID file compare against this value.
#[cfg(any(test, feature = "test-utils"))]
pub const MOCK_SPAWNED_PID: u32 = 424_242;

#[cfg(any(test, feature = "test-utils"))]
impl MockSysRunner {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(vec![]),
            sudo_run_fails_when: None,
        }
    }

    /// A mock whose `sudo_run` records the call and then fails whenever `pred`
    /// matches its arguments.
    ///
    /// For exercising tolerance of commands that legitimately fail against
    /// partial state — e.g. `systemctl stop` on a unit that was never installed.
    pub fn failing_sudo_run(pred: impl Fn(&[&str]) -> bool + Send + Sync + 'static) -> Self {
        Self {
            calls: Mutex::new(vec![]),
            sudo_run_fails_when: Some(Box::new(pred)),
        }
    }

    /// Every argument passed to `sudo_run`, flattened in call order.
    ///
    /// This is what assertions actually want — both "these verbs ran in this
    /// order" and "this path was removed" read off a flat list.
    pub fn sudo_run_args(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                MockCall::SudoRun { args } => Some(args.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Paths passed to `sudo_write`, in call order.
    pub fn sudo_write_paths(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                MockCall::SudoWrite { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect()
    }

    /// Content written to `path` by `sudo_write`, if any.
    pub fn sudo_written_content(&self, path: &str) -> Option<String> {
        self.calls.lock().unwrap().iter().find_map(|c| match c {
            MockCall::SudoWrite { path: p, content } if p == path => Some(content.clone()),
            _ => None,
        })
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

    fn sudo_run(&self, args: &[&str]) -> Result<(), Error> {
        self.calls.lock().unwrap().push(MockCall::SudoRun {
            args: args.iter().map(|s| s.to_string()).collect(),
        });
        if self.sudo_run_fails_when.as_ref().is_some_and(|f| f(args)) {
            return Err(Error::Subprocess(format!("mock failure for {args:?}")));
        }
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
        envs: &[(&str, &str)],
        log_path: &Path,
    ) -> Result<u32, Error> {
        self.calls.lock().unwrap().push(MockCall::SpawnDetached {
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            working_dir: working_dir.to_path_buf(),
            envs: envs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            log_path: log_path.to_path_buf(),
        });
        Ok(MOCK_SPAWNED_PID)
    }

    fn kill_pid(&self, pid: &str) -> Result<(), Error> {
        self.calls.lock().unwrap().push(MockCall::KillPid {
            pid: pid.to_string(),
        });
        Ok(())
    }
}
