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

