use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
#[cfg(any(test, feature = "test-utils"))]
use std::path::PathBuf;
use std::process::Command;
#[cfg(any(test, feature = "test-utils"))]
use std::sync::Mutex;
use std::time::Duration;

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
    ///
    /// Takes the PID as a number so that callers reading one out of a file must
    /// parse it before reaching this point, rather than each call site being
    /// trusted to validate a string.
    fn kill_pid(&self, pid: u32) -> Result<(), Error>;

    /// Send `GET {path}` to `addr` (a `host:port`) and return the HTTP status
    /// code from the response's status line.
    ///
    /// This is the readiness seam. A booting service binds its port long before
    /// it serves anything — Ghost answers its own maintenance page for the whole
    /// of first boot — so a caller asking "is it up yet?" needs the status code,
    /// not the fact that a connection succeeded.
    ///
    /// An `Err` means "no HTTP answer": connection refused, a timeout, or a
    /// reply that is not an HTTP status line. For a caller polling a service
    /// that is still starting, that is a not-ready observation like any other,
    /// not a fatal condition.
    fn http_probe(&self, addr: &str, path: &str) -> Result<u16, Error>;
}

/// Per-probe connect, write and read timeout.
///
/// Generous enough that a busy host is not mistaken for a dead one, short
/// enough that a black-holed connection cannot stall a polling caller past its
/// own budget.
const PROBE_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Parses the status code out of an HTTP status line (`HTTP/1.1 200 OK`).
///
/// Anything that is not a status line means we are not talking to an HTTP
/// server, which for a readiness caller is simply "not ready yet".
fn parse_status_code(status_line: &str) -> Result<u16, Error> {
    let code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::Subprocess(format!("not an HTTP status line: {status_line:?}")))?;
    code.parse()
        .map_err(|_| Error::Subprocess(format!("unparseable HTTP status: {code:?}")))
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

    fn kill_pid(&self, pid: u32) -> Result<(), Error> {
        info!(%pid, "killing process");
        let out = Command::new("kill")
            .args([pid.to_string()])
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

    /// A hand-rolled HTTP/1.1 request over [`std::net::TcpStream`].
    ///
    /// gl-core has no HTTP client dependency, and one probe of one status line
    /// does not justify growing one. Only the status line is read; the
    /// connection is then dropped, which `Connection: close` makes clean.
    fn http_probe(&self, addr: &str, path: &str) -> Result<u16, Error> {
        // `path` is written straight into the request line, so a stray CR or LF
        // would be request splitting. Every caller passes a literal today; this
        // keeps that from being load-bearing.
        if path.chars().any(|c| c.is_control() || c == ' ') {
            return Err(Error::Invalid);
        }

        let socket = addr
            .to_socket_addrs()
            .map_err(Error::Io)?
            .next()
            .ok_or_else(|| Error::Config(format!("no socket address for {addr:?}")))?;

        let stream = TcpStream::connect_timeout(&socket, PROBE_IO_TIMEOUT).map_err(Error::Io)?;
        stream
            .set_read_timeout(Some(PROBE_IO_TIMEOUT))
            .map_err(Error::Io)?;
        stream
            .set_write_timeout(Some(PROBE_IO_TIMEOUT))
            .map_err(Error::Io)?;

        let mut writer = &stream;
        write!(
            writer,
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUser-Agent: goopy-life-readiness\r\nConnection: close\r\n\r\n"
        )
        .map_err(Error::Io)?;
        writer.flush().map_err(Error::Io)?;

        let mut status_line = String::new();
        BufReader::new(&stream)
            .read_line(&mut status_line)
            .map_err(Error::Io)?;

        let code = parse_status_code(status_line.trim_end())?;
        debug!(addr, path, code, "http probe");
        Ok(code)
    }
}

// ── MockSysRunner ─────────────────────────────────────────────────────────────

/// Decides whether a recorded `sudo_run` should fail, from its arguments.
#[cfg(any(test, feature = "test-utils"))]
type SudoRunPredicate = Box<dyn Fn(&[&str]) -> bool + Send + Sync>;

/// What a scripted [`MockSysRunner::http_probe`] answers with.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone, Copy)]
pub enum MockProbe {
    /// The service answered with this HTTP status code.
    Status(u16),
    /// Nothing answered — the connection was refused, as it is before the
    /// service has bound its port.
    Unreachable,
}

