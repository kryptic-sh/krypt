//! `winget` package manager implementation (Windows Package Manager).
//!
//! winget does not reliably accept multiple packages in one call, so each
//! package is installed in a separate process invocation.

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// Package manager implementation for Windows (winget).
pub struct Winget;

impl PackageManager for Winget {
    fn name(&self) -> &'static str {
        "winget"
    }

    fn is_available(&self) -> bool {
        which::which("winget").is_ok()
    }

    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, stdout, .. } = runner.run("winget", &["list", "--id", pkg])?;
        Ok(status == 0 && !stdout.trim().is_empty())
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        for pkg in packages {
            let RunOutcome { status, stderr, .. } = runner.run(
                "winget",
                &[
                    "install",
                    "--silent",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                    pkg.as_str(),
                ],
            )?;
            if status != 0 {
                return Err(PackageError::ExitFailure { status, stderr });
            }
        }
        Ok(())
    }
}
