use crate::shared_types::*;

use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct Goopy {
    pub slug: String,
    pub life_in_days: i32,
    pub created_at: DateTime<Utc>,
}

impl Goopy {
    pub fn new(
        slug: String,
        life_in_days: i32,
        created_at: DateTime<Utc>,
    ) -> Result<Self, GlError> {
        if life_in_days <= 0 {
            return Err(GlError::Invalid);
        }

        if slug.is_empty() {
            return Err(GlError::Invalid);
        }

        Ok(Self {
            slug,
            life_in_days,
            created_at,
        })
    }
}
