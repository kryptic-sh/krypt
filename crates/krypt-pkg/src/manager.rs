//! Core `PackageManager` and `Runner` traits plus production/test impls.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

use thiserror::Error;

// ─── PackageError ─────────────────────────────────────────────────────────────

/// Errors produced by package manager operations.
#[derive(Debug, Error)]
pub enum PackageError {
    /// A process could not be spawned or its output could not be read.
    #[error("io error running package manager: {0}")]
    Io(#[from] std::io::Error),

    /// The manager binary is not available on PATH.
    #[error("package manager not available on PATH")]
    NotAvailable,

    /// The manager exited with a non-zero status code.
    #[error("package manager exited with status {status}: {stderr}")]
    ExitFailure {
        /// Exit code returned by the process.
        status: i32,
        /// Captured stderr from the process.
        stderr: String,
    },
}

// ─── RunOutcome ───────────────────────────────────────────────────────────────

/// Result of a single process invocation.
pub struct RunOutcome {
    /// Process exit code.
    pub status: i32,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

// ─── Runner ───────────────────────────────────────────────────────────────────

/// Abstraction over process execution so tests can verify behaviour without
/// invoking real system commands.
pub trait Runner: Send + Sync {
    /// Run `cmd` with `args`. Returns a `RunOutcome` on success, or an I/O
    /// error if the process could not be spawned at all.
    fn run(&self, cmd: &str, args: &[&str]) -> Result<RunOutcome, std::io::Error>;
}

// ─── RealRunner ───────────────────────────────────────────────────────────────

/// Production runner — spawns a child process via [`Command`].
///
/// stdout and stderr are captured (not inherited) and returned in
/// [`RunOutcome`] so callers can include them in reports.
pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<RunOutcome, std::io::Error> {
        let out = Command::new(cmd).args(args).output()?;
        Ok(RunOutcome {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

// ─── MockRunner ───────────────────────────────────────────────────────────────

/// Key used to look up scripted responses in the mock runner.
type CallKey = (String, Vec<String>);

/// Scripted response for one call.
#[derive(Clone)]
pub struct MockResponse {
    /// Exit code to return.
    pub status: i32,
    /// Content to return as stdout.
    pub stdout: String,
    /// Content to return as stderr.
    pub stderr: String,
}

impl MockResponse {
    /// Convenience: exit 0, empty output.
    pub fn success() -> Self {
        Self {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// Convenience: exit 1, empty output.
    pub fn failure() -> Self {
        Self {
            status: 1,
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

/// Test runner that records every call and returns scripted responses.
///
/// Calls not registered with [`MockRunner::register`] return exit code 0 with
/// empty output.
pub struct MockRunner {
    responses: HashMap<CallKey, MockResponse>,
    calls: Mutex<Vec<(String, Vec<String>)>>,
}

impl MockRunner {
    /// Create a new empty mock runner (all calls succeed by default).
    pub fn new() -> Self {
        Self {
            responses: HashMap::new(),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Register a scripted response. `cmd` and `args` must match exactly.
    #[must_use]
    pub fn with(mut self, cmd: &str, args: &[&str], resp: MockResponse) -> Self {
        let key = (cmd.to_owned(), args.iter().map(|s| s.to_string()).collect());
        self.responses.insert(key, resp);
        self
    }

    /// Return a snapshot of all calls made so far.
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Default for MockRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl Runner for MockRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<RunOutcome, std::io::Error> {
        let key: CallKey = (cmd.to_owned(), args.iter().map(|s| s.to_string()).collect());
        self.calls.lock().unwrap().push(key.clone());
        let resp = self
            .responses
            .get(&key)
            .cloned()
            .unwrap_or(MockResponse::success());
        Ok(RunOutcome {
            status: resp.status,
            stdout: resp.stdout,
            stderr: resp.stderr,
        })
    }
}

// ─── PackageManager ───────────────────────────────────────────────────────────

/// Abstraction over a system package manager.
pub trait PackageManager: Send + Sync {
    /// Stable lowercase identifier (e.g. `"pacman"`, `"apt"`).
    ///
    /// This matches the field name in `DepsGroup` in the config schema.
    fn name(&self) -> &'static str;

    /// Returns `true` when the manager's binary is on `PATH`.
    fn is_available(&self) -> bool;

    /// Returns `true` when `pkg` is already installed.
    ///
    /// Only errors on unexpected conditions — a clean "not installed" (exit 1
    /// from a query command) is returned as `Ok(false)`.
    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError>;

    /// Install the given packages.
    ///
    /// Implementations may batch packages into a single invocation or loop one
    /// at a time (winget).
    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError>;
}
