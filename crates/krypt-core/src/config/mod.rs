//! `.krypt.toml` configuration schema and parser.
//!
//! See the [project README](https://github.com/kryptic-sh/krypt) for the
//! full schema reference. This module defines the in-memory Rust shape of
//! the config (`Config` and friends) and a parser that turns a `.krypt.toml`
//! file or string into one of these structs.
//!
//! What this module **does not** do (delegated to sibling modules):
//!
//! - `include = [...]` expansion → [`crate::include`] (issue #10)
//! - `${VAR}` path resolution     → [`crate::paths`]   (issue #11)
//!
//! The parser here treats `${VAR}` strings as opaque text. Resolving them
//! is a separate pass once the platform and overrides are known.

mod parse;
mod schema;

pub use parse::{ConfigError, parse_file, parse_str};
pub use schema::{
    Command, Config, DepsGroup, Hook, Link, Meta, PromptField, PromptSection, Step, Template,
};
