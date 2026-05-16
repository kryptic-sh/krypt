//! Predicate grammar + evaluator for `if =` conditions in step definitions.
//!
//! # Grammar
//!
//! | Form | Meaning |
//! |------|---------|
//! | `command_exists:<name>` | command is on `PATH` |
//! | `env:VAR` | env var is present (any value, including empty) |
//! | `env:VAR=value` | env var is present **and** equals `value` |
//! | `platform:linux` / `platform:macos` / `platform:windows` | current OS |
//! | `file_exists:<path>` | path exists after `${VAR}` resolution |
//! | `!<predicate>` | negation of any of the above |
//! | `a,b,c` | logical AND; whitespace around commas is allowed |
//!
//! Negation binds tighter than AND. An empty predicate string evaluates to
//! `Ok(true)` (vacuously true). OR (`||`) is not supported yet — tracked as a
//! follow-up.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::paths::{Platform, ResolveError, Resolver};

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Everything that can go wrong while parsing or evaluating a predicate string.
#[derive(Debug, Error)]
pub enum PredicateError {
    /// A predicate kind that isn't part of the grammar (e.g. `weather:sunny`).
    #[error("unknown predicate kind `{kind}`")]
    UnknownKind {
        /// The unrecognised kind name (text before the colon).
        kind: String,
    },

    /// The predicate string is syntactically invalid.
    #[error("malformed predicate `{input}`: {reason}")]
    Malformed {
        /// The exact input that failed.
        input: String,
        /// Why it failed.
        reason: String,
    },

    /// Variable resolution inside a `file_exists:` path failed.
    #[error("resolve error: {source}")]
    Resolve {
        /// The underlying resolver error.
        #[source]
        source: Box<ResolveError>,
    },
}

impl From<ResolveError> for PredicateError {
    fn from(e: ResolveError) -> Self {
        Self::Resolve {
            source: Box::new(e),
        }
    }
}

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Environment abstraction used by the predicate evaluator.
///
/// Implement this trait to control what the evaluator sees. The default
/// production implementation is [`DefaultPredicateEnv`]; a mock suitable for
/// unit tests is [`MockPredicateEnv`].
pub trait PredicateEnv {
    /// The platform to match `platform:linux` / `platform:macos` /
    /// `platform:windows` against.
    fn platform(&self) -> Platform;

    /// Look up an environment variable. Returns `None` when the variable is
    /// not set (as opposed to set-but-empty, which returns `Some("")`).
    fn env(&self, var: &str) -> Option<String>;

    /// Returns `true` when `name` is found on `PATH`.
    fn command_exists(&self, name: &str) -> bool;

    /// Returns `true` when `path` exists on the filesystem.
    fn file_exists(&self, path: &Path) -> bool;

    /// The [`Resolver`] used to expand `${VAR}` tokens in `file_exists:` paths.
    fn resolver(&self) -> &Resolver;
}

// ─── DefaultPredicateEnv ─────────────────────────────────────────────────────

/// Production [`PredicateEnv`] backed by the real OS.
pub struct DefaultPredicateEnv {
    resolver: Resolver,
}

impl DefaultPredicateEnv {
    /// Create with a freshly-detected platform and real process env.
    pub fn new() -> Self {
        Self {
            resolver: Resolver::new(),
        }
    }

    /// Create with a specific [`Resolver`] (useful for tests that want a
    /// controlled env snapshot without full mocking).
    pub fn with_resolver(resolver: Resolver) -> Self {
        Self { resolver }
    }
}

impl Default for DefaultPredicateEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl PredicateEnv for DefaultPredicateEnv {
    fn platform(&self) -> Platform {
        Platform::current()
    }

    fn env(&self, var: &str) -> Option<String> {
        std::env::var(var).ok()
    }

    fn command_exists(&self, name: &str) -> bool {
        which::which(name).is_ok()
    }

    fn file_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn resolver(&self) -> &Resolver {
        &self.resolver
    }
}

// ─── MockPredicateEnv ────────────────────────────────────────────────────────

