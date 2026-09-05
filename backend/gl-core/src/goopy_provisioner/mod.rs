mod dev_process;
pub mod hello_provisioner;

use crate::Goopy;
use crate::shared_types::*;

pub trait GoopyProvisioner {
    fn provision(&self, goopy: &Goopy) -> Result<(), Error>;
    fn deprovision(&self, goopy: &Goopy) -> Result<(), Error>;
    fn kind(&self) -> ProvisionerKind;
}
