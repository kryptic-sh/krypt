//! `pacman` / `paru` package manager implementation (Arch Linux).
//!
//! Prefers `paru` if available (adds AUR support), falls back to `pacman`.
//! Both use the same install syntax. The manager name is always `"pacman"` to
//! match the `DepsGroup.pacman` config field.

use crate::manager::{PackageError, PackageManager, RunOutcome, Runner};

/// The AUR RPC `info` endpoint; see <https://aur.archlinux.org/rpc>.
pub const AUR_INFO_URL: &str = "https://aur.archlinux.org/rpc/v5/info";

/// Whether the AUR has a package named exactly `pkg`.
fn in_aur(runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
    let url = format!("{AUR_INFO_URL}?arg[]={}", percent_encode(pkg));
    let RunOutcome {
        status,
        stdout,
        stderr,
    } = runner.run("curl", &["-fsSL", &url])?;
    if status != 0 {
        return Err(PackageError::ExitFailure { status, stderr });
    }
    let body: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| PackageError::Io(std::io::Error::other(format!("AUR response: {e}"))))?;
    Ok(body["results"]
        .as_array()
        .is_some_and(|results| results.iter().any(|r| r["Name"] == pkg)))
}

/// Percent-encode everything but RFC 3986 unreserved characters, so a
/// package name such as `libc++` survives the query string.
fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Package manager implementation for Arch Linux (pacman / paru).
pub struct Pacman;

impl Pacman {
    /// Binary to use for installation: `paru` if available, else `pacman`.
    fn binary(&self) -> &'static str {
        if which::which("paru").is_ok() {
            "paru"
        } else {
            "pacman"
        }
    }
}

impl PackageManager for Pacman {
    fn name(&self) -> &'static str {
        "pacman"
    }

    fn is_available(&self) -> bool {
        which::which("pacman").is_ok()
    }

    /// `pacman -Si` against the sync databases, then the AUR's RPC interface
    /// (through `curl`, which every Arch install has). An AUR package counts
    /// as found, although installing it still needs `paru`.
    fn exists(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } = runner.run("pacman", &["-Si", pkg])?;
        if status == 0 {
            return Ok(true);
        }
        in_aur(runner, pkg)
    }

    fn is_installed(&self, runner: &dyn Runner, pkg: &str) -> Result<bool, PackageError> {
        let RunOutcome { status, .. } = runner.run("pacman", &["-Q", pkg])?;
        match status {
            0 => Ok(true),
            _ => Ok(false),
        }
    }

    fn install(&self, runner: &dyn Runner, packages: &[String]) -> Result<(), PackageError> {
        let bin = self.binary();
        let mut args = vec!["-S", "--noconfirm"];
        args.extend(packages.iter().map(String::as_str));
        let RunOutcome { status, stderr, .. } = runner.run_as_root(bin, &args)?;
        if status != 0 {
            return Err(PackageError::ExitFailure { status, stderr });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encode_keeps_unreserved_and_escapes_the_rest() {
        assert_eq!(percent_encode("hjkl-bin"), "hjkl-bin");
        assert_eq!(percent_encode("libc++"), "libc%2B%2B");
        assert_eq!(percent_encode("a b@1"), "a%20b%401");
    }
}
