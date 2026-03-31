use chrono::{DateTime, Utc};

pub struct Goopy {
    pub slug: String,
    pub life_in_days: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub enum GlError {
    Failed(String),
    Invalid,
    NotFound,
}

pub trait GoopyRuntime {
    fn spawn(&self, goopy: &Goopy) -> Result<(), GlError>;
    fn despawn(&self, goopy: &Goopy) -> Result<(), GlError>;
    fn get(&self, slug: String) -> Result<(), GlError>;
}
