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

    fn meta_file_name(&self, slug: &String) -> PathBuf {
        self.base_dir.join(format!("{}_meta.toml", slug))
    }
}

impl GoopyStore for SimpleFsStore {
    fn save(&self, gp: &Goopy) -> Result<(), Error> {
        let working_dir = self.base_dir.join(&gp.slug);
        let serialized = toml::to_string(&gp).map_err(|_| Error::Invalid)?;

        fs::create_dir_all(&working_dir).map_err(Error::Io)?;
        fs::write(self.meta_file_name(&gp.slug), serialized).map_err(Error::Io)?;

        Ok(())
    }

    fn update_status(&self, slug: &String, new_status: Status) -> Result<(), Error> {
        let loaded = self.load(slug)?;

        match loaded {
            Some(mut gp) => {
                gp.status = new_status;
                self.save(&gp)
            }
            None => {
                Err(Error::NotFound)
            }
        }
    }

    fn load(&self, slug: &String) -> Result<Option<Goopy>, Error> {
        let gp_path = self.base_dir.join(slug);

        if Path::new(&gp_path).is_dir() {
            let serialized = fs::read_to_string(self.meta_file_name(slug)).map_err(Error::Io)?;
            return toml::from_str(&serialized).map_err(|_| Error::Invalid);
        }

        Ok(None)
    }

    fn archive(&self, slug: &String) -> Result<(), Error> {
        let working_dir = self.base_dir.join(slug);
        let new_dir = self.base_dir.join(format!("{}_archived", slug));
        fs::rename(working_dir, new_dir).map_err(Error::Io)?;

        let meta_file = self.meta_file_name(slug);
        let new_file = meta_file.join("_archived");
        fs::rename(meta_file, new_file).map_err(Error::Io)
    }

    fn delete(&self, slug: &String) -> Result<(), Error> {
        fs::remove_file(self.meta_file_name(slug)).map_err(Error::Io)?;
        fs::remove_dir(self.base_dir.join(slug)).map_err(Error::Io)
    }

    fn list(&self) -> Result<Vec<Goopy>, Error> {
        fs::read_dir(&self.base_dir)
            .map_err(Error::Io)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|dir| {
                let slug = dir.file_name().to_string_lossy().to_string();
                self.load(&slug)?.ok_or(Error::NotFound)
            })
            .collect::<Result<Vec<_>, _>>()
    }
}
