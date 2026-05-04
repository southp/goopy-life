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

    fn port_file_name(&self, slug: &String) -> PathBuf {
        self.base_dir.join(format!("{}_{}", "port", slug))
    }

    fn status_file_name(&self, slug: &String) -> PathBuf {
        self.base_dir.join(format!("{}_{}", "status", slug))
    }
}

impl GoopyStore for SimpleFsStore {
    fn save(&self, gp: &Goopy) -> Result<(), Error> {
        let working_dir = self.base_dir.join(&gp.slug);

        fs::create_dir_all(&working_dir).map_err(Error::Io)?;
        fs::write(self.port_file_name(&gp.slug), gp.port.to_string()).map_err(Error::Io)?;
        fs::write(self.status_file_name(&gp.slug), gp.status.to_string()).map_err(Error::Io)?;

        Ok(())
    }

    fn update_status(&self, slug: &String, new_status: Status) -> Result<(), Error> {
        fs::write(self.status_file_name(slug), new_status.to_string()).map_err(Error::Io)
    }

    fn load(&self, slug: &String) -> Result<Option<Goopy>, Error> {
        let gp_path = self.base_dir.join(slug);

        if Path::new(&gp_path).is_dir() {
            let created_time = fs::metadata(&gp_path)
                .and_then(|m| m.created())
                .map_err(Error::Io)?;

            let port = fs::read_to_string(self.port_file_name(slug))
                .map_err(Error::Io)?
                .trim()
                .parse()
                .map_err(|_| Error::Invalid)?;
            let status = fs::read_to_string(self.status_file_name(slug))
                .map_err(Error::Io)?
                .trim()
                .parse()?;

            return Ok(Some(Goopy::from_stored(
                slug.clone(),
                999,
                created_time.into(),
                &gp_path,
                port,
                status
            )));
        }

        Ok(None)
    }

    fn archive(&self, slug: &String) -> Result<(), Error> {
        let working_dir = self.base_dir.join(slug);
        let new_dir = self.base_dir.join(format!("_{}", slug));
        fs::rename(working_dir, new_dir).map_err(Error::Io)?;
        fs::remove_file(self.port_file_name(slug)).map_err(Error::Io)?;
        fs::remove_file(self.status_file_name(slug)).map_err(Error::Io)
    }

    fn delete(&self, slug: &String) -> Result<(), Error> {
        // these two files might be removed in advanced by `archive`, so it's okay if they are
        // missing here
        fs::remove_file(self.port_file_name(slug)).or_else(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(e)
            }
        }).map_err(Error::Io)?;

        fs::remove_file(self.status_file_name(slug)).or_else(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(e)
            }
        }).map_err(Error::Io)?;

        fs::remove_dir(self.base_dir.join(slug)).map_err(Error::Io)?;
        fs::remove_dir(self.base_dir.join(format!("_{}",slug))).map_err(Error::Io)
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
