#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Empty,
    Failed,
    Spawning,
    Despawning,
    Done,
}

#[derive(Debug)]
pub enum Error {
    NotFound,
    Invalid,
    AlreadyExists,
    Io(std::io::Error),
    Other(String),
}
