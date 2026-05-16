//! Step runner — the engine behind `[[command]]` and `[[hook]]` execution.
//!
//! This module executes a [`Vec<Step>`] declaratively, with injected
//! dependencies for process execution, desktop notification, and user
//! prompting. Every downstream feature that needs to run user-defined steps
//! (post-update hooks #43, `krypt menu` #25, `krypt <group> <name>` #25)
//! delegates here.
//!
//! # Predicate evaluator (stub)
//!
//! The `eval_predicate` parameter is a stub for issue #24. All predicate
//! strings currently evaluate to `true` (no-op). Issue #24 will implement
//! the real grammar: `command_exists:foo`, `platform:linux`, `env:FOO=bar`,
//! `!file_exists:/path`, and so on. Tests that need predicate gating supply
//! their own closure.
//!
//! # on_fail semantics
//!
//! | value            | behaviour                                         |
//! |-----------------|---------------------------------------------------|
//! | `abort` (default) | bubble up `RunnerError::NonZeroExit`            |
//! | `ignore`          | swallow, increment `steps_failed_ignored`        |
//! | `notify`          | call Notifier with failure details, then abort   |
//! | `prompt`          | ask Prompter; `true` → ignore, `false` → abort  |
//!
//! `ignore_failure = true` is a shortcut alias for `on_fail = "ignore"` and
//! wins over a conflicting `on_fail = "abort"` if both are present (with a
//! `tracing::warn!`).
//!
//! # Cross-platform note
//!
//! [`RealProcessExec`] wraps `std::process::Command` directly. No shell is
//! injected — `run = ["echo", "hi"]` must reference a real binary in `PATH`.
//! Shell builtins (e.g. `echo` on Windows outside Git Bash) are the caller's
//! responsibility. Path-containing args are passed through unchanged.

#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::process::Command as StdCommand;
use std::process::Stdio;

use thiserror::Error;

use crate::config::{Command as KryptCommand, Hook, Step};

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Anything that can go wrong while running steps.
#[derive(Debug, Error)]
pub enum RunnerError {
    /// A step has an invalid shape (e.g. `run` and `pipe` both set).
    #[error("step {step_index}: invalid shape — {reason}")]
    StepShape {
        /// Zero-based index of the offending step.
        step_index: usize,
        /// Human-readable description of the problem.
        reason: &'static str,
    },

