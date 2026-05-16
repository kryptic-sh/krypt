//! `krypt deps` orchestration — installs dependency groups.
//!
//! This module is decoupled from `krypt-core`: callers extract the relevant
//! fields from their config and pass a [`DepGroup`] slice so that `krypt-pkg`
//! remains free of the `krypt-core` crate dependency.

use thiserror::Error;

use crate::detect::{pick_by_name, pick_default};
use crate::manager::{PackageError, PackageManager, Runner};

// ─── DepGroup ─────────────────────────────────────────────────────────────────

/// Caller-supplied representation of one `[[deps]]` group.
///
/// Mirrors the relevant fields from `krypt_core::config::DepsGroup`; the CLI
/// layer constructs these from the parsed config so that `krypt-pkg` does not
/// need to take a dependency on `krypt-core`.
#[derive(Debug, Clone, Default)]
pub struct DepGroup {
    /// Group name (e.g. `"core"`, `"fonts"`).
    pub group: String,
    /// Packages for the `pacman` manager.
    pub pacman: Vec<String>,
    /// Packages for the `apt` manager.
    pub apt: Vec<String>,
    /// Packages for the `dnf` manager.
    pub dnf: Vec<String>,
    /// Packages for the `brew` manager.
    pub brew: Vec<String>,
    /// Packages for the `scoop` manager.
    pub scoop: Vec<String>,
    /// Packages for the `winget` manager.
    pub winget: Vec<String>,
}

// ─── DepsError ────────────────────────────────────────────────────────────────

/// Errors from [`install_deps`].
#[derive(Debug, Error)]
pub enum DepsError {
    /// No package manager could be detected on this platform.
    #[error("no package manager detected; install one or use --manager")]
    NoManagerDetected,

    /// `--manager <name>` was given but the name is unknown.
    #[error("unknown package manager: {0}")]
    UnknownManager(String),

    /// A package installation failed.
    #[error("install error: {0}")]
    Install(#[from] PackageError),
}

// ─── DepsOpts ─────────────────────────────────────────────────────────────────

/// Inputs for [`install_deps`].
pub struct DepsOpts {
    /// Dependency groups, already filtered by platform by the caller.
    pub groups: Vec<DepGroup>,
    /// Explicit manager override (e.g. `"apt"`). `None` = auto-detect.
    pub manager: Option<String>,
    /// Install only the named group. `None` = all groups.
    pub group_filter: Option<String>,
    /// Dry-run: skip actual installation.
    pub dry_run: bool,
}

// ─── DepsReport ───────────────────────────────────────────────────────────────

/// Summary of a [`install_deps`] run.
pub struct DepsReport {
    /// Name of the manager that was (or would have been) used.
    pub manager_used: String,
    /// Packages that were installed (or would have been in dry-run).
    pub installed: Vec<String>,
    /// Packages already present — skipped.
    pub already_installed: Vec<String>,
    /// Groups whose package list was empty for the chosen manager.
    pub skipped_unavailable: Vec<String>,
    /// Packages that failed to install: `(package, error_message)`.
    pub failed: Vec<(String, String)>,
}

// ─── helpers ──────────────────────────────────────────────────────────────────

/// Extract the package list for `manager_name` from a dep group.
fn packages_for<'a>(group: &'a DepGroup, manager_name: &str) -> &'a [String] {
    match manager_name {
        "pacman" => &group.pacman,
        "apt" => &group.apt,
        "dnf" => &group.dnf,
        "brew" => &group.brew,
        "scoop" => &group.scoop,
        "winget" => &group.winget,
        _ => &[],
    }
}

// ─── install_deps ─────────────────────────────────────────────────────────────

/// Install dependency groups according to the options.
///
/// Groups should already be filtered by platform before calling this function.
pub fn install_deps(opts: &DepsOpts, runner: &dyn Runner) -> Result<DepsReport, DepsError> {
    let manager: Box<dyn PackageManager> = match &opts.manager {
        Some(name) => pick_by_name(name).ok_or_else(|| DepsError::UnknownManager(name.clone()))?,
        None => pick_default().ok_or(DepsError::NoManagerDetected)?,
    };

    let manager_name = manager.name().to_owned();
    let mut report = DepsReport {
        manager_used: manager_name.clone(),
        installed: Vec::new(),
        already_installed: Vec::new(),
        skipped_unavailable: Vec::new(),
        failed: Vec::new(),
    };

    for group in &opts.groups {
        if opts
            .group_filter
            .as_deref()
            .is_some_and(|f| f != group.group)
        {
            continue;
        }

        let pkgs = packages_for(group, &manager_name);
        if pkgs.is_empty() {
            report.skipped_unavailable.push(group.group.clone());
            continue;
        }

        let mut to_install: Vec<String> = Vec::new();
        if opts.dry_run {
            // Skip is_installed check in dry-run — assume all packages need installing.
            to_install.extend_from_slice(pkgs);
        } else {
            for pkg in pkgs {
                match manager.is_installed(runner, pkg) {
                    Ok(true) => report.already_installed.push(pkg.clone()),
                    Ok(false) => to_install.push(pkg.clone()),
                    Err(e) => report.failed.push((pkg.clone(), e.to_string())),
                }
            }
        }

        if to_install.is_empty() {
            continue;
        }

        if opts.dry_run {
            report.installed.extend(to_install);
        } else {
            match manager.install(runner, &to_install) {
                Ok(()) => report.installed.extend(to_install),
                Err(e) => {
                    let msg = e.to_string();
                    for pkg in to_install {
                        report.failed.push((pkg, msg.clone()));
                    }
                }
            }
        }
    }

    Ok(report)
}
