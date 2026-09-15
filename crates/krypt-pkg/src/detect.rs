//! Auto-detection of available package managers.

use crate::apt::Apt;
use crate::brew::Brew;
use crate::dnf::Dnf;
use crate::manager::PackageManager;
use crate::pacman::Pacman;
use crate::scoop::Scoop;
use crate::winget::Winget;

/// Return every package manager whose binary is on `PATH`, ordered by
/// platform preference.
///
/// Order:
/// - macOS: brew
/// - Windows: scoop, winget
/// - Linux: pacman, dnf, apt
pub fn detect_all() -> Vec<Box<dyn PackageManager>> {
    candidates_for_os(std::env::consts::OS)
        .into_iter()
        .filter(|m| m.is_available())
        .collect()
}

/// Every manager for `os` (a [`std::env::consts::OS`] value) in preference
/// order, installed or not. Any OS other than macOS and Windows gets the Linux
/// list.
fn candidates_for_os(os: &str) -> Vec<Box<dyn PackageManager>> {
    match os {
        "macos" => vec![Box::new(Brew)],
        "windows" => vec![Box::new(Scoop), Box::new(Winget)],
        _ => vec![Box::new(Pacman), Box::new(Dnf), Box::new(Apt)],
    }
}

/// Return the first available manager for the current platform.
pub fn pick_default() -> Option<Box<dyn PackageManager>> {
    detect_all().into_iter().next()
}

/// Return the manager with the given name regardless of availability.
///
/// Returns `None` when no manager with that name is registered.
pub fn pick_by_name(name: &str) -> Option<Box<dyn PackageManager>> {
    let all: Vec<Box<dyn PackageManager>> = vec![
        Box::new(Pacman),
        Box::new(Apt),
        Box::new(Dnf),
        Box::new(Brew),
        Box::new(Scoop),
        Box::new(Winget),
    ];
    all.into_iter().find(|m| m.name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(os: &str) -> Vec<&'static str> {
        candidates_for_os(os).iter().map(|m| m.name()).collect()
    }

    // Pure, so every OS's preference order is checked on every runner.
    #[test]
    fn candidates_follow_each_platforms_preference_order() {
        assert_eq!(names("macos"), ["brew"]);
        assert_eq!(names("windows"), ["scoop", "winget"]);
        assert_eq!(names("linux"), ["pacman", "dnf", "apt"]);
    }

    #[test]
    fn unlisted_unix_likes_get_the_linux_order() {
        assert_eq!(names("freebsd"), names("linux"));
    }
}
