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
//! - `${VAR}` path resolution in *path fields* → [`crate::paths`] (issue #11)
//!
//! The parser here treats `${VAR}` strings as opaque text in path fields;
//! those are resolved by the [`crate::paths::Resolver`] separately.
//!
//! For *step args*, `${VAR}` is resolved eagerly by [`resolve_step_vars`]
//! (issue #55) after include expansion — see that function for full semantics.

mod parse;
mod schema;

pub use parse::{ConfigError, parse_file, parse_str, resolve_step_vars};
pub use schema::{
    Command, Config, DepsGroup, Hook, Link, Meta, PromptField, PromptSection, Step, Template,
};
