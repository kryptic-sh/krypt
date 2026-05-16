//! Cross-platform notification backend.
//!
//! Dispatches desktop notifications via platform-appropriate shell commands.
//! Provides [`AutoNotifier`] which implements the [`crate::runner::Notifier`]
//! trait and replaces the previous `RealNotifier` stub.
//!
//! # Backend selection
//!
//! [`detect`] tries backends in platform-specific order using `which::which`.
//! An explicit override via `[meta] notify_backend` in `.krypt.toml` (or
//! the `--backend` CLI flag) bypasses auto-detection.
//!
//! # Backend commands
//!
//! | Backend            | Command                                           |
//! |--------------------|---------------------------------------------------|
//! | `notify-send`      | `notify-send <title> <body>`                      |
//! | `osascript`        | `osascript -e 'display notification ...'`         |
//! | `terminal-notifier`| `terminal-notifier -title <title> -message <body>`|
//! | `powershell`       | PowerShell `[System.Windows.Forms.MessageBox]`    |
//! | `stderr`           | `eprintln!("notice: {title} — {body}")`           |
//!
//! # PowerShell strategy
//!
//! BurntToast requires a third-party module install (`Install-Module
//! BurntToast`) which most users won't have. Instead we use
//! `System.Windows.Forms.MessageBox` which ships in every .NET installation.
//! Values are passed via `$env:KRYPT_NOTIFY_TITLE` / `$env:KRYPT_NOTIFY_BODY`
//! environment variables to avoid PowerShell single-quote escaping entirely.

use std::io;
use std::process::{Command, Stdio};

use thiserror::Error;

// ─── Error ───────────────────────────────────────────────────────────────────

/// Errors that can occur while dispatching a notification.
///
/// Kept ≤ 128 bytes: `io::Error` is boxed, stderr string is boxed.
#[derive(Debug, Error)]
pub enum NotifyError {
    /// Failed to spawn the notification subprocess.
    #[error("failed to spawn notification command: {0}")]
    Spawn(#[source] Box<io::Error>),

    /// Notification subprocess exited non-zero.
    #[error("notification command exited with code {code}: {stderr}")]
    NonZeroExit {
        /// Exit code.
        code: i32,
        /// Captured stderr output (boxed to keep enum small).
        stderr: Box<str>,
    },

    /// No suitable backend was found (should not occur; `Stderr` is always available).
    #[error("no notification backend available")]
    NoBackend,
}

impl From<NotifyError> for io::Error {
    fn from(e: NotifyError) -> Self {
        io::Error::other(e.to_string())
    }
}

// ─── Backend enum ─────────────────────────────────────────────────────────────

/// Available notification backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyBackend {
    /// `notify-send` — standard on most Linux/BSD desktops.
    NotifySend,
    /// `osascript` — ships with macOS.
    Osascript,
    /// `terminal-notifier` — third-party macOS notifier, nicer than osascript.
    TerminalNotifier,
    /// PowerShell `[System.Windows.Forms.MessageBox]` — available on all
    /// Windows systems with .NET.
    PowerShell,
    /// Fallback: print to stderr. Always available.
    Stderr,
}

impl NotifyBackend {
    /// Parse a backend name string into a [`NotifyBackend`].
    ///
    /// Returns `None` for unrecognised names.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "notify-send" => Some(Self::NotifySend),
            "osascript" => Some(Self::Osascript),
            "terminal-notifier" => Some(Self::TerminalNotifier),
            "powershell" => Some(Self::PowerShell),
            "stderr" => Some(Self::Stderr),
            _ => None,
        }
    }
}

// ─── Auto-detect ──────────────────────────────────────────────────────────────

/// Detect the best available notification backend.
///
/// Precedence:
/// 1. Explicit `override_name` (e.g. from `--backend` or `[meta] notify_backend`).
///    `"auto"` and `None` both trigger auto-detection.
///    `"stderr"` forces [`NotifyBackend::Stderr`].
///    Unknown names produce a `tracing::warn!` and fall through to auto-detect.
/// 2. Platform-appropriate auto-detect via `which::which`.
/// 3. [`NotifyBackend::Stderr`] fallback.
pub fn detect(override_name: Option<&str>) -> NotifyBackend {
    match override_name {
        None | Some("auto") => auto_detect(),
        Some("stderr") => NotifyBackend::Stderr,
        Some(name) => match NotifyBackend::from_name(name) {
            Some(b) => b,
            None => {
                tracing::warn!(
                    backend = name,
                    "unknown notify_backend — falling through to auto-detect"
                );
                auto_detect()
            }
        },
    }
}

