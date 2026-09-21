//! Core `PackageManager` and `Runner` traits plus production/test impls.

use std::collections::HashMap;
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

    /// The manager's install command finished, but reading its state back
    /// shows these packages are still not installed. Raised by managers that
    /// report success regardless of the outcome (scoop).
    #[error("not installed: {}; output: {output}", packages.join(", "))]
    NotInstalled {
        /// The packages that were asked for and are not installed.
        packages: Vec<String>,
        /// Everything the install command printed.
        output: String,
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

    /// Run `cmd` with root privileges: through `sudo` when it is on `PATH`,
    /// otherwise directly. Containers and minimal installs often run as root
    /// with no `sudo` at all; a non-root user without `sudo` gets the
    /// manager's own permission error.
    fn run_as_root(&self, cmd: &str, args: &[&str]) -> Result<RunOutcome, std::io::Error> {
        let (program, argv) = root_invocation(which::which("sudo").is_ok(), cmd, args);
        self.run(program, &argv)
    }
}

/// The program and arguments that run `cmd args…` as root: `sudo cmd args…`
/// when `sudo_on_path`, else `cmd args…` unchanged.
pub fn root_invocation<'a>(
    sudo_on_path: bool,
    cmd: &'a str,
    args: &[&'a str],
) -> (&'a str, Vec<&'a str>) {
    if sudo_on_path {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(cmd);
        argv.extend_from_slice(args);
        ("sudo", argv)
    } else {
        (cmd, args.to_vec())
    }
}

// ─── RealRunner ───────────────────────────────────────────────────────────────

/// Production runner — spawns a child process via
/// [`krypt_platform::process::command`], so Windows `.cmd` / `.bat` shims
/// such as `scoop` spawn.
///
/// stdout and stderr are captured (not inherited) and returned in
/// [`RunOutcome`] so callers can include them in reports.
pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<RunOutcome, std::io::Error> {
        let out = krypt_platform::process::command(cmd).args(args).output()?;
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
/// Calls not registered with [`MockRunner::with`] return exit code 0 with
/// empty output. [`Runner::run_as_root`] behaves as if `sudo` is on `PATH`
/// unless [`MockRunner::without_sudo`] is used, independent of the host.
pub struct MockRunner {
    responses: Mutex<HashMap<CallKey, Vec<MockResponse>>>,
    calls: Mutex<Vec<(String, Vec<String>)>>,
    sudo_on_path: bool,
}

impl MockRunner {
    /// Create a new empty mock runner (all calls succeed by default).
    pub fn new() -> Self {
        Self {
            responses: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            sudo_on_path: true,
        }
    }

    /// Behave as a host without `sudo`: root commands run directly.
    #[must_use]
    pub fn without_sudo(mut self) -> Self {
        self.sudo_on_path = false;
        self
    }

    /// Register a scripted response. `cmd` and `args` must match exactly.
    ///
    /// Registering the same call again queues another response: each call
    /// takes the next one in order, and the last one answers every call after
    /// it, so a command whose output changes (state read before and after an
    /// install) can be scripted.
    #[must_use]
    pub fn with(self, cmd: &str, args: &[&str], resp: MockResponse) -> Self {
        let key = (cmd.to_owned(), args.iter().map(|s| s.to_string()).collect());
        self.responses
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .push(resp);
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
        let resp = match self.responses.lock().unwrap().get_mut(&key) {
            Some(queue) if queue.len() > 1 => queue.remove(0),
            Some(queue) => queue[0].clone(),
            None => MockResponse::success(),
        };
        Ok(RunOutcome {
            status: resp.status,
            stdout: resp.stdout,
            stderr: resp.stderr,
        })
    }

    fn run_as_root(&self, cmd: &str, args: &[&str]) -> Result<RunOutcome, std::io::Error> {
        let (program, argv) = root_invocation(self.sudo_on_path, cmd, args);
        self.run(program, &argv)
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

    /// Returns `true` when `pkg` can be installed from the manager's
    /// configured sources (repositories, taps, buckets, registry), whether
    /// or not it is installed. Used by `krypt deps --check`.
    fn exists(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError>;

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
