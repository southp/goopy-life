pub mod simple_fs_store;

use crate::goopy::Goopy;
use crate::shared_types::*;

pub trait GoopyStore {
    fn save(&self, gp: &Goopy) -> Result<(), Error>;
    fn load(&self, slug: &String) -> Result<Option<Goopy>, Error>;
    fn delete(&self, slug: &String) -> Result<(), Error>;
    fn list(&self) -> Result<Vec<Goopy>, Error>;
    fn update_status(&self, slug: &String, new_status: Status) -> Result<(), Error>;
}