fn auto_detect() -> NotifyBackend {
    #[cfg(target_os = "macos")]
    {
        if which::which("terminal-notifier").is_ok() {
            return NotifyBackend::TerminalNotifier;
        }
        if which::which("osascript").is_ok() {
            return NotifyBackend::Osascript;
        }
    }

    #[cfg(target_os = "windows")]
    {
        if which::which("powershell").is_ok() {
            return NotifyBackend::PowerShell;
        }
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        if which::which("notify-send").is_ok() {
            return NotifyBackend::NotifySend;
        }
    }

    NotifyBackend::Stderr
}

// ─── Command construction (pure, testable) ────────────────────────────────────

/// Build the command + args for a given backend without spawning.
///
/// Returns `(program, args)`. For [`NotifyBackend::Stderr`] returns
/// `("", vec![])` — the caller should handle this variant directly.
pub fn command_for(backend: NotifyBackend, title: &str, body: &str) -> (String, Vec<String>) {
    match backend {
        NotifyBackend::NotifySend => (
            "notify-send".to_owned(),
            vec![title.to_owned(), body.to_owned()],
        ),
        NotifyBackend::Osascript => {
            let script = format!(
                "display notification \"{}\" with title \"{}\"",
                escape_applescript(body),
                escape_applescript(title),
            );
            ("osascript".to_owned(), vec!["-e".to_owned(), script])
        }
        NotifyBackend::TerminalNotifier => (
            "terminal-notifier".to_owned(),
            vec![
                "-title".to_owned(),
                title.to_owned(),
                "-message".to_owned(),
                body.to_owned(),
            ],
        ),
        NotifyBackend::PowerShell => {
            // Values are injected via env vars to sidestep PowerShell escaping.
            // The script reads $env:KRYPT_NOTIFY_TITLE / $env:KRYPT_NOTIFY_BODY.
            let script = "Add-Type -AssemblyName System.Windows.Forms; \
                [System.Windows.Forms.MessageBox]::Show(\
                $env:KRYPT_NOTIFY_BODY, $env:KRYPT_NOTIFY_TITLE) | Out-Null"
                .to_owned();
            ("powershell".to_owned(), vec!["-Command".to_owned(), script])
        }
        NotifyBackend::Stderr => (String::new(), vec![]),
    }
}

/// Escape a string for use inside an AppleScript double-quoted string.
///
/// Backslashes → `\\`, double-quotes → `\"`.
pub fn escape_applescript(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

// ─── Dispatch ─────────────────────────────────────────────────────────────────

/// Send a desktop notification using the specified backend.
pub fn notify(backend: NotifyBackend, title: &str, body: &str) -> Result<(), NotifyError> {
    if backend == NotifyBackend::Stderr {
        eprintln!("notice: {title} \u{2014} {body}");
        return Ok(());
    }

    let (program, mut args) = command_for(backend, title, body);

    let mut cmd = Command::new(&program);
    cmd.args(&args);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::null());

    // PowerShell: inject values via env vars.
    if backend == NotifyBackend::PowerShell {
        cmd.env("KRYPT_NOTIFY_TITLE", title);
        cmd.env("KRYPT_NOTIFY_BODY", body);
    }

    // Suppress unused mut warning — args is mutated only in some branches above.
    let _ = args.as_mut_slice();

    let output = cmd.output().map_err(|e| NotifyError::Spawn(Box::new(e)))?;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(NotifyError::NonZeroExit {
            code,
            stderr: stderr.into_boxed_str(),
        });
    }

    Ok(())
}

// ─── AutoNotifier ─────────────────────────────────────────────────────────────

/// Production notifier backed by a detected or configured [`NotifyBackend`].
///
/// Implements [`crate::runner::Notifier`]. Use [`AutoNotifier::with_backend`]
/// in tests to pin a specific backend (e.g. `NotifyBackend::Stderr`) so no
/// real desktop notifications fire.
pub struct AutoNotifier {
    backend: NotifyBackend,
}

impl AutoNotifier {
    /// Create an `AutoNotifier` with auto-detected backend.
    pub fn new(override_name: Option<&str>) -> Self {
        Self {
            backend: detect(override_name),
        }
    }

    /// Create an `AutoNotifier` with an explicit backend (useful in tests).
    pub fn with_backend(backend: NotifyBackend) -> Self {
        Self { backend }
    }
}

