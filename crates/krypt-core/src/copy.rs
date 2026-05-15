//! Copy engine — deploy files from the repo to `$HOME`.
//!
//! The engine works in two phases:
//!
//! 1. **Plan** — pure, no I/O for writes. Walks a [`Config`]'s
//!    `[[link]]` and `[[template]]` entries, expands globs, applies
//!    `${VAR}` resolution to destinations, and skips entries that
//!    don't match the current platform. Returns a [`Plan`] which is a
//!    `Vec<Action>` of [`Copy`][`Action::Copy`] / [`Conflict`][`Action::Conflict`]
//!    items the caller can inspect or print.
//! 2. **Execute** — performs the planned copies atomically, preserving
//!    the source's mtime + (on Unix) file mode.
//!
//! What's deferred:
//!
//! - **Manifest-aware idempotency** — issue #13 will replace the naive
//!   `dst exists?` conflict check with a hash comparison against a
//!   recorded manifest.
//! - **Interactive prompts** for conflicts — issue #15 (`krypt link`)
//!   wires the CLI on top.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use thiserror::Error;

use crate::config::{Config, Link, Template};
use crate::paths::{ResolveError, Resolver};

// ─── Errors ─────────────────────────────────────────────────────────────────

/// Errors building a [`Plan`] from a Config.
#[derive(Debug, Error)]
pub enum PlanError {
    /// Failed to resolve `${VAR}` in a destination path.
    #[error("resolve dst {dst:?}: {source}")]
    Resolve {
        /// The unresolved destination string.
        dst: String,
        /// Underlying resolver error.
        #[source]
        source: ResolveError,
    },

    /// Glob pattern was syntactically invalid.
    #[error("invalid glob pattern {pattern:?}: {reason}")]
    Glob {
        /// The pattern from the config.
        pattern: String,
        /// Error from the `glob` crate.
        reason: String,
    },

    /// A platform string in the config wasn't one of `linux` / `macos`
    /// / `windows`. The parser also catches this, but the planner
    /// double-checks for callers who hand-build Configs.
    #[error("unknown platform string {value:?}")]
    UnknownPlatform {
        /// The bad string.
        value: String,
    },
}

/// Errors executing a [`Plan`].
#[derive(Debug, Error)]
pub enum ExecError {
    /// I/O failure during copy.
    #[error("copy {src:?} -> {dst:?}: {source}")]
    Io {
        /// Source path.
        src: PathBuf,
        /// Destination path.
        dst: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },

    /// Source path didn't exist when we tried to copy it.
    #[error("source missing: {0:?}")]
    SourceMissing(PathBuf),
}

// ─── Plan + Action ──────────────────────────────────────────────────────────

/// Which schema section an action came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// `[[link]]`.
    Link,
    /// `[[template]]`.
    Template,
}

/// What the engine intends to do for one (src, dst) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Destination does not exist — safe to write.
    Copy {
        /// Absolute source path.
        src: PathBuf,
        /// Absolute destination path.
        dst: PathBuf,
        /// Whether this came from a `[[link]]` or `[[template]]`.
        kind: EntryKind,
    },

    /// Destination already exists. The caller decides what to do.
    /// Manifest-aware drift detection in #13 will let us narrow some
    /// of these to `Skip` (content matches) vs a real conflict.
    Conflict {
        /// Absolute source path.
        src: PathBuf,
        /// Absolute destination path.
        dst: PathBuf,
        /// Section the entry came from.
        kind: EntryKind,
    },
}

impl Action {
    /// Source path for any variant.
    pub fn src(&self) -> &Path {
        match self {
            Action::Copy { src, .. } | Action::Conflict { src, .. } => src,
        }
    }

    /// Destination path for any variant.
    pub fn dst(&self) -> &Path {
        match self {
            Action::Copy { dst, .. } | Action::Conflict { dst, .. } => dst,
        }
    }

    /// Section the entry came from.
    pub fn kind(&self) -> EntryKind {
        match self {
            Action::Copy { kind, .. } | Action::Conflict { kind, .. } => *kind,
        }
    }
}

/// A list of [`Action`]s to perform (or print, in dry-run mode).
#[derive(Debug, Default, Clone)]
pub struct Plan {
    /// Ordered actions, in the order discovered from the config.
    pub actions: Vec<Action>,
}

impl Plan {
    /// Number of [`Action::Copy`] entries.
    pub fn copy_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, Action::Copy { .. }))
            .count()
    }

    /// Number of [`Action::Conflict`] entries.
    pub fn conflict_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, Action::Conflict { .. }))
            .count()
    }
}

// ─── Planner ────────────────────────────────────────────────────────────────

