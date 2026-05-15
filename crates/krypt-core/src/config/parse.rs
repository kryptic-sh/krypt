//! `.krypt.toml` parser + semantic validator.
//!
//! The TOML deserializer already covers syntax errors, type mismatches, and
//! (thanks to `deny_unknown_fields`) typo'd keys. On top of that we run a
//! semantic pass that enforces invariants the type system can't:
//!
//! - exactly one of `src` / `src_glob` on each `[[link]]`
//! - exactly one of `run` / `pipe` / `notify` on each step
//! - platform strings are one of `linux` / `macos` / `windows`
//! - prompt field `requires` references a key that exists in the same section

use std::path::{Path, PathBuf};
use std::{fs, io};

use thiserror::Error;

use super::schema::{Config, Link, PromptSection, Step};

/// Errors that can come out of parsing or validating a `.krypt.toml`.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Failed to read the file from disk.
    #[error("read {path}: {source}")]
    Io {
        /// The path we tried to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// Syntax / type / unknown-field error from the TOML deserializer.
    /// The inner error already carries a span for nice error reporting.
    #[error("{path}: {source}")]
    Toml {
        /// File that failed to parse.
        path: PathBuf,
        /// Underlying TOML error.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// Semantic validation failure. Includes a JSON-pointer-ish location
    /// hint so the user can find the offending entry.
    #[error("{path}: {location}: {message}")]
    Validation {
        /// File that failed validation.
        path: PathBuf,
        /// Where in the doc the problem is (e.g. `link[2]`, `prompts.git.fields[0]`).
        location: String,
        /// Human-readable explanation.
        message: String,
    },
}

/// Parse a `.krypt.toml` from disk.
///
/// Runs syntactic parse + semantic validation. The returned `Config` is
/// ready to feed into the include pass (#10) and path resolver (#11).
pub fn parse_file(path: impl AsRef<Path>) -> Result<Config, ConfigError> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })?;
    parse_with_path(&raw, path)
}

/// Parse from an in-memory string. Useful for tests and `include` expansion
/// where we already have the bytes. `path_hint` shows up in error messages
/// when the source isn't an on-disk file.
pub fn parse_str(raw: &str, path_hint: impl AsRef<Path>) -> Result<Config, ConfigError> {
    parse_with_path(raw, path_hint.as_ref())
}

fn parse_with_path(raw: &str, path: &Path) -> Result<Config, ConfigError> {
    let cfg: Config = toml::from_str(raw).map_err(|e| ConfigError::Toml {
        path: path.to_owned(),
        source: Box::new(e),
    })?;
    validate(&cfg, path)?;
    Ok(cfg)
}

/// Semantic validation pass.
///
/// Type-system-enforceable invariants are checked by serde; this catches
/// rules the type system can't express.
fn validate(cfg: &Config, path: &Path) -> Result<(), ConfigError> {
    for (idx, link) in cfg.links.iter().enumerate() {
        validate_link(link, &format!("link[{idx}]"), path)?;
    }

    for (idx, t) in cfg.templates.iter().enumerate() {
        validate_platform(&t.platform, &format!("template[{idx}]"), path)?;
    }

    for (name, section) in &cfg.prompts {
        validate_prompt_section(section, &format!("prompts.{name}"), path)?;
    }

    for (idx, hook) in cfg.hooks.iter().enumerate() {
        if hook.run.is_empty() {
            return Err(ConfigError::Validation {
                path: path.to_owned(),
                location: format!("hook[{idx}]"),
                message: "`run` must contain at least one argument".into(),
            });
        }
    }

    for (idx, cmd) in cfg.commands.iter().enumerate() {
        let loc = format!("command[{idx}] ({}/{})", cmd.group, cmd.name);
        validate_platform(&cmd.platform, &loc, path)?;
        if cmd.steps.is_empty() {
            return Err(ConfigError::Validation {
                path: path.to_owned(),
                location: loc,
                message: "command must have at least one step".into(),
            });
        }
        for (sidx, step) in cmd.steps.iter().enumerate() {
            validate_step(step, &format!("{loc}.steps[{sidx}]"), path)?;
        }
    }

    Ok(())
}

