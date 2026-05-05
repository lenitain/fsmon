//! Systemd integration (deprecated).
//!
//! fsmon now uses a Podman-style architecture: users run `sudo fsmon daemon &`
//! directly instead of installing a systemd service.
//! This module exists only for backward compatibility and to guide users.

use anyhow::{Result, bail};

pub fn install(_force: bool) -> Result<()> {
    bail!(
        "fsmon no longer uses systemd services.\n\
         Run the daemon directly:  sudo fsmon daemon &\n\
         To stop:                  kill %1  (or Ctrl+C)"
    );
}

pub fn uninstall() -> Result<()> {
    bail!(
        "fsmon no longer uses systemd services.\n\
         There is no systemd service to uninstall.\n\
         If you installed one previously, remove it with:\n\
           sudo rm /etc/systemd/system/fsmon.service\n\
           sudo systemctl daemon-reload"
    );
}
