#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AllocatorKind {
    PlainDir,
    Zfs,
}

impl std::fmt::Display for AllocatorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AllocatorKind::PlainDir => write!(f, "PlainDir"),
            AllocatorKind::Zfs => write!(f, "Zfs"),
        }
    }
}

impl std::str::FromStr for AllocatorKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "PlainDir" => Ok(AllocatorKind::PlainDir),
            "Zfs" => Ok(AllocatorKind::Zfs),
            _ => Err(Error::Invalid),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProvisionerKind {
    /// Minimal PoC provisioner serving a static "Hello, I am {slug}" page.
    Hello,
    /// Production provisioner: a real Ghost instance soft-linked to a shared base install.
    Ghost,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Status {
    Empty,
    Failed,
    Archived,
    Spawning,
    Despawning,
    Done,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Empty => write!(f, "Empty"),
            Status::Failed => write!(f, "Failed"),
            Status::Archived => write!(f, "Archived"),
            Status::Spawning => write!(f, "Spawning"),
            Status::Despawning => write!(f, "Despawning"),
            Status::Done => write!(f, "Done"),
        }
    }
}

impl std::str::FromStr for Status {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Empty" => Ok(Status::Empty),
            "Failed" => Ok(Status::Failed),
            "Archived" => Ok(Status::Archived),
            "Spawning" => Ok(Status::Spawning),
            "Despawning" => Ok(Status::Despawning),
            "Done" => Ok(Status::Done),
            _ => Err(Error::Invalid),
        }
    }
}

impl std::fmt::Display for ProvisionerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProvisionerKind::Hello => write!(f, "Hello"),
            ProvisionerKind::Ghost => write!(f, "Ghost"),
        }
    }
}

impl std::str::FromStr for ProvisionerKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Hello" => Ok(ProvisionerKind::Hello),
            "Ghost" => Ok(ProvisionerKind::Ghost),
            _ => Err(Error::Invalid),
        }
    }
}

/// Concrete source for [`Error::Registry`] failures.
/// Covers connection-pool timeouts, SQL errors, and configuration conditions
/// detected during registry initialisation.
#[derive(Debug)]
pub enum RegistrySource {
    Pool(r2d2::Error),
    Sqlite(rusqlite::Error),
    /// SQLite WAL mode could not be enabled; contains the mode string that was returned.
    WalModeUnavailable(String),
}

impl std::fmt::Display for RegistrySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistrySource::Pool(e) => write!(f, "{e}"),
            RegistrySource::Sqlite(e) => write!(f, "{e}"),
            RegistrySource::WalModeUnavailable(got) => {
                write!(f, "WAL mode not available (got: {got})")
            }
        }
    }
}

impl std::error::Error for RegistrySource {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RegistrySource::Pool(e) => Some(e),
            RegistrySource::Sqlite(e) => Some(e),
            RegistrySource::WalModeUnavailable(_) => None,
        }
    }
}

impl From<r2d2::Error> for RegistrySource {
    fn from(e: r2d2::Error) -> Self {
        RegistrySource::Pool(e)
    }
}