fn validate_link(link: &Link, loc: &str, path: &Path) -> Result<(), ConfigError> {
    match (link.src.as_deref(), link.src_glob.as_deref()) {
        (Some(_), Some(_)) => Err(ConfigError::Validation {
            path: path.to_owned(),
            location: loc.into(),
            message: "set exactly one of `src` or `src_glob`, not both".into(),
        }),
        (None, None) => Err(ConfigError::Validation {
            path: path.to_owned(),
            location: loc.into(),
            message: "missing `src` or `src_glob`".into(),
        }),
        _ => Ok(()),
    }?;
    validate_platform(&link.platform, loc, path)
}

fn validate_platform(platform: &Option<String>, loc: &str, path: &Path) -> Result<(), ConfigError> {
    if let Some(p) = platform {
        if !matches!(p.as_str(), "linux" | "macos" | "windows") {
            return Err(ConfigError::Validation {
                path: path.to_owned(),
                location: loc.into(),
                message: format!(
                    "platform = {p:?} is not one of \"linux\" / \"macos\" / \"windows\""
                ),
            });
        }
    }
    Ok(())
}

fn validate_prompt_section(
    section: &PromptSection,
    loc: &str,
    path: &Path,
) -> Result<(), ConfigError> {
    if section.fields.is_empty() {
        return Err(ConfigError::Validation {
            path: path.to_owned(),
            location: loc.into(),
            message: "prompt section must have at least one field".into(),
        });
    }
    let known: std::collections::HashSet<&str> =
        section.fields.iter().map(|f| f.key.as_str()).collect();
    for (idx, field) in section.fields.iter().enumerate() {
        if let Some(req) = &field.requires {
            if !known.contains(req.as_str()) {
                return Err(ConfigError::Validation {
                    path: path.to_owned(),
                    location: format!("{loc}.fields[{idx}]"),
                    message: format!(
                        "`requires = \"{req}\"` references a field that doesn't exist in this section"
                    ),
                });
            }
        }
        if !matches!(field.r#type.as_str(), "string" | "bool" | "int") {
            return Err(ConfigError::Validation {
                path: path.to_owned(),
                location: format!("{loc}.fields[{idx}]"),
                message: format!(
                    "type = {:?} is not one of \"string\" / \"bool\" / \"int\"",
                    field.r#type
                ),
            });
        }
    }
    Ok(())
}

