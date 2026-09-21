//! Drives the real `gl-serv` binary through `--check-config`.
//!
//! The unit tests in `main.rs` cover `check_config` and `check_host_paths` as
//! functions. What they cannot reach is the wiring between the flag and the
//! process exit status — and that status is the entire contract
//! `deploy/push-binary.sh` depends on: the gate is a `run ssh ... --check-config`
//! whose failure is caught by `set -e` and nothing else.
//!
//! Without this file that wiring is uncovered in both directions. Changing
//! either `std::process::exit(1)` to a `return`, or inverting the
//! `if cli.check_config` condition, leaves every unit test and every
//! `push-binary.test.sh` case green while turning the gate into a no-op that
//! reports success unconditionally — a regression invisible until the next bad
//! config reaches a droplet, which is the one occasion the gate exists for.
//!
//! `push-binary.test.sh` cannot close this either: it stubs `ssh` to fail on
//! `--check-config`, so it asserts what the script does with a failure rather
//! than that the binary produces one.

use std::io::Write;
use std::process::{Command, Stdio};

/// A config gl-serv can run with. Hello, so there are no host paths to satisfy.
const VALID_CONFIG: &str = r#"
base_dir = "/tmp/goopy-check-cli"
domain = "goopy.life"
life_in_days = 7
port_range_start = 9000
port_range_end = 9100
dev_mode = true
cors_origin = "https://goopy.life"
bind_address = "127.0.0.1:8080"
[registry]
path = "/tmp/goopy-check-cli.db"
[allocator]
kind = "PlainDir"
[provisioner]
kind = "Hello"
"#;

fn write_config(toml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    f.write_all(toml.as_bytes()).expect("write config");
    f.flush().expect("flush config");
    f
}

/// Run the built binary with `--check-config` against `path`.
///
/// Spawned and polled rather than `Command::output()`, which would block
/// forever: if the `return` on the Ok arm is ever dropped, the process falls
/// through into `main()` proper and starts serving. A hung CI job is a much
/// worse way to learn that than a failed assertion, so a process still alive at
/// the deadline is killed and reported.
fn run_check(path: &std::path::Path) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gl-serv"))
        .args(["--check-config", "--config"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gl-serv");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match child.try_wait().expect("poll gl-serv") {
            Some(_) => break,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "--check-config did not exit within 30s: it fell through into \
                     the server instead of returning after the check"
                );
            }
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    }

    child.wait_with_output().expect("collect gl-serv output")
}

#[test]
fn check_config_exits_zero_and_prints_the_summary_for_a_valid_config() {
    let f = write_config(VALID_CONFIG);
    let out = run_check(f.path());

    assert!(
        out.status.success(),
        "a valid config must exit 0, got {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("is valid"), "stdout was: {stdout}");
}

#[test]
fn check_config_exits_non_zero_for_a_config_the_binary_cannot_start_on() {
    // A portless bind_address: valid TOML, valid IP, and an address gl-serv
    // can never listen on. The gate has to be what says so, because the next
    // thing to notice is `TcpListener::bind` after the swap.
    let f = write_config(&VALID_CONFIG.replace(
        r#"bind_address = "127.0.0.1:8080""#,
        r#"bind_address = "0.0.0.0""#,
    ));
    let out = run_check(f.path());

    assert!(
        !out.status.success(),
        "a config gl-serv cannot start on must exit non-zero, got {:?}",
        out.status.code(),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("bind_address"), "stderr was: {stderr}");
}

#[test]
fn check_config_exits_non_zero_when_a_host_path_is_missing() {
    // The second exit path out of the same branch, and the one added latest:
    // the config parses cleanly and is rejected only on a host fact.
    let f = write_config(&VALID_CONFIG.replace(
        "[provisioner]\nkind = \"Hello\"",
        "[provisioner]\nkind = \"Ghost\"\nversion = \"6.63.0\"\n\
         source_dir = \"/nonexistent/goopy-life/ghost-6.63.0\"\n\
         node_bin = \"/nonexistent/goopy-life/node\"",
    ));
    let out = run_check(f.path());

    assert!(
        !out.status.success(),
        "a missing source_dir must exit non-zero, got {:?}",
        out.status.code(),
    );
    // The summary still goes out: a host failure is reported next to the
    // values it was judged against.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("is valid"), "stdout was: {stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("source_dir"), "stderr was: {stderr}");
}

#[test]
fn check_config_starts_no_server() {
    // The flag's other half: `--check-config` must take the branch rather than
    // fall through. `run_check` already fails a process that outlives the
    // deadline; this pins the visible consequence -- the address named by the
    // config is still free once the check has exited.
    //
    // The port is borrowed from the OS rather than hardcoded: a fixed one
    // would fail on any machine already using it, which is a flake rather than
    // a finding.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the OS should hand out a free port")
        .local_addr()
        .expect("a bound listener has an address")
        .port();
    let f = write_config(&VALID_CONFIG.replace(
        r#"bind_address = "127.0.0.1:8080""#,
        &format!(r#"bind_address = "127.0.0.1:{port}""#),
    ));

    let out = run_check(f.path());
    assert!(out.status.success(), "a valid config must exit 0");

    std::net::TcpListener::bind(("127.0.0.1", port))
        .expect("--check-config must not have bound the configured address");
}
