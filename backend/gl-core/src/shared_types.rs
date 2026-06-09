#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProvisionerKind {
    Hello,
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
    fn from(e: r2d2::Error) -> Self { RegistrySource::Pool(e) }
}

impl From<rusqlite::Error> for RegistrySource {
    fn from(e: rusqlite::Error) -> Self { RegistrySource::Sqlite(e) }
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
    Registry { context: &'static str, source: RegistrySource },
    SchemaMigration(rusqlite::Error),
    PortExhausted,
    Other(String),
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
            Error::PortExhausted => write!(f, "port range exhausted"),
            Error::Other(msg) => write!(f, "{}", msg),
        }
    }
}
