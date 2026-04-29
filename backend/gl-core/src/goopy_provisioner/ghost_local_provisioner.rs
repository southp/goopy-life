use super::GoopyProvisioner;
use crate::shared_types::*;
use std::path::Path;

pub struct GhostLocalProvisioner;

impl GhostLocalProvisioner {
    pub fn new() -> Self {
        GhostLocalProvisioner {}
    }
}

impl GoopyProvisioner for GhostLocalProvisioner {
    fn provision(slug: &String, working_dir: &Path, port: u32) -> Result<(), Error> {
        Ok(())
    }

    fn deprovision(slug: &String, working_dir: &Path) -> Result<(), Error> {
        Ok(())
    }
}
