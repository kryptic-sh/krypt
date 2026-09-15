//! `cargo install` as a package source for Rust crates.
//!
//! Not a system package manager: it is never auto-detected and has no field
//! of its own in a `[[deps]]` group. A group routes an entry here by writing
//! it as `cargo:<crate>` in any manager's list, so each manager decides per
//! package whether to use its own package or build the crate.

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// Prefix that routes a `[[deps]]` entry to [`Cargo`].
pub const PREFIX: &str = "cargo:";

/// Installs crates with `cargo install --locked` into Cargo's own bin
/// directory (`$CARGO_HOME/bin`); no root privileges.
pub struct Cargo;

impl Cargo {
    /// The crate name of a `name` or `name@version` spec.
    fn crate_name(spec: &str) -> &str {
        spec.split_once('@').map_or(spec, |(name, _)| name)
    }
}

impl PackageManager for Cargo {
    fn name(&self) -> &'static str {
        "cargo"
    }

    fn is_available(&self) -> bool {
        which::which("cargo").is_ok()
    }

    /// `cargo info` queries the registry and fails for an unknown crate.
    fn exists(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } = runner.run("cargo", &["info", pkg])?;
        Ok(status == 0)
    }

    /// Looks for the crate in `cargo install --list`, whose crate lines read
    /// `name v1.2.3:` and whose binary lines are indented.
    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome {
            status,
            stdout,
            stderr,
        } = runner.run("cargo", &["install", "--list"])?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        let name = Self::crate_name(pkg);
        Ok(stdout
            .lines()
            .filter(|line| !line.starts_with(char::is_whitespace))
            .any(|line| line.split_whitespace().next() == Some(name)))
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let mut args = vec!["install", "--locked"];
        args.extend(packages.iter().map(String::as_str));
        let RunOutcome { status, stderr, .. } = runner.run("cargo", &args)?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        Ok(())
    }
}
