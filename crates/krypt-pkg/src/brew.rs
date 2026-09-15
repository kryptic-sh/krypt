//! `brew` package manager implementation (macOS Homebrew).

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// Package manager implementation for macOS (Homebrew).
pub struct Brew;

impl PackageManager for Brew {
    fn name(&self) -> &'static str {
        "brew"
    }

    fn is_available(&self) -> bool {
        which::which("brew").is_ok()
    }

    /// `brew info` resolves formulae and casks. It does not tap a tap that is
    /// not installed yet, so for a tap-qualified `user/repo/name` the tap is
    /// added first — as `brew install` would do — and a tap that cannot be
    /// added means the package is missing.
    fn exists(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        if let Some((tap, _)) = pkg.rsplit_once('/').filter(|(tap, _)| tap.contains('/')) {
            let RunOutcome { status, .. } = runner.run("brew", &["tap", tap])?;
            if status != 0 {
                return Ok(false);
            }
        }
        let RunOutcome { status, .. } = runner.run("brew", &["info", pkg])?;
        Ok(status == 0)
    }

    /// `brew list --versions` covers casks as well as formulae; limiting it to
    /// `--formula` reported every installed cask (e.g. `alacritty`) missing.
    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, stdout, .. } = runner.run("brew", &["list", "--versions", pkg])?;
        Ok(status == 0 && !stdout.trim().is_empty())
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let mut args = vec!["install"];
        let pkg_refs: Vec<&str> = packages.iter().map(String::as_str).collect();
        args.extend_from_slice(&pkg_refs);
        let RunOutcome { status, stderr, .. } = runner.run("brew", &args)?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        Ok(())
    }
}