/// Build a [`Plan`] from a parsed (+ include-expanded) [`Config`].
///
/// `repo_root` is the directory the dotfiles repo lives in — `src` and
/// `src_glob` fields are joined under it. `resolver` expands `${VAR}` in
/// destination paths.
pub fn plan(cfg: &Config, repo_root: &Path, resolver: &Resolver) -> Result<Plan, PlanError> {
    let mut actions = Vec::new();
    let current_platform = current_platform_str();

    for link in &cfg.links {
        if !platform_matches(&link.platform, current_platform)? {
            continue;
        }
        plan_link(link, repo_root, resolver, &mut actions)?;
    }
    for tmpl in &cfg.templates {
        if !platform_matches(&tmpl.platform, current_platform)? {
            continue;
        }
        plan_template(tmpl, repo_root, resolver, &mut actions)?;
    }
    Ok(Plan { actions })
}

fn plan_link(
    link: &Link,
    repo_root: &Path,
    resolver: &Resolver,
    out: &mut Vec<Action>,
) -> Result<(), PlanError> {
    let dst_str = resolver
        .resolve(&link.dst)
        .map_err(|e| PlanError::Resolve {
            dst: link.dst.clone(),
            source: e,
        })?;
    let dst_base = PathBuf::from(dst_str);

    if let Some(src) = &link.src {
        let src_path = repo_root.join(src);
        let action = build_action(&src_path, &dst_base, EntryKind::Link);
        out.push(action);
        return Ok(());
    }

    if let Some(src_glob) = &link.src_glob {
        let full_pattern = repo_root.join(src_glob).to_string_lossy().into_owned();
        let matches = glob::glob(&full_pattern).map_err(|e| PlanError::Glob {
            pattern: full_pattern.clone(),
            reason: e.to_string(),
        })?;
        let glob_prefix = glob_prefix_of(src_glob);
        let strip_root = repo_root.join(&glob_prefix);
        let mut paths: Vec<PathBuf> = matches.filter_map(|r| r.ok()).collect();
        paths.sort();
        for src_path in paths {
            // Glob can match directories; skip those, copy only files.
            if !src_path.is_file() {
                continue;
            }
            let rel = src_path
                .strip_prefix(&strip_root)
                .unwrap_or(&src_path)
                .to_path_buf();
            let dst_path = dst_base.join(rel);
            out.push(build_action(&src_path, &dst_path, EntryKind::Link));
        }
        return Ok(());
    }

    // The parser refuses to load a [[link]] with neither src nor src_glob.
    // If a caller hand-builds a Config and bypasses validation, that's their
    // problem — we just skip the entry.
    Ok(())
}

fn plan_template(
    tmpl: &Template,
    repo_root: &Path,
    resolver: &Resolver,
    out: &mut Vec<Action>,
) -> Result<(), PlanError> {
    let dst_str = resolver
        .resolve(&tmpl.dst)
        .map_err(|e| PlanError::Resolve {
            dst: tmpl.dst.clone(),
            source: e,
        })?;
    let dst_path = PathBuf::from(dst_str);
    let src_path = repo_root.join(&tmpl.src);
    out.push(build_action(&src_path, &dst_path, EntryKind::Template));
    Ok(())
}

fn build_action(src: &Path, dst: &Path, kind: EntryKind) -> Action {
    if dst.exists() {
        Action::Conflict {
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
            kind,
        }
    } else {
        Action::Copy {
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
            kind,
        }
    }
}

/// Leading directory component of a glob pattern (everything before the
/// first segment that contains a glob meta-character). Used to compute
/// the relative-path for src→dst mapping.
fn glob_prefix_of(pattern: &str) -> PathBuf {
    let mut prefix = PathBuf::new();
    for part in Path::new(pattern).components() {
        let s = part.as_os_str().to_string_lossy();
        if s.contains(['*', '?', '[']) {
            break;
        }
        prefix.push(part.as_os_str());
    }
    prefix
}

fn current_platform_str() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn platform_matches(entry_platform: &Option<String>, current: &str) -> Result<bool, PlanError> {
    let Some(p) = entry_platform else {
        return Ok(true);
    };
    match p.as_str() {
        "linux" | "macos" | "windows" => Ok(p == current),
        other => Err(PlanError::UnknownPlatform {
            value: other.to_string(),
        }),
    }
}

// ─── Executor ───────────────────────────────────────────────────────────────

/// Knobs for [`execute`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ExecOpts {
    /// If true, print what would be done without touching disk.
    pub dry_run: bool,
    /// If true, overwrite [`Action::Conflict`] destinations. Default
    /// false — conflicts are surfaced as `skipped` in the report.
    pub overwrite_conflicts: bool,
}

