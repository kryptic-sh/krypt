//! `krypt-core` — the engine.
//!
//! Everything that does real work lives here, behind a stable Rust API.
//! The `krypt` binary (in `krypt-cli`) is a thin shell around this crate.
//!
//! Current state: scaffolding. Real modules land in Phase 1 (see issues
//! #9–#22 in the upstream repo).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Crate version, exposed for `krypt --version` aggregation.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
