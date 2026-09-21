//! `[[hook]]` execution for the phases krypt runs: [`POST_UPDATE`] after
//! `krypt update`, [`POST_SETUP`] after `krypt setup`.

use thiserror::Error;

use crate::config::Config;
use crate::predicate::{DefaultPredicateEnv, default_predicate_evaluator, eval};
use crate::runner::{Context, Notifier, ProcessExec, Prompter, RunnerError, execute_hook};

/// The phase `krypt update` runs, after pulling and linking.
pub const POST_UPDATE: &str = "post-update";

/// The phase `krypt setup` runs, after writing its templates.
pub const POST_SETUP: &str = "post-setup";

/// Every phase some command runs. A hook naming another phase never runs.
pub const PHASES: [&str; 2] = [POST_UPDATE, POST_SETUP];

// ─── Summary & error ─────────────────────────────────────────────────────────

/// Summary of running one phase's hooks.
#[derive(Debug, Default)]
pub struct HookSummary {
    /// Hooks of the phase found in the config.
    pub total: usize,
    /// Hooks successfully run to completion.
    pub ran: usize,
    /// Hooks skipped because `r#if` predicate evaluated false.
    pub skipped_by_predicate: usize,
    /// Hooks skipped because `--skip-hooks` was set.
    pub skipped_by_flag: usize,
    /// Hooks that failed but had `ignore_failure: true`.
    pub failed_ignored: usize,
    /// Set when `--dry-run` was used; `ran`/`skipped` counters stay 0.
    pub dry_run: bool,
}

/// A hook failed and its `ignore_failure` was not set.
#[derive(Debug, Error)]
#[error("hook {name:?} failed: {source}")]
pub struct HookError {
    /// The hook's `name` field.
    pub name: String,
    /// The underlying runner error, boxed to keep the error small.
    #[source]
    pub source: Box<RunnerError>,
}

// ─── Runner ──────────────────────────────────────────────────────────────────

/// Run the hooks of `phase` from `cfg`, notifying through the config's
/// `[meta] notify_backend`.
///
/// With `skip`, none run (all count as skipped by flag); with `dry_run`, the
/// plan is printed and none run. Stops at the first hook that fails without
/// `ignore_failure`.
pub fn run(
    cfg: Option<&Config>,
    phase: &str,
    skip: bool,
    dry_run: bool,
) -> Result<HookSummary, HookError> {
    let notifier =
        crate::notify::AutoNotifier::new(cfg.and_then(|c| c.meta.notify_backend.as_deref()));
    run_with_exec(
        cfg,
        phase,
        skip,
        dry_run,
        &crate::runner::RealProcessExec,
        &notifier,
        &mut crate::runner::RealPrompter,
    )
}

