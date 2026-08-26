pub mod hello_provisioner;
mod nginx;

use crate::Goopy;
use crate::shared_types::*;

pub trait GoopyProvisioner {
    fn provision(&self, goopy: &Goopy) -> Result<(), Error>;
    fn deprovision(&self, goopy: &Goopy) -> Result<(), Error>;
    fn kind(&self) -> ProvisionerKind;
}
