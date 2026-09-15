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

    /// `dnf repoquery --whatprovides` matches package names, virtual provides
    /// (Fedora's `nodejs` is provided by `nodejs22`) and file paths, as
    /// `dnf install` does; it exits 0 with no output when nothing matches. A
    /// `@group` is looked up with `dnf group info`.
    fn exists(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        if let Some(group) = pkg.strip_prefix('@') {
            let RunOutcome { status, .. } = runner.run("dnf", &["group", "info", group])?;
            return Ok(status == 0);
        }
        let RunOutcome { status, stdout, .. } =
            runner.run("dnf", &["repoquery", "--quiet", "--whatprovides", pkg])?;
        Ok(status == 0 && !stdout.trim().is_empty())
    }

    /// `rpm -q --whatprovides`, so a name that another package provides counts
    /// once `dnf install` has resolved it (Fedora's `wget` is `wget2-wget`).
    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } = runner.run("rpm", &["-q", "--whatprovides", pkg])?;
        Ok(status == 0)
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let mut args = vec!["install", "-y"];
        args.extend(packages.iter().map(String::as_str));
        let RunOutcome { status, stderr, .. } = runner.run_as_root("dnf", &args)?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        Ok(())
    }
}
