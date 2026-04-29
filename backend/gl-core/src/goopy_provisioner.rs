pub mod ghost_local_provisioner;

use std::path::Path;
use crate::shared_types::*;

pub trait GoopyProvisioner {
    fn provision(slug: &String, working_dir: &Path, port: u32) -> Result<(), Error>;
    fn deprovision(slug: &String, working_dir: &Path) -> Result<(), Error>;
}
