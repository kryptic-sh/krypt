//! `#include` directive — glob-based config composition.
//!
//! The [`include`][`Config::include`] field in a `.krypt.toml` lists glob
//! patterns (relative to the file's directory) that name additional config
//! files to merge in.  This module performs that expansion, merges the
//! resulting configs, and returns a single flattened [`Config`].
//!
//! Entry points:
//! - [`load_with_includes`] — read a root file and fully expand it.
//! - [`expand_includes`] — expand an already-parsed `Config` given its
//!   directory.  Useful when the caller has already parsed the root itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

use glob::glob;

use crate::config::{Config, ConfigError, Meta, parse_str};

/// Maximum include-nesting depth before we bail out.
const DEFAULT_MAX_DEPTH: usize = 8;

// ─── Public error type ──────────────────────────────────────────────────────

/// Errors that can arise during include expansion.
#[derive(Debug, thiserror::Error)]
pub enum IncludeError {
    /// A parse or validation error in one of the included files.
    #[error("{0}")]
    Config(#[from] ConfigError),

    /// Could not read a file that matched a glob.
    #[error("read {path}: {source}")]
    Io {
        /// The file we failed to read.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },

    /// A file was reachable from itself through the include chain.
    #[error("include cycle detected: {chain}")]
    Cycle {
        /// Human-readable `a -> b -> a` chain.
        chain: String,
    },

    /// The include chain exceeded [`DEFAULT_MAX_DEPTH`] hops.
    #[error("include depth limit exceeded (max {max}) at {last}")]
    DepthExceeded {
        /// The limit that was hit.
        max: usize,
        /// The file that would have been the next hop.
        last: PathBuf,
    },

    /// A glob pattern was syntactically invalid.
    #[error("glob pattern {pattern:?} from {base}: {reason}")]
    Glob {
        /// The raw pattern string from the config.
        pattern: String,
        /// Directory the pattern was relative to.
        base: PathBuf,
        /// Human-readable error from the `glob` crate.
        reason: String,
    },
}

// ─── Public API ─────────────────────────────────────────────────────────────

/// Parse `path` from disk and fully expand its `include` directives.
///
/// This is the most convenient entry point: it reads, parses, validates, and
/// merges everything, returning a single flat [`Config`] with `include`
/// cleared.
pub fn load_with_includes(path: impl AsRef<Path>) -> Result<Config, IncludeError> {
    let path = path.as_ref();
    let raw = read_file(path)?;
    let cfg = parse_str(&raw, path)?;
    let base_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let canon = canonicalize_for_cycle(path)?;
    let mut visited = vec![canon];
    expand_includes_inner(cfg, &base_dir, &mut visited, 0)
}

/// Expand the `include` list in an already-parsed `cfg`, resolving patterns
/// relative to `base_dir`.
///
/// `base_dir` should be the directory of the file that produced `cfg`.  The
/// returned `Config` has its `include` field cleared.
pub fn expand_includes(cfg: Config, base_dir: &Path) -> Result<Config, IncludeError> {
    let mut visited: Vec<PathBuf> = Vec::new();
    expand_includes_inner(cfg, base_dir, &mut visited, 0)
}

// ─── Internal recursion ─────────────────────────────────────────────────────

fn expand_includes_inner(
    cfg: Config,
    base_dir: &Path,
    visited: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<Config, IncludeError> {
    if cfg.include.is_empty() {
        let mut out = cfg;
        out.include.clear();
        return Ok(out);
    }

    // Collect the expanded paths for every glob pattern, sorted + de-duped.
    let mut matched_paths: Vec<PathBuf> = Vec::new();
    for pattern in &cfg.include {
        let full_pattern = base_dir.join(pattern);
        let pattern_str = full_pattern.to_string_lossy().into_owned();
        let entries = glob(&pattern_str).map_err(|e| IncludeError::Glob {
            pattern: pattern.clone(),
            base: base_dir.to_path_buf(),
            reason: e.to_string(),
        })?;
        let mut local: Vec<PathBuf> = entries.filter_map(|res| res.ok()).collect();
        local.sort();
        for p in local {
            if !matched_paths.contains(&p) {
                matched_paths.push(p);
            }
        }
    }

    // Start with the root config (include cleared).
    let mut merged = cfg;
    merged.include.clear();

    for inc_path in matched_paths {
        // Depth check.
        if depth + 1 > DEFAULT_MAX_DEPTH {
            return Err(IncludeError::DepthExceeded {
                max: DEFAULT_MAX_DEPTH,
                last: inc_path,
            });
        }

        // Cycle check via canonical path.
        let canon = canonicalize_for_cycle(&inc_path)?;
        if visited.contains(&canon) {
            let chain = build_cycle_chain(visited, &canon);
            return Err(IncludeError::Cycle { chain });
        }

        // Parse the included file.
        let raw = read_file(&inc_path)?;
        let inc_cfg = parse_str(&raw, &inc_path)?;

        // Recurse into its own includes.
        let inc_base = inc_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        visited.push(canon.clone());
        let inc_cfg = expand_includes_inner(inc_cfg, &inc_base, visited, depth + 1)?;
        visited.pop();

        // Merge the (fully expanded) included config on top.
        merged = merge(merged, inc_cfg);
    }

    Ok(merged)
}

// ─── Merge logic ────────────────────────────────────────────────────────────

/// Merge `later` on top of `base`.
///
/// - **Vec fields** (`links`, `templates`, `deps`, `hooks`, `commands`):
///   appended in order.
/// - **BTreeMap fields** (`paths`, `prompts`): later wins on conflicting keys.
/// - **`meta`**: later non-empty fields win; empty ones keep the base value.
/// - **`include`**: always cleared (expansion is done).
fn merge(base: Config, later: Config) -> Config {
    Config {
        meta: merge_meta(base.meta, later.meta),
        include: Vec::new(),
        paths: merge_map(base.paths, later.paths),
        links: concat(base.links, later.links),
        templates: concat(base.templates, later.templates),
        prompts: merge_map(base.prompts, later.prompts),
        deps: concat(base.deps, later.deps),
        hooks: concat(base.hooks, later.hooks),
        commands: concat(base.commands, later.commands),
    }
}

fn merge_meta(base: Meta, later: Meta) -> Meta {
    Meta {
        name: nonempty_or(later.name, base.name),
        description: nonempty_or(later.description, base.description),
        krypt_min: later.krypt_min.or(base.krypt_min),
    }
}

/// Return `preferred` if it is non-empty, otherwise `fallback`.
fn nonempty_or(preferred: String, fallback: String) -> String {
    if preferred.is_empty() {
        fallback
    } else {
        preferred
    }
}

fn concat<T>(mut a: Vec<T>, b: Vec<T>) -> Vec<T> {
    a.extend(b);
    a
}

fn merge_map<V>(mut base: BTreeMap<String, V>, later: BTreeMap<String, V>) -> BTreeMap<String, V> {
    base.extend(later);
    base
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn read_file(path: &Path) -> Result<String, IncludeError> {
    fs::read_to_string(path).map_err(|source| IncludeError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Canonicalize a path for cycle detection.  If the path doesn't exist yet
/// (unlikely in practice), fall back to the absolute path so the error is
/// still useful.
fn canonicalize_for_cycle(path: &Path) -> Result<PathBuf, IncludeError> {
    std::fs::canonicalize(path).map_err(|source| IncludeError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Build a `a -> b -> c` chain string for a cycle error message.
fn build_cycle_chain(visited: &[PathBuf], repeated: &Path) -> String {
    let mut parts: Vec<String> = visited.iter().map(|p| p.display().to_string()).collect();
    parts.push(repeated.display().to_string());
    parts.join(" -> ")
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_config() -> Config {
        Config::default()
    }

    fn config_with_name(name: &str) -> Config {
        let mut c = Config::default();
        c.meta.name = name.to_string();
        c
    }

    // merge_meta: later non-empty name wins
    #[test]
    fn merge_meta_later_name_wins() {
        let base = config_with_name("base");
        let later = config_with_name("later");
        let merged = merge(base, later);
        assert_eq!(merged.meta.name, "later");
    }

    // merge_meta: later empty name keeps base
    #[test]
    fn merge_meta_empty_later_keeps_base() {
        let base = config_with_name("base");
        let later = empty_config();
        let merged = merge(base, later);
        assert_eq!(merged.meta.name, "base");
    }

    // merge clears include on result
    #[test]
    fn merge_clears_include() {
        let mut base = empty_config();
        base.include = vec!["a.toml".into()];
        let mut later = empty_config();
        later.include = vec!["b.toml".into()];
        let merged = merge(base, later);
        assert!(merged.include.is_empty());
    }

    // merge_map: later key wins
    #[test]
    fn merge_map_later_wins() {
        let mut base = BTreeMap::new();
        base.insert("KEY".to_string(), "a".to_string());
        let mut later = BTreeMap::new();
        later.insert("KEY".to_string(), "b".to_string());
        let merged = merge_map(base, later);
        assert_eq!(merged["KEY"], "b");
    }

    // nonempty_or picks preferred when non-empty
    #[test]
    fn nonempty_or_picks_preferred() {
        assert_eq!(nonempty_or("x".into(), "y".into()), "x");
        assert_eq!(nonempty_or(String::new(), "y".into()), "y");
    }
}
