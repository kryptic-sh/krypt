//! Generic `krypt <group> <name>` dispatcher.
//!
//! This module is the core logic behind both `krypt menu` and any arbitrary
//! user-defined group (`krypt battery report`, `krypt kanata toggle`, etc.).
//! It keeps the CLI thin by handling config loading, platform filtering,
//! dry-run formatting, and runner dispatch from here.
//!
//! # Step interpolation note
//!
//! Steps use `{name}` for named captures and `{0}`..`{9}` for positional args
//! forwarded via `krypt <group> <name> -- arg0 arg1 ...`. The `${VAR}` syntax
//! (e.g. `${HOME}`) is **not** expanded inside step args — that is a
//! `.krypt.toml`-level path-variable syntax resolved by [`crate::paths::Resolver`]
//! at config-load time, not at step-execution time. If a step needs `$HOME`,
//! use `run = ["sh", "-c", "echo $HOME"]` or capture it into a named variable
//! first.
//!
//! The resolver **is** used by predicate evaluation (`file_exists:${HOME}/.bashrc`
//! etc.) because `predicate.rs` calls `Resolver::resolve` internally.

#![allow(clippy::result_large_err)]

use std::path::PathBuf;

use thiserror::Error;

use crate::config::Command as KryptCommand;
use crate::include::{IncludeError, load_with_includes};
use crate::paths::Platform;
use crate::predicate::{DefaultPredicateEnv, default_predicate_evaluator};
use crate::runner::{
    Context, Notifier, ProcessExec, Prompter, RunReport, RunnerError, execute_command,
};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Options shared by both listing and running dispatch groups.
#[derive(Debug)]
pub struct DispatchOpts {
    /// Resolved path to `.krypt.toml`.
    pub config_path: PathBuf,
    /// Positional args forwarded to step `{0}`..`{9}` placeholders.
    pub args: Vec<String>,
    /// When `true`, print the step plan without executing any process.
    pub dry_run: bool,
}

/// One entry returned by [`list_in_group`].
#[derive(Debug)]
pub struct DispatchListEntry {
    /// Command name (`[[command]] name = ...`).
    pub name: String,
    /// Short description (`[[command]] description = ...`).
    pub description: String,
    /// `true` when the command's `platform` field doesn't match the current OS
    /// and it was included only because `show_all = true` was requested.
    pub platform_filtered: bool,
    /// The declared platform restriction, if any.
    pub platform: Option<String>,
}

/// Summary of a completed dispatch run.
#[derive(Debug)]
pub struct DispatchReport {
    /// Steps that were executed (including those with ignored failures).
    pub steps_run: usize,
    /// Steps skipped because their `if` predicate returned false.
    pub steps_skipped: usize,
    /// Steps that failed but were ignored via `on_fail = "ignore"` or
    /// `ignore_failure = true`.
    pub steps_failed_ignored: usize,
    /// Rendered dry-run plan, populated when [`DispatchOpts::dry_run`] is `true`.
    pub dry_run_plan: Option<String>,
}

/// Everything that can go wrong in the dispatch subsystem.
///
/// Boxed large variants to stay ≤ 128 bytes on all targets.
#[derive(Debug, Error)]
pub enum DispatchError {
    /// The config file could not be loaded or parsed.
    #[error("loading config: {0}")]
    ConfigLoad(#[from] Box<IncludeError>),

    /// No `[[command]]` entries exist for the requested group.
    #[error(
        "unknown group {name:?} — no [[command]] entries with group = {name:?}\n\navailable groups:\n{}",
        format_groups(available)
    )]
    GroupNotFound {
        /// The group name that was looked up.
        name: String,
        /// All groups defined in the config.
        available: Vec<String>,
    },

    /// No `[[command]] group = "<group>"` entry matched the requested name.
    #[error(
        "command {name:?} not found in group {group:?}; available: {}",
        available_in_group.join(", ")
    )]
    CommandNotFound {
        /// The group being searched.
        group: String,
        /// The name that was looked up.
        name: String,
        /// All command names defined in the group (regardless of platform).
        available_in_group: Vec<String>,
    },

    /// The requested command exists but is restricted to a different platform.
    #[error(
        "command {name:?} in group {group:?} is restricted to {required}; current platform is {current}"
    )]
    PlatformMismatch {
        /// Group name.
        group: String,
        /// Command name.
        name: String,
        /// Platform the command requires.
        required: String,
        /// Platform the binary is running on.
        current: String,
    },

    /// The step runner returned an error.
    #[error("runner error: {0}")]
    Runner(#[from] Box<RunnerError>),
}