/// Test double for [`PredicateEnv`].
///
/// Build an instance, populate the public fields, and pass a reference to
/// [`eval`]. All lookups are pure in-memory — the host filesystem, PATH, and
/// environment are never consulted.
pub struct MockPredicateEnv {
    /// Platform reported to `platform:` predicates.
    pub platform: Platform,
    /// Env vars available to `env:` predicates.
    pub env: BTreeMap<String, String>,
    /// Commands that are considered to exist for `command_exists:` predicates.
    pub commands: BTreeSet<String>,
    /// Paths that are considered to exist for `file_exists:` predicates.
    pub files: BTreeSet<PathBuf>,
    /// Resolver used to expand `${VAR}` tokens in `file_exists:` paths.
    pub resolver: Resolver,
}

impl MockPredicateEnv {
    /// Create a blank mock on the given platform.
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            env: BTreeMap::new(),
            commands: BTreeSet::new(),
            files: BTreeSet::new(),
            resolver: Resolver::for_platform(platform),
        }
    }
}

impl PredicateEnv for MockPredicateEnv {
    fn platform(&self) -> Platform {
        self.platform
    }

    fn env(&self, var: &str) -> Option<String> {
        self.env.get(var).cloned()
    }

    fn command_exists(&self, name: &str) -> bool {
        self.commands.contains(name)
    }

    fn file_exists(&self, path: &Path) -> bool {
        self.files.contains(path)
    }

    fn resolver(&self) -> &Resolver {
        &self.resolver
    }
}

// ─── Evaluator ───────────────────────────────────────────────────────────────