impl From<rusqlite::Error> for RegistrySource {
    fn from(e: rusqlite::Error) -> Self {
        RegistrySource::Sqlite(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn status_display_round_trip() {
        for status in [
            Status::Empty,
            Status::Failed,
            Status::Archived,
            Status::Spawning,
            Status::Despawning,
            Status::Done,
        ] {
            let s = status.to_string();
            let parsed = Status::from_str(&s).expect("should round-trip");
            assert_eq!(parsed, status, "round-trip failed for {s}");
        }
    }

    #[test]
    fn status_from_str_unknown_returns_invalid() {
        assert!(matches!(Status::from_str("Unknown"), Err(Error::Invalid)));
    }

    #[test]
    fn provisioner_kind_display_round_trip() {
        for kind in [ProvisionerKind::Hello, ProvisionerKind::Ghost] {
            let s = kind.to_string();
            let parsed = ProvisionerKind::from_str(&s).expect("should round-trip");
            assert_eq!(parsed, kind, "round-trip failed for {s}");
        }
    }

    #[test]
    fn provisioner_kind_from_str_unknown_returns_invalid() {
        assert!(matches!(
            ProvisionerKind::from_str("NonExistent"),
            Err(Error::Invalid)
        ));
    }

    #[test]
    fn allocator_kind_display_round_trip() {
        for kind in [AllocatorKind::PlainDir, AllocatorKind::Zfs] {
            let s = kind.to_string();
            let parsed = AllocatorKind::from_str(&s).expect("should round-trip");
            assert_eq!(parsed, kind, "round-trip failed for {s}");
        }
    }

    #[test]
    fn allocator_kind_from_str_unknown_returns_invalid() {
        assert!(matches!(
            AllocatorKind::from_str("NonExistent"),
            Err(Error::Invalid)
        ));
    }

    /// The expected code for every `Error` variant, written out a second time.
    ///
    /// Deliberately a duplicate of [`Error::code`] rather than a call to it:
    /// this match is exhaustive, so adding a variant fails to compile here
    /// until its code is written down in a test, which is what makes the
    /// mapping a decision rather than a default.
    fn expected_code(e: &Error) -> &'static str {
        match e {
            Error::NotFound => "not_found",
            Error::Invalid => "invalid",
            Error::AlreadyExists => "already_exists",
            Error::Config(_) => "config",
            Error::Io(_) => "io",
            Error::Registry { .. } => "registry",
            Error::SchemaMigration(_) => "schema_migration",
            Error::SchemaVersionTooNew { .. } => "schema_version_too_new",
            Error::PortExhausted => "port_exhausted",
            Error::Subprocess(_) => "subprocess",
            Error::SlugExhausted => "slug_exhausted",
            Error::RowParse { .. } => "row_parse",
            Error::CapacityFull { .. } => "capacity_full",
            Error::ReadinessTimeout { .. } => "readiness_timeout",
        }
    }

    /// One instance of each variant, for the tests that range over them.
    ///
    /// Not exhaustive by construction: a new variant is forced into
    /// `expected_code` by the compiler, but nothing forces it in here. Add it
    /// here too, or `error_codes_are_unique_per_variant` never sees it.
    fn one_of_every_error() -> Vec<Error> {
        vec![
            Error::NotFound,
            Error::Invalid,
            Error::AlreadyExists,
            Error::Config("bad".into()),
            Error::Io(std::io::Error::other("boom")),
            Error::Registry {
                context: "save",
                source: RegistrySource::WalModeUnavailable("delete".into()),
            },
            Error::SchemaMigration(rusqlite::Error::QueryReturnedNoRows),
            Error::SchemaVersionTooNew {
                found: 9,
                supported: 2,
            },
            Error::PortExhausted,
            Error::Subprocess("zfs: permission denied".into()),
            Error::SlugExhausted,
            Error::RowParse {
                slug: "a-b-c".into(),
                field: "status",
                value: "Bogus".into(),
            },
            Error::CapacityFull {
                kind: CapacityKind::Provisioned,
            },
            Error::ReadinessTimeout {
                slug: "a-b-c".into(),
                waited_secs: 120,
                last: "connection refused".into(),
            },
        ]
    }

    /// `Error::code` is published on every instance event, so it is a
    /// contract: pin every variant's string (#118).
    #[test]
    fn error_codes_are_stable() {
        for e in one_of_every_error() {
            assert_eq!(e.code(), expected_code(&e), "code changed for {e:?}");
        }
    }

    /// Two variants sharing a code would silently merge two distinct failures
    /// in the event log, which is the one thing the log exists to tell apart.
    #[test]
    fn error_codes_are_unique_per_variant() {
        let errors = one_of_every_error();
        let mut codes: Vec<&str> = errors.iter().map(|e| e.code()).collect();
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "duplicate error codes: {codes:?}");
    }

    /// The codes travel through JSON and log fields, so keep them to the
    /// lowercase-and-underscore shape gl-serv already uses (`server_full`).
    #[test]
    fn error_codes_are_snake_case() {
        for e in one_of_every_error() {
            let code = e.code();
            assert!(!code.is_empty(), "empty code for {e:?}");
            assert!(
                code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "code {code:?} is not snake_case",
            );
        }
    }

    #[test]
    fn row_parse_error_display() {
        let e = Error::RowParse {
            slug: "a-b-c".into(),
            field: "status",
            value: "Bogus".into(),
        };
        let s = e.to_string();
        assert!(s.contains("a-b-c"), "{s}");
        assert!(s.contains("status"), "{s}");
        assert!(s.contains("Bogus"), "{s}");
    }
}

