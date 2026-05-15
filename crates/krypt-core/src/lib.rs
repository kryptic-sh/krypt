//! `krypt-core` — the engine.
//!
//! Everything that does real work lives here, behind a stable Rust API.
//! The `krypt` binary (in `krypt-cli`) is a thin shell around this crate.
//!
//! Current modules:
//!
//! - [`config`]   — `.krypt.toml` schema, parser, validator (issue #9)
//! - [`paths`]    — `${VAR}` resolution with XDG defaults + platform gating
//!   (issue #11)
//! - [`include`]  — `include = [...]` glob expansion and config merging
//!   (issue #10)
//! - [`copy`]     — plan + atomic deploy of [[link]] and [[template]]
//!   entries to their resolved destinations (issue #12)
//! - [`manifest`] — versioned record of what was deployed, with sha256
//!   hashes for drift detection (issue #13)
//! - [`deploy`]   — high-level link / unlink / relink orchestration
//!   over the other modules (issue #15)
//!
//! Planned for Phase 1: `runner`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod copy;
pub mod deploy;
pub mod include;
pub mod manifest;
pub mod paths;

pub use include::{expand_includes, load_with_includes};

/// Crate version, exposed for `krypt --version` aggregation.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
