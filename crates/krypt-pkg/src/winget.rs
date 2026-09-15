//! `winget` package manager implementation (Windows Package Manager).
//!
//! winget does not reliably accept multiple packages in one call, so each
//! package is installed in a separate process invocation.
//!
//! Every call pins the package with `--id <pkg> --exact`: without `--exact`
//! winget matches IDs by substring, so `OpenJS.NodeJS` would report an
//! installed `OpenJS.NodeJS.22` as present, and an install query can resolve
//! to several packages and refuse to pick one.

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// `APPINSTALLER_CLI_ERROR_UPDATE_NOT_APPLICABLE`: `winget install` found the
/// package already installed with no newer version to upgrade to.
pub const UPDATE_NOT_APPLICABLE: i32 = 0x8A15_002B_u32.cast_signed();

/// Package manager implementation for Windows (winget).
pub struct Winget;

impl PackageManager for Winget {
    fn name(&self) -> &'static str {
        "winget"
    }

    fn is_available(&self) -> bool {
        which::which("winget").is_ok()
    }

    fn exists(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } = runner.run(
            "winget",
            &["show", "--id", pkg, "--exact", "--accept-source-agreements"],
        )?;
        Ok(status == 0)
    }

    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, stdout, .. } =
            runner.run("winget", &["list", "--id", pkg, "--exact"])?;
        Ok(status == 0 && !stdout.trim().is_empty())
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        for pkg in packages {
            let RunOutcome { status, stderr, .. } = runner.run(
                "winget",
                &[
                    "install",
                    "--id",
                    pkg.as_str(),
                    "--exact",
                    "--silent",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                ],
            )?;
            if status != 0 && status != UPDATE_NOT_APPLICABLE {
                return Err(PackageError::ExitFailure { status, stderr });
            }
        }
        Ok(())
    }
}
