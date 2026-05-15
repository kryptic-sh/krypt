//! `scoop` package manager implementation (Windows).

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// Package manager implementation for Windows (Scoop).
pub struct Scoop;

impl PackageManager for Scoop {
    fn name(&self) -> &'static str {
        "scoop"
    }

    fn is_available(&self) -> bool {
        which::which("scoop").is_ok()
    }

    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, stdout, .. } = runner.run("scoop", &["list", pkg])?;
        Ok(status == 0 && !stdout.trim().is_empty())
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let mut args = vec!["install"];
        let pkg_refs: Vec<&str> = packages.iter().map(String::as_str).collect();
        args.extend_from_slice(&pkg_refs);
        let RunOutcome { status, stderr, .. } = runner.run("scoop", &args)?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        Ok(())
    }
}
