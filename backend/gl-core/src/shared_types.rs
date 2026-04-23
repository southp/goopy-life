#[derive(Debug)]
pub enum GlError {
    Failed(String),
    Invalid,
    NotFound,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GlStatus {
    Failed,
    InProgress,
    InDestructing,
    Done,
}

#[derive(Debug)]
pub enum StoreError {
    NotFound,
    AlreadyExists,
    Io(std::io::Error),
    Other(String),
}
