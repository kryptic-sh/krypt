//! `krypt deps` orchestration — installs dependency groups.
//!
//! This module is decoupled from `krypt-core`: callers extract the relevant
//! fields from their config and pass a [`DepGroup`] slice so that `krypt-pkg`
//! remains free of the `krypt-core` crate dependency.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::cargo::{self, Cargo};
use crate::detect::{detect_all, pick_by_name};
use crate::manager::{PackageError, PackageManager, Runner};
use crate::scoop::{self, Scoop};

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
    /// URLs of scoop buckets that `bucket/app` entries in [`Self::scoop`] name
    /// and Scoop does not know by name, keyed by bucket name.
    pub scoop_buckets: BTreeMap<String, String>,
    /// Packages for the `winget` manager.
    pub winget: Vec<String>,
}

// ─── DepsError ────────────────────────────────────────────────────────────────

/// Errors from [`install_deps`] and [`check_deps`].
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

/// Inputs for [`install_deps`] and [`check_deps`].
pub struct DepsOpts {
    /// Dependency groups, already filtered by platform by the caller.
    pub groups: Vec<DepGroup>,
    /// Explicit manager override (e.g. `"apt"`). `None` = every manager
    /// detected on this platform, in preference order.
    pub manager: Option<String>,
    /// Install only the named group. `None` = all groups.
    pub group_filter: Option<String>,
    /// Dry-run: skip actual installation.
    pub dry_run: bool,
}

// ─── Reports ──────────────────────────────────────────────────────────────────

/// Summary of a [`install_deps`] run.
///
/// Package entries are reported as written in the config, so a crate routed
/// to cargo appears as `cargo:<crate>`.
pub struct DepsReport {
    /// Managers that handled at least one group, in first-use order; `cargo`
    /// is listed when a `cargo:` entry was processed.
    pub managers_used: Vec<String>,
    /// Packages that were installed (or would have been in dry-run).
    pub installed: Vec<String>,
    /// Packages already present — skipped.
    pub already_installed: Vec<String>,
    /// Groups with no packages for any of the candidate managers.
    pub skipped_unavailable: Vec<String>,
    /// Packages that failed to install: `(package, error_message)`.
    pub failed: Vec<(String, String)>,
}

/// Summary of a [`check_deps`] run.
pub struct CheckReport {
    /// Managers asked about at least one package, in first-use order.
    pub managers_used: Vec<String>,
    /// Packages the manager can install.
    pub found: Vec<String>,
    /// Packages the manager does not know.
    pub missing: Vec<String>,
    /// Groups with no packages for any of the candidate managers.
    pub skipped_unavailable: Vec<String>,
    /// Packages whose lookup itself failed: `(package, error_message)`.
    pub failed: Vec<(String, String)>,
}

// ─── Planning ─────────────────────────────────────────────────────────────────

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

/// The managers a run may use: the `--manager` override alone, or every
/// manager detected on this platform in preference order.
fn candidate_managers(opts: &DepsOpts) -> Result<Vec<Box<dyn PackageManager>>, DepsError> {
    match &opts.manager {
        Some(name) => pick_by_name(name)
            .map(|m| vec![m])
            .ok_or_else(|| DepsError::UnknownManager(name.clone())),
        None => {
            let detected = detect_all();
            if detected.is_empty() {
                Err(DepsError::NoManagerDetected)
            } else {
                Ok(detected)
            }
        }
    }
}

/// One group's packages, split between the manager that owns the group's
/// list and cargo. Each entry is `(as written, name passed to the manager)`.
struct GroupPlan<'a> {
    group: &'a DepGroup,
    manager: &'a dyn PackageManager,
    native: Vec<(&'a str, &'a str)>,
    cargo: Vec<(&'a str, &'a str)>,
}

/// The groups `opts` selects, each assigned to the first candidate manager
/// that lists packages for it. Groups no candidate lists packages for are
/// returned by name.
fn plan_groups<'a>(
    opts: &'a DepsOpts,
    candidates: &'a [Box<dyn PackageManager>],
) -> (Vec<GroupPlan<'a>>, Vec<String>) {
    let mut plans = Vec::new();
    let mut unavailable = Vec::new();

    for group in &opts.groups {
        if opts
            .group_filter
            .as_deref()
            .is_some_and(|f| f != group.group)
        {
            continue;
        }

        let Some((manager, pkgs)) = candidates.iter().find_map(|m| {
            let pkgs = packages_for(group, m.name());
            (!pkgs.is_empty()).then_some((m.as_ref(), pkgs))
        }) else {
            unavailable.push(group.group.clone());
            continue;
        };

        let mut plan = GroupPlan {
            group,
            manager,
            native: Vec::new(),
            cargo: Vec::new(),
        };
        for entry in pkgs {
            match entry.strip_prefix(cargo::PREFIX) {
                Some(krate) => plan.cargo.push((entry.as_str(), krate)),
                None => plan.native.push((entry.as_str(), entry.as_str())),
            }
        }
        plans.push(plan);
    }

    (plans, unavailable)
}

