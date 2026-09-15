//! `krypt-platform` — cross-platform OS abstractions.
//!
//! Where the OSes genuinely differ, the difference is handled here so the rest
//! of the workspace does not need `cfg` arms:
//!
//! - [`process`] — spawning a program by name, including Windows `.cmd` /
//!   `.bat` shims that `std::process::Command` cannot find on its own.
//!
//! The notification backends live in `krypt_core::notify`, not here.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod process;

/// Crate version, exposed for `krypt --version` aggregation.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
