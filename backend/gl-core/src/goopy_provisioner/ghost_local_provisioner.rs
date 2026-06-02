use super::GoopyProvisioner;
use crate::shared_types::*;
use crate::Goopy;

use std::process::Command;

pub struct GhostLocalProvisioner;

impl GhostLocalProvisioner {
    pub fn new() -> Self {
        GhostLocalProvisioner {}
    }
}

impl GoopyProvisioner for GhostLocalProvisioner {
    fn kind(&self) -> ProvisionerKind {
        ProvisionerKind::GhostLocal
    }

    #[tracing::instrument(skip(self))]
    fn provision(&self, goopy: &Goopy) -> Result<(), Error> {
        let result = std::fs::create_dir_all(&goopy.working_dir).and_then(|_| {
            Command::new("ghost")
                .args([
                    "install",
                    "6.28.0",
                    "--pname",
                    &goopy.slug,
                    "--port",
                    &goopy.port.to_string(),
                    "--local",
                ])
                .current_dir(&goopy.working_dir)
                .output()
        });

        match result {
            Ok(cmd) => {
                if cmd.status.success() {
                    return Ok(());
                } else {
                    return Err(Error::Subprocess(format!("stderr: {}", String::from_utf8_lossy(&cmd.stderr))));
                }
            },
            Err(err) => {
                return Err(Error::Io(err));
            }
        }
    }

    #[tracing::instrument(skip(self))]
    fn deprovision(&self, goopy: &Goopy) -> Result<(), Error> {
        let result = Command::new("ghost")
            .args(["uninstall", "-f"])
            .current_dir(&goopy.working_dir)
            .output();

        match result {
            Ok(cmd) => {
                if cmd.status.success() {
                    return Ok(());
                } else {
                    return Err(Error::Subprocess(format!("stderr: {}", String::from_utf8_lossy(&cmd.stderr))));

                }
            }
            Err(err) => {
                return Err(Error::Io(err));
            }
        }
    }
}
