//! `files-pkg` — package manager abstraction.
//!
//! Cross-distro / cross-platform install surface. One trait, several
//! impls. Auto-detects the right manager at runtime; users can override.
//!
//! Current state: scaffolding. Real impls land in Phase 1 (issue #19).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Crate version, exposed for `files --version` aggregation.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
