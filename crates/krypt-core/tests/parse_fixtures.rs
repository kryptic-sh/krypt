//! Integration tests that exercise [`krypt_core::config::parse_file`]
//! against on-disk fixtures.
//!
//! Lives in `tests/` so the parser API is exercised exactly the way a
//! downstream consumer (`krypt-cli`, third-party reuse) would.

use std::path::PathBuf;

use krypt_core::config::{parse_file, ConfigError};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn kitchen_sink_parses() {
    let cfg = parse_file(fixture("kitchen-sink.toml")).expect("kitchen-sink should parse");
    assert_eq!(cfg.meta.name, "kitchen-sink dotfiles");
    assert_eq!(cfg.links.len(), 3);
    assert_eq!(cfg.templates.len(), 2);
    assert_eq!(cfg.prompts.len(), 2);
    assert_eq!(cfg.deps.len(), 2);
    assert_eq!(cfg.hooks.len(), 2);
    assert_eq!(cfg.commands.len(), 2);
    assert_eq!(cfg.include.len(), 2);
}

#[test]
fn invalid_unknown_field_errors() {
    let e = parse_file(fixture("invalid-unknown-field.toml")).unwrap_err();
    assert!(matches!(e, ConfigError::Toml { .. }), "got: {e:?}");
}

#[test]
fn invalid_link_both_srcs_errors() {
    let e = parse_file(fixture("invalid-link-both-srcs.toml")).unwrap_err();
    assert!(matches!(e, ConfigError::Validation { .. }), "got: {e:?}");
}

#[test]
fn invalid_link_no_src_errors() {
    let e = parse_file(fixture("invalid-link-no-src.toml")).unwrap_err();
    assert!(matches!(e, ConfigError::Validation { .. }), "got: {e:?}");
}

#[test]
fn invalid_platform_errors() {
    let e = parse_file(fixture("invalid-platform.toml")).unwrap_err();
    let msg = format!("{e}");
    assert!(msg.contains("freebsd"), "got: {msg}");
}

#[test]
fn invalid_step_multiple_kinds_errors() {
    let e = parse_file(fixture("invalid-step-multiple-kinds.toml")).unwrap_err();
    let msg = format!("{e}");
    assert!(msg.contains("multiple kinds"), "got: {msg}");
}

#[test]
fn invalid_prompt_bad_requires_errors() {
    let e = parse_file(fixture("invalid-prompt-bad-requires.toml")).unwrap_err();
    let msg = format!("{e}");
    assert!(msg.contains("requires"), "got: {msg}");
}
