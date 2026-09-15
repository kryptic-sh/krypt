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

    /// A simulated `apt-get install`, so a virtual package counts only when
    /// apt can pick a provider for it. Needs no root, but does need the
    /// package lists (`apt-get update`).
    fn exists(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } =
            runner.run("apt-get", &["install", "--simulate", "--quiet", pkg])?;
        Ok(status == 0)
    }

    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } = runner.run("dpkg", &["-s", pkg])?;
        Ok(status == 0)
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let mut args = vec!["install", "-y"];
        args.extend(packages.iter().map(String::as_str));
        let RunOutcome { status, stderr, .. } = runner.run_as_root("apt-get", &args)?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        Ok(())
    }
}
