//! `krypt-pkg` — package manager abstraction.
//!
//! Cross-distro / cross-platform install surface. One trait, several
//! impls. Auto-detects the right manager at runtime; users can override.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use krypt_pkg::{detect::pick_default, manager::RealRunner};
//!
//! let runner = RealRunner;
//! if let Some(mgr) = pick_default() {
//!     mgr.install(&runner, &["git".to_string()]).unwrap();
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod apt;
pub mod brew;
pub mod deps;
pub mod detect;
pub mod dnf;
pub mod manager;
pub mod pacman;
pub mod scoop;
pub mod winget;

/// Crate version, exposed for `krypt --version` aggregation.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
