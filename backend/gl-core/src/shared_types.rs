use crate::goopy::Goopy;

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

pub trait GoopyStore {
    fn save(&self, gp: &Goopy) -> Result<(), StoreError>;
    fn load(&self, slug: &String) -> Result<Option<Goopy>, StoreError>;
    fn delete(&self, slug: &String) -> Result<(), StoreError>;
    fn list(&self) -> Result<Vec<Goopy>, StoreError>;
}
