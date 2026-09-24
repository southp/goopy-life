//! Which commit this binary was built from.
//!
//! Exists so that "is the thing I merged the thing that is running?" has an
//! answer that does not involve ssh'ing to a droplet and reading file mtimes.
//! `gl-serv` serves these values from `GET /version`, and
//! `deploy/push-binary.sh` compares what it built against what the restarted
//! service reports — turning the post-deploy check from a liveness check into
//! an identity check.
//!
//! Both values are injected at compile time by `build.rs` from environment
//! variables the two deploy paths set. A build made by neither — a local
//! `cargo run` — reports [`UNKNOWN`].

/// What a build that was handed no commit id reports.
///
/// Deliberately a plain word rather than an empty string or a zeroed sha: a
/// caller that cannot tell "no id" from "this id" will eventually display the
/// second when it has the first.
pub const UNKNOWN: &str = "unknown";

/// The full commit id this binary was built from, or [`UNKNOWN`].
///
/// Carries a `-dirty` suffix when the tree it was built from had uncommitted
/// changes (see `deploy/deploy.sh`). That matters more than it looks: a
/// hand-deployed working copy claiming to be a clean commit is precisely the
/// failure this module exists to eliminate, reproduced inside the fix.
pub const GIT_SHA: &str = env!("GL_GIT_SHA");

/// When this binary was built, as an RFC 3339 UTC timestamp, or [`UNKNOWN`].
pub const BUILT_AT: &str = env!("GL_BUILT_AT");

/// How many hex digits an abbreviated commit id keeps — `git rev-parse
/// --short`'s default, so the two agree on what to print.
const SHORT_SHA_LEN: usize = 7;

/// [`GIT_SHA`] abbreviated for display, keeping any `-dirty` suffix.
///
/// [`UNKNOWN`] — and anything else that is not a long hex id — is returned
/// whole, because truncating it would produce a string that reads like a
/// commit.
pub fn short_git_sha() -> String {
    abbreviate(GIT_SHA)
}

/// The `--version` line both binaries print: `0.1.0 (c50c932, built
/// 2026-09-23T12:00:00Z)`.
///
/// Shared so that `gl-serv --version` and `gl-cli --version` on a host are
/// comparable at a glance: the deploy installs both from one build, and the
/// two lines match exactly when that held. `pkg_version` is the caller's own
/// `CARGO_PKG_VERSION`, since this crate's is not the binary's.
pub fn describe(pkg_version: &str) -> String {
    format_version(pkg_version, GIT_SHA, BUILT_AT)
}

/// [`describe`] with the stamp passed in, so both shapes are testable from a
/// build that has only one of them.
fn format_version(pkg_version: &str, sha: &str, built_at: &str) -> String {
    if sha == UNKNOWN {
        return format!("{pkg_version} ({UNKNOWN})");
    }
    if built_at == UNKNOWN {
        return format!("{pkg_version} ({})", abbreviate(sha));
    }
    format!("{pkg_version} ({}, built {built_at})", abbreviate(sha))
}

/// Shorten `sha` to [`SHORT_SHA_LEN`] hex digits, preserving a trailing
/// `-<suffix>` if there is one.
fn abbreviate(sha: &str) -> String {
    let (hex, suffix) = match sha.split_once('-') {
        Some((hex, suffix)) => (hex, Some(suffix)),
        None => (sha, None),
    };

    let is_long_hex = hex.len() > SHORT_SHA_LEN && hex.chars().all(|c| c.is_ascii_hexdigit());
    if !is_long_hex {
        return sha.to_string();
    }

    match suffix {
        Some(suffix) => format!("{}-{}", &hex[..SHORT_SHA_LEN], suffix),
        None => hex[..SHORT_SHA_LEN].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviate_shortens_a_full_commit_id() {
        assert_eq!(
            abbreviate("c50c932ab1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8"),
            "c50c932"
        );
    }

    #[test]
    fn abbreviate_keeps_the_dirty_suffix() {
        assert_eq!(
            abbreviate("c50c932ab1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8-dirty"),
            "c50c932-dirty",
            "a dirty build must still read as dirty once shortened"
        );
    }

    #[test]
    fn abbreviate_returns_unknown_whole() {
        // "unknown" happens to be exactly SHORT_SHA_LEN characters long, so a
        // blind truncation would pass this by accident. The guard is that it is
        // not hex, and this asserts the guard is what is doing the work.
        assert_eq!(abbreviate(UNKNOWN), UNKNOWN);
        assert_eq!(abbreviate("unknown-ish"), "unknown-ish");
    }

    #[test]
    fn format_version_names_the_commit_and_the_build_time() {
        assert_eq!(
            format_version(
                "0.1.0",
                "c50c932ab1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8-dirty",
                "2026-09-23T12:00:00Z"
            ),
            "0.1.0 (c50c932-dirty, built 2026-09-23T12:00:00Z)"
        );
    }

    #[test]
    fn format_version_says_unknown_for_an_unstamped_build() {
        assert_eq!(format_version("0.1.0", UNKNOWN, UNKNOWN), "0.1.0 (unknown)");
    }

    #[test]
    fn format_version_omits_a_missing_build_time() {
        // A hand build that set only GL_GIT_SHA: say what is known, and do not
        // print "built unknown" as if it were a time.
        assert_eq!(
            format_version("0.1.0", "c50c932ab1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8", UNKNOWN),
            "0.1.0 (c50c932)"
        );
    }

    #[test]
    fn abbreviate_leaves_an_already_short_id_alone() {
        assert_eq!(abbreviate("c50c932"), "c50c932");
        assert_eq!(abbreviate("c50c9"), "c50c9");
    }
}