fn note_manager(used: &mut Vec<String>, manager: &dyn PackageManager) {
    if !used.iter().any(|m| m == manager.name()) {
        used.push(manager.name().to_owned());
    }
}

/// The plan's native entries with its manager, then its cargo entries with
/// [`Cargo`], skipping whichever side is empty.
fn sources<'p, 'a>(
    plan: &'p GroupPlan<'a>,
) -> impl Iterator<Item = (&'a dyn PackageManager, &'p [(&'a str, &'a str)])> {
    [
        (plan.manager, plan.native.as_slice()),
        (&Cargo as &dyn PackageManager, plan.cargo.as_slice()),
    ]
    .into_iter()
    .filter(|(_, entries)| !entries.is_empty())
}

/// Entries split into those ready to look up or install and those whose
/// source could not be set up, the latter as `(as written, reason)`.
type Prepared<'a> = (Vec<(&'a str, &'a str)>, Vec<(String, String)>);

/// Sets up the sources `manager` needs for `entries` before they are looked
/// up or installed: for scoop, the buckets that `bucket/app` entries name
/// (see [`scoop::add_buckets`]). Entries of other managers are all ready.
fn prepare<'a>(
    runner: &dyn Runner,
    plan: &GroupPlan<'_>,
    manager: &dyn PackageManager,
    entries: &[(&'a str, &'a str)],
) -> Result<Prepared<'a>, PackageError> {
    if manager.name() != Scoop.name() {
        return Ok((entries.to_vec(), Vec::new()));
    }
    let names: Vec<&str> = entries.iter().map(|&(_, name)| name).collect();
    let missing = scoop::add_buckets(runner, &names, &plan.group.scoop_buckets)?;

    let mut ready = Vec::new();
    let mut unready = Vec::new();
    for &(written, name) in entries {
        match scoop::split_entry(name)
            .0
            .filter(|bucket| missing.iter().any(|m| m.eq_ignore_ascii_case(bucket)))
        {
            Some(bucket) => unready.push((
                written.to_owned(),
                format!("scoop bucket `{bucket}` could not be added"),
            )),
            None => ready.push((written, name)),
        }
    }
    Ok((ready, unready))
}

// ─── install_deps ─────────────────────────────────────────────────────────────

