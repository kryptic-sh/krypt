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

    /// Pick up directories an install has just added to `PATH`, so later
    /// [`Runner::run`] calls find the programs it put there.
    fn refresh_path(&self) -> Result<(), std::io::Error>;

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
///
/// Commands see this process's `PATH` until [`Runner::refresh_path`] extends
/// it, after which they see the extended one.
#[derive(Default)]
pub struct RealRunner {
    path: Mutex<Option<std::ffi::OsString>>,
}

/// Prints the machine and then the user `PATH` Windows starts new processes
/// with, one per line. .NET expands `%VAR%` references in them.
const REGISTRY_PATH_SCRIPT: &str = "[Environment]::GetEnvironmentVariable('Path', 'Machine'); \
     [Environment]::GetEnvironmentVariable('Path', 'User')";

impl Runner for RealRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> Result<RunOutcome, std::io::Error> {
        let mut command = match &*self.path.lock().unwrap() {
            Some(path) => krypt_platform::process::command_in_path(cmd, path),
            None => krypt_platform::process::command(cmd),
        };
        let out = command.args(args).output()?;
        Ok(RunOutcome {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Windows keeps the `PATH` new processes get in the registry, and an
    /// installer that extends it (scoop's rustup adds its `.cargo\bin`) does
    /// not reach processes already running. So this appends the registry's
    /// directories this process lacks, read through Windows PowerShell, which
    /// every Windows ships, run from its fixed place under `%SystemRoot%`
    /// rather than looked up in the `PATH` being repaired. On other systems a
    /// package manager installs into a directory already on `PATH`, so there
    /// is nothing to pick up and this does nothing.
    fn refresh_path(&self) -> Result<(), std::io::Error> {
        if !cfg!(windows) {
            return Ok(());
        }
        let system_root = std::env::var_os("SystemRoot")
            .ok_or_else(|| std::io::Error::other("SystemRoot is not set"))?;
        let powershell = std::path::Path::new(&system_root)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let out = self.run(
            &powershell.to_string_lossy(),
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                REGISTRY_PATH_SCRIPT,
            ],
        )?;
        if out.status != 0 {
            return Err(std::io::Error::other(format!(
                "reading PATH from the registry exited with {}: {}",
                out.status,
                out.stderr.trim()
            )));
        }
        let mut path = self.path.lock().unwrap();
        let current = path
            .clone()
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        *path = Some(krypt_platform::process::append_new_paths(
            &current,
            out.stdout.lines(),
        ));
        Ok(())
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

    /// What [`MockRunner::calls`] records for a [`Runner::refresh_path`], so a
    /// test can see where in the run the refresh happened.
    pub const REFRESH_PATH: &str = "<refresh_path>";

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

    /// Records [`MockRunner::REFRESH_PATH`] and succeeds.
    fn refresh_path(&self) -> Result<(), std::io::Error> {
        self.calls
            .lock()
            .unwrap()
            .push((Self::REFRESH_PATH.to_owned(), Vec::new()));
        Ok(())
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Windows PowerShell's folder is on the machine `PATH` of every Windows
    /// install but is not `System32` itself, so a runner started with only
    /// `System32` cannot find `powershell` until the refresh adds it.
    #[test]
    fn refresh_path_adds_registry_dirs_to_what_commands_find() {
        let system32 = std::path::Path::new(&std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .into_os_string();
        let runner = RealRunner {
            path: Mutex::new(Some(system32)),
        };
        let finds_powershell = || runner.run("where", &["powershell"]).unwrap().status == 0;

        assert!(
            !finds_powershell(),
            "System32 alone already finds powershell"
        );
        runner.refresh_path().unwrap();
        assert!(
            finds_powershell(),
            "refresh did not add Windows PowerShell's folder"
        );
    }
}
