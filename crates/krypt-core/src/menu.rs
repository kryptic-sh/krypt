//! `krypt menu` — list and run `[[command]] group = "menu"` entries.
//!
//! This module is the core logic behind the `krypt menu` subcommand. It keeps
//! the CLI thin by handling config loading, platform filtering, dry-run
//! formatting, and runner dispatch from here.
//!
//! # Step interpolation note
//!
//! Steps use `{name}` for named captures and `{0}`..`{9}` for positional args
//! forwarded via `krypt menu <name> -- arg0 arg1 ...`. The `${VAR}` syntax
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

/// Options shared by both listing and running menus.
pub struct MenuOpts {
    /// Resolved path to `.krypt.toml`. When `None` the caller supplies it
    /// through `config_path`.
    pub config_path: PathBuf,
    /// Positional args forwarded to step `{0}`..`{9}` placeholders.
    pub args: Vec<String>,
    /// When `true`, print the step plan without executing any process.
    pub dry_run: bool,
}

/// One entry returned by [`list_menus`].
pub struct MenuListEntry {
    /// Menu name (`[[command]] name = ...`).
    pub name: String,
    /// Short description (`[[command]] description = ...`).
    pub description: String,
    /// `true` when the menu's `platform` field doesn't match the current OS
    /// and it was included only because `show_all = true` was requested.
    pub platform_filtered: bool,
    /// The declared platform restriction, if any.
    pub platform: Option<String>,
}

/// Summary of a completed menu run.
#[derive(Debug)]
pub struct MenuReport {
    /// Steps that were executed (including those with ignored failures).
    pub steps_run: usize,
    /// Steps skipped because their `if` predicate returned false.
    pub steps_skipped: usize,
    /// Steps that failed but were ignored via `on_fail = "ignore"` or
    /// `ignore_failure = true`.
    pub steps_failed_ignored: usize,
    /// Rendered dry-run plan, populated when [`MenuOpts::dry_run`] is `true`.
    pub dry_run_plan: Option<String>,
}

