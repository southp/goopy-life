//! Drives the real `gl-cli` binary through the mode check (#163).
//!
//! The unit tests pin `check_prod_assertion`; these pin the wiring around it,
//! which a refactor of `main` can silently break: that a refused run exits
//! non-zero *before* the registry is opened, and that a production config
//! without `--prod` really runs in production mode.

use std::path::Path;
use std::process::{Command, Output};

/// A config whose registry and base_dir live inside `dir`, so a test can tell
/// whether the CLI touched them. PlainDir/Hello, so nothing needs root.
fn write_config(dir: &Path, dev_mode: bool) -> std::path::PathBuf {
    let path = dir.join("config.toml");
    let toml = format!(
        r#"
base_dir = "{base_dir}"
domain = "goopy.life"
life_in_hours = 168
port_range_start = 9000
port_range_end = 9100
dev_mode = {dev_mode}
cors_origin = "https://goopy.life"
bind_address = "127.0.0.1:8080"
[registry]
path = "{registry}"
[allocator]
kind = "PlainDir"
[provisioner]
kind = "Hello"
"#,
        base_dir = dir.join("data").display(),
        registry = dir.join("registry.db").display(),
    );
    std::fs::write(&path, toml).expect("write config");
    path
}

fn run(config: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gl-cli"))
        .arg("--config")
        .arg(config)
        .args(extra)
        .output()
        .expect("run gl-cli")
}

#[test]
fn prod_against_a_dev_config_exits_before_opening_the_registry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(dir.path(), true);

    let output = run(&config, &["--prod", "list"]);

    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "must exit non-zero:\n{all}");
    assert!(
        !dir.path().join("registry.db").exists(),
        "a refused run must not create (open) the registry"
    );
    assert!(
        all.contains(&config.display().to_string()),
        "the refusal should name the config path:\n{all}"
    );
}

#[test]
fn a_production_config_without_prod_runs_in_production_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = write_config(dir.path(), false);

    let output = run(&config, &["list"]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "list should succeed:\n{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.trim_start().starts_with("mode:") && l.trim_end().ends_with("production")),
        "the config, not the missing flag, should decide the mode:\n{stdout}"
    );
}
