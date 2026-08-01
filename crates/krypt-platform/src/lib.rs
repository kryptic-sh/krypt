//! `krypt-platform` — cross-platform OS abstractions.
//!
//! **Scaffolding.** This crate currently contains nothing but [`VERSION`],
//! which `krypt --version` prints alongside the other workspace crates.
//!
//! *Planned* (none of this exists yet): cfg-gated per-OS modules to keep the
//! noise out of `krypt-core`, covering things like native path resolution and
//! filesystem capability probing. Note that the notification backends already
//! live in `krypt_core::notify`, not here.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Crate version, exposed for `krypt --version` aggregation.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
