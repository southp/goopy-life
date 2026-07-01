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
    Hello,
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
        }
    }
}

impl std::str::FromStr for ProvisionerKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Hello" => Ok(ProvisionerKind::Hello),
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
        let kind = ProvisionerKind::Hello;
        let s = kind.to_string();
        let parsed = ProvisionerKind::from_str(&s).expect("should round-trip");
        assert_eq!(parsed, kind, "round-trip failed for {s}");
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
    /// A capacity limit was hit; `reason` names which cap was exceeded
    /// (e.g. `"max_provisioned"` or `"max_active"`).
    CapacityFull {
        reason: &'static str,
    },
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
            Error::CapacityFull { reason } => write!(f, "capacity full: {reason}"),
        }
    }
}
