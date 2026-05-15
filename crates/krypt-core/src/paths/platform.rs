//! OS detection.

use std::fmt;

/// Operating system we're resolving paths for.
///
/// Auto-detected from `cfg!(target_os)` in [`Platform::current`], but can
/// be set explicitly (mainly for tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Linux (and Linux-likes — BSD, etc., until we have a reason to split).
    Linux,
    /// macOS.
    Macos,
    /// Windows.
    Windows,
}

impl Platform {
    /// The platform the current binary is running on.
    ///
    /// Resolves at compile time via `cfg!`. Falls back to [`Platform::Linux`]
    /// on platforms we don't have a dedicated variant for, with the
    /// understanding that the platform-gated vars (`${WIN_*}`, `${MAC_*}`)
    /// will simply not resolve there.
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Macos
        } else {
            Platform::Linux
        }
    }

    /// String slug used in `.krypt.toml` (`platform = "linux"`, etc.).
    pub const fn as_str(self) -> &'static str {
        match self {
            Platform::Linux => "linux",
            Platform::Macos => "macos",
            Platform::Windows => "windows",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
