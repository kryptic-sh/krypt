//! `pacman` / `paru` package manager implementation (Arch Linux).
//!
//! Prefers `paru` if available (adds AUR support), falls back to `pacman`.
//! Both use the same install syntax. The manager name is always `"pacman"` to
//! match the `DepsGroup.pacman` config field.

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// Package manager implementation for Arch Linux (pacman / paru).
pub struct Pacman;

impl Pacman {
    /// Binary to use for installation: `paru` if available, else `pacman`.
    fn binary(&self) -> &'static str {
        if which::which("paru").is_ok() {
            "paru"
        } else {
            "pacman"
        }
    }
}

impl PackageManager for Pacman {
    fn name(&self) -> &'static str {
        "pacman"
    }

    fn is_available(&self) -> bool {
        which::which("pacman").is_ok()
    }

    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } = runner.run("pacman", &["-Q", pkg])?;
        match status {
            0 => Ok(true),
            _ => Ok(false),
        }
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let bin = self.binary();
        let mut args = vec![bin, "-S", "--noconfirm"];
        let pkg_refs: Vec<&str> = packages.iter().map(String::as_str).collect();
        args.extend_from_slice(&pkg_refs);
        let RunOutcome { status, stderr, .. } = runner.run("sudo", &args)?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        Ok(())
    }
}