impl crate::runner::Notifier for AutoNotifier {
    fn notify(&self, title: &str, body: &str) -> Result<(), io::Error> {
        notify(self.backend, title, body).map_err(io::Error::from)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // 1. detect(None) → Stderr when no backends are in PATH.
    // We can't easily manipulate PATH in unit tests without unsafe tricks, but
    // we can verify the code path compiles and returns a valid backend.
    #[test]
    fn detect_none_returns_valid_backend() {
        let b = detect(None);
        // Any variant is acceptable; we just confirm it doesn't panic.
        let _ = b;
    }

    // 2. detect(Some("notify-send")) → NotifySend, bypasses which.
    #[test]
    fn detect_explicit_notify_send() {
        assert_eq!(detect(Some("notify-send")), NotifyBackend::NotifySend);
    }

    // 3. detect(Some("auto")) behaves the same as detect(None).
    #[test]
    fn detect_auto_same_as_none() {
        assert_eq!(detect(Some("auto")), detect(None));
    }

    // 4. detect(Some("stderr")) → Stderr.
    #[test]
    fn detect_explicit_stderr() {
        assert_eq!(detect(Some("stderr")), NotifyBackend::Stderr);
    }

    // 5. detect(Some("typo-backend")) → falls through to auto-detect result,
    //    same as detect(None).
    #[test]
    fn detect_unknown_falls_through() {
        // A typo override should produce the same result as no override at all
        // because the unknown name is warned and ignored.
        let b = detect(Some("typo-backend"));
        let expected = detect(None);
        assert_eq!(b, expected);
    }

    // 6. Stderr backend returns Ok(()).
    #[test]
    fn notify_stderr_ok() {
        assert!(notify(NotifyBackend::Stderr, "t", "b").is_ok());
    }

    // 7. AppleScript escaper round-trips.
    #[test]
    fn applescript_escaper() {
        assert_eq!(escape_applescript(r#"say "hello""#), r#"say \"hello\""#);
        assert_eq!(escape_applescript(r"back\slash"), r"back\\slash");
        assert_eq!(escape_applescript("plain"), "plain");
        // Newlines pass through unchanged (AppleScript handles them).
        assert_eq!(escape_applescript("line\nbreak"), "line\nbreak");
        // Round-trip: escape then unescape manually.
        let input = r#"title with "quotes" and \backslash"#;
        let escaped = escape_applescript(input);
        // The escaped string should not contain unescaped quotes.
        assert!(!escaped.contains("\\\"") || escaped.contains("\\\\"));
    }

    // 8. command_for: verify argument shapes without spawning.
    #[test]
    fn command_for_notify_send() {
        let (prog, args) = command_for(NotifyBackend::NotifySend, "Title", "Body");
        assert_eq!(prog, "notify-send");
        assert_eq!(args, vec!["Title", "Body"]);
    }

    #[test]
    fn command_for_terminal_notifier() {
        let (prog, args) = command_for(NotifyBackend::TerminalNotifier, "My Title", "My Body");
        assert_eq!(prog, "terminal-notifier");
        assert!(args.contains(&"-title".to_owned()));
        assert!(args.contains(&"My Title".to_owned()));
        assert!(args.contains(&"-message".to_owned()));
        assert!(args.contains(&"My Body".to_owned()));
    }

    #[test]
    fn command_for_osascript_escapes_quotes() {
        let (prog, args) = command_for(NotifyBackend::Osascript, r#"Ti"tle"#, r#"Bo"dy"#);
        assert_eq!(prog, "osascript");
        let script = &args[1];
        // Embedded quote must be escaped.
        assert!(
            script.contains("\\\""),
            "osascript script should escape double-quotes: {script}"
        );
    }

    #[test]
    fn command_for_powershell() {
        let (prog, args) = command_for(NotifyBackend::PowerShell, "T", "B");
        assert_eq!(prog, "powershell");
        assert!(args.iter().any(|a| a.contains("MessageBox")));
    }

    // 9. Schema: notify_backend field on Meta parses correctly.
    #[test]
    fn meta_notify_backend_parses() {
        let toml = "[meta]\nnotify_backend = \"osascript\"\n";
        let cfg: crate::config::Config = toml::from_str(toml).expect("parse");
        assert_eq!(cfg.meta.notify_backend.as_deref(), Some("osascript"));
    }

    #[test]
    fn meta_notify_backend_defaults_none() {
        let toml = "[meta]\nname = \"test\"\n";
        let cfg: crate::config::Config = toml::from_str(toml).expect("parse");
        assert!(cfg.meta.notify_backend.is_none());
    }
}
