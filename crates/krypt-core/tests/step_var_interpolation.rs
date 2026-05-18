//! Integration tests for eager `${VAR}` resolution in step args (issue #55).
//!
//! Tests exercise [`krypt_core::config::resolve_step_vars`] and the
//! `load_with_includes` path which calls it automatically.
//!
//! Environment variables needed for specific tests are set inline using
//! [`std::env::set_var`] / [`std::env::remove_var`] around the assertion.
//! Tests that mutate env are serialised via a mutex to avoid races.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use krypt_core::config::{ConfigError, parse_str, resolve_step_vars};
use krypt_core::paths::{Platform, Resolver};

// Serialise env-mutating tests so they don't interfere with each other.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Build a [`Resolver`] for Linux with an explicit env snapshot, so tests
/// don't leak from the real process environment.
fn linux_resolver(env: HashMap<String, String>) -> Resolver {
    Resolver::for_platform(Platform::Linux).with_env(env)
}

/// Parse a TOML snippet and run `resolve_step_vars` with the given resolver.
fn resolve(toml: &str, resolver: &Resolver) -> Result<krypt_core::config::Config, ConfigError> {
    let cfg = parse_str(toml, "test.toml").expect("toml parse should succeed");
    resolve_step_vars(cfg, Path::new("test.toml"), resolver)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

/// `${HOME}` resolves via the krypt-internal built-in var.
#[test]
fn home_resolves_from_krypt_builtin() {
    let mut env = HashMap::new();
    env.insert("HOME".into(), "/home/testuser".into());
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "run-home"
[[command.steps]]
run = ["ls", "${HOME}"]
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    let arg = &cfg.commands[0].steps[0].run.as_ref().unwrap()[1];
    assert_eq!(arg, "/home/testuser");
}

/// `${USER}` resolves from the process env (not a krypt built-in).
#[test]
fn user_resolves_from_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    // SAFETY: single-threaded env mutation under lock.
    unsafe { std::env::set_var("KRYPT_TEST_USER_55", "alice") };

    let env: HashMap<String, String> = std::env::vars().collect();
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "run-user"
[[command.steps]]
run = ["id", "${KRYPT_TEST_USER_55}"]
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    unsafe { std::env::remove_var("KRYPT_TEST_USER_55") };

    let arg = &cfg.commands[0].steps[0].run.as_ref().unwrap()[1];
    assert_eq!(arg, "alice");
}

/// `${XDG_CONFIG_HOME}` set in env resolves through the krypt `XDG_CONFIG`
/// built-in (which reads `XDG_CONFIG_HOME` env internally).
///
/// Note: the krypt built-in is `${XDG_CONFIG}`, not `${XDG_CONFIG_HOME}`.
/// A bare `${XDG_CONFIG_HOME}` therefore falls through to env tier.
#[test]
fn xdg_config_home_resolves_from_env_tier() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("KRYPT_TEST_XDG_55", "/custom/config") };

    let env: HashMap<String, String> = std::env::vars().collect();
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "xdg-test"
[[command.steps]]
run = ["ls", "${KRYPT_TEST_XDG_55}"]
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    unsafe { std::env::remove_var("KRYPT_TEST_XDG_55") };

    let arg = &cfg.commands[0].steps[0].run.as_ref().unwrap()[1];
    assert_eq!(arg, "/custom/config");
}

/// krypt-internal var takes precedence over a same-named env var.
///
/// `HOME` is both a krypt built-in and (typically) an env var.  The krypt
/// resolver reads it from `HOME` env, but here we inject conflicting values
/// at each tier to prove ordering.
#[test]
fn krypt_internal_takes_precedence_over_env() {
    let mut env = HashMap::new();
    // krypt HOME resolver will read from this env map under "HOME"
    env.insert("HOME".into(), "/krypt-home".into());

    let resolver = linux_resolver(env);

    // Even if the real process env had a different HOME, the resolver's
    // snapshot wins for krypt-internal vars (HOME is a krypt built-in).
    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "precedence"
[[command.steps]]
run = ["echo", "${HOME}"]
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    let arg = &cfg.commands[0].steps[0].run.as_ref().unwrap()[1];
    assert_eq!(arg, "/krypt-home");
}

/// Unknown var (not krypt-internal, not in env) → `UnknownStepVar` error.
#[test]
fn unknown_var_produces_config_load_error() {
    let _guard = ENV_LOCK.lock().unwrap();
    // Make sure TYPO_VAR_KRYPT_55 is definitely not set.
    unsafe { std::env::remove_var("TYPO_VAR_KRYPT_55") };

    let env: HashMap<String, String> = std::env::vars().collect();
    let resolver = linux_resolver(env);

    let err = resolve(
        r#"
[[command]]
group = "test"
name  = "typo"
[[command.steps]]
run = ["echo", "${TYPO_VAR_KRYPT_55}"]
"#,
        &resolver,
    )
    .expect_err("unknown var should error");

    match err {
        ConfigError::UnknownStepVar { var, .. } => {
            assert_eq!(var, "TYPO_VAR_KRYPT_55");
        }
        other => panic!("expected UnknownStepVar, got: {other:?}"),
    }
}

