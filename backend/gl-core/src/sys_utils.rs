use std::io::Write;
use std::process::Command;
#[cfg(any(test, feature = "test-utils"))]
use std::sync::Mutex;

use tracing::{error, info};

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
    Run { program: String, args: Vec<String> },
    SudoRun { args: Vec<String> },
    SudoWrite { path: String, content: String },
}

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
}