/// Install dependency groups according to the options.
///
/// Each group goes to the first candidate manager (see [`DepsOpts::manager`])
/// with packages listed for it, so on Windows a group listed only for winget
/// still installs when scoop is also present. Entries written `cargo:<crate>`
/// are installed with `cargo install` instead of the manager. Scoop buckets
/// that `bucket/app` entries name are added before the group installs, except
/// in dry-run.
///
/// Groups should already be filtered by platform before calling this function.
pub fn install_deps(opts: &DepsOpts, runner: &dyn Runner) -> Result<DepsReport, DepsError> {
    let candidates = candidate_managers(opts)?;
    let (plans, skipped_unavailable) = plan_groups(opts, &candidates);
    let mut report = DepsReport {
        managers_used: Vec::new(),
        installed: Vec::new(),
        already_installed: Vec::new(),
        skipped_unavailable,
        failed: Vec::new(),
    };

    for plan in &plans {
        for (manager, entries) in sources(plan) {
            note_manager(&mut report.managers_used, manager);

            if opts.dry_run {
                // Skip is_installed in dry-run — assume everything needs installing.
                report
                    .installed
                    .extend(entries.iter().map(|(written, _)| (*written).to_owned()));
                continue;
            }

            let entries = match prepare(runner, plan, manager, entries) {
                Ok((ready, unready)) => {
                    report.failed.extend(unready);
                    ready
                }
                Err(e) => {
                    let msg = e.to_string();
                    report
                        .failed
                        .extend(entries.iter().map(|(w, _)| ((*w).to_owned(), msg.clone())));
                    continue;
                }
            };

            let mut to_install: Vec<(&str, &str)> = Vec::new();
            for &(written, name) in &entries {
                match manager.is_installed(runner, name) {
                    Ok(true) => report.already_installed.push(written.to_owned()),
                    Ok(false) => to_install.push((written, name)),
                    Err(e) => report.failed.push((written.to_owned(), e.to_string())),
                }
            }
            if to_install.is_empty() {
                continue;
            }

            let names: Vec<String> = to_install.iter().map(|(_, n)| (*n).to_owned()).collect();
            match manager.install(runner, &names) {
                Ok(()) => report
                    .installed
                    .extend(to_install.iter().map(|(w, _)| (*w).to_owned())),
                // Only the named packages failed; the rest of the batch landed.
                Err(PackageError::NotInstalled { packages, output }) => {
                    for &(written, name) in &to_install {
                        if packages.iter().any(|p| p == name) {
                            let e = PackageError::NotInstalled {
                                packages: vec![name.to_owned()],
                                output: output.clone(),
                            };
                            report.failed.push((written.to_owned(), e.to_string()));
                        } else {
                            report.installed.push(written.to_owned());
                        }
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    report.failed.extend(
                        to_install
                            .iter()
                            .map(|(w, _)| ((*w).to_owned(), msg.clone())),
                    );
                }
            }
        }
    }

    Ok(report)
}

// ─── check_deps ───────────────────────────────────────────────────────────────

/// Ask each package's manager whether it can install the package, without
/// installing anything. Groups and managers are chosen as in [`install_deps`];
/// `opts.dry_run` is ignored. Scoop buckets that `bucket/app` entries name are
/// added first, since Scoop can only look an app up in an added bucket; an
/// entry whose bucket cannot be added is missing.
pub fn check_deps(opts: &DepsOpts, runner: &dyn Runner) -> Result<CheckReport, DepsError> {
    let candidates = candidate_managers(opts)?;
    let (plans, skipped_unavailable) = plan_groups(opts, &candidates);
    let mut report = CheckReport {
        managers_used: Vec::new(),
        found: Vec::new(),
        missing: Vec::new(),
        skipped_unavailable,
        failed: Vec::new(),
    };

    for plan in &plans {
        for (manager, entries) in sources(plan) {
            note_manager(&mut report.managers_used, manager);
            let entries = match prepare(runner, plan, manager, entries) {
                Ok((ready, unready)) => {
                    report.missing.extend(unready.into_iter().map(|(w, _)| w));
                    ready
                }
                Err(e) => {
                    let msg = e.to_string();
                    report
                        .failed
                        .extend(entries.iter().map(|(w, _)| ((*w).to_owned(), msg.clone())));
                    continue;
                }
            };
            for &(written, name) in &entries {
                match manager.exists(runner, name) {
                    Ok(true) => report.found.push(written.to_owned()),
                    Ok(false) => report.missing.push(written.to_owned()),
                    Err(e) => report.failed.push((written.to_owned(), e.to_string())),
                }
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoop::Scoop;
    use crate::winget::Winget;

    fn group(name: &str, scoop: &[&str], winget: &[&str]) -> DepGroup {
        DepGroup {
            group: name.into(),
            scoop: scoop.iter().map(|p| (*p).to_owned()).collect(),
            winget: winget.iter().map(|p| (*p).to_owned()).collect(),
            ..Default::default()
        }
    }

    // Planning takes the candidates as given, so the fallback is checked the
    // same way on every host.
    #[test]
    fn each_group_goes_to_the_first_candidate_listing_packages() {
        let opts = DepsOpts {
            groups: vec![
                group("both", &["git"], &["Git.Git"]),
                group("winget-only", &[], &["Neovim.Neovim"]),
                group("neither", &[], &[]),
            ],
            manager: None,
            group_filter: None,
            dry_run: true,
        };
        let candidates: Vec<Box<dyn PackageManager>> = vec![Box::new(Scoop), Box::new(Winget)];

        let (plans, unavailable) = plan_groups(&opts, &candidates);
        let chosen: Vec<&str> = plans.iter().map(|p| p.manager.name()).collect();
        assert_eq!(chosen, ["scoop", "winget"]);
        assert_eq!(plans[1].native, [("Neovim.Neovim", "Neovim.Neovim")]);
        assert_eq!(unavailable, ["neither"]);
    }

    #[test]
    fn cargo_entries_are_split_from_the_managers_own() {
        let opts = DepsOpts {
            groups: vec![group(
                "tools",
                &[],
                &["BurntSushi.ripgrep.MSVC", "cargo:hjkl"],
            )],
            manager: None,
            group_filter: None,
            dry_run: true,
        };
        let candidates: Vec<Box<dyn PackageManager>> = vec![Box::new(Winget)];

        let (plans, _) = plan_groups(&opts, &candidates);
        assert_eq!(
            plans[0].native,
            [("BurntSushi.ripgrep.MSVC", "BurntSushi.ripgrep.MSVC")]
        );
        assert_eq!(plans[0].cargo, [("cargo:hjkl", "hjkl")]);
    }
}
