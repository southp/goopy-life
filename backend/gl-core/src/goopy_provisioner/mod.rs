pub mod hello_provisioner;

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
