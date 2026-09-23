//! Stamps the build with the commit it was made from.
//!
//! Both values are handed in by the environment rather than discovered here:
//! the deploy cross-compiles from a checkout that the *deploy script* knows the
//! state of, and shelling out to `git` from a build script would report the
//! state of whatever tree cargo happened to run in — which, for a vendored or
//! packaged build, is no tree at all. See `deploy/deploy.sh` and
//! `.github/workflows/backend-deploy.yml` for the two callers that set them.
//!
//! A build made by neither path reports `unknown`, which is the honest answer
//! and is deliberately not dressed up as anything else.

/// What a build that was handed no commit id reports.
///
/// Kept in step with `gl_core::build_info::UNKNOWN`; a build script cannot
/// share a constant with the crate it builds.
const UNKNOWN: &str = "unknown";

fn main() {
    // Cargo's fingerprint does not include environment variables unless a build
    // script asks it to. Without these, a rebuild in a tree where nothing but
    // GL_GIT_SHA changed is a cache hit, and the binary keeps the previous
    // commit's id — an endpoint reporting a stale commit with total confidence,
    // which is strictly worse than having no endpoint at all.
    println!("cargo::rerun-if-env-changed=GL_GIT_SHA");
    println!("cargo::rerun-if-env-changed=GL_BUILT_AT");
    // Emitting any `rerun-if-*` replaces cargo's default rule ("rerun when any
    // file in the package changes"), so this script has to name itself.
    println!("cargo::rerun-if-changed=build.rs");

    // Passed through `rustc-env` rather than read straight from the source with
    // `option_env!`: `rerun-if-env-changed` only guarantees that *this script*
    // runs again, and cargo still skips recompiling the crate when the script's
    // output is unchanged. Routing the value through the output is what ties
    // the crate's fingerprint to it.
    println!("cargo::rustc-env=GL_GIT_SHA={}", stamp("GL_GIT_SHA"));
    println!("cargo::rustc-env=GL_BUILT_AT={}", stamp("GL_BUILT_AT"));
}

/// Read `var` as a single-line stamp, falling back to [`UNKNOWN`].
///
/// A cargo directive is line-oriented, so a value carrying a newline would be
/// read as a second (bogus) directive. Only the first line is taken.
fn stamp(var: &str) -> String {
    let raw = match std::env::var(var) {
        Ok(raw) => raw,
        Err(_) => return UNKNOWN.to_string(),
    };
    let first_line = raw.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return UNKNOWN.to_string();
    }
    first_line.to_string()
}