/// Everything that can go wrong in the menu subsystem.
///
/// Boxed large variants to stay ≤ 128 bytes on all targets.
#[derive(Debug, Error)]
pub enum MenuError {
    /// The config file could not be loaded or parsed.
    #[error("loading config: {0}")]
    ConfigLoad(#[from] Box<IncludeError>),

    /// No `[[command]] group = "menu"` entry matched the requested name.
    #[error("menu {name:?} not found; available: {}", available.join(", "))]
    MenuNotFound {
        /// The name that was looked up.
        name: String,
        /// All menu names defined in the config (regardless of platform).
        available: Vec<String>,
    },

    /// The requested menu exists but is restricted to a different platform.
    #[error("menu {name:?} is restricted to {required}; current platform is {current}")]
    PlatformMismatch {
        /// Menu name.
        name: String,
        /// Platform the menu requires.
        required: String,
        /// Platform the binary is running on.
        current: String,
    },

    /// The step runner returned an error.
    #[error("runner error: {0}")]
    Runner(#[from] Box<RunnerError>),
}

impl From<IncludeError> for MenuError {
    fn from(e: IncludeError) -> Self {
        MenuError::ConfigLoad(Box::new(e))
    }
}

impl From<RunnerError> for MenuError {
    fn from(e: RunnerError) -> Self {
        MenuError::Runner(Box::new(e))
    }
}

// ─── Listing ──────────────────────────────────────────────────────────────────

/// Return all menus defined in the config.
///
/// When `show_all` is `false` (the default for `krypt menu`), menus whose
/// `platform` field doesn't match [`Platform::current`] are excluded.
/// When `show_all` is `true` they are included with `platform_filtered = true`.
///
/// Results are sorted alphabetically by name.
pub fn list_menus(opts: &MenuOpts, show_all: bool) -> Result<Vec<MenuListEntry>, MenuError> {
    let cfg = load_with_includes(&opts.config_path)?;
    let current = Platform::current();

    let mut entries: Vec<MenuListEntry> = cfg
        .commands
        .into_iter()
        .filter(|cmd| cmd.group == "menu")
        .filter_map(|cmd| {
            let filtered = cmd
                .platform
                .as_deref()
                .map(|p| p != current.as_str())
                .unwrap_or(false);

            if filtered && !show_all {
                return None;
            }

            Some(MenuListEntry {
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

/// Run the named menu using production process/notify/prompt implementations.
pub fn run_menu(name: &str, opts: &MenuOpts) -> Result<MenuReport, MenuError> {
    use crate::notify::{AutoNotifier, NotifyBackend};
    use crate::runner::RealPrompter;

    let notifier = AutoNotifier::with_backend(NotifyBackend::Stderr);
    let mut prompter = RealPrompter;
    run_menu_with(
        name,
        opts,
        &crate::runner::RealProcessExec,
        &notifier,
        &mut prompter,
    )
}

// ─── Running (injectable) ─────────────────────────────────────────────────────

/// Run the named menu with injected dependencies (used by tests and dry-run).
pub fn run_menu_with(
    name: &str,
    opts: &MenuOpts,
    process: &dyn ProcessExec,
    notifier: &dyn Notifier,
    prompter: &mut dyn Prompter,
) -> Result<MenuReport, MenuError> {
    let cfg = load_with_includes(&opts.config_path)?;

    let all_menu_names: Vec<String> = cfg
        .commands
        .iter()
        .filter(|c| c.group == "menu")
        .map(|c| c.name.clone())
        .collect();

    let cmd: &KryptCommand = cfg
        .commands
        .iter()
        .find(|c| c.group == "menu" && c.name == name)
        .ok_or_else(|| MenuError::MenuNotFound {
            name: name.to_owned(),
            available: all_menu_names.clone(),
        })?;

    // Platform gate.
    let current = Platform::current();
    if let Some(ref required) = cmd.platform
        && required.as_str() != current.as_str()
    {
        return Err(MenuError::PlatformMismatch {
            name: name.to_owned(),
            required: required.clone(),
            current: current.to_string(),
        });
    }

    if opts.dry_run {
        return dry_run_plan(cmd, opts);
    }

    let env = DefaultPredicateEnv::new();
    let eval = default_predicate_evaluator(env);

    let report: RunReport =
        execute_command(cmd, opts.args.clone(), process, notifier, prompter, &eval)?;

    Ok(MenuReport {
        steps_run: report.steps_run,
        steps_skipped: report.steps_skipped_by_predicate,
        steps_failed_ignored: report.steps_failed_ignored,
        dry_run_plan: None,
    })
}

// ─── Dry-run ──────────────────────────────────────────────────────────────────

/// Build a dry-run plan string for the given command.
///
/// Predicate evaluation IS performed (predicates are pure; no side effects).
/// Process execution and notifications are stubbed — nothing is spawned.
fn dry_run_plan(cmd: &KryptCommand, opts: &MenuOpts) -> Result<MenuReport, MenuError> {
    use std::collections::BTreeMap;

    use crate::predicate::{MockPredicateEnv, eval};
    use crate::runner::interpolate;

    let env = MockPredicateEnv::new(Platform::current());

    let ctx = Context {
        captures: BTreeMap::new(),
        args: opts.args.clone(),
        stdin: None,
    };

    let n = cmd.steps.len();
    let mut plan = format!("dry-run: menu {:?} ({n} steps)\n", cmd.name);

    for (i, step) in cmd.steps.iter().enumerate() {
        let num = i + 1;

        // Evaluate predicate.
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

        // Format the step.
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

    Ok(MenuReport {
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

    /// Write a `.krypt.toml` in a temp dir and return (TempDir, path to toml).
    fn write_config(contents: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".krypt.toml");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    fn opts(config_path: PathBuf) -> MenuOpts {
        MenuOpts {
            config_path,
            args: Vec::new(),
            dry_run: false,
        }
    }

    fn opts_with_args(config_path: PathBuf, args: Vec<String>) -> MenuOpts {
        MenuOpts {
            config_path,
            args,
            dry_run: false,
        }
    }

    fn opts_dry(config_path: PathBuf) -> MenuOpts {
        MenuOpts {
            config_path,
            args: Vec::new(),
            dry_run: true,
        }
    }

    // ── 1. list_menus: platform filter ───────────────────────────────────────

    #[test]
    fn list_menus_filters_by_platform() {
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

        let listed = list_menus(&o, false).unwrap();
        assert_eq!(listed.len(), 1, "only native should be listed");
        assert_eq!(listed[0].name, "native");

        let all = list_menus(&o, true).unwrap();
        assert_eq!(all.len(), 2, "show_all should return both");
        let foreign = all.iter().find(|e| e.name == "foreign").unwrap();
        assert!(foreign.platform_filtered, "foreign should be flagged");
    }

    // ── 2. run_menu: not found ────────────────────────────────────────────────

    #[test]
    fn run_menu_not_found_returns_menu_not_found_error() {
        let toml = concat!(
            "[[command]]\ngroup = \"menu\"\nname = \"wifi\"\ndescription = \"WiFi\"\n",
            "steps = [{ run = [\"echo\", \"hi\"] }]\n",
        );
        let (_dir, path) = write_config(toml);
        let o = opts(path);

        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let err = run_menu_with("nonexistent", &o, &process, &notifier, &mut prompter).unwrap_err();
        match err {
            MenuError::MenuNotFound { name, available } => {
                assert_eq!(name, "nonexistent");
                assert!(available.contains(&"wifi".to_owned()));
            }
            other => panic!("expected MenuNotFound, got {other:?}"),
        }
    }

    // ── 3. run_menu: platform mismatch ────────────────────────────────────────

    #[test]
    fn run_menu_platform_mismatch_on_wrong_platform() {
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

        let err = run_menu_with("mac-only", &o, &process, &notifier, &mut prompter).unwrap_err();
        assert!(
            matches!(err, MenuError::PlatformMismatch { .. }),
            "expected PlatformMismatch"
        );
    }

    // ── 4. run_menu: steps execute, arg forwarding works ──────────────────────

    #[test]
    fn run_menu_executes_steps_and_forwards_args() {
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

        let report = run_menu_with("pass", &o, &process, &notifier, &mut prompter).unwrap();
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

        let report = run_menu_with("demo", &o, &process, &notifier, &mut prompter).unwrap();
        assert!(
            report.dry_run_plan.is_some(),
            "dry-run should produce a plan"
        );
        let plan = report.dry_run_plan.unwrap();
        assert!(plan.contains("echo"), "plan should mention the command");
        assert!(plan.contains("notify"), "plan should mention notify step");
        // No process was spawned.
        assert!(
            process.calls.borrow().is_empty(),
            "dry-run must not invoke ProcessExec"
        );
    }
}
