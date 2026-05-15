//! `dnf` package manager implementation (Fedora / RHEL).

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// Package manager implementation for Fedora-family systems.
pub struct Dnf;

impl PackageManager for Dnf {
    fn name(&self) -> &'static str {
        "dnf"
    }

    fn is_available(&self) -> bool {
        which::which("dnf").is_ok()
    }

    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } = runner.run("rpm", &["-q", pkg])?;
        Ok(status == 0)
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let mut args = vec!["dnf", "install", "-y"];
        let pkg_refs: Vec<&str> = packages.iter().map(String::as_str).collect();
        args.extend_from_slice(&pkg_refs);
        let RunOutcome { status, stderr, .. } = runner.run("sudo", &args)?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        Ok(())
    }
}
