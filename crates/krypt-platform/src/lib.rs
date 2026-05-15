//! `krypt-platform` — cross-platform OS abstractions.
//!
//! cfg-gated modules per OS keep the noise out of `krypt-core`. Symlink
//! capability detection, notification backends, native path resolution.
//!
//! Current state: scaffolding.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Crate version, exposed for `krypt --version` aggregation.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
