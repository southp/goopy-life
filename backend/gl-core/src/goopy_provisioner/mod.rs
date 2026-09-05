pub mod hello_provisioner;

use crate::Goopy;
use crate::shared_types::*;

pub trait GoopyProvisioner {
    fn provision(&self, goopy: &Goopy) -> Result<(), Error>;
    fn deprovision(&self, goopy: &Goopy) -> Result<(), Error>;
    fn kind(&self) -> ProvisionerKind;
}

/// Forwarding impl so a provisioner chosen at runtime (`Config::build_provisioner`)
/// still satisfies `GoopyManager`'s generic `Provisioner` bound.
impl GoopyProvisioner for Box<dyn GoopyProvisioner + Send + Sync> {
    fn provision(&self, goopy: &Goopy) -> Result<(), Error> {
        (**self).provision(goopy)
    }

    fn deprovision(&self, goopy: &Goopy) -> Result<(), Error> {
        (**self).deprovision(goopy)
    }

    fn kind(&self) -> ProvisionerKind {
        (**self).kind()
    }
}
