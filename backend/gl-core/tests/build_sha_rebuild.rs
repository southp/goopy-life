//! Proves that changing `GL_GIT_SHA` alone produces a different binary.
//!
//! This is the one assumption the whole build-stamp feature rests on, and it is
//! an assumption about *cargo*, not about gl-core: cargo's fingerprint ignores
//! environment variables unless a build script declares an interest in them, so
//! the failure mode of getting this wrong is a rebuild that is silently a cache
//! hit and a `GET /version` that reports the previous commit with total
//! confidence. That is worse than having no endpoint — so it is checked rather
//! than documented.
//!
//! gl-core's real `build.rs` is copied into a throwaway crate and driven twice.
//! Building gl-core itself twice would take minutes (bundled SQLite); the crate
//! under test here has no dependencies and compiles in about a second, while
//! still exercising the actual file rather than a restatement of it.

use std::path::Path;
use std::process::Command;

/// Build the scratch crate with `sha` in the environment and return what its
/// binary prints.
fn build_and_run(crate_dir: &Path, target_dir: &Path, sha: &str) -> String {
    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(crate_dir.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", target_dir)
        .env("GL_GIT_SHA", sha)
        .env("GL_BUILT_AT", "2026-09-23T00:00:00Z")
        .status()
        .expect("cargo build should be runnable from a test");
    assert!(status.success(), "scratch crate failed to build with {sha}");

    let binary = target_dir.join("debug").join("sha-stamp");
    let output = Command::new(&binary)
        .output()
        .expect("the scratch binary should be runnable");
    assert!(
        output.status.success(),
        "the scratch binary exited non-zero"
    );
    String::from_utf8(output.stdout)
        .expect("the scratch binary prints utf-8")
        .trim()
        .to_string()
}

/// Lay out a minimal crate that stamps itself with gl-core's own `build.rs`.
fn scaffold(root: &Path) -> std::path::PathBuf {
    let crate_dir = root.join("sha-stamp");
    std::fs::create_dir_all(crate_dir.join("src")).unwrap();

    // `[workspace]` keeps the scratch crate from being adopted by any workspace
    // above the temporary directory.
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]\n\
         name = \"sha-stamp\"\n\
         version = \"0.0.0\"\n\
         edition = \"2024\"\n\
         \n\
         [workspace]\n",
    )
    .unwrap();

    std::fs::write(
        crate_dir.join("src/main.rs"),
        "fn main() {\n\
         \x20   println!(\"{}\", env!(\"GL_GIT_SHA\"));\n\
         }\n",
    )
    .unwrap();

    // The real thing, not a copy kept in step by hand: if build.rs stops
    // declaring `rerun-if-env-changed` this test starts failing.
    let real_build_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs");
    std::fs::copy(&real_build_rs, crate_dir.join("build.rs")).unwrap();

    crate_dir
}

#[test]
fn a_changed_sha_alone_rebuilds_the_binary() {
    let temp = tempfile::tempdir().unwrap();
    let crate_dir = scaffold(temp.path());
    let target_dir = temp.path().join("target");

    let first = build_and_run(
        &crate_dir,
        &target_dir,
        "1111111aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    assert_eq!(first, "1111111aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    // Nothing but the environment variable differs between the two builds: same
    // sources, same target directory, same cargo. Without
    // `cargo::rerun-if-env-changed=GL_GIT_SHA` in build.rs this second build is
    // a cache hit and still prints the first sha.
    let second = build_and_run(
        &crate_dir,
        &target_dir,
        "2222222bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );
    assert_eq!(
        second, "2222222bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "a rebuild that changed only GL_GIT_SHA must not be a cache hit",
    );
}

#[test]
fn an_unset_sha_stamps_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let crate_dir = scaffold(temp.path());
    let target_dir = temp.path().join("target");

    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(crate_dir.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target_dir)
        .env_remove("GL_GIT_SHA")
        .env_remove("GL_BUILT_AT")
        .status()
        .expect("cargo build should be runnable from a test");
    assert!(status.success());

    let output = Command::new(target_dir.join("debug").join("sha-stamp"))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        gl_core::build_info::UNKNOWN,
        "a build made by neither deploy path must say so rather than guess",
    );
}
