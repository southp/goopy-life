mod dev_process;
pub mod ghost_provisioner;
pub mod hello_provisioner;
mod nginx;
mod systemd;

use crate::Goopy;
use crate::shared_types::*;

pub trait GoopyProvisioner {
    fn provision(&self, goopy: &Goopy) -> Result<(), Error>;
    fn deprovision(&self, goopy: &Goopy) -> Result<(), Error>;
    fn kind(&self) -> ProvisionerKind;

    /// Version of the service this provisioner installs, recorded on every
    /// instance it creates as `Goopy::service_version`.
    ///
    /// Instances are pinned to the version they were provisioned with: changing
    /// the configured version affects only instances created afterwards.
    fn service_version(&self) -> &str;
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