/// Records all calls so tests can assert on the exact sequence of commands.
#[cfg(any(test, feature = "test-utils"))]
pub struct MockSysRunner {
    calls: Mutex<Vec<MockCall>>,
    sudo_run_fails_when: Option<SudoRunPredicate>,
    /// Answers for successive `http_probe` calls. The last entry repeats once
    /// the script runs out, so a test states only the transition it cares
    /// about — `[Unreachable, Status(503), Status(200)]` is "ready on the
    /// third probe, and stays ready".
    probe_script: Mutex<Vec<MockProbe>>,
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
        pid: u32,
    },
    HttpProbe {
        addr: String,
        path: String,
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
            // A mock service is ready the moment it is asked, so that tests
            // about provisioning steps are not also tests about waiting.
            probe_script: Mutex::new(vec![MockProbe::Status(200)]),
        }
    }

    /// A mock whose `http_probe` answers `script` in order, repeating the last
    /// entry once it is exhausted.
    pub fn with_probes(script: Vec<MockProbe>) -> Self {
        assert!(
            !script.is_empty(),
            "a probe script needs at least one entry"
        );
        Self {
            probe_script: Mutex::new(script),
            ..Self::new()
        }
    }

    /// The `addr`/`path` pairs passed to `http_probe`, in call order.
    pub fn http_probes(&self) -> Vec<(String, String)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                MockCall::HttpProbe { addr, path } => Some((addr.clone(), path.clone())),
                _ => None,
            })
            .collect()
    }

    /// A mock whose `sudo_run` records the call and then fails whenever `pred`
    /// matches its arguments.
    ///
    /// For exercising tolerance of commands that legitimately fail against
    /// partial state — e.g. `systemctl stop` on a unit that was never installed.
    pub fn failing_sudo_run(pred: impl Fn(&[&str]) -> bool + Send + Sync + 'static) -> Self {
        Self {
            sudo_run_fails_when: Some(Box::new(pred)),
            ..Self::new()
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

    /// Returns all recorded calls in order.
    ///
    /// The escape hatch for assertions the flat projections above cannot
    /// express — chiefly how `sudo_write` and `sudo_run` interleave, which they
    /// deliberately flatten away.
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

    fn kill_pid(&self, pid: u32) -> Result<(), Error> {
        self.calls.lock().unwrap().push(MockCall::KillPid { pid });
        Ok(())
    }

    fn http_probe(&self, addr: &str, path: &str) -> Result<u16, Error> {
        self.calls.lock().unwrap().push(MockCall::HttpProbe {
            addr: addr.to_string(),
            path: path.to_string(),
        });

        let mut script = self.probe_script.lock().unwrap();
        // Keep the final entry in place rather than consuming it: it is the
        // steady state the script settles into.
        let answer = if script.len() > 1 {
            script.remove(0)
        } else {
            script[0]
        };

        match answer {
            MockProbe::Status(code) => Ok(code),
            MockProbe::Unreachable => Err(Error::Subprocess(format!(
                "mock: connection refused for {addr}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    /// Serves `response` once on a loopback port and returns its address.
    ///
    /// A real socket rather than a fake: the point of these tests is that the
    /// hand-rolled request is one a server accepts and that the status line is
    /// read back off the wire.
    fn serve_once(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response.as_bytes());
        });
        addr
    }

    #[test]
    fn http_probe_reports_the_status_code_of_a_serving_instance() {
        let addr = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(RealSysRunner.http_probe(&addr, "/").unwrap(), 200);
    }

    /// The whole reason the probe speaks HTTP: Ghost binds its port and answers
    /// its maintenance page for the entire first boot, so a connect-only check
    /// would call a booting instance ready.
    #[test]
    fn http_probe_reports_a_maintenance_response_rather_than_succeeding() {
        let addr = serve_once("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(RealSysRunner.http_probe(&addr, "/").unwrap(), 503);
    }

    #[test]
    fn http_probe_errors_when_nothing_is_listening() {
        // Bind and drop, so the port is one nothing is listening on.
        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().to_string()
        };
        assert!(
            RealSysRunner.http_probe(&addr, "/").is_err(),
            "a refused connection is not a status code"
        );
    }

    #[test]
    fn http_probe_rejects_a_path_that_could_split_the_request() {
        let addr = serve_once("HTTP/1.1 200 OK\r\n\r\n");
        let err = RealSysRunner
            .http_probe(&addr, "/ HTTP/1.1\r\nX-Injected: yes")
            .expect_err("a path with a newline must never reach the socket");
        assert!(matches!(err, Error::Invalid), "got {err:?}");
    }

    #[test]
    fn parse_status_code_reads_the_code_out_of_a_status_line() {
        assert_eq!(parse_status_code("HTTP/1.1 200 OK").unwrap(), 200);
        assert_eq!(
            parse_status_code("HTTP/1.1 503 Service Unavailable").unwrap(),
            503
        );
    }

    #[test]
    fn parse_status_code_rejects_a_reply_that_is_not_http() {
        assert!(parse_status_code("").is_err());
        assert!(parse_status_code("gibberish").is_err());
        assert!(parse_status_code("HTTP/1.1 nope OK").is_err());
    }

    #[test]
    fn mock_http_probe_walks_its_script_and_then_holds_the_last_answer() {
        let sys = MockSysRunner::with_probes(vec![
            MockProbe::Unreachable,
            MockProbe::Status(503),
            MockProbe::Status(200),
        ]);

        assert!(sys.http_probe("127.0.0.1:9000", "/").is_err());
        assert_eq!(sys.http_probe("127.0.0.1:9000", "/").unwrap(), 503);
        assert_eq!(sys.http_probe("127.0.0.1:9000", "/").unwrap(), 200);
        assert_eq!(
            sys.http_probe("127.0.0.1:9000", "/").unwrap(),
            200,
            "the last entry is the steady state, not a one-off"
        );
        assert_eq!(sys.http_probes().len(), 4);
    }

    #[test]
    fn mock_http_probe_defaults_to_a_ready_instance() {
        assert_eq!(
            MockSysRunner::new()
                .http_probe("127.0.0.1:9000", "/")
                .unwrap(),
            200,
            "tests about provisioning steps should not also be tests about waiting"
        );
    }
}