fn validate_step(step: &Step, loc: &str, path: &Path) -> Result<(), ConfigError> {
    let kinds = [
        ("run", step.run.is_some()),
        ("pipe", step.pipe.is_some()),
        ("notify", step.notify.is_some()),
    ];
    let set: Vec<&str> = kinds
        .iter()
        .filter(|(_, has)| *has)
        .map(|(n, _)| *n)
        .collect();
    if set.is_empty() {
        return Err(ConfigError::Validation {
            path: path.to_owned(),
            location: loc.into(),
            message: "step must set one of `run`, `pipe`, or `notify`".into(),
        });
    }
    if set.len() > 1 {
        return Err(ConfigError::Validation {
            path: path.to_owned(),
            location: loc.into(),
            message: format!(
                "step has multiple kinds set ({}); pick exactly one",
                set.join(", ")
            ),
        });
    }

    // notify[0] = title, notify[1] = body. Allow 1- or 2-element form.
    if let Some(n) = &step.notify {
        if n.is_empty() || n.len() > 2 {
            return Err(ConfigError::Validation {
                path: path.to_owned(),
                location: loc.into(),
                message: format!("`notify` takes 1 or 2 strings, got {}", n.len()),
            });
        }
    }

    if let Some(of) = &step.on_fail {
        if !matches!(of.as_str(), "abort" | "notify" | "ignore" | "prompt") {
            return Err(ConfigError::Validation {
                path: path.to_owned(),
                location: loc.into(),
                message: format!(
                    "on_fail = {of:?} is not one of \"abort\" / \"notify\" / \"ignore\" / \"prompt\""
                ),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> Config {
        parse_str(s, "test.toml").expect("parse + validate should succeed")
    }

    fn err(s: &str) -> ConfigError {
        parse_str(s, "test.toml").expect_err("expected an error")
    }

    #[test]
    fn empty_file_is_valid_with_defaults() {
        let cfg = ok("");
        assert!(cfg.links.is_empty());
        assert!(cfg.prompts.is_empty());
    }

    #[test]
    fn link_requires_one_of_src_or_src_glob() {
        let e = err(r#"
[[link]]
dst = "/tmp/x"
"#);
        assert!(matches!(e, ConfigError::Validation { .. }));
    }

    #[test]
    fn link_rejects_both_src_and_src_glob() {
        let e = err(r#"
[[link]]
src = "a"
src_glob = "b/*"
dst = "/tmp/x"
"#);
        match e {
            ConfigError::Validation { message, .. } => {
                assert!(message.contains("exactly one"), "got: {message}");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn platform_must_be_known() {
        let e = err(r#"
[[link]]
src = "a"
dst = "/tmp/x"
platform = "freebsd"
"#);
        assert!(matches!(e, ConfigError::Validation { .. }));
    }

    #[test]
    fn unknown_top_level_field_is_loud() {
        let e = err(r#"
made_up_field = "oops"
"#);
        // serde's deny_unknown_fields → TOML error, not Validation
        assert!(matches!(e, ConfigError::Toml { .. }));
    }

    #[test]
    fn step_requires_exactly_one_kind() {
        let e = err(r#"
[[command]]
group = "x"
name = "y"
steps = [
  { capture = "z" }
]
"#);
        assert!(matches!(e, ConfigError::Validation { .. }));
    }

    #[test]
    fn step_rejects_multiple_kinds() {
        let e = err(r#"
[[command]]
group = "x"
name = "y"
steps = [
  { run = ["echo"], pipe = ["cat"] }
]
"#);
        match e {
            ConfigError::Validation { message, .. } => {
                assert!(message.contains("multiple kinds"), "got: {message}");
            }
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn prompt_requires_must_reference_known_field() {
        let e = err(r#"
[prompts.x]
writer = "noop"
fields = [
  { key = "a", prompt = "Aye?", requires = "nonexistent" },
]
"#);
        assert!(matches!(e, ConfigError::Validation { .. }));
    }

    #[test]
    fn full_round_trip() {
        let cfg = ok(r#"
include = ["other.toml"]

[meta]
name = "test"
krypt_min = "0.1.0"

[paths]
HOME = "${env:HOME}"

[[link]]
src = ".gitconfig"
dst = "${HOME}/.gitconfig"

[[link]]
src_glob = ".config/nvim/**/*"
dst = "${HOME}/.config/nvim/"
platform = "linux"

[[template]]
src = ".gitconfig.local.template"
dst = "${HOME}/.gitconfig.local"
prompts = ["git"]

[prompts.git]
heading = "Git identity"
writer = "gitconfig"
fields = [
  { key = "name",  prompt = "Your name" },
  { key = "email", prompt = "Your email" },
  { key = "key",   prompt = "GPG key", optional = true, default_from = "field:email" },
  { key = "sign",  prompt = "Sign commits?", type = "bool", default = false, requires = "key" },
]

[[deps]]
group = "core"
pacman = ["alacritty", "fish"]
brew   = ["alacritty", "fish"]

[[hook]]
name = "fisher"
when = "post-update"
if   = "command_exists:fish"
run  = ["fish", "-c", "fisher update"]
ignore_failure = true

[[command]]
group = "menu"
name  = "wifi"
platform = "linux"
description = "Pick + connect to a WiFi network"
steps = [
  { run = ["nmcli", "-t", "device", "wifi", "list"], capture = "list" },
  { pipe = ["rofi", "-dmenu"], input = "{list}", capture = "ssid" },
  { run = ["nmcli", "device", "wifi", "connect", "{ssid}"], on_fail = "notify" },
]
"#);
        assert_eq!(cfg.meta.name, "test");
        assert_eq!(cfg.links.len(), 2);
        assert_eq!(cfg.templates.len(), 1);
        assert_eq!(cfg.prompts["git"].fields.len(), 4);
        assert_eq!(cfg.commands.len(), 1);
        assert_eq!(cfg.commands[0].steps.len(), 3);
    }
}
