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
    /// Write `content` to a privileged `path` via `sudo tee`.
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

}