/// [`run`] with its process runner, notifier and prompter injected — the seam
/// tests use.
pub(crate) fn run_with_exec(
    cfg: Option<&Config>,
    phase: &str,
    skip: bool,
    dry_run: bool,
    process: &dyn ProcessExec,
    notifier: &dyn Notifier,
    prompter: &mut dyn Prompter,
) -> Result<HookSummary, HookError> {
    let Some(cfg) = cfg else {
        return Ok(HookSummary::default());
    };

    let hooks: Vec<_> = cfg.hooks.iter().filter(|h| h.when == phase).collect();

    let total = hooks.len();
    let mut summary = HookSummary {
        total,
        dry_run,
        ..Default::default()
    };

    if total == 0 {
        return Ok(summary);
    }

    // Build predicate evaluator with [paths] overrides from config.
    let mut resolver = crate::paths::Resolver::new();
    resolver = resolver.with_overrides(cfg.paths.clone().into_iter().collect());
    let env = DefaultPredicateEnv::with_resolver(resolver);
    let eval_predicate = default_predicate_evaluator(env);

    if skip {
        summary.skipped_by_flag = total;
        return Ok(summary);
    }

    if dry_run {
        // Dry-run: evaluate predicates but don't execute. Print a hook plan.
        println!("hooks (dry-run):");

        // We need a fresh evaluator for each hook's predicate check in dry-run.
        // Re-build it since the closure above moved `env`.
        let mut resolver2 = crate::paths::Resolver::new();
        resolver2 = resolver2.with_overrides(cfg.paths.clone().into_iter().collect());
        let env2 = DefaultPredicateEnv::with_resolver(resolver2);

        for hook in &hooks {
            let predicate_result = if let Some(ref pred) = hook.r#if {
                match eval(pred, &env2) {
                    Ok(true) => "ok",
                    Ok(false) => "would-skip",
                    Err(_) => "predicate-error",
                }
            } else {
                "ok"
            };
            let run_preview = hook.run.first().map(String::as_str).unwrap_or("<empty>");
            println!(
                "  hook {:?}: {} — {}",
                hook.name, predicate_result, run_preview
            );
        }
        // counters stay 0 in dry-run
        return Ok(summary);
    }

    // Live execution.
    let ctx = Context {
        captures: std::collections::BTreeMap::new(),
        args: Vec::new(),
        stdin: None,
    };

    for hook in &hooks {
        // Evaluate predicate first (skip silently if false).
        if hook
            .r#if
            .as_deref()
            .is_some_and(|pred| !eval_predicate(pred, &ctx))
        {
            summary.skipped_by_predicate += 1;
            continue;
        }

        // Execute the hook.
        match execute_hook(hook, process, notifier, prompter, &eval_predicate) {
            Ok(report) if report.steps_failed_ignored > 0 => {
                // The hook's ignore_failure absorbed the error inside the runner.
                tracing::warn!(
                    hook = %hook.name,
                    phase,
                    "hook failed (ignore_failure = true) — continuing"
                );
                summary.failed_ignored += 1;
            }
            Ok(_) => {
                summary.ran += 1;
            }
            Err(e) => {
                // ignore_failure = false (the runner would have returned Err only then).
                return Err(HookError {
                    name: hook.name.clone(),
                    source: Box::new(e),
                });
            }
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{MockNotifier, MockProcessExec, MockPrompter};

    // ── Hook helper tests (no full git setup needed) ─────────────────────────

    fn make_cfg_with_hooks(toml: &str) -> Config {
        toml::from_str(toml).expect("parse config")
    }

    // ── 1. No hooks ──────────────────────────────────────────────────────────

    #[test]
    fn no_hooks_returns_zero_summary() {
        let cfg = make_cfg_with_hooks("");
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let summary = run_with_exec(
            Some(&cfg),
            POST_UPDATE,
            false,
            false,
            &MockProcessExec::new([]),
            &notifier,
            &mut prompter,
        )
        .unwrap();

        assert_eq!(summary.total, 0);
        assert_eq!(summary.ran, 0);
        assert_eq!(summary.skipped_by_predicate, 0);
        assert_eq!(summary.skipped_by_flag, 0);
        assert_eq!(summary.failed_ignored, 0);
        assert!(!summary.dry_run);
    }

    // ── 2. One hook, succeeds ────────────────────────────────────────────────

    #[test]
    fn one_hook_succeeds() {
        use crate::runner::ProcessResult;

        let cfg = make_cfg_with_hooks(
            r#"
[[hook]]
name = "my-hook"
when = "post-update"
run  = ["echo", "hi"]
"#,
        );

        let process = MockProcessExec::new([Ok(ProcessResult {
            status: 0,
            stdout: "hi\n".to_owned(),
            stderr: String::new(),
        })]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let summary = run_with_exec(
            Some(&cfg),
            POST_UPDATE,
            false,
            false,
            &process,
            &notifier,
            &mut prompter,
        )
        .unwrap();

        assert_eq!(summary.total, 1);
        assert_eq!(summary.ran, 1);
        assert_eq!(summary.skipped_by_predicate, 0);
        assert_eq!(summary.failed_ignored, 0);
        // Verify the process was actually called.
        let calls = process.calls.borrow();
        assert_eq!(calls[0].0, "echo");
    }

    // ── 3. Predicate false → skipped_by_predicate ────────────────────────────

    #[test]
    fn hook_with_false_predicate_skipped() {
        // Use `env:KRYPT_TEST_IMPOSSIBLE_VAR_NEVER_SET` — an env var that is
        // guaranteed not to exist on any CI runner, so the predicate is always
        // false on Linux, macOS, and Windows alike.
        let cfg = make_cfg_with_hooks(
            r#"
[[hook]]
name  = "impossible-env"
when  = "post-update"
if    = "env:KRYPT_TEST_IMPOSSIBLE_VAR_NEVER_SET"
run   = ["echo", "nope"]
"#,
        );

        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let summary = run_with_exec(
            Some(&cfg),
            POST_UPDATE,
            false,
            false,
            &process,
            &notifier,
            &mut prompter,
        )
        .unwrap();

        assert_eq!(summary.total, 1);
        assert_eq!(summary.ran, 0);
        assert_eq!(summary.skipped_by_predicate, 1);
        // Process must never have been called.
        assert!(process.calls.borrow().is_empty());
    }

    // ── 4. Hook fails, ignore_failure = true → failed_ignored ────────────────

    #[test]
    fn hook_fails_ignore_failure_true_continues() {
        use crate::runner::ProcessResult;

        let cfg = make_cfg_with_hooks(
            r#"
[[hook]]
name           = "lenient"
when           = "post-update"
run            = ["false-cmd"]
ignore_failure = true
"#,
        );

        let process = MockProcessExec::new([Ok(ProcessResult {
            status: 1,
            stdout: String::new(),
            stderr: "error".to_owned(),
        })]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let result = run_with_exec(
            Some(&cfg),
            POST_UPDATE,
            false,
            false,
            &process,
            &notifier,
            &mut prompter,
        );

        let summary = result.expect("should return Ok despite hook failure");
        assert_eq!(summary.failed_ignored, 1);
        assert_eq!(summary.ran, 0);
    }

    // ── 5. Hook fails, ignore_failure = false → Err(HookError) ───────

    #[test]
    fn hook_fails_ignore_failure_false_returns_err() {
        use crate::runner::ProcessResult;

        let cfg = make_cfg_with_hooks(
            r#"
[[hook]]
name = "strict"
when = "post-update"
run  = ["bad-cmd"]
"#,
        );

        let process = MockProcessExec::new([Ok(ProcessResult {
            status: 1,
            stdout: String::new(),
            stderr: "boom".to_owned(),
        })]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let err = run_with_exec(
            Some(&cfg),
            POST_UPDATE,
            false,
            false,
            &process,
            &notifier,
            &mut prompter,
        )
        .unwrap_err();

        assert!(
            err.name == "strict",
            "expected the failure of \"strict\", got {err:?}"
        );
    }

    // ── 6. --skip-hooks → skipped_by_flag == total ───────────────────────────

    #[test]
    fn skip_hooks_flag_skips_all() {
        let cfg = make_cfg_with_hooks(
            r#"
[[hook]]
name = "h1"
when = "post-update"
run  = ["echo", "one"]

[[hook]]
name = "h2"
when = "post-update"
run  = ["echo", "two"]
"#,
        );

        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let summary = run_with_exec(
            Some(&cfg),
            POST_UPDATE,
            true, // skip = true
            false,
            &process,
            &notifier,
            &mut prompter,
        )
        .unwrap();

        assert_eq!(summary.total, 2);
        assert_eq!(summary.skipped_by_flag, 2);
        assert_eq!(summary.ran, 0);
        // Process must never have been called.
        assert!(process.calls.borrow().is_empty());
    }

    // ── 7. --dry-run → HookSummary.dry_run = true, counters = 0 ─────────────

    #[test]
    fn dry_run_sets_flag_no_execution() {
        let cfg = make_cfg_with_hooks(
            r#"
[[hook]]
name = "deploy"
when = "post-update"
run  = ["echo", "deploying"]
"#,
        );

        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let summary = run_with_exec(
            Some(&cfg),
            POST_UPDATE,
            false,
            true, // dry_run = true
            &process,
            &notifier,
            &mut prompter,
        )
        .unwrap();

        assert!(summary.dry_run);
        assert_eq!(summary.ran, 0);
        assert_eq!(summary.skipped_by_predicate, 0);
        assert_eq!(summary.skipped_by_flag, 0);
        assert_eq!(summary.failed_ignored, 0);
        // No process spawned.
        assert!(process.calls.borrow().is_empty());
    }

    // ── 8. Only the requested phase runs ─────────────────────────────────────

    #[test]
    fn only_hooks_of_the_phase_run() {
        use crate::runner::ProcessResult;

        let cfg = make_cfg_with_hooks(
            r#"
[[hook]]
name = "after-update"
when = "post-update"
run  = ["update-cmd"]

[[hook]]
name = "after-setup"
when = "post-setup"
run  = ["setup-cmd"]
"#,
        );
        let process = MockProcessExec::new([Ok(ProcessResult {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let summary = run_with_exec(
            Some(&cfg),
            POST_SETUP,
            false,
            false,
            &process,
            &notifier,
            &mut prompter,
        )
        .unwrap();

        assert_eq!(summary.total, 1);
        assert_eq!(summary.ran, 1);
        let calls = process.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "setup-cmd");
    }
}