/// Evaluate a predicate string against the supplied environment.
///
/// An empty string returns `Ok(true)` — no condition means no constraint.
/// A comma-separated list (`a,b,c`) is a logical AND; all terms must be true.
/// Whitespace around commas is ignored. Negation (`!`) is a prefix on any atom.
pub fn eval(predicate: &str, env: &dyn PredicateEnv) -> Result<bool, PredicateError> {
    if predicate.is_empty() {
        return Ok(true);
    }

    for term in predicate.split(',') {
        let term = term.trim();

        if term.is_empty() {
            return Err(PredicateError::Malformed {
                input: predicate.to_owned(),
                reason: "empty term after splitting by `,` (consecutive commas?)".to_owned(),
            });
        }

        if !eval_atom(term, predicate, env)? {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Evaluate a single (possibly negated) atom against the environment.
fn eval_atom(atom: &str, original: &str, env: &dyn PredicateEnv) -> Result<bool, PredicateError> {
    if let Some(inner) = atom.strip_prefix('!') {
        if inner.is_empty() {
            return Err(PredicateError::Malformed {
                input: original.to_owned(),
                reason: "`!` must be followed by a predicate, not end-of-input".to_owned(),
            });
        }
        return eval_atom(inner, original, env).map(|v| !v);
    }

    let (kind, arg) = atom
        .split_once(':')
        .ok_or_else(|| PredicateError::Malformed {
            input: original.to_owned(),
            reason: format!("`{atom}` has no `:` separator — expected `<kind>:<arg>`"),
        })?;

    match kind {
        "command_exists" => {
            if arg.is_empty() {
                return Err(PredicateError::Malformed {
                    input: original.to_owned(),
                    reason: "`command_exists:` requires a non-empty command name".to_owned(),
                });
            }
            Ok(env.command_exists(arg))
        }

        "env" => {
            if arg.is_empty() {
                return Err(PredicateError::Malformed {
                    input: original.to_owned(),
                    reason: "`env:` requires a variable name".to_owned(),
                });
            }
            if let Some((var, expected)) = arg.split_once('=') {
                // env:VAR=value — present AND matches
                Ok(env.env(var).as_deref() == Some(expected))
            } else {
                // env:VAR — present with any value (including empty string)
                Ok(env.env(arg).is_some())
            }
        }

        "platform" => {
            let expected = match arg {
                "linux" => Platform::Linux,
                "macos" => Platform::Macos,
                "windows" => Platform::Windows,
                other => {
                    return Err(PredicateError::Malformed {
                        input: original.to_owned(),
                        reason: format!(
                            "`platform:{other}` is not a recognised platform; \
                             use `linux`, `macos`, or `windows`"
                        ),
                    });
                }
            };
            Ok(env.platform() == expected)
        }

        "file_exists" => {
            if arg.is_empty() {
                return Err(PredicateError::Malformed {
                    input: original.to_owned(),
                    reason: "`file_exists:` requires a path".to_owned(),
                });
            }
            let resolved = env.resolver().resolve(arg)?;
            Ok(env.file_exists(Path::new(&resolved)))
        }

        other => Err(PredicateError::UnknownKind {
            kind: other.to_owned(),
        }),
    }
}

// ─── Runner convenience adapter ──────────────────────────────────────────────

/// Wrap a [`PredicateEnv`] into the closure signature expected by
/// [`crate::runner::execute_steps`].
///
/// Parse errors are swallowed (logged as `tracing::warn!`) and the step is
/// skipped (`false`). This matches the failing-closed principle: a config bug
/// is surfaced as a warning, not a process abort.
pub fn default_predicate_evaluator(
    env: impl PredicateEnv + 'static,
) -> impl Fn(&str, &crate::runner::Context) -> bool {
    move |pred, _ctx| match eval(pred, &env) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(predicate = pred, error = %e, "predicate eval error — skipping step");
            false
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::paths::Resolver;

    fn mock(platform: Platform) -> MockPredicateEnv {
        MockPredicateEnv::new(platform)
    }

    fn linux() -> MockPredicateEnv {
        mock(Platform::Linux)
    }

    fn macos() -> MockPredicateEnv {
        mock(Platform::Macos)
    }

    fn windows_env() -> MockPredicateEnv {
        mock(Platform::Windows)
    }

    // ── 1. command_exists ─────────────────────────────────────────────────────

    #[test]
    fn command_exists_false_when_not_in_set() {
        let env = linux();
        assert!(!eval("command_exists:nonexistent_binary_xyz", &env).unwrap());
    }

    #[test]
    fn command_exists_true_when_in_set() {
        let mut env = linux();
        env.commands.insert("my_tool".to_owned());
        assert!(eval("command_exists:my_tool", &env).unwrap());
    }

    // ── 2. env:VAR ────────────────────────────────────────────────────────────

    #[test]
    fn env_var_true_when_set() {
        let mut env = linux();
        env.env.insert("HOME".to_owned(), "/home/user".to_owned());
        assert!(eval("env:HOME", &env).unwrap());
    }

    #[test]
    fn env_var_false_when_not_set() {
        let env = linux();
        assert!(!eval("env:DOES_NOT_EXIST_XYZ", &env).unwrap());
    }

    #[test]
    fn env_var_true_when_set_to_empty() {
        let mut env = linux();
        env.env.insert("EMPTY_VAR".to_owned(), String::new());
        // present-but-empty still counts as present
        assert!(eval("env:EMPTY_VAR", &env).unwrap());
    }

    // ── 3. env:VAR=value ─────────────────────────────────────────────────────

    #[test]
    fn env_value_match_true() {
        let mut env = linux();
        env.env.insert("USER".to_owned(), "root".to_owned());
        assert!(eval("env:USER=root", &env).unwrap());
    }

    #[test]
    fn env_value_match_false_when_different() {
        let mut env = linux();
        env.env.insert("USER".to_owned(), "alice".to_owned());
        assert!(!eval("env:USER=root", &env).unwrap());
    }

    #[test]
    fn env_value_match_false_when_unset() {
        let env = linux();
        assert!(!eval("env:USER=root", &env).unwrap());
    }

    // ── 4. platform ───────────────────────────────────────────────────────────

    #[test]
    fn platform_linux_true_on_linux() {
        let env = linux();
        assert!(eval("platform:linux", &env).unwrap());
    }

    #[test]
    fn platform_linux_false_on_macos() {
        let env = macos();
        assert!(!eval("platform:linux", &env).unwrap());
    }

    #[test]
    fn platform_macos_true_on_macos() {
        let env = macos();
        assert!(eval("platform:macos", &env).unwrap());
    }

    #[test]
    fn platform_windows_true_on_windows() {
        let env = windows_env();
        assert!(eval("platform:windows", &env).unwrap());
    }

    // ── 5. file_exists ────────────────────────────────────────────────────────

    #[test]
    fn file_exists_true_when_in_set() {
        let mut env = linux();
        env.files.insert(PathBuf::from("/etc/passwd"));
        assert!(eval("file_exists:/etc/passwd", &env).unwrap());
    }

    #[test]
    fn file_exists_false_when_not_in_set() {
        let env = linux();
        assert!(!eval("file_exists:/no/such/file", &env).unwrap());
    }

    #[test]
    fn file_exists_resolves_env_var_before_checking() {
        let mut env = linux();
        // Provide a resolver with HOME set so ${HOME} expands.
        let resolver = Resolver::for_platform(Platform::Linux).with_env(HashMap::from([(
            "HOME".to_owned(),
            "/home/testuser".to_owned(),
        )]));
        env.resolver = resolver;
        env.files.insert(PathBuf::from("/home/testuser/.bashrc"));
        assert!(eval("file_exists:${HOME}/.bashrc", &env).unwrap());
    }

    // ── 6. Negation ───────────────────────────────────────────────────────────

    #[test]
    fn negation_of_false_is_true() {
        let env = linux();
        assert!(eval("!command_exists:nonexistent_xyz", &env).unwrap());
    }

    #[test]
    fn negation_of_true_is_false() {
        let env = linux();
        assert!(!eval("!platform:linux", &env).unwrap());
    }

    // ── 7. AND (comma-separated) ──────────────────────────────────────────────

    #[test]
    fn and_both_true_is_true() {
        let mut env = linux();
        env.commands.insert("sh".to_owned());
        assert!(eval("platform:linux,command_exists:sh", &env).unwrap());
    }

    #[test]
    fn and_one_false_is_false() {
        let env = linux();
        // sh is NOT in commands set → second term is false
        assert!(!eval("platform:linux,command_exists:sh", &env).unwrap());
    }

    #[test]
    fn and_three_terms_all_true() {
        let mut env = linux();
        env.commands.insert("sh".to_owned());
        env.commands.insert("ls".to_owned());
        assert!(eval("platform:linux,command_exists:sh,command_exists:ls", &env).unwrap());
    }

    #[test]
    fn and_three_terms_one_false() {
        let mut env = linux();
        env.commands.insert("sh".to_owned());
        // ls NOT in set
        assert!(!eval("platform:linux,command_exists:sh,command_exists:ls", &env).unwrap());
    }

    // ── 8. Whitespace around commas ───────────────────────────────────────────

    #[test]
    fn whitespace_around_commas_accepted() {
        let mut env = linux();
        env.commands.insert("sh".to_owned());
        assert!(eval("platform:linux , command_exists:sh", &env).unwrap());
    }

    // ── 9. Empty predicate ────────────────────────────────────────────────────

    #[test]
    fn empty_predicate_is_vacuously_true() {
        let env = linux();
        assert!(eval("", &env).unwrap());
    }

    // ── 10. Empty after split ─────────────────────────────────────────────────

    #[test]
    fn empty_after_split_is_malformed() {
        let env = linux();
        assert!(matches!(
            eval("platform:linux,,command_exists:sh", &env),
            Err(PredicateError::Malformed { .. })
        ));
    }

    // ── 11. Unknown kind ──────────────────────────────────────────────────────

    #[test]
    fn unknown_kind_is_error() {
        let env = linux();
        assert!(matches!(
            eval("weather:sunny", &env),
            Err(PredicateError::UnknownKind { kind }) if kind == "weather"
        ));
    }

    // ── 12. Malformed predicates ──────────────────────────────────────────────

    #[test]
    fn no_colon_is_malformed() {
        let env = linux();
        assert!(matches!(
            eval("command_exists", &env),
            Err(PredicateError::Malformed { .. })
        ));
    }

    #[test]
    fn env_empty_var_name_is_malformed() {
        let env = linux();
        assert!(matches!(
            eval("env:", &env),
            Err(PredicateError::Malformed { .. })
        ));
    }

    // ── 13. Negation of empty ─────────────────────────────────────────────────

    #[test]
    fn negation_of_empty_is_malformed() {
        let env = linux();
        assert!(matches!(
            eval("!", &env),
            Err(PredicateError::Malformed { .. })
        ));
    }

    // ── 14. Negation combined with AND ────────────────────────────────────────

    #[test]
    fn negation_and_combo() {
        // !platform:linux on linux → false; combo is false
        let mut env = linux();
        env.commands.insert("rofi".to_owned());
        assert!(!eval("!platform:linux,command_exists:rofi", &env).unwrap());
    }

    #[test]
    fn negation_and_combo_true_on_macos() {
        // !platform:linux on macos → true; rofi present → true → AND true
        let mut env = macos();
        env.commands.insert("rofi".to_owned());
        assert!(eval("!platform:linux,command_exists:rofi", &env).unwrap());
    }
}