#[derive(Debug)]
pub enum Error {
    NotFound,
    Invalid,
    AlreadyExists,
    Config(String),
    Io(std::io::Error),
    /// General database operation failure.
    /// `context` describes what was being attempted (e.g. `"save"`, `"pool get"`).
    Registry {
        context: &'static str,
        source: RegistrySource,
    },
    SchemaMigration(rusqlite::Error),
    /// The database's `PRAGMA user_version` is newer than the schema this build
    /// understands — e.g. after a binary downgrade.  Refusing to proceed is
    /// safer than operating on a schema we cannot interpret.
    SchemaVersionTooNew {
        found: u32,
        supported: u32,
    },
    PortExhausted,
    /// A subprocess (zfs, ghost, systemctl, etc.) ran but returned a non-zero
    /// exit status.  The string contains the command name and stderr.
    Subprocess(String),
    /// Slug generation failed after exhausting all retry attempts.
    SlugExhausted,
    /// A row read from the database could not be parsed back into a `Goopy`.
    /// Carries the slug being loaded, the field that failed, and its raw value.
    RowParse {
        slug: String,
        field: &'static str,
        value: String,
    },
    /// A capacity limit was hit; `kind` names which cap was exceeded.
    CapacityFull {
        kind: CapacityKind,
    },
    /// A freshly provisioned instance never served HTTP within its readiness
    /// budget, so it was never handed to a visitor.
    ///
    /// `last` records the final probe observation — a status code, or why
    /// nothing answered — because that is the one detail worth keeping about a
    /// boot that never finished (#118).
    ReadinessTimeout {
        slug: String,
        waited_secs: u64,
        last: String,
    },
}

/// Which of the two instance caps a spawn ran into.
///
/// Modelled as an enum rather than a string so every consumer that branches on
/// it — notably gl-serv, which maps each variant onto a distinct public error
/// code — is checked exhaustively by the compiler. A renamed config field then
/// breaks the build instead of silently changing the API response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityKind {
    /// The disk-bound `max_provisioned` cap: total rows in the registry.
    Provisioned,
    /// The RAM-bound `max_active` cap: resident instances.
    Active,
}

impl std::fmt::Display for CapacityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapacityKind::Provisioned => write!(f, "max_provisioned"),
            CapacityKind::Active => write!(f, "max_active"),
        }
    }
}

impl Error {
    /// A stable, coarse discriminant for this error.
    ///
    /// Distinct from [`Display`], which renders whatever the failure happened
    /// to carry — a command's stderr, a corrupt row's raw value, a path. That
    /// rendering is useful to an operator and unsafe to publish: `Subprocess`
    /// alone can carry the output of any command a provisioner ran.
    ///
    /// This is the half that *is* safe to publish, and it is recorded on every
    /// [`InstanceEvent`] alongside the full rendering (#118). Once an event log
    /// or an API response carries one of these strings it is a contract, so the
    /// mapping is pinned by `error_codes_are_stable` — change a string only
    /// with the same care as changing a public JSON field name.
    ///
    /// Deliberately one code per variant. `CapacityFull` does not split by
    /// [`CapacityKind`]: gl-serv already has its own public split
    /// (`server_full` / `server_busy`) for that, and which cap was hit is in
    /// the rendered detail.
    ///
    /// [`Display`]: std::fmt::Display
    /// [`InstanceEvent`]: crate::instance_event::InstanceEvent
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound => "not_found",
            Error::Invalid => "invalid",
            Error::AlreadyExists => "already_exists",
            Error::Config(_) => "config",
            Error::Io(_) => "io",
            Error::Registry { .. } => "registry",
            Error::SchemaMigration(_) => "schema_migration",
            Error::SchemaVersionTooNew { .. } => "schema_version_too_new",
            Error::PortExhausted => "port_exhausted",
            Error::Subprocess(_) => "subprocess",
            Error::SlugExhausted => "slug_exhausted",
            Error::RowParse { .. } => "row_parse",
            Error::CapacityFull { .. } => "capacity_full",
            Error::ReadinessTimeout { .. } => "readiness_timeout",
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Registry { source, .. } => Some(source),
            Error::SchemaMigration(e) => Some(e),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::Invalid => write!(f, "invalid"),
            Error::AlreadyExists => write!(f, "already exists"),
            Error::Config(msg) => write!(f, "config error: {}", msg),
            Error::Io(e) => write!(f, "io error: {}", e),
            Error::Registry { context, source } => write!(f, "registry error: {context}: {source}"),
            Error::SchemaMigration(e) => write!(f, "schema migration error: {}", e),
            Error::SchemaVersionTooNew { found, supported } => write!(
                f,
                "database schema version {found} is newer than the highest version \
                 this build supports ({supported}); upgrade the binary or restore \
                 a compatible database"
            ),
            Error::PortExhausted => write!(f, "port range exhausted"),
            Error::Subprocess(msg) => write!(f, "subprocess error: {}", msg),
            Error::SlugExhausted => write!(f, "slug generation exhausted"),
            Error::RowParse { slug, field, value } => {
                write!(
                    f,
                    "corrupt row (slug={slug}, field={field}, value={value:?})"
                )
            }
            Error::CapacityFull { kind } => write!(f, "capacity full: {kind}"),
            Error::ReadinessTimeout {
                slug,
                waited_secs,
                last,
            } => write!(
                f,
                "{slug} did not serve HTTP within {waited_secs}s (last probe: {last})"
            ),
        }
    }
}
