//! `krypt-core` — the engine.
//!
//! Everything that does real work lives here, behind a stable Rust API.
//! The `krypt` binary (in `krypt-cli`) is a thin shell around this crate.
//!
//! Current modules:
//!
//! - [`config`] — `.krypt.toml` schema, parser, validator (issue #9)
//! - [`paths`]  — `${VAR}` resolution with XDG defaults + platform gating
//!                (issue #11)
//!
//! Planned for Phase 1: `include`, `copy`, `manifest`, `runner`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod paths;

/// Crate version, exposed for `krypt --version` aggregation.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
