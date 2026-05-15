//! `krypt-core` — the engine.
//!
//! Everything that does real work lives here, behind a stable Rust API.
//! The `krypt` binary (in `krypt-cli`) is a thin shell around this crate.
//!
//! Current modules:
//!
//! - [`config`]  — `.krypt.toml` schema, parser, validator (issue #9)
//! - [`paths`]   — `${VAR}` resolution with XDG defaults + platform gating
//!                 (issue #11)
//! - [`include`] — `include = [...]` glob expansion and config merging
//!                 (issue #10)
//!
//! Planned for Phase 1: `copy`, `manifest`, `runner`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod include;
pub mod paths;

pub use include::{expand_includes, load_with_includes};

/// Crate version, exposed for `krypt --version` aggregation.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
