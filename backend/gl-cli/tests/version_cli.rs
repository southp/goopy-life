//! Drives the real `gl-cli` binary through `--version`.
//!
//! `gl-serv --version` and `gl-cli --version` are how an operator on a host
//! tells whether the two came from the same build, so both must print
//! `build_info::describe` and not just the crate version, which has never
//! been bumped and so tells two builds apart not at all. Asserted against the
//! binary rather than the function, because the wiring from the flag to the
//! string is the part a refactor of `main` can silently drop.

use std::process::Command;

#[test]
fn version_names_the_commit_it_was_built_from() {
    let output = Command::new(env!("CARGO_BIN_EXE_gl-cli"))
        .arg("--version")
        .output()
        .expect("run gl-cli --version");

    assert!(output.status.success(), "--version must exit 0");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "gl-cli {}\n",
            gl_core::build_info::describe(env!("CARGO_PKG_VERSION"))
        ),
    );
}