fn format_groups(groups: &[String]) -> String {
    if groups.is_empty() {
        return "  (none)".to_owned();
    }
    groups
        .iter()
        .map(|g| format!("  {g}"))
        .collect::<Vec<_>>()
        .join("\n")
}

impl From<IncludeError> for DispatchError {
    fn from(e: IncludeError) -> Self {
        DispatchError::ConfigLoad(Box::new(e))
    }
}

impl From<RunnerError> for DispatchError {
    fn from(e: RunnerError) -> Self {
        DispatchError::Runner(Box::new(e))
    }
}

// ─── Group listing ────────────────────────────────────────────────────────────

/// Return all distinct group names present in the config, sorted.
pub fn list_groups(opts: &DispatchOpts) -> Result<Vec<String>, DispatchError> {
    let cfg = load_with_includes(&opts.config_path)?;
    let groups: Vec<String> = cfg
        .commands
        .iter()
        .map(|c| c.group.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(groups)
}

// ─── Listing ──────────────────────────────────────────────────────────────────

/// Return all commands defined in the given group.
///
/// When `show_all` is `false`, commands whose `platform` field doesn't match
/// [`Platform::current`] are excluded. When `show_all` is `true` they are
/// included with `platform_filtered = true`.
///
/// Returns [`DispatchError::GroupNotFound`] when the group has no entries.
/// Results are sorted alphabetically by name.
pub fn list_in_group(
    group: &str,
    opts: &DispatchOpts,
    show_all: bool,
) -> Result<Vec<DispatchListEntry>, DispatchError> {
    let cfg = load_with_includes(&opts.config_path)?;
    let current = Platform::current();

    let in_group: Vec<KryptCommand> = cfg
        .commands
        .into_iter()
        .filter(|cmd| cmd.group == group)
        .collect();

    if in_group.is_empty() {
        let cfg2 = load_with_includes(&opts.config_path)?;
        let available: Vec<String> = cfg2
            .commands
            .iter()
            .map(|c| c.group.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        return Err(DispatchError::GroupNotFound {
            name: group.to_owned(),
            available,
        });
    }

    let mut entries: Vec<DispatchListEntry> = in_group
        .into_iter()
        .filter_map(|cmd| {
            let filtered = cmd
                .platform
                .as_deref()
                .map(|p| p != current.as_str())
                .unwrap_or(false);

            if filtered && !show_all {
                return None;
            }

            Some(DispatchListEntry {
                name: cmd.name,
                description: cmd.description,
                platform_filtered: filtered,
                platform: cmd.platform,
            })
        })
        .collect();

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

// ─── Running (production) ─────────────────────────────────────────────────────

/// Run the named command in the given group using production process/notify/prompt implementations.
pub fn run_in_group(
    group: &str,
    name: &str,
    opts: &DispatchOpts,
) -> Result<DispatchReport, DispatchError> {
    use crate::notify::{AutoNotifier, NotifyBackend};
    use crate::runner::RealPrompter;

    let notifier = AutoNotifier::with_backend(NotifyBackend::Stderr);
    let mut prompter = RealPrompter;
    run_in_group_with(
        group,
        name,
        opts,
        &crate::runner::RealProcessExec,
        &notifier,
        &mut prompter,
    )
}

// ─── Running (injectable) ─────────────────────────────────────────────────────

/// Run the named command in the given group with injected dependencies (used by tests and dry-run).
pub fn run_in_group_with(
    group: &str,
    name: &str,
    opts: &DispatchOpts,
    process: &dyn ProcessExec,
    notifier: &dyn Notifier,
    prompter: &mut dyn Prompter,
) -> Result<DispatchReport, DispatchError> {
    let cfg = load_with_includes(&opts.config_path)?;

    let all_in_group: Vec<String> = cfg
        .commands
        .iter()
        .filter(|c| c.group == group)
        .map(|c| c.name.clone())
        .collect();

    if all_in_group.is_empty() {
        let available: Vec<String> = cfg
            .commands
            .iter()
            .map(|c| c.group.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        return Err(DispatchError::GroupNotFound {
            name: group.to_owned(),
            available,
        });
    }

    let cmd: &KryptCommand = cfg
        .commands
        .iter()
        .find(|c| c.group == group && c.name == name)
        .ok_or_else(|| DispatchError::CommandNotFound {
            group: group.to_owned(),
            name: name.to_owned(),
            available_in_group: all_in_group.clone(),
        })?;

    // Platform gate.
    let current = Platform::current();
    if let Some(ref required) = cmd.platform
        && required.as_str() != current.as_str()
    {
        return Err(DispatchError::PlatformMismatch {
            group: group.to_owned(),
            name: name.to_owned(),
            required: required.clone(),
            current: current.to_string(),
        });
    }

    if opts.dry_run {
        return dry_run_plan(group, cmd, opts);
    }

    let env = DefaultPredicateEnv::new();
    let eval = default_predicate_evaluator(env);

    let report: RunReport =
        execute_command(cmd, opts.args.clone(), process, notifier, prompter, &eval)?;

    Ok(DispatchReport {
        steps_run: report.steps_run,
        steps_skipped: report.steps_skipped_by_predicate,
        steps_failed_ignored: report.steps_failed_ignored,
        dry_run_plan: None,
    })
}

// ─── Dry-run ──────────────────────────────────────────────────────────────────

fn dry_run_plan(
    group: &str,
    cmd: &KryptCommand,
    opts: &DispatchOpts,
) -> Result<DispatchReport, DispatchError> {
    use std::collections::BTreeMap;

    use crate::predicate::{DefaultPredicateEnv, eval};
    use crate::runner::interpolate;

    let env = DefaultPredicateEnv::new();

    let ctx = Context {
        captures: BTreeMap::new(),
        args: opts.args.clone(),
        stdin: None,
    };

    let n = cmd.steps.len();
    let mut plan = format!("dry-run: {group} {:?} ({n} steps)\n", cmd.name);

    for (i, step) in cmd.steps.iter().enumerate() {
        let num = i + 1;

        let skipped = if let Some(ref pred) = step.r#if {
            match eval(pred, &env) {
                Ok(true) => false,
                Ok(false) => true,
                Err(e) => {
                    plan.push_str(&format!("\n  [{num}] (skipped — predicate error: {e})\n"));
                    continue;
                }
            }
        } else {
            false
        };

        if skipped {
            plan.push_str(&format!(
                "\n  [{num}] (skipped — predicate {:?} failed)\n",
                step.r#if.as_deref().unwrap_or("")
            ));
            continue;
        }

        if let Some(ref args) = step.run {
            let interp: Vec<String> = args.iter().map(|a| interpolate(a, &ctx)).collect();
            plan.push_str(&format!("\n  [{num}] run: {}\n", interp.join(" ")));
        } else if let Some(ref args) = step.pipe {
            let interp: Vec<String> = args.iter().map(|a| interpolate(a, &ctx)).collect();
            let input_display = step.input.as_deref().unwrap_or("{stdin}");
            plan.push_str(&format!(
                "\n  [{num}] pipe: {}  input: {}\n",
                interp.join(" "),
                input_display
            ));
        } else if let Some(ref parts) = step.notify {
            let title = parts.first().map(String::as_str).unwrap_or("");
            let body = parts.get(1).map(String::as_str).unwrap_or("");
            plan.push_str(&format!("\n  [{num}] notify: {:?} -> {:?}\n", title, body));
        } else {
            plan.push_str(&format!("\n  [{num}] (unknown step kind)\n"));
        }

        if let Some(ref var) = step.capture {
            plan.push_str(&format!("       capture -> {var}\n"));
        }
    }

    Ok(DispatchReport {
        steps_run: 0,
        steps_skipped: 0,
        steps_failed_ignored: 0,
        dry_run_plan: Some(plan),
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io;

    use tempfile::TempDir;

    use super::*;
    use crate::runner::{MockNotifier, MockProcessExec, MockPrompter, ProcessResult};

    fn ok_result(stdout: &str) -> Result<ProcessResult, io::Error> {
        Ok(ProcessResult {
            status: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }

    fn write_config(contents: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".krypt.toml");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    fn opts(config_path: PathBuf) -> DispatchOpts {
        DispatchOpts {
            config_path,
            args: Vec::new(),
            dry_run: false,
        }
    }

    fn opts_with_args(config_path: PathBuf, args: Vec<String>) -> DispatchOpts {
        DispatchOpts {
            config_path,
            args,
            dry_run: false,
        }
    }

    fn opts_dry(config_path: PathBuf) -> DispatchOpts {
        DispatchOpts {
            config_path,
            args: Vec::new(),
            dry_run: true,
        }
    }

    // ── 1. list_in_group: platform filter ────────────────────────────────────

    #[test]
    fn list_in_group_filters_by_platform() {
        let current = Platform::current();
        let other = match current {
            Platform::Linux => "macos",
            Platform::Macos => "linux",
            Platform::Windows => "linux",
        };

        let toml = format!(
            concat!(
                "[[command]]\ngroup = \"menu\"\nname = \"native\"\ndescription = \"native\"\n",
                "steps = [{{ run = [\"echo\", \"hi\"] }}]\n\n",
                "[[command]]\ngroup = \"menu\"\nname = \"foreign\"\ndescription = \"other\"\n",
                "platform = \"{other}\"\n",
                "steps = [{{ run = [\"echo\", \"hi\"] }}]\n",
            ),
            other = other
        );

        let (_dir, path) = write_config(&toml);
        let o = opts(path);

        let listed = list_in_group("menu", &o, false).unwrap();
        assert_eq!(listed.len(), 1, "only native should be listed");
        assert_eq!(listed[0].name, "native");

        let all = list_in_group("menu", &o, true).unwrap();
        assert_eq!(all.len(), 2, "show_all should return both");
        let foreign = all.iter().find(|e| e.name == "foreign").unwrap();
        assert!(foreign.platform_filtered, "foreign should be flagged");
    }

    // ── 2. run_in_group: not found ────────────────────────────────────────────

    #[test]
    fn run_in_group_not_found_returns_command_not_found_error() {
        let toml = concat!(
            "[[command]]\ngroup = \"menu\"\nname = \"wifi\"\ndescription = \"WiFi\"\n",
            "steps = [{ run = [\"echo\", \"hi\"] }]\n",
        );
        let (_dir, path) = write_config(toml);
        let o = opts(path);

        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let err = run_in_group_with(
            "menu",
            "nonexistent",
            &o,
            &process,
            &notifier,
            &mut prompter,
        )
        .unwrap_err();
        match err {
            DispatchError::CommandNotFound {
                group,
                name,
                available_in_group,
            } => {
                assert_eq!(group, "menu");
                assert_eq!(name, "nonexistent");
                assert!(available_in_group.contains(&"wifi".to_owned()));
            }
            other => panic!("expected CommandNotFound, got {other:?}"),
        }
    }

    // ── 3. run_in_group: platform mismatch ───────────────────────────────────

    #[test]
    fn run_in_group_platform_mismatch_on_wrong_platform() {
        let current = Platform::current();
        let other = match current {
            Platform::Linux => "macos",
            Platform::Macos => "linux",
            Platform::Windows => "linux",
        };

        let toml = format!(
            concat!(
                "[[command]]\ngroup = \"menu\"\nname = \"mac-only\"\n",
                "platform = \"{other}\"\n",
                "steps = [{{ run = [\"echo\"] }}]\n",
            ),
            other = other
        );
        let (_dir, path) = write_config(&toml);
        let o = opts(path);

        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let err = run_in_group_with("menu", "mac-only", &o, &process, &notifier, &mut prompter)
            .unwrap_err();
        assert!(
            matches!(err, DispatchError::PlatformMismatch { .. }),
            "expected PlatformMismatch"
        );
    }

    // ── 4. run_in_group: steps execute, arg forwarding works ─────────────────

    #[test]
    fn run_in_group_executes_steps_and_forwards_args() {
        let toml = concat!(
            "[[command]]\ngroup = \"menu\"\nname = \"pass\"\n",
            "steps = [\n",
            "  { run = [\"echo\", \"{0}\"] },\n",
            "  { run = [\"echo\", \"step2\"] },\n",
            "]\n",
        );
        let (_dir, path) = write_config(toml);

        let mut o = opts_with_args(path, vec!["argzero".to_owned()]);
        o.dry_run = false;

        let process = MockProcessExec::new([ok_result("argzero\n"), ok_result("step2\n")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report =
            run_in_group_with("menu", "pass", &o, &process, &notifier, &mut prompter).unwrap();
        assert_eq!(report.steps_run, 2);

        let calls = process.calls.borrow();
        assert_eq!(calls[0].1, vec!["argzero".to_owned()]);
    }

    // ── 5. dry-run: no process calls, plan is non-empty ──────────────────────

    #[test]
    fn dry_run_produces_plan_without_spawning() {
        let toml = concat!(
            "[[command]]\ngroup = \"menu\"\nname = \"demo\"\n",
            "steps = [\n",
            "  { run = [\"echo\", \"hello\"] },\n",
            "  { notify = [\"Title\", \"Body\"] },\n",
            "]\n",
        );
        let (_dir, path) = write_config(toml);
        let o = opts_dry(path);

        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report =
            run_in_group_with("menu", "demo", &o, &process, &notifier, &mut prompter).unwrap();
        assert!(
            report.dry_run_plan.is_some(),
            "dry-run should produce a plan"
        );
        let plan = report.dry_run_plan.unwrap();
        assert!(plan.contains("echo"), "plan should mention the command");
        assert!(plan.contains("notify"), "plan should mention notify step");
        assert!(
            process.calls.borrow().is_empty(),
            "dry-run must not invoke ProcessExec"
        );
    }

    // ── 6. list_groups: returns distinct sorted group names ──────────────────

    #[test]
    fn list_groups_returns_sorted_distinct_groups() {
        let toml = concat!(
            "[[command]]\ngroup = \"battery\"\nname = \"report\"\n",
            "steps = [{ run = [\"echo\"] }]\n\n",
            "[[command]]\ngroup = \"menu\"\nname = \"wifi\"\n",
            "steps = [{ run = [\"echo\"] }]\n\n",
            "[[command]]\ngroup = \"menu\"\nname = \"bluetooth\"\n",
            "steps = [{ run = [\"echo\"] }]\n\n",
            "[[command]]\ngroup = \"kanata\"\nname = \"toggle\"\n",
            "steps = [{ run = [\"echo\"] }]\n",
        );
        let (_dir, path) = write_config(toml);
        let o = opts(path);

        let groups = list_groups(&o).unwrap();
        assert_eq!(groups, vec!["battery", "kanata", "menu"]);
    }

    // ── 7. run_in_group: GroupNotFound for nonexistent group ─────────────────

    #[test]
    fn run_in_group_group_not_found() {
        let toml = concat!(
            "[[command]]\ngroup = \"menu\"\nname = \"wifi\"\n",
            "steps = [{ run = [\"echo\"] }]\n",
        );
        let (_dir, path) = write_config(toml);
        let o = opts(path);

        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let err = run_in_group_with("nonexistent", "foo", &o, &process, &notifier, &mut prompter)
            .unwrap_err();
        match err {
            DispatchError::GroupNotFound { name, available } => {
                assert_eq!(name, "nonexistent");
                assert!(available.contains(&"menu".to_owned()));
            }
            other => panic!("expected GroupNotFound, got {other:?}"),
        }
    }

    // ── 8. list_in_group: GroupNotFound for nonexistent group ────────────────

    #[test]
    fn list_in_group_group_not_found() {
        let toml = concat!(
            "[[command]]\ngroup = \"menu\"\nname = \"wifi\"\n",
            "steps = [{ run = [\"echo\"] }]\n",
        );
        let (_dir, path) = write_config(toml);
        let o = opts(path);

        let err = list_in_group("nonexistent", &o, false).unwrap_err();
        match err {
            DispatchError::GroupNotFound { name, available } => {
                assert_eq!(name, "nonexistent");
                assert!(available.contains(&"menu".to_owned()));
            }
            other => panic!("expected GroupNotFound, got {other:?}"),
        }
    }

    // ── 9. mixed groups: battery/kanata/menu each listable and runnable ───────

    #[test]
    fn mixed_groups_each_listable_and_runnable() {
        let toml = concat!(
            "[[command]]\ngroup = \"battery\"\nname = \"report\"\n",
            "steps = [{ run = [\"echo\", \"battery\"] }]\n\n",
            "[[command]]\ngroup = \"kanata\"\nname = \"toggle\"\n",
            "steps = [{ run = [\"echo\", \"kanata\"] }]\n\n",
            "[[command]]\ngroup = \"menu\"\nname = \"wifi\"\n",
            "steps = [{ run = [\"echo\", \"wifi\"] }]\n",
        );
        let (_dir, path) = write_config(toml);

        for group in &["battery", "kanata", "menu"] {
            let o = opts(path.clone());
            let entries = list_in_group(group, &o, false).unwrap();
            assert_eq!(entries.len(), 1, "group {group} should have 1 entry");
        }

        for (group, name) in &[
            ("battery", "report"),
            ("kanata", "toggle"),
            ("menu", "wifi"),
        ] {
            let o = opts(path.clone());
            let process = MockProcessExec::new([ok_result("")]);
            let notifier = MockNotifier::default();
            let mut prompter = MockPrompter::default();
            run_in_group_with(group, name, &o, &process, &notifier, &mut prompter).unwrap();
        }
    }
}