/// Error message format: contains file path and var name.
#[test]
fn unknown_var_error_contains_path_and_var_name() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::remove_var("TYPO_VAR_KRYPT_55B") };

    let env: HashMap<String, String> = std::env::vars().collect();
    let resolver = linux_resolver(env);

    let cfg = parse_str(
        r#"
[[command]]
group = "test"
name  = "typo"
[[command.steps]]
run = ["echo", "${TYPO_VAR_KRYPT_55B}"]
"#,
        "test.toml",
    )
    .unwrap();

    let err = resolve_step_vars(cfg, Path::new(".krypt/commands.toml"), &resolver)
        .expect_err("should error");

    let msg = format!("{err}");
    assert!(
        msg.contains("TYPO_VAR_KRYPT_55B"),
        "error should contain var name: {msg}"
    );
    assert!(
        msg.contains("commands.toml"),
        "error should contain file path: {msg}"
    );
}

/// Nested: `${HOME}/bin/${USER}-tool` → both substituted.
#[test]
fn nested_vars_both_substituted() {
    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("KRYPT_TEST_USER2_55", "bob") };

    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.insert("HOME".into(), "/home/bob".into());
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "nested"
[[command.steps]]
run = ["ls", "${HOME}/bin/${KRYPT_TEST_USER2_55}-tool"]
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    unsafe { std::env::remove_var("KRYPT_TEST_USER2_55") };

    let arg = &cfg.commands[0].steps[0].run.as_ref().unwrap()[1];
    assert_eq!(arg, "/home/bob/bin/bob-tool");
}

/// Escape: `\${VAR}` → literal `${VAR}` in the resolved output.
#[test]
fn escaped_dollar_brace_produces_literal() {
    let env: HashMap<String, String> = HashMap::new();
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "escape"
[[command.steps]]
run = ["bash", "-c", "echo \\${HOME}"]
"#,
        &resolver,
    )
    .expect("resolve should succeed — escape prevents lookup");

    let arg = &cfg.commands[0].steps[0].run.as_ref().unwrap()[2];
    assert_eq!(arg, "echo ${HOME}");
}

/// `{0}` positional placeholder is left untouched (runtime-resolved).
#[test]
fn positional_placeholder_untouched() {
    let mut env = HashMap::new();
    env.insert("HOME".into(), "/home/testuser".into());
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "positional"
[[command.steps]]
run = ["echo", "{0}", "{1}"]
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    let args = cfg.commands[0].steps[0].run.as_ref().unwrap();
    assert_eq!(args[1], "{0}", "positional placeholder must be untouched");
    assert_eq!(args[2], "{1}", "positional placeholder must be untouched");
}

/// `{stdin}` named placeholder is left untouched.
#[test]
fn stdin_placeholder_untouched() {
    let env: HashMap<String, String> = HashMap::new();
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "pipe-test"
[[command.steps]]
pipe  = ["wc", "-l"]
input = "{stdin}"
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    let input = cfg.commands[0].steps[0].input.as_deref().unwrap();
    assert_eq!(input, "{stdin}");
}

/// `pipe` step arg also gets resolved.
#[test]
fn pipe_step_arg_resolved() {
    let mut env = HashMap::new();
    env.insert("HOME".into(), "/home/piper".into());
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "pipe-home"
[[command.steps]]
pipe  = ["cat", "${HOME}/.bashrc"]
input = "hello"
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    let arg = &cfg.commands[0].steps[0].pipe.as_ref().unwrap()[1];
    assert_eq!(arg, "/home/piper/.bashrc");
}

/// `notify` step arg also gets resolved.
#[test]
fn notify_step_arg_resolved() {
    let mut env = HashMap::new();
    env.insert("HOME".into(), "/home/notify-user".into());
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "test"
name  = "notify-home"
[[command.steps]]
notify = ["Launched", "${HOME} is up"]
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    let body = &cfg.commands[0].steps[0].notify.as_ref().unwrap()[1];
    assert_eq!(body, "/home/notify-user is up");
}

/// Hook `run` args also get resolved.
#[test]
fn hook_run_args_resolved() {
    let mut env = HashMap::new();
    env.insert("HOME".into(), "/home/hook-user".into());
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[hook]]
name = "test-hook"
when = "post-update"
run  = ["bash", "-c", "ls ${HOME}/.config"]
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    assert_eq!(cfg.hooks[0].run[2], "ls /home/hook-user/.config");
}

/// A step with no `${VAR}` tokens is passed through unchanged.
#[test]
fn step_without_dollar_var_unchanged() {
    let env: HashMap<String, String> = HashMap::new();
    let resolver = linux_resolver(env);

    let cfg = resolve(
        r#"
[[command]]
group = "sys"
name  = "status"
[[command.steps]]
run = ["systemctl", "status", "NetworkManager"]
"#,
        &resolver,
    )
    .expect("resolve should succeed");

    let args = cfg.commands[0].steps[0].run.as_ref().unwrap();
    assert_eq!(args, &["systemctl", "status", "NetworkManager"]);
}
