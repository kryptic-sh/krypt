//! `apt` package manager implementation (Debian / Ubuntu).

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// Package manager implementation for Debian-family systems.
pub struct Apt;

impl PackageManager for Apt {
    fn name(&self) -> &'static str {
        "apt"
    }

    fn is_available(&self) -> bool {
        which::which("apt").is_ok()
    }

    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } = runner.run("dpkg", &["-s", pkg])?;
        Ok(status == 0)
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let mut args = vec!["apt-get", "install", "-y"];
        let pkg_refs: Vec<&str> = packages.iter().map(String::as_str).collect();
        args.extend_from_slice(&pkg_refs);
        let RunOutcome { status, stderr, .. } = runner.run("sudo", &args)?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        Ok(())
    }
}
