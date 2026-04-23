use crate::goopy::Goopy;
use crate::shared_types::*;

pub trait GoopyStore {
    fn save(&self, gp: &Goopy) -> Result<(), StoreError>;
    fn load(&self, slug: &String) -> Result<Option<Goopy>, StoreError>;
    fn delete(&self, slug: &String) -> Result<(), StoreError>;
    fn list(&self) -> Result<Vec<Goopy>, StoreError>;
}
