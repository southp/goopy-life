use super::GoopyStore;
use crate::goopy::Goopy;
use crate::shared_types::*;

use std::path::{Path, PathBuf};
use std::fs;

pub struct SimpleFsStore {
    pub base_dir: PathBuf,
}

impl SimpleFsStore {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().into(),
        }
    }
}

impl GoopyStore for SimpleFsStore {
    fn save(&self, gp: &Goopy) -> Result<(), Error> {
        match fs::create_dir_all(self.base_dir.join(gp.slug.clone())) {
            Ok(_) => Ok(()),
            Err(e) => Err(Error::Io(e))
        }
    }

    fn update_status(&self, _slug: &String, _new_status: Status) -> Result<(), Error> {
        Ok(())
    }

    fn load(&self, slug: &String) -> Result<Option<Goopy>, Error> {
        let gp_path = self.base_dir.join(slug);

        if Path::new(&gp_path).is_dir() {
            let created_time = fs::metadata(gp_path)
                .and_then(|m| m.created())
                .map_err(Error::Io)?;

            return Ok(Some(Goopy::from_stored(
                slug.clone(),
                999,
                created_time.into(),
                Status::Done
            )));
        }

        Ok(None)
    }

    fn delete(&self, slug: &String) -> Result<(), Error> {
        match fs::remove_dir(self.base_dir.join(slug)) {
            Ok(_) => Ok(()),
            Err(e) => Err(Error::Io(e))
        }
    }

    fn list(&self) -> Result<Vec<Goopy>, Error> {
        fs::read_dir(&self.base_dir)
            .map_err(Error::Io)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|dir| {
                let slug = dir.file_name().to_string_lossy().to_string();
                let created = dir.metadata()
                    .and_then(|m| m.created())
                    .map_err(Error::Io)?;

                Ok(Goopy::from_stored(slug, 999, created.into(), Status::Done))
            })
            .collect::<Result<Vec<_>, _>>()
    }
}
