//! `${...}` expansion engine.

use std::collections::{BTreeMap, HashMap, HashSet};

use thiserror::Error;

use super::platform::Platform;

/// Things that can go wrong while resolving a path expression.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResolveError {
    /// The expression couldn't be tokenized (unclosed `${`, empty `${}`, ...).
    #[error("malformed path expression `{input}`: {reason}")]
    Malformed {
        /// The exact expression that failed.
        input: String,
        /// What went wrong.
        reason: String,
    },

    /// `${NAME}` referenced a var we don't know about.
    #[error("unknown path variable `${{{name}}}`")]
    UnknownVar {
        /// Name of the missing var.
        name: String,
    },

    /// Var resolution looped back on itself.
    #[error("cycle resolving `${{{name}}}`: {chain}")]
    Cycle {
        /// Var that started the cycle.
        name: String,
        /// `a -> b -> c -> a` style chain for the error message.
        chain: String,
    },

    /// `${WIN_*}` used outside Windows or `${MAC_*}` outside macOS.
    #[error("`${{{name}}}` is only available on {required} (current platform: {current})")]
    WrongPlatform {
        /// Var that's platform-gated.
        name: String,
        /// Platform that var is available on.
        required: Platform,
        /// Platform we're actually running on.
        current: Platform,
    },
}

/// Resolver for `${...}` expressions.
///
/// Build with [`Resolver::new`] for production use; the [`Resolver::for_platform`]
/// and `with_*` builders are for tests and explicit overrides.
#[derive(Debug, Clone)]
pub struct Resolver {
    platform: Platform,
    overrides: BTreeMap<String, String>,
    env: HashMap<String, String>,
}

impl Resolver {
    /// Auto-detect platform and snapshot the current process env.
    pub fn new() -> Self {
        Self {
            platform: Platform::current(),
            overrides: BTreeMap::new(),
            env: std::env::vars().collect(),
        }
    }

    /// Construct with an explicit platform. Useful in tests.
    pub fn for_platform(platform: Platform) -> Self {
        Self {
            platform,
            overrides: BTreeMap::new(),
            env: std::env::vars().collect(),
        }
    }

    /// Replace the env snapshot used for `${env:...}` lookups. Useful in
    /// tests to avoid leaking the host's environment in.
    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// Layer `[paths]`-style overrides on top of the built-in defaults.
    /// Override values may themselves contain `${...}` expressions.
    pub fn with_overrides(mut self, overrides: BTreeMap<String, String>) -> Self {
        self.overrides = overrides;
        self
    }

    /// Resolve a string containing zero or more `${...}` expressions.
    pub fn resolve(&self, input: &str) -> Result<String, ResolveError> {
        self.resolve_with_stack(input, &mut Vec::new())
    }

    /// Resolve a single bare variable name (no `${}` syntax) to its value.
    /// `${NAME}` in a template eventually calls this.
    pub fn resolve_var(&self, name: &str) -> Result<String, ResolveError> {
        self.resolve_var_with_stack(name, &mut Vec::new())
    }

    /// Iterate over every variable name the resolver knows how to expand —
    /// built-ins available on the current platform, plus all overrides.
    /// Useful for `krypt paths` and `krypt doctor`.
    pub fn known_vars(&self) -> Vec<String> {
        let mut names: HashSet<String> = self
            .builtin_names()
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        names.extend(self.overrides.keys().cloned());
        let mut out: Vec<String> = names.into_iter().collect();
        out.sort();
        out
    }

