//! Daemon process manager for fsmon integration tests.
//!
//! Spawns `fsmon daemon` as a subprocess, manages its lifecycle,
//! and provides cleanup on drop.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

/// Managed fsmon daemon child process.
pub struct FsmonDaemon {
    child: Child,
    #[allow(dead_code)]
    home_dir: PathBuf,
}

impl FsmonDaemon {
    /// Check whether we can run the daemon (requires root for fanotify).
    /// Non-root environments skip daemon integration tests.
    pub fn can_run() -> bool {
        nix::unistd::geteuid().is_root()
    }

    /// Spawn `fsmon daemon` in an isolated home directory.
    ///
    /// The daemon writes logs to `{home_dir}/.local/state/fsmon/` and
    /// reads config from `{home_dir}/.config/fsmon/`.
    pub fn spawn(home_dir: &Path) -> Self {
        let exe = super::fsmon_client::fsmon_binary();

        let state_dir = home_dir.join(".local/state/fsmon");
        std::fs::create_dir_all(&state_dir).expect("create state dir");

        let config_dir = home_dir.join(".config/fsmon");
        std::fs::create_dir_all(&config_dir).expect("create config dir");

        let child = Command::new(&exe)
            .arg("daemon")
            .env("HOME", home_dir)
            .env("XDG_CONFIG_HOME", config_dir)
            .env("XDG_STATE_HOME", home_dir.join(".local/state"))
            .spawn()
            .expect("failed to spawn fsmon daemon");

        // Give the daemon time to initialize (fanotify, socket binding, etc.)
        std::thread::sleep(Duration::from_millis(1000));

        Self {
            child,
            home_dir: home_dir.to_path_buf(),
        }
    }

    /// PID of the spawned daemon process.
    #[allow(dead_code)]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Send SIGTERM and wait for graceful shutdown.
    #[cfg(unix)]
    pub fn terminate(mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        }
        // Wait up to 5 seconds for graceful shutdown
        for _ in 0..50 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => return,
            }
        }
        // Force kill if still running
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    #[cfg(not(unix))]
    pub fn terminate(self) {
        drop(self);
    }
}