    /// The underlying process could not be spawned.
    #[error("step {step_index}: process I/O error — {source}")]
    Process {
        /// Zero-based index of the step.
        step_index: usize,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// The process exited with a non-zero status and `on_fail` was `abort`.
    #[error("step {step_index}: exited with status {status} — {stderr}")]
    NonZeroExit {
        /// Zero-based index of the step.
        step_index: usize,
        /// The exit status code.
        status: i32,
        /// Captured stderr output.
        stderr: String,
    },

    /// The notifier returned an error.
    #[error("step {step_index}: notify error — {source}")]
    Notify {
        /// Zero-based index of the step.
        step_index: usize,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// The prompter returned an I/O error.
    #[error("step {step_index}: prompt I/O error — {source}")]
    PromptIo {
        /// Zero-based index of the step.
        step_index: usize,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Variable interpolation produced an error (reserved for future use).
    #[error("step {step_index}: interpolation error — {reason}")]
    Interpolation {
        /// Zero-based index of the step.
        step_index: usize,
        /// Description of why interpolation failed.
        reason: String,
    },
}

// ─── Traits ──────────────────────────────────────────────────────────────────

/// Outcome of a single process execution.
pub struct ProcessResult {
    /// Exit status code (0 = success).
    pub status: i32,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Abstraction over process spawning, allowing test mocks.
pub trait ProcessExec {
    /// Execute `cmd` with `args`, optionally piping `stdin` to the process.
    ///
    /// Returns [`ProcessResult`] even on non-zero exit — the runner decides
    /// whether a non-zero status is an error.
    fn exec(
        &self,
        cmd: &str,
        args: &[String],
        stdin: Option<&str>,
    ) -> Result<ProcessResult, io::Error>;
}

/// Abstraction over desktop notification, allowing test mocks.
///
/// # Implementation note
///
/// [`RealNotifier`] currently prints to stderr (`notice: <title> — <body>`).
/// Real desktop notification (libnotify on Linux, `osascript` on macOS,
/// Windows toast via `notify-rust`) is tracked in issue #26.
pub trait Notifier {
    /// Send a desktop notification with the given title and body.
    fn notify(&self, title: &str, body: &str) -> Result<(), io::Error>;
}

/// Abstraction over interactive prompting, allowing test mocks.
pub trait Prompter {
    /// Ask the user whether to continue after a step fails with
    /// `on_fail = "prompt"`. Returns `true` to continue, `false` to abort.
    fn ask_continue(&mut self, step_description: &str, error: &str) -> Result<bool, io::Error>;
}

// ─── Context ─────────────────────────────────────────────────────────────────

/// Execution context threaded through all steps.
///
/// Captures accumulated during earlier steps are stored in [`captures`] and
/// become available for `{name}` interpolation in later steps.
pub struct Context {
    /// Named captures accumulated from earlier `capture =` steps.
    pub captures: BTreeMap<String, String>,
    /// Positional arguments passed to the command, indexed as `{0}`..`{9}`.
    pub args: Vec<String>,
    /// Optional pipeline input available as `{stdin}`.
    pub stdin: Option<String>,
}

// ─── Report ──────────────────────────────────────────────────────────────────

/// Summary of a completed step sequence.
#[derive(Debug, Default)]
pub struct RunReport {
    /// Number of steps that were executed (including ignored failures).
    pub steps_run: usize,
    /// Number of steps skipped because their `if` predicate returned false.
    pub steps_skipped_by_predicate: usize,
    /// Number of steps that failed but were ignored via `on_fail = "ignore"`
    /// or `ignore_failure = true`.
    pub steps_failed_ignored: usize,
    /// All captures accumulated across the run.
    pub final_captures: BTreeMap<String, String>,
}

// ─── Real implementations ────────────────────────────────────────────────────

/// Production process executor using [`std::process::Command`].
///
/// No shell wrapping is applied. `run = ["echo", "hi"]` must reference a real
/// binary in `PATH`. Shell builtins (e.g. `echo` on Windows outside Git Bash)
/// are the caller's responsibility.
pub struct RealProcessExec;

impl ProcessExec for RealProcessExec {
    fn exec(
        &self,
        cmd: &str,
        args: &[String],
        stdin: Option<&str>,
    ) -> Result<ProcessResult, io::Error> {
        let mut child = StdCommand::new(cmd);
        child.args(args);
        child.stdout(Stdio::piped());
        child.stderr(Stdio::piped());
        if stdin.is_some() {
            child.stdin(Stdio::piped());
        } else {
            child.stdin(Stdio::null());
        }

        let mut handle = child.spawn()?;

        if let Some(input) = stdin {
            use io::Write as _;
            let stdin_handle = handle.stdin.take().expect("stdin piped");
            let mut writer = io::BufWriter::new(stdin_handle);
            writer.write_all(input.as_bytes())?;
        }

        let output = handle.wait_with_output()?;
        Ok(ProcessResult {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Stub notifier that prints to stderr.
///
/// Real desktop notification (libnotify on Linux, `osascript` on macOS,
/// Windows toast) is tracked in issue #26.
pub struct RealNotifier;

impl Notifier for RealNotifier {
    fn notify(&self, title: &str, body: &str) -> Result<(), io::Error> {
        eprintln!("notice: {title} — {body}");
        Ok(())
    }
}

/// Interactive prompter backed by stdin readline.
///
/// `dialoguer::Confirm` integration is tracked alongside issue #24; for now
/// we read a line and treat `y`/`Y` as "continue".
pub struct RealPrompter;

impl Prompter for RealPrompter {
    fn ask_continue(&mut self, step_description: &str, error: &str) -> Result<bool, io::Error> {
        use io::BufRead as _;
        eprintln!("Step failed: {step_description}");
        eprintln!("Error: {error}");
        eprint!("Continue? [y/N] ");
        let stdin = io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        Ok(matches!(line.trim(), "y" | "Y"))
    }
}

// ─── Mock implementations (test helpers) ─────────────────────────────────────

/// Mock process executor with scripted responses.
///
/// Responses are consumed in FIFO order. The recorded calls can be inspected
/// via [`MockProcessExec::calls`] after the run.
pub struct MockProcessExec {
    /// Scripted responses, consumed in order. Panics if exhausted before the
    /// test ends.
    responses: RefCell<VecDeque<Result<ProcessResult, io::Error>>>,
    /// Record of `(cmd, args, stdin)` tuples, in invocation order.
    #[allow(clippy::type_complexity)]
    pub calls: RefCell<Vec<(String, Vec<String>, Option<String>)>>,
}

impl MockProcessExec {
    /// Create a new mock with the given scripted responses.
    pub fn new(responses: impl IntoIterator<Item = Result<ProcessResult, io::Error>>) -> Self {
        Self {
            responses: RefCell::new(responses.into_iter().collect()),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// Return a snapshot of recorded calls, cloned out.
    pub fn recorded_calls(&self) -> Vec<(String, Vec<String>, Option<String>)> {
        self.calls.borrow().clone()
    }
}

impl ProcessExec for MockProcessExec {
    fn exec(
        &self,
        cmd: &str,
        args: &[String],
        stdin: Option<&str>,
    ) -> Result<ProcessResult, io::Error> {
        self.calls
            .borrow_mut()
            .push((cmd.to_owned(), args.to_vec(), stdin.map(ToOwned::to_owned)));
        self.responses
            .borrow_mut()
            .pop_front()
            .expect("MockProcessExec: no more scripted responses")
    }
}

/// Mock notifier that records calls.
#[derive(Default)]
pub struct MockNotifier {
    /// Recorded `(title, body)` pairs, in invocation order.
    pub calls: RefCell<Vec<(String, String)>>,
}

impl Notifier for MockNotifier {
    fn notify(&self, title: &str, body: &str) -> Result<(), io::Error> {
        self.calls
            .borrow_mut()
            .push((title.to_owned(), body.to_owned()));
        Ok(())
    }
}

/// Mock prompter with scripted boolean responses.
#[derive(Default)]
pub struct MockPrompter {
    /// Scripted responses, consumed in order.
    pub responses: VecDeque<bool>,
}

impl MockPrompter {
    /// Create a new mock from an iterator of boolean responses.
    pub fn new(responses: impl IntoIterator<Item = bool>) -> Self {
        Self {
            responses: responses.into_iter().collect(),
        }
    }
}

impl Prompter for MockPrompter {
    fn ask_continue(&mut self, _step_description: &str, _error: &str) -> Result<bool, io::Error> {
        Ok(self
            .responses
            .pop_front()
            .expect("MockPrompter: no more scripted responses"))
    }
}

// ─── Interpolation ───────────────────────────────────────────────────────────

/// Interpolate `{name}`, `{0}`..`{9}`, `{stdin}`, and `{{`/`}}` escapes.
///
/// Unknown `{xyz}` placeholders are left as-is with a `tracing::warn!`. This
/// is intentional: a step that references `{1}` but received no positional
/// args should degrade gracefully rather than hard-error.
pub fn interpolate(template: &str, ctx: &Context) -> String {
    let mut out = String::with_capacity(template.len());
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '{' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                // Escaped `{{` → literal `{`
                out.push('{');
                i += 2;
                continue;
            }
            // Find the closing `}`
            if let Some(close) = chars[i + 1..].iter().position(|&c| c == '}') {
                let key: String = chars[i + 1..i + 1 + close].iter().collect();
                i += 2 + close; // skip past `}`

                if key.is_empty() {
                    out.push_str("{}");
                } else if key == "stdin" {
                    out.push_str(ctx.stdin.as_deref().unwrap_or(""));
                } else if let Ok(idx) = key.parse::<usize>() {
                    out.push_str(ctx.args.get(idx).map(String::as_str).unwrap_or(""));
                } else if let Some(val) = ctx.captures.get(&key) {
                    out.push_str(val);
                } else {
                    tracing::warn!(key, "unknown interpolation variable — leaving literal");
                    out.push('{');
                    out.push_str(&key);
                    out.push('}');
                }
                continue;
            }
            // No closing brace found — emit literal `{`
            out.push(chars[i]);
            i += 1;
        } else if chars[i] == '}' && i + 1 < chars.len() && chars[i + 1] == '}' {
            // Escaped `}}` → literal `}`
            out.push('}');
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }

    out
}

// ─── Core engine ─────────────────────────────────────────────────────────────

/// Execute a slice of steps with the given execution context and injected
/// dependencies.
///
/// Returns a [`RunReport`] on success. If any step fails with `on_fail =
/// "abort"` (the default), execution stops and the error is returned.
pub fn execute_steps(
    steps: &[Step],
    mut ctx: Context,
    process: &dyn ProcessExec,
    notifier: &dyn Notifier,
    prompter: &mut dyn Prompter,
    eval_predicate: &dyn Fn(&str, &Context) -> bool,
) -> Result<RunReport, RunnerError> {
    let mut report = RunReport::default();

    for (idx, step) in steps.iter().enumerate() {
        // ── Shape validation ────────────────────────────────────────────────
        let kind_count =
            step.run.is_some() as u8 + step.pipe.is_some() as u8 + step.notify.is_some() as u8;

        if kind_count == 0 {
            return Err(RunnerError::StepShape {
                step_index: idx,
                reason: "exactly one of run / pipe / notify must be set; none are",
            });
        }
        if kind_count > 1 {
            return Err(RunnerError::StepShape {
                step_index: idx,
                reason: "exactly one of run / pipe / notify must be set; multiple are",
            });
        }

        // ── Conflicting ignore_failure + on_fail warning ────────────────────
        if step.ignore_failure && step.on_fail.as_deref().is_some_and(|of| of != "ignore") {
            tracing::warn!(
                step_index = idx,
                on_fail = %step.on_fail.as_deref().unwrap_or(""),
                "ignore_failure = true conflicts with on_fail; ignore_failure wins",
            );
        }

        // ── Predicate gating ────────────────────────────────────────────────
        if let Some(ref predicate) = step.r#if
            && !eval_predicate(predicate, &ctx)
        {
            report.steps_skipped_by_predicate += 1;
            continue;
        }

        // ── Dispatch by kind ────────────────────────────────────────────────
        if let Some(ref args_raw) = step.run.clone() {
            run_process_step(
                idx,
                args_raw,
                None,
                step,
                &mut ctx,
                process,
                notifier,
                prompter,
                &mut report,
            )?;
        } else if let Some(ref args_raw) = step.pipe.clone() {
            let stdin_val = if let Some(ref input_tmpl) = step.input {
                interpolate(input_tmpl, &ctx)
            } else {
                ctx.stdin.clone().unwrap_or_default()
            };
            run_process_step(
                idx,
                args_raw,
                Some(&stdin_val),
                step,
                &mut ctx,
                process,
                notifier,
                prompter,
                &mut report,
            )?;
        } else if let Some(ref parts_raw) = step.notify.clone() {
            // Validate: notify steps must not have `capture`.
            if step.capture.is_some() {
                return Err(RunnerError::StepShape {
                    step_index: idx,
                    reason: "notify steps cannot use capture",
                });
            }

            let title = interpolate(parts_raw.first().map(String::as_str).unwrap_or(""), &ctx);
            let body = interpolate(parts_raw.get(1).map(String::as_str).unwrap_or(""), &ctx);

            report.steps_run += 1;
            if let Err(e) = notifier.notify(&title, &body) {
                handle_failure(
                    idx,
                    step,
                    RunnerError::Notify {
                        step_index: idx,
                        source: e,
                    },
                    notifier,
                    prompter,
                    &mut report,
                )?;
            }
        }
    }

    report.final_captures = ctx.captures;
    Ok(report)
}

/// Execute a [`KryptCommand`] as a step sequence.
pub fn execute_command(
    cmd: &KryptCommand,
    args: Vec<String>,
    process: &dyn ProcessExec,
    notifier: &dyn Notifier,
    prompter: &mut dyn Prompter,
    eval_predicate: &dyn Fn(&str, &Context) -> bool,
) -> Result<RunReport, RunnerError> {
    let ctx = Context {
        captures: BTreeMap::new(),
        args,
        stdin: None,
    };
    execute_steps(&cmd.steps, ctx, process, notifier, prompter, eval_predicate)
}

/// Execute a [`Hook`] as a single-step equivalent.
///
/// The hook's `run` field maps to a `run`-kind step with the hook's
/// `ignore_failure` and `r#if` applied.
pub fn execute_hook(
    hook: &Hook,
    process: &dyn ProcessExec,
    notifier: &dyn Notifier,
    prompter: &mut dyn Prompter,
    eval_predicate: &dyn Fn(&str, &Context) -> bool,
) -> Result<RunReport, RunnerError> {
    let step = Step {
        run: Some(hook.run.clone()),
        pipe: None,
        notify: None,
        capture: None,
        input: None,
        r#if: hook.r#if.clone(),
        on_fail: None,
        ignore_failure: hook.ignore_failure,
    };
    let ctx = Context {
        captures: BTreeMap::new(),
        args: Vec::new(),
        stdin: None,
    };
    execute_steps(&[step], ctx, process, notifier, prompter, eval_predicate)
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// Run a `run`- or `pipe`-kind step, handling capture and on_fail.
#[allow(clippy::too_many_arguments)]
fn run_process_step(
    idx: usize,
    args_raw: &[String],
    stdin: Option<&str>,
    step: &Step,
    ctx: &mut Context,
    process: &dyn ProcessExec,
    notifier: &dyn Notifier,
    prompter: &mut dyn Prompter,
    report: &mut RunReport,
) -> Result<(), RunnerError> {
    if args_raw.is_empty() {
        return Err(RunnerError::StepShape {
            step_index: idx,
            reason: "run/pipe args list is empty",
        });
    }

    let interpolated: Vec<String> = args_raw.iter().map(|a| interpolate(a, ctx)).collect();
    let (cmd, rest) = interpolated.split_first().expect("checked non-empty above");

    report.steps_run += 1;

    let result = process
        .exec(cmd, rest, stdin)
        .map_err(|e| RunnerError::Process {
            step_index: idx,
            source: e,
        })?;

    if result.status != 0 {
        let err = RunnerError::NonZeroExit {
            step_index: idx,
            status: result.status,
            stderr: result.stderr.clone(),
        };
        return handle_failure(idx, step, err, notifier, prompter, report);
    }

    // Capture stdout if requested.
    if let Some(ref var) = step.capture {
        let value = result.stdout.trim_end_matches('\n').to_owned();
        ctx.captures.insert(var.clone(), value);
    }

    Ok(())
}

/// Apply `on_fail` semantics for a failed step.
///
/// Returns `Ok(())` if the failure is absorbed; returns `Err(err)` to abort.
fn handle_failure(
    idx: usize,
    step: &Step,
    err: RunnerError,
    notifier: &dyn Notifier,
    prompter: &mut dyn Prompter,
    report: &mut RunReport,
) -> Result<(), RunnerError> {
    // ignore_failure wins over on_fail if set.
    if step.ignore_failure {
        report.steps_failed_ignored += 1;
        return Ok(());
    }

    let mode = step.on_fail.as_deref().unwrap_or("abort");

    match mode {
        "ignore" => {
            report.steps_failed_ignored += 1;
            Ok(())
        }
        "notify" => {
            let desc = err.to_string();
            let _ = notifier.notify("krypt step failed", &desc);
            Err(err)
        }
        "prompt" => {
            let desc = format!("step {idx}");
            let err_str = err.to_string();
            match prompter.ask_continue(&desc, &err_str) {
                Ok(true) => {
                    report.steps_failed_ignored += 1;
                    Ok(())
                }
                Ok(false) => Err(err),
                Err(e) => Err(RunnerError::PromptIo {
                    step_index: idx,
                    source: e,
                }),
            }
        }
        // "abort" or any unknown value
        _ => Err(err),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_result(stdout: &str) -> Result<ProcessResult, io::Error> {
        Ok(ProcessResult {
            status: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }

    fn fail_result(status: i32, stderr: &str) -> Result<ProcessResult, io::Error> {
        Ok(ProcessResult {
            status,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        })
    }

    fn noop_predicate(_: &str, _: &Context) -> bool {
        true
    }

    fn step_run(args: &[&str]) -> Step {
        Step {
            run: Some(args.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }
    }

    fn step_notify(title: &str, body: &str) -> Step {
        Step {
            notify: Some(vec![title.to_owned(), body.to_owned()]),
            ..Default::default()
        }
    }

    fn empty_ctx() -> Context {
        Context {
            captures: BTreeMap::new(),
            args: Vec::new(),
            stdin: None,
        }
    }

    // ── 1. Acceptance: 5-step fixture ────────────────────────────────────────

    #[test]
    fn acceptance_five_step_fixture() {
        // Step 0: run echo "hello" → capture out
        // Step 1: run echo "{out}-world" → capture out2
        // Step 2: pipe wc -c with input={out2} → capture len
        // Step 3: notify "title" "{len} bytes"
        // Step 4: run printf {0} (positional)
        let steps = vec![
            Step {
                run: Some(vec!["echo".to_owned(), "hello".to_owned()]),
                capture: Some("out".to_owned()),
                ..Default::default()
            },
            Step {
                run: Some(vec!["echo".to_owned(), "{out}-world".to_owned()]),
                capture: Some("out2".to_owned()),
                ..Default::default()
            },
            Step {
                pipe: Some(vec!["wc".to_owned(), "-c".to_owned()]),
                input: Some("{out2}".to_owned()),
                capture: Some("len".to_owned()),
                ..Default::default()
            },
            step_notify("title", "{len} bytes"),
            Step {
                run: Some(vec!["printf".to_owned(), "{0}".to_owned()]),
                ..Default::default()
            },
        ];

        let process = MockProcessExec::new([
            ok_result("hello\n"),       // step 0
            ok_result("hello-world\n"), // step 1
            ok_result("12\n"),          // step 2: wc -c
            ok_result("ok\n"),          // step 4: printf
        ]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let ctx = Context {
            captures: BTreeMap::new(),
            args: vec!["argzero".to_owned()],
            stdin: None,
        };

        let report = execute_steps(
            &steps,
            ctx,
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap();

        assert_eq!(report.steps_run, 5); // 4 process steps + 1 notify step
        assert_eq!(report.steps_skipped_by_predicate, 0);
        assert_eq!(report.steps_failed_ignored, 0);
        assert_eq!(report.final_captures["out"], "hello");
        assert_eq!(report.final_captures["out2"], "hello-world");
        assert_eq!(report.final_captures["len"], "12");

        // Verify process was called correctly
        let pcalls = process.calls.borrow();
        assert_eq!(pcalls[0].0, "echo");
        assert_eq!(pcalls[0].1, &["hello".to_owned()]);
        assert_eq!(pcalls[1].1, &["hello-world".to_owned()]);
        // step 2: pipe with interpolated input
        assert_eq!(pcalls[2].0, "wc");
        assert_eq!(pcalls[2].2.as_deref(), Some("hello-world"));
        // step 4: positional arg
        assert_eq!(pcalls[3].1, &["argzero".to_owned()]);
        drop(pcalls);
        // step 3: notify
        let ncalls = notifier.calls.borrow();
        assert_eq!(ncalls[0].0, "title");
        assert_eq!(ncalls[0].1, "12 bytes");
    }

    // ── 2. Variable interpolation ─────────────────────────────────────────────

    #[test]
    fn interpolate_named_capture() {
        let ctx = Context {
            captures: [("foo".to_owned(), "bar".to_owned())].into(),
            args: Vec::new(),
            stdin: None,
        };
        assert_eq!(interpolate("{foo}", &ctx), "bar");
    }

    #[test]
    fn interpolate_positional() {
        let ctx = Context {
            captures: BTreeMap::new(),
            args: vec!["first".to_owned(), "second".to_owned()],
            stdin: None,
        };
        assert_eq!(interpolate("{0} {1}", &ctx), "first second");
    }

    #[test]
    fn interpolate_stdin() {
        let ctx = Context {
            captures: BTreeMap::new(),
            args: Vec::new(),
            stdin: Some("pipe-input".to_owned()),
        };
        assert_eq!(interpolate("{stdin}", &ctx), "pipe-input");
    }

    #[test]
    fn interpolate_escaped_braces() {
        let ctx = empty_ctx();
        assert_eq!(interpolate("{{literal}}", &ctx), "{literal}");
        assert_eq!(interpolate("{{}}", &ctx), "{}");
    }

    #[test]
    fn interpolate_unknown_var_left_literal() {
        let ctx = empty_ctx();
        // Unknown {xyz} → left as {xyz}, no panic.
        assert_eq!(interpolate("{xyz}", &ctx), "{xyz}");
    }

    #[test]
    fn interpolate_out_of_range_positional_empty() {
        let ctx = Context {
            captures: BTreeMap::new(),
            args: vec!["only-one".to_owned()],
            stdin: None,
        };
        assert_eq!(interpolate("{5}", &ctx), "");
    }

    // ── 3. Mutual exclusion ───────────────────────────────────────────────────

    #[test]
    fn mutual_exclusion_run_and_pipe_errors() {
        let step = Step {
            run: Some(vec!["echo".to_owned()]),
            pipe: Some(vec!["cat".to_owned()]),
            ..Default::default()
        };
        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let err = execute_steps(
            &[step],
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap_err();

        assert!(matches!(err, RunnerError::StepShape { step_index: 0, .. }));
    }

    #[test]
    fn no_kind_set_errors() {
        let step = Step::default();
        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let err = execute_steps(
            &[step],
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap_err();

        assert!(matches!(err, RunnerError::StepShape { step_index: 0, .. }));
    }

    // ── 4. Predicate gating ───────────────────────────────────────────────────

    #[test]
    fn predicate_false_skips_step() {
        let step = Step {
            run: Some(vec!["echo".to_owned(), "should-not-run".to_owned()]),
            r#if: Some("platform:windows".to_owned()),
            capture: Some("out".to_owned()),
            ..Default::default()
        };
        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report = execute_steps(
            &[step],
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &|_pred, _ctx| false, // always false
        )
        .unwrap();

        assert_eq!(report.steps_run, 0);
        assert_eq!(report.steps_skipped_by_predicate, 1);
        assert!(!report.final_captures.contains_key("out"));
        assert!(process.calls.borrow().is_empty());
    }

    // ── 5. on_fail = "abort" (default) ───────────────────────────────────────

    #[test]
    fn on_fail_abort_default_stops_execution() {
        let steps = vec![
            step_run(&["bad-cmd"]),
            step_run(&["echo", "should-not-run"]),
        ];
        let process = MockProcessExec::new([fail_result(1, "bad exit")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let err = execute_steps(
            &steps,
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap_err();

        assert!(matches!(
            err,
            RunnerError::NonZeroExit {
                step_index: 0,
                status: 1,
                ..
            }
        ));
        // Second step was never executed.
        assert_eq!(process.calls.borrow().len(), 1);
    }

    // ── 6. on_fail = "ignore" ────────────────────────────────────────────────

    #[test]
    fn on_fail_ignore_continues_after_failure() {
        let steps = vec![
            Step {
                run: Some(vec!["bad-cmd".to_owned()]),
                on_fail: Some("ignore".to_owned()),
                ..Default::default()
            },
            step_run(&["echo", "continued"]),
        ];
        let process = MockProcessExec::new([fail_result(1, "err"), ok_result("continued\n")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report = execute_steps(
            &steps,
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap();

        assert_eq!(report.steps_run, 2);
        assert_eq!(report.steps_failed_ignored, 1);
    }

    // ── 7. ignore_failure = true ──────────────────────────────────────────────

    #[test]
    fn ignore_failure_true_same_as_on_fail_ignore() {
        let steps = vec![
            Step {
                run: Some(vec!["bad-cmd".to_owned()]),
                ignore_failure: true,
                ..Default::default()
            },
            step_run(&["echo", "next"]),
        ];
        let process = MockProcessExec::new([fail_result(2, "oops"), ok_result("next\n")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report = execute_steps(
            &steps,
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap();

        assert_eq!(report.steps_failed_ignored, 1);
        assert_eq!(report.steps_run, 2);
    }

    // ── 8. on_fail = "notify" ────────────────────────────────────────────────

    #[test]
    fn on_fail_notify_calls_notifier_then_aborts() {
        let step = Step {
            run: Some(vec!["bad-cmd".to_owned()]),
            on_fail: Some("notify".to_owned()),
            ..Default::default()
        };
        let process = MockProcessExec::new([fail_result(1, "boom")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let err = execute_steps(
            &[step],
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap_err();

        assert!(matches!(err, RunnerError::NonZeroExit { .. }));
        let ncalls = notifier.calls.borrow();
        assert_eq!(ncalls.len(), 1);
        assert_eq!(ncalls[0].0, "krypt step failed");
    }

    // ── 9. on_fail = "prompt" ────────────────────────────────────────────────

    #[test]
    fn on_fail_prompt_true_treats_as_ignore() {
        let steps = vec![
            Step {
                run: Some(vec!["bad-cmd".to_owned()]),
                on_fail: Some("prompt".to_owned()),
                ..Default::default()
            },
            step_run(&["echo", "after"]),
        ];
        let process = MockProcessExec::new([fail_result(1, "err"), ok_result("after\n")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::new([true]); // user says "continue"

        let report = execute_steps(
            &steps,
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap();

        assert_eq!(report.steps_failed_ignored, 1);
        assert_eq!(report.steps_run, 2);
    }

    #[test]
    fn on_fail_prompt_false_aborts() {
        let step = Step {
            run: Some(vec!["bad-cmd".to_owned()]),
            on_fail: Some("prompt".to_owned()),
            ..Default::default()
        };
        let process = MockProcessExec::new([fail_result(1, "err")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::new([false]); // user says "abort"

        let err = execute_steps(
            &[step],
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap_err();

        assert!(matches!(err, RunnerError::NonZeroExit { .. }));
    }

    // ── 10. notify step ───────────────────────────────────────────────────────

    #[test]
    fn notify_step_calls_notifier_with_interpolated_values() {
        let step = Step {
            notify: Some(vec!["My Title".to_owned(), "{msg} sent".to_owned()]),
            ..Default::default()
        };
        let ctx = Context {
            captures: [("msg".to_owned(), "hello".to_owned())].into(),
            args: Vec::new(),
            stdin: None,
        };
        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        execute_steps(
            &[step],
            ctx,
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap();

        let ncalls = notifier.calls.borrow();
        assert_eq!(ncalls.len(), 1);
        assert_eq!(ncalls[0].0, "My Title");
        assert_eq!(ncalls[0].1, "hello sent");
    }

    // ── 11. pipe step with captured input ─────────────────────────────────────

    #[test]
    fn pipe_step_passes_captured_value_as_stdin() {
        let steps = vec![
            Step {
                run: Some(vec!["echo".to_owned(), "captured-data".to_owned()]),
                capture: Some("data".to_owned()),
                ..Default::default()
            },
            Step {
                pipe: Some(vec!["wc".to_owned(), "-c".to_owned()]),
                input: Some("{data}".to_owned()),
                capture: Some("count".to_owned()),
                ..Default::default()
            },
        ];
        let process = MockProcessExec::new([ok_result("captured-data\n"), ok_result("13\n")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report = execute_steps(
            &steps,
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &noop_predicate,
        )
        .unwrap();

        // The stdin passed to wc should be the captured value.
        assert_eq!(
            process.calls.borrow()[1].2.as_deref(),
            Some("captured-data")
        );
        assert_eq!(report.final_captures["count"], "13");
    }

    // ── 12. execute_hook ──────────────────────────────────────────────────────

    #[test]
    fn execute_hook_runs_single_step() {
        let hook = Hook {
            name: "test-hook".to_owned(),
            when: "post-update".to_owned(),
            r#if: None,
            run: vec!["echo".to_owned(), "hooked".to_owned()],
            ignore_failure: false,
        };
        let process = MockProcessExec::new([ok_result("hooked\n")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report =
            execute_hook(&hook, &process, &notifier, &mut prompter, &noop_predicate).unwrap();

        assert_eq!(report.steps_run, 1);
        assert_eq!(process.calls.borrow()[0].0, "echo");
    }

    #[test]
    fn execute_hook_respects_if_predicate() {
        let hook = Hook {
            name: "guarded".to_owned(),
            when: "post-update".to_owned(),
            r#if: Some("platform:linux".to_owned()),
            run: vec!["echo".to_owned()],
            ignore_failure: false,
        };
        let process = MockProcessExec::new([]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report = execute_hook(&hook, &process, &notifier, &mut prompter, &|_pred, _ctx| {
            false
        })
        .unwrap();

        assert_eq!(report.steps_skipped_by_predicate, 1);
        assert_eq!(report.steps_run, 0);
    }

    // ── 15. default_predicate_evaluator integration ───────────────────────────

    #[test]
    fn default_predicate_evaluator_gates_step_via_mock() {
        use crate::paths::Platform;
        use crate::predicate::{MockPredicateEnv, default_predicate_evaluator};

        // Build a mock env: linux, has "sh", does NOT have "rofi"
        let mut mock = MockPredicateEnv::new(Platform::Linux);
        mock.commands.insert("sh".to_owned());

        let evaluator = default_predicate_evaluator(mock);

        // Step 0: if = "platform:linux,command_exists:sh" → should run
        // Step 1: if = "command_exists:rofi" → should skip
        let steps = vec![
            Step {
                run: Some(vec!["echo".to_owned(), "runs".to_owned()]),
                r#if: Some("platform:linux,command_exists:sh".to_owned()),
                capture: Some("ran".to_owned()),
                ..Default::default()
            },
            Step {
                run: Some(vec!["echo".to_owned(), "skipped".to_owned()]),
                r#if: Some("command_exists:rofi".to_owned()),
                ..Default::default()
            },
        ];

        let process = MockProcessExec::new([ok_result("runs\n")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report = execute_steps(
            &steps,
            empty_ctx(),
            &process,
            &notifier,
            &mut prompter,
            &evaluator,
        )
        .unwrap();

        assert_eq!(report.steps_run, 1, "only the first step should run");
        assert_eq!(
            report.steps_skipped_by_predicate, 1,
            "second step should be skipped"
        );
        assert_eq!(
            report.final_captures.get("ran").map(String::as_str),
            Some("runs")
        );
    }

    #[test]
    fn execute_hook_respects_ignore_failure() {
        let hook = Hook {
            name: "lenient".to_owned(),
            when: "post-update".to_owned(),
            r#if: None,
            run: vec!["bad-cmd".to_owned()],
            ignore_failure: true,
        };
        let process = MockProcessExec::new([fail_result(1, "fail")]);
        let notifier = MockNotifier::default();
        let mut prompter = MockPrompter::default();

        let report =
            execute_hook(&hook, &process, &notifier, &mut prompter, &noop_predicate).unwrap();

        assert_eq!(report.steps_failed_ignored, 1);
    }
}