    /// Names of built-in vars defined on the current platform.
    fn builtin_names(&self) -> &'static [&'static str] {
        match self.platform {
            Platform::Windows => &[
                "HOME",
                "XDG_CONFIG",
                "XDG_DATA",
                "XDG_STATE",
                "XDG_CACHE",
                "XDG_RUNTIME",
                "LOCAL_BIN",
                "DOCUMENTS",
                "WIN_LOCALAPPDATA",
                "WIN_APPDATA",
            ],
            Platform::Macos => &[
                "HOME",
                "XDG_CONFIG",
                "XDG_DATA",
                "XDG_STATE",
                "XDG_CACHE",
                "XDG_RUNTIME",
                "LOCAL_BIN",
                "DOCUMENTS",
                "MAC_LIBRARY",
            ],
            Platform::Linux => &[
                "HOME",
                "XDG_CONFIG",
                "XDG_DATA",
                "XDG_STATE",
                "XDG_CACHE",
                "XDG_RUNTIME",
                "LOCAL_BIN",
                "DOCUMENTS",
            ],
        }
    }

    // ─── Internals ─────────────────────────────────────────────────────

    fn resolve_with_stack(
        &self,
        input: &str,
        stack: &mut Vec<String>,
    ) -> Result<String, ResolveError> {
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(idx) = rest.find("${") {
            out.push_str(&rest[..idx]);
            rest = &rest[idx + 2..]; // skip `${`
                                     // Find the matching `}` accounting for nested `${...}` (needed
                                     // for `${env:VAR:-${HOME}/sub}` patterns).
            let end = find_matching_brace(rest).ok_or_else(|| ResolveError::Malformed {
                input: input.into(),
                reason: "unclosed `${`".into(),
            })?;
            let expr = &rest[..end];
            if expr.is_empty() {
                return Err(ResolveError::Malformed {
                    input: input.into(),
                    reason: "empty `${}`".into(),
                });
            }
            let resolved = self.resolve_expr(expr, stack)?;
            out.push_str(&resolved);
            rest = &rest[end + 1..]; // skip `}`
        }
        out.push_str(rest);
        Ok(out)
    }

    /// One `${...}` body: either `NAME`, `env:NAME`, or `env:NAME:-fallback`.
    fn resolve_expr(&self, expr: &str, stack: &mut Vec<String>) -> Result<String, ResolveError> {
        if let Some(env_expr) = expr.strip_prefix("env:") {
            let (name, fallback) = match env_expr.split_once(":-") {
                Some((n, fb)) => (n, Some(fb)),
                None => (env_expr, None),
            };
            if name.is_empty() {
                return Err(ResolveError::Malformed {
                    input: format!("${{{expr}}}"),
                    reason: "empty env var name".into(),
                });
            }
            return match self.env.get(name) {
                Some(v) if !v.is_empty() => Ok(v.clone()),
                _ => match fallback {
                    Some(fb) => self.resolve_with_stack(fb, stack),
                    None => Ok(String::new()),
                },
            };
        }
        self.resolve_var_with_stack(expr, stack)
    }

    fn resolve_var_with_stack(
        &self,
        name: &str,
        stack: &mut Vec<String>,
    ) -> Result<String, ResolveError> {
        if stack.iter().any(|n| n == name) {
            let chain = stack
                .iter()
                .chain(std::iter::once(&name.to_string()))
                .cloned()
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(ResolveError::Cycle {
                name: name.into(),
                chain,
            });
        }
        stack.push(name.into());
        let result = self.lookup_var(name, stack);
        stack.pop();
        result
    }

    fn lookup_var(&self, name: &str, stack: &mut Vec<String>) -> Result<String, ResolveError> {
        if let Some(template) = self.overrides.get(name) {
            return self.resolve_with_stack(template, stack);
        }
        self.builtin_var(name, stack)
    }

    fn builtin_var(&self, name: &str, stack: &mut Vec<String>) -> Result<String, ResolveError> {
        // Platform-gated first — error helpfully if used on the wrong OS.
        match name {
            "WIN_LOCALAPPDATA" | "WIN_APPDATA" => {
                if self.platform != Platform::Windows {
                    return Err(ResolveError::WrongPlatform {
                        name: name.into(),
                        required: Platform::Windows,
                        current: self.platform,
                    });
                }
            }
            "MAC_LIBRARY" => {
                if self.platform != Platform::Macos {
                    return Err(ResolveError::WrongPlatform {
                        name: name.into(),
                        required: Platform::Macos,
                        current: self.platform,
                    });
                }
            }
            _ => {}
        }

        // HOME is the root of every other built-in. Read directly from the
        // platform-appropriate env var. Override-driven HOME goes through
        // `lookup_var` upstream, so this branch only runs when there's no
        // override and we genuinely need the env value.
        let home_env_key = match self.platform {
            Platform::Windows => "USERPROFILE",
            Platform::Linux | Platform::Macos => "HOME",
        };
        let home_from_env = || -> Result<String, ResolveError> {
            self.env
                .get(home_env_key)
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or_else(|| ResolveError::UnknownVar { name: name.into() })
        };
        // For derived vars (XDG_*, LOCAL_BIN, DOCUMENTS, MAC_LIBRARY) we
        // re-enter the resolver so that overriding `HOME` also reroutes
        // everything that derives from it.
        let derived_from_home =
            |suffix: &str, stack: &mut Vec<String>| -> Result<String, ResolveError> {
                let h = self.resolve_var_with_stack("HOME", stack)?;
                Ok(format!("{h}{suffix}"))
            };
        let xdg = |env_key: &str,
                   fallback_suffix: &str,
                   stack: &mut Vec<String>|
         -> Result<String, ResolveError> {
            if let Some(v) = self.env.get(env_key) {
                if !v.is_empty() {
                    return Ok(v.clone());
                }
            }
            derived_from_home(fallback_suffix, stack)
        };

        match name {
            "HOME" => home_from_env(),
            "XDG_CONFIG" => xdg("XDG_CONFIG_HOME", "/.config", stack),
            "XDG_DATA" => xdg("XDG_DATA_HOME", "/.local/share", stack),
            "XDG_STATE" => xdg("XDG_STATE_HOME", "/.local/state", stack),
            "XDG_CACHE" => xdg("XDG_CACHE_HOME", "/.cache", stack),
            "XDG_RUNTIME" => {
                if let Some(v) = self.env.get("XDG_RUNTIME_DIR") {
                    if !v.is_empty() {
                        return Ok(v.clone());
                    }
                }
                let fallback_keys = match self.platform {
                    Platform::Windows => ["TEMP", "TMP"].as_slice(),
                    Platform::Linux | Platform::Macos => ["TMPDIR"].as_slice(),
                };
                for k in fallback_keys {
                    if let Some(v) = self.env.get(*k) {
                        if !v.is_empty() {
                            return Ok(v.clone());
                        }
                    }
                }
                Ok(match self.platform {
                    Platform::Windows => "C:/Windows/Temp".into(),
                    _ => "/tmp".into(),
                })
            }
            "LOCAL_BIN" => derived_from_home("/.local/bin", stack),
            "DOCUMENTS" => derived_from_home("/Documents", stack),
            "WIN_LOCALAPPDATA" => self
                .env
                .get("LOCALAPPDATA")
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or_else(|| ResolveError::UnknownVar { name: name.into() }),
            "WIN_APPDATA" => self
                .env
                .get("APPDATA")
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or_else(|| ResolveError::UnknownVar { name: name.into() }),
            "MAC_LIBRARY" => derived_from_home("/Library", stack),
            _ => Err(ResolveError::UnknownVar { name: name.into() }),
        }
    }
}

impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}

/// Find the byte index of the `}` that matches the opening `${` we just
/// consumed, treating nested `${...}` as opaque pairs. Returns `None` if
/// no matching close brace exists.
fn find_matching_brace(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: usize = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            depth += 1;
            i += 2;
            continue;
        }
        if bytes[i] == b'}' {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux(home: &str) -> Resolver {
        let mut env = HashMap::new();
        env.insert("HOME".into(), home.into());
        Resolver::for_platform(Platform::Linux).with_env(env)
    }

    fn windows(profile: &str) -> Resolver {
        let mut env = HashMap::new();
        env.insert("USERPROFILE".into(), profile.into());
        env.insert("LOCALAPPDATA".into(), format!("{profile}/AppData/Local"));
        env.insert("APPDATA".into(), format!("{profile}/AppData/Roaming"));
        Resolver::for_platform(Platform::Windows).with_env(env)
    }

    fn macos(home: &str) -> Resolver {
        let mut env = HashMap::new();
        env.insert("HOME".into(), home.into());
        Resolver::for_platform(Platform::Macos).with_env(env)
    }

    #[test]
    fn home_resolves() {
        let r = linux("/home/user");
        assert_eq!(r.resolve("${HOME}/x").unwrap(), "/home/user/x");
    }

    #[test]
    fn xdg_falls_back_to_default() {
        let r = linux("/home/user");
        assert_eq!(r.resolve("${XDG_CONFIG}").unwrap(), "/home/user/.config");
        assert_eq!(r.resolve("${XDG_DATA}").unwrap(), "/home/user/.local/share");
    }

    #[test]
    fn xdg_env_override_wins() {
        let mut env = HashMap::new();
        env.insert("HOME".into(), "/home/user".into());
        env.insert("XDG_CONFIG_HOME".into(), "/custom/cfg".into());
        let r = Resolver::for_platform(Platform::Linux).with_env(env);
        assert_eq!(r.resolve("${XDG_CONFIG}").unwrap(), "/custom/cfg");
    }

    #[test]
    fn env_passthrough() {
        let mut env = HashMap::new();
        env.insert("HOME".into(), "/h".into());
        env.insert("EDITOR".into(), "nvim".into());
        let r = Resolver::for_platform(Platform::Linux).with_env(env);
        assert_eq!(r.resolve("editor=${env:EDITOR}").unwrap(), "editor=nvim");
    }

    #[test]
    fn env_fallback_used_when_unset() {
        let r = linux("/h");
        assert_eq!(r.resolve("${env:NOPE:-default}").unwrap(), "default");
    }

    #[test]
    fn env_fallback_can_reference_other_vars() {
        let r = linux("/h");
        assert_eq!(r.resolve("${env:NOPE:-${HOME}/x}").unwrap(), "/h/x");
    }

    #[test]
    fn user_override_shadows_builtin() {
        let mut overrides = BTreeMap::new();
        overrides.insert("HOME".into(), "/custom".into());
        let r = linux("/orig").with_overrides(overrides);
        assert_eq!(r.resolve("${HOME}/x").unwrap(), "/custom/x");
    }

    #[test]
    fn home_override_cascades_into_derived_vars() {
        let mut overrides = BTreeMap::new();
        overrides.insert("HOME".into(), "/custom".into());
        let r = linux("/orig").with_overrides(overrides);
        assert_eq!(r.resolve("${XDG_CONFIG}").unwrap(), "/custom/.config");
        assert_eq!(r.resolve("${LOCAL_BIN}").unwrap(), "/custom/.local/bin");
        assert_eq!(r.resolve("${DOCUMENTS}").unwrap(), "/custom/Documents");
    }

    #[test]
    fn override_loop_through_derived_vars_is_caught() {
        let mut overrides = BTreeMap::new();
        // HOME -> XDG_CONFIG -> HOME (via derived_from_home)
        overrides.insert("HOME".into(), "${XDG_CONFIG}/parent".into());
        let r = linux("/orig").with_overrides(overrides);
        assert!(matches!(
            r.resolve("${HOME}").unwrap_err(),
            ResolveError::Cycle { .. }
        ));
    }

    #[test]
    fn override_can_reference_other_vars() {
        let mut overrides = BTreeMap::new();
        overrides.insert("MYBIN".into(), "${HOME}/bin".into());
        let r = linux("/h").with_overrides(overrides);
        assert_eq!(r.resolve("${MYBIN}/x").unwrap(), "/h/bin/x");
    }

    #[test]
    fn cycle_is_detected() {
        let mut overrides = BTreeMap::new();
        overrides.insert("A".into(), "${B}".into());
        overrides.insert("B".into(), "${A}".into());
        let r = linux("/h").with_overrides(overrides);
        match r.resolve("${A}").unwrap_err() {
            ResolveError::Cycle { chain, .. } => assert!(chain.contains("A -> B -> A")),
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn unknown_var_errors() {
        let r = linux("/h");
        match r.resolve("${NOPE}").unwrap_err() {
            ResolveError::UnknownVar { name } => assert_eq!(name, "NOPE"),
            other => panic!("expected UnknownVar, got {other:?}"),
        }
    }

    #[test]
    fn win_var_errors_on_linux() {
        let r = linux("/h");
        let err = r.resolve("${WIN_LOCALAPPDATA}").unwrap_err();
        assert!(matches!(err, ResolveError::WrongPlatform { .. }));
    }

    #[test]
    fn win_var_resolves_on_windows() {
        let r = windows("C:/Users/u");
        assert_eq!(
            r.resolve("${WIN_LOCALAPPDATA}/x").unwrap(),
            "C:/Users/u/AppData/Local/x"
        );
        assert_eq!(
            r.resolve("${WIN_APPDATA}/x").unwrap(),
            "C:/Users/u/AppData/Roaming/x"
        );
    }

    #[test]
    fn mac_var_errors_on_linux() {
        let r = linux("/h");
        assert!(matches!(
            r.resolve("${MAC_LIBRARY}").unwrap_err(),
            ResolveError::WrongPlatform { .. }
        ));
    }

    #[test]
    fn mac_var_resolves_on_macos() {
        let r = macos("/Users/u");
        assert_eq!(r.resolve("${MAC_LIBRARY}/x").unwrap(), "/Users/u/Library/x");
    }

    #[test]
    fn unclosed_brace_errors() {
        let r = linux("/h");
        let err = r.resolve("${HOME").unwrap_err();
        assert!(matches!(err, ResolveError::Malformed { .. }));
    }

    #[test]
    fn empty_braces_errors() {
        let r = linux("/h");
        let err = r.resolve("${}").unwrap_err();
        assert!(matches!(err, ResolveError::Malformed { .. }));
    }

    #[test]
    fn no_vars_passthrough() {
        let r = linux("/h");
        assert_eq!(r.resolve("/etc/hosts").unwrap(), "/etc/hosts");
    }

    #[test]
    fn known_vars_lists_platform_appropriate() {
        let linux_r = linux("/h");
        let names = linux_r.known_vars();
        assert!(names.contains(&"HOME".to_string()));
        assert!(names.contains(&"XDG_CONFIG".to_string()));
        assert!(names.contains(&"DOCUMENTS".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("WIN_")));
        assert!(!names.iter().any(|n| n.starts_with("MAC_")));

        let win_r = windows("C:/U");
        let win_names = win_r.known_vars();
        assert!(win_names.contains(&"WIN_LOCALAPPDATA".to_string()));
        assert!(win_names.contains(&"WIN_APPDATA".to_string()));
        assert!(!win_names.iter().any(|n| n.starts_with("MAC_")));
    }

    #[test]
    fn xdg_runtime_uses_env_then_tmpdir() {
        let mut env = HashMap::new();
        env.insert("HOME".into(), "/h".into());
        env.insert("XDG_RUNTIME_DIR".into(), "/run/user/1000".into());
        let r = Resolver::for_platform(Platform::Linux).with_env(env);
        assert_eq!(r.resolve("${XDG_RUNTIME}").unwrap(), "/run/user/1000");

        let mut env2 = HashMap::new();
        env2.insert("HOME".into(), "/h".into());
        env2.insert("TMPDIR".into(), "/var/tmp".into());
        let r2 = Resolver::for_platform(Platform::Linux).with_env(env2);
        assert_eq!(r2.resolve("${XDG_RUNTIME}").unwrap(), "/var/tmp");
    }
}