/// What [`execute`] actually did.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// Number of files written.
    pub written: usize,
    /// Conflict entries that were skipped (caller didn't opt into overwrite).
    pub skipped_conflicts: usize,
}

/// Run a [`Plan`] against the filesystem.
pub fn execute(plan: &Plan, opts: ExecOpts) -> Result<Report, ExecError> {
    let mut report = Report::default();
    for action in &plan.actions {
        match action {
            Action::Copy { src, dst, .. } => {
                if !opts.dry_run {
                    copy_atomic(src, dst)?;
                }
                report.written += 1;
            }
            Action::Conflict { src, dst, .. } => {
                if opts.overwrite_conflicts {
                    if !opts.dry_run {
                        copy_atomic(src, dst)?;
                    }
                    report.written += 1;
                } else {
                    report.skipped_conflicts += 1;
                }
            }
        }
    }
    Ok(report)
}

/// Atomically copy `src` -> `dst`, preserving mtime (and, on Unix,
/// permission bits). Creates parent directories as needed.
fn copy_atomic(src: &Path, dst: &Path) -> Result<(), ExecError> {
    let mk_err = |e: std::io::Error| ExecError::Io {
        src: src.to_path_buf(),
        dst: dst.to_path_buf(),
        source: e,
    };

    if !src.exists() {
        return Err(ExecError::SourceMissing(src.to_path_buf()));
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(mk_err)?;
    }
    let tmp = tmp_sibling(dst);
    // Remove a leftover from a previous failed run so the copy doesn't
    // fail-fast with EEXIST on systems where copy is strict.
    let _ = fs::remove_file(&tmp);

    // fs::copy preserves mode on Unix; on Windows there's no concept to
    // preserve for our purposes.
    fs::copy(src, &tmp).map_err(mk_err)?;

    // Preserve mtime. Read from source metadata; ignore failures for
    // platforms where it isn't supported.
    if let Ok(meta) = fs::metadata(src) {
        if let Ok(modified) = meta.modified()
            && let Ok(f) = File::options().write(true).open(&tmp)
        {
            let _ = f.set_modified(modified);
        }
    } else {
        // Even if we couldn't read mtime, the copy itself succeeded.
        let _: SystemTime = SystemTime::now(); // keep `SystemTime` import live
    }

    fs::rename(&tmp, dst).map_err(mk_err)?;
    Ok(())
}

fn tmp_sibling(dst: &Path) -> PathBuf {
    let mut name = dst.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".krypt-tmp-{}", std::process::id()));
    dst.with_file_name(name)
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_prefix_strips_at_first_wildcard() {
        assert_eq!(
            glob_prefix_of(".config/nvim/**/*"),
            PathBuf::from(".config/nvim")
        );
        assert_eq!(glob_prefix_of("**/*"), PathBuf::new());
        assert_eq!(glob_prefix_of("a/b/c"), PathBuf::from("a/b/c"));
        assert_eq!(glob_prefix_of("foo/*.toml"), PathBuf::from("foo"));
    }

    #[test]
    fn platform_match_accepts_omitted() {
        assert!(platform_matches(&None, "linux").unwrap());
    }

    #[test]
    fn platform_match_filters_other_os() {
        assert!(platform_matches(&Some("linux".into()), "linux").unwrap());
        assert!(!platform_matches(&Some("macos".into()), "linux").unwrap());
        assert!(!platform_matches(&Some("windows".into()), "linux").unwrap());
    }

    #[test]
    fn platform_match_rejects_unknown() {
        assert!(matches!(
            platform_matches(&Some("freebsd".into()), "linux"),
            Err(PlanError::UnknownPlatform { .. })
        ));
    }

    #[test]
    fn tmp_sibling_lives_next_to_dst() {
        let dst = PathBuf::from("/some/where/file.conf");
        let tmp = tmp_sibling(&dst);
        assert_eq!(tmp.parent(), dst.parent());
        let name = tmp.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("file.conf.krypt-tmp-"));
    }

    #[test]
    fn plan_counts_match_actions() {
        let actions = vec![
            Action::Copy {
                src: "/a".into(),
                dst: "/b".into(),
                kind: EntryKind::Link,
            },
            Action::Conflict {
                src: "/c".into(),
                dst: "/d".into(),
                kind: EntryKind::Template,
            },
            Action::Copy {
                src: "/e".into(),
                dst: "/f".into(),
                kind: EntryKind::Link,
            },
        ];
        let p = Plan { actions };
        assert_eq!(p.copy_count(), 2);
        assert_eq!(p.conflict_count(), 1);
    }
}
