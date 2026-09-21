//! `scoop` package manager implementation (Windows).
//!
//! Scoop exits 0 whether or not a command did what was asked: `scoop install`
//! of an unknown app, `scoop info` of one, and `scoop bucket add` without git
//! all print an error and still succeed (checked against Scoop 0.5.3). So no
//! answer here is taken from an exit status. State is read back from
//! `scoop export`, which prints the installed apps and added buckets as JSON,
//! and a lookup counts as found only when `scoop cat` prints a JSON manifest.
//!
//! An entry is an app name (`git`), or an app qualified by the bucket it comes
//! from (`extras/alacritty`). [`add_buckets`] adds the buckets that qualified
//! entries name before they are looked up or installed.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// Package manager implementation for Windows (Scoop).
pub struct Scoop;

/// The bucket and app of an entry: `extras/alacritty` is
/// `(Some("extras"), "alacritty")`, `git` is `(None, "git")`.
pub fn split_entry(entry: &str) -> (Option<&str>, &str) {
    match entry.split_once('/') {
        Some((bucket, app)) => (Some(bucket), app),
        None => (None, entry),
    }
}

/// What `scoop export` reports: installed app names and added bucket names.
struct Export {
    apps: Vec<String>,
    buckets: Vec<String>,
}

/// Runs `scoop export` and collects the `Name` of every entry under `apps` and
/// under `buckets`. An app whose install failed half-way is listed by Scoop
/// with the info `Install failed`; it is left out, since it is not usable.
fn export(runner: &dyn Runner) -> Result<Export, PackageError> {
    let RunOutcome { stdout, .. } = runner.run("scoop", &["export"])?;
    let json: Value = serde_json::from_str(&stdout)
        .map_err(|e| PackageError::Io(std::io::Error::other(format!("scoop export JSON: {e}"))))?;
    let names = |key: &str, keep: fn(&Value) -> bool| -> Vec<String> {
        json[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| keep(entry))
            .filter_map(|entry| entry["Name"].as_str().map(str::to_owned))
            .collect()
    };
    Ok(Export {
        apps: names("apps", |app| {
            !app["Info"]
                .as_str()
                .is_some_and(|info| info.contains("Install failed"))
        }),
        buckets: names("buckets", |_| true),
    })
}

/// Scoop app and bucket names are case-insensitive, like the directories
/// they are stored in.
fn contains(names: &[String], name: &str) -> bool {
    names.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// Adds every bucket that a bucket-qualified entry in `entries` names and that
/// is not added yet, and returns the ones still missing afterwards.
///
/// A bucket in `urls` is added from its URL; any other is added by name, which
/// Scoop resolves for the buckets `scoop bucket known` lists (`extras`,
/// `nerd-fonts`, ...). Scoop clones buckets with git, so when git is not on
/// `PATH` it is installed from the main bucket first — the fix Scoop's own
/// error message gives.
pub fn add_buckets(
    runner: &dyn Runner,
    entries: &[&str],
    urls: &BTreeMap<String, String>,
) -> Result<Vec<String>, PackageError> {
    add_buckets_with_git(runner, entries, urls, which::which("git").is_ok())
}

/// [`add_buckets`] with git's presence on `PATH` given, so it is testable the
/// same way on every host.
fn add_buckets_with_git(
    runner: &dyn Runner,
    entries: &[&str],
    urls: &BTreeMap<String, String>,
    git_on_path: bool,
) -> Result<Vec<String>, PackageError> {
    let added = export(runner)?.buckets;
    let mut wanted: Vec<&str> = Vec::new();
    for bucket in entries.iter().filter_map(|e| split_entry(e).0) {
        if !contains(&added, bucket) && !wanted.iter().any(|w| w.eq_ignore_ascii_case(bucket)) {
            wanted.push(bucket);
        }
    }
    if wanted.is_empty() {
        return Ok(Vec::new());
    }

    if !git_on_path {
        runner.run("scoop", &["install", "git"])?;
    }
    for bucket in &wanted {
        let mut args = vec!["bucket", "add", *bucket];
        if let Some(url) = urls.get(*bucket) {
            args.push(url);
        }
        runner.run("scoop", &args)?;
    }

    let added = export(runner)?.buckets;
    Ok(wanted
        .into_iter()
        .filter(|bucket| !contains(&added, bucket))
        .map(str::to_owned)
        .collect())
}

impl PackageManager for Scoop {
    fn name(&self) -> &'static str {
        "scoop"
    }

    fn is_available(&self) -> bool {
        which::which("scoop").is_ok()
    }

    /// `scoop cat` prints the app's manifest, which is JSON, or an error
    /// message when no added bucket has it.
    fn exists(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { stdout, .. } = runner.run("scoop", &["cat", pkg])?;
        Ok(serde_json::from_str::<Value>(&stdout).is_ok_and(|manifest| manifest.is_object()))
    }

    /// Installed means listed by `scoop export`, whichever bucket it came from.
    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        Ok(contains(&export(runner)?.apps, split_entry(pkg).1))
    }

    /// Runs one `scoop install` per package — given several, scoop installs
    /// none of them when one is unknown — then reads `scoop export` back; the
    /// packages it does not list are returned as
    /// [`PackageError::NotInstalled`], with what scoop printed for them.
    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let mut printed = Vec::with_capacity(packages.len());
        for pkg in packages {
            let RunOutcome { stdout, stderr, .. } = runner.run("scoop", &["install", pkg])?;
            printed.push(format!("{stdout}{stderr}").trim().to_owned());
        }

        let installed = export(runner)?.apps;
        let (missing, output): (Vec<String>, Vec<String>) = packages
            .iter()
            .zip(printed)
            .filter(|(pkg, _)| !contains(&installed, split_entry(pkg).1))
            .map(|(pkg, out)| (pkg.clone(), out))
            .unzip();
        if missing.is_empty() {
            return Ok(());
        }
        Err(PackageError::NotInstalled {
            packages: missing,
            output: output.join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::{MockResponse, MockRunner};

    #[test]
    fn buckets_install_git_first_only_when_it_is_missing() {
        let export = MockResponse {
            status: 0,
            stdout: r#"{"apps":[],"buckets":[{"Name":"main"}]}"#.into(),
            stderr: String::new(),
        };
        let installs_git = |git_on_path| {
            let runner = MockRunner::new().with("scoop", &["export"], export.clone());
            add_buckets_with_git(
                &runner,
                &["extras/alacritty"],
                &BTreeMap::new(),
                git_on_path,
            )
            .unwrap();
            runner
                .calls()
                .iter()
                .any(|(_, args)| args == &["install", "git"])
        };
        assert!(installs_git(false));
        assert!(!installs_git(true));
    }

    #[test]
    fn entries_split_on_the_bucket() {
        assert_eq!(
            split_entry("extras/alacritty"),
            (Some("extras"), "alacritty")
        );
        assert_eq!(split_entry("git"), (None, "git"));
    }
}
