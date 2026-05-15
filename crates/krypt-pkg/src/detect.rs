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
    let candidates: Vec<Box<dyn PackageManager>> = if cfg!(target_os = "macos") {
        vec![Box::new(Brew)]
    } else if cfg!(target_os = "windows") {
        vec![Box::new(Scoop), Box::new(Winget)]
    } else {
        vec![Box::new(Pacman), Box::new(Dnf), Box::new(Apt)]
    };
    candidates
        .into_iter()
        .filter(|m| m.is_available())
        .collect()
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
