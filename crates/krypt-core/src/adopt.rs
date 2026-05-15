//! `krypt adopt` and `krypt adopt-edits` — import existing dotfiles into the repo.
//!
//! `adopt` copies a file that already lives at its deployed location (`dst`)
//! into the repo at the derived or user-supplied `src` path, then records a
//! manifest entry.  The original file at `dst` is left untouched.
//!
//! `adopt_edits` scans every manifest entry for drift and, for each drifted
//! entry, copies the current `dst` bytes back into `<repo>/<src>` and refreshes
//! the manifest hashes.  This is the "I edited my deployed dotfiles in-place;
//! sync those edits back to the repo" workflow.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::copy::EntryKind;
use crate::manifest::{
    DriftStatus, Manifest, ManifestEntry, ManifestError, detect_drift, hash_file,
};
use crate::paths::{ResolveError, Resolver};

// ─── Errors ─────────────────────────────────────────────────────────────────

/// Errors from [`adopt`] or [`adopt_edits`].
#[derive(Debug, Error)]
pub enum AdoptError {
    /// The file at `dst` does not exist.
    #[error("dst does not exist: {0:?}")]
    DstMissing(PathBuf),

    /// `dst` is not under `$HOME` and no `--src` override was supplied.
    #[error("dst {dst:?} is outside $HOME; provide --src <rel> to name the repo-relative path")]
    OutsideHome {
        /// The destination path that is outside `$HOME`.
        dst: PathBuf,
    },

    /// A file already exists at `<repo>/<src>` and `--force` was not set.
    #[error(
        "repo already has {src:?}; use --force to overwrite, or --src to pick a different name"
    )]
    RepoCollision {
        /// The repo-relative path that already exists.
        src: PathBuf,
    },

    /// I/O failure.
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// Manifest read/write failure (boxed to keep enum ≤ 128 bytes).
    #[error(transparent)]
    Manifest(#[from] Box<ManifestError>),

    /// Path variable resolution failure (boxed to keep enum ≤ 128 bytes).
    #[error(transparent)]
    Resolve(#[from] Box<ResolveError>),
}

// ─── adopt ──────────────────────────────────────────────────────────────────

/// Options for [`adopt`].
pub struct AdoptOpts {
    /// Absolute path to the file currently on disk (the "deployed" location).
    pub dst: PathBuf,
    /// Override the auto-derived repo-relative source path.
    pub src_override: Option<PathBuf>,
    /// Absolute path to the dotfiles repo root.
    pub repo_path: PathBuf,
    /// Absolute path to the manifest JSON file.
    pub manifest_path: PathBuf,
    /// Overwrite an existing file in the repo without erroring.
    pub force: bool,
    /// Print what would happen without touching disk or writing the manifest.
    pub dry_run: bool,
    /// Resolver used to discover `$HOME` for auto-deriving `src`.
    pub resolver: Resolver,
}

/// Result of a successful [`adopt`] call.
#[derive(Debug)]
pub struct AdoptReport {
    /// Repo-relative source path that was written.
    pub src: PathBuf,
    /// Absolute destination path that was adopted.
    pub dst: PathBuf,
    /// Copy-pasteable `[[link]]` block the user should add to `.krypt.toml`.
    pub link_suggestion: String,
}

/// Import `opts.dst` into the repo and record it in the manifest.
///
/// The file at `dst` is copied (not moved) to `<repo_path>/<src>`.  A manifest
/// entry is created with `hash_src = hash_dst = sha256(file)`.  The original
/// file at `dst` is left in place.
///
/// Returns an [`AdoptReport`] with a ready-to-paste `[[link]]` block.
pub fn adopt(opts: &AdoptOpts) -> Result<AdoptReport, AdoptError> {
    if !opts.dst.exists() {
        return Err(AdoptError::DstMissing(opts.dst.clone()));
    }

    let src_rel: PathBuf = match &opts.src_override {
        Some(r) => r.clone(),
        None => derive_src(&opts.dst, &opts.resolver)?,
    };

    let repo_target = opts.repo_path.join(&src_rel);

    if !opts.force && repo_target.exists() {
        return Err(AdoptError::RepoCollision {
            src: src_rel.clone(),
        });
    }

    let link_suggestion = build_link_suggestion(&src_rel, &opts.dst);

    if opts.dry_run {
        return Ok(AdoptReport {
            src: src_rel,
            dst: opts.dst.clone(),
            link_suggestion,
        });
    }

    copy_atomic_simple(&opts.dst, &repo_target)?;

    let hash = hash_file(&repo_target).map_err(AdoptError::Io)?;
    let now = now_unix();

    let mut manifest = Manifest::load(&opts.manifest_path)
        .map_err(|e| AdoptError::Manifest(Box::new(e)))?
        .unwrap_or_else(|| Manifest::new(opts.repo_path.clone()));

    manifest.record(ManifestEntry {
        src: src_rel.clone(),
        dst: opts.dst.clone(),
        kind: EntryKind::Link,
        hash_src: hash.clone(),
        hash_dst: hash,
        deployed_at: now,
    });
    manifest
        .save(&opts.manifest_path)
        .map_err(|e| AdoptError::Manifest(Box::new(e)))?;

    Ok(AdoptReport {
        src: src_rel,
        dst: opts.dst.clone(),
        link_suggestion,
    })
}

// ─── adopt_edits ─────────────────────────────────────────────────────────────

/// Options for [`adopt_edits`].
pub struct AdoptEditsOpts {
    /// Absolute path to the manifest JSON file.
    pub manifest_path: PathBuf,
    /// Absolute path to the dotfiles repo root (used to resolve `<repo>/<src>`).
    pub repo_path: PathBuf,
    /// Print what would happen without touching disk or saving the manifest.
    pub dry_run: bool,
}

/// Result of a successful [`adopt_edits`] call.
#[derive(Debug)]
pub struct AdoptEditsReport {
    /// Number of drifted entries whose edits were adopted.
    pub adopted: usize,
    /// Number of entries that were already clean.
    pub clean: usize,
    /// Number of entries whose `dst` was missing (skipped with a warning).
    pub missing: usize,
}

/// For every drifted manifest entry, copy `dst` back into `<repo>/<src>` and
/// refresh the manifest hashes.
///
/// Clean entries are silently skipped.  Missing-dst entries are skipped with a
/// warning to stderr.  After processing, the manifest is saved atomically
/// (unless `dry_run` is set).
pub fn adopt_edits(opts: &AdoptEditsOpts) -> Result<AdoptEditsReport, AdoptError> {
    let Some(mut manifest) =
        Manifest::load(&opts.manifest_path).map_err(|e| AdoptError::Manifest(Box::new(e)))?
    else {
        return Ok(AdoptEditsReport {
            adopted: 0,
            clean: 0,
            missing: 0,
        });
    };

    let drift = detect_drift(&manifest);
    let mut report = AdoptEditsReport {
        adopted: 0,
        clean: 0,
        missing: 0,
    };

    let mut updated: Vec<ManifestEntry> = Vec::new();

    for record in drift {
        match record.status {
            DriftStatus::Clean => {
                report.clean += 1;
            }
            DriftStatus::DstMissing => {
                report.missing += 1;
                eprintln!(
                    "warning: dst missing: {:?}, leaving manifest entry alone",
                    record.dst
                );
            }
            DriftStatus::Drifted => {
                let repo_src = opts.repo_path.join(&record.src);
                if !opts.dry_run {
                    copy_atomic_simple(&record.dst, &repo_src)?;
                }
                let hash = if opts.dry_run {
                    record
                        .current_hash
                        .unwrap_or_else(|| record.recorded_hash.clone())
                } else {
                    hash_file(&repo_src).map_err(AdoptError::Io)?
                };
                updated.push(ManifestEntry {
                    src: record.src,
                    dst: record.dst,
                    kind: record.kind,
                    hash_src: hash.clone(),
                    hash_dst: hash,
                    deployed_at: now_unix(),
                });
                report.adopted += 1;
            }
        }
    }

    if !opts.dry_run {
        for entry in updated {
            manifest.record(entry);
        }
        manifest
            .save(&opts.manifest_path)
            .map_err(|e| AdoptError::Manifest(Box::new(e)))?;
    }

    Ok(report)
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// Derive the repo-relative `src` path by stripping the `$HOME` prefix from
/// `dst`.  Returns [`AdoptError::OutsideHome`] when `dst` is not under `HOME`.
fn derive_src(dst: &Path, resolver: &Resolver) -> Result<PathBuf, AdoptError> {
    let home_str = resolver
        .resolve_var("HOME")
        .map_err(|e| AdoptError::Resolve(Box::new(e)))?;
    let home = PathBuf::from(&home_str);
    dst.strip_prefix(&home)
        .map(|rel| rel.to_path_buf())
        .map_err(|_| AdoptError::OutsideHome {
            dst: dst.to_path_buf(),
        })
}

/// Build the `[[link]]` block the user should paste into `.krypt.toml`.
///
/// The `dst` string always uses forward slashes and `${HOME}/…` — that is the
/// krypt convention regardless of host OS.  The resolver translates it at
/// deploy time.
fn build_link_suggestion(src_rel: &Path, dst: &Path) -> String {
    // src display: forward slashes, no leading ./
    let src_display = src_rel.to_string_lossy().replace('\\', "/");

    // dst: express as ${HOME}/... with forward slashes.
    // We don't have the home prefix here, so we just display the absolute dst
    // with forward slashes.  For files *under* home the caller has already
    // stripped the prefix into src_rel; we reconstruct the canonical form.
    let dst_display = format!("${{HOME}}/{src_display}");

    // Use the actual dst path for non-home cases (--src override outside home).
    // If dst ends with the same relative portion as src_rel, use ${HOME}/...
    // Otherwise, use the raw absolute path with forward slashes.
    let dst_str = dst.to_string_lossy().replace('\\', "/");
    let src_str_fwd = src_rel.to_string_lossy().replace('\\', "/");
    let suggestion_dst = if dst_str.ends_with(&src_str_fwd) && dst_str.len() > src_str_fwd.len() {
        dst_display
    } else {
        dst_str
    };

    format!(
        "Add this to .krypt.toml:\n\n[[link]]\nsrc = \"{src_str_fwd}\"\ndst = \"{suggestion_dst}\""
    )
}

/// Atomically copy `src` → `dst`, creating parent directories as needed.
fn copy_atomic_simple(src: &Path, dst: &Path) -> Result<(), io::Error> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp_name = dst.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".krypt-tmp-{}", std::process::id()));
    let tmp = dst.with_file_name(tmp_name);
    let _ = fs::remove_file(&tmp);
    fs::copy(src, &tmp)?;
    fs::rename(&tmp, dst)?;
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Platform;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn linux_resolver(home: &Path) -> Resolver {
        let mut env = HashMap::new();
        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        Resolver::for_platform(Platform::Linux).with_env(env)
    }

    // ── adopt ───────────────────────────────────────────────────────────────

    #[test]
    fn adopt_file_under_home() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();

        let dst = home.path().join(".foo");
        fs::write(&dst, b"cfg content").unwrap();

        let manifest_path = state.path().join("manifest.json");

        let report = adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: manifest_path.clone(),
            force: false,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap();

        // Derived src should be ".foo"
        assert_eq!(report.src, PathBuf::from(".foo"));
        assert_eq!(report.dst, dst);

        // Repo now has the file.
        let repo_file = repo.path().join(".foo");
        assert!(repo_file.exists());
        assert_eq!(fs::read(&repo_file).unwrap(), b"cfg content");

        // Original at dst still exists.
        assert!(dst.exists());

        // Manifest has one entry.
        let manifest = Manifest::load(&manifest_path).unwrap().unwrap();
        assert_eq!(manifest.entries.len(), 1);
        let entry = &manifest.entries[&dst];
        assert_eq!(entry.src, PathBuf::from(".foo"));
        assert_eq!(entry.dst, dst);
        assert_eq!(entry.hash_src, entry.hash_dst);
        assert!(entry.hash_src.starts_with("sha256:"));

        // Suggestion contains src and dst.
        assert!(report.link_suggestion.contains("src = \".foo\""));
        assert!(report.link_suggestion.contains("dst = \"${HOME}/.foo\""));
    }

    #[test]
    fn adopt_with_src_override_outside_home() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();
        let outside = tempdir().unwrap();

        let dst = outside.path().join("some.conf");
        fs::write(&dst, b"data").unwrap();

        let report = adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: Some(PathBuf::from("some.conf")),
            repo_path: repo.path().to_path_buf(),
            manifest_path: state.path().join("manifest.json"),
            force: false,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap();

        assert_eq!(report.src, PathBuf::from("some.conf"));
        assert!(repo.path().join("some.conf").exists());
    }

    #[test]
    fn adopt_outside_home_no_override_errors() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();
        let outside = tempdir().unwrap();

        let dst = outside.path().join("file.txt");
        fs::write(&dst, b"x").unwrap();

        let err = adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: state.path().join("manifest.json"),
            force: false,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap_err();

        assert!(matches!(err, AdoptError::OutsideHome { .. }));
    }

    #[test]
    fn adopt_repo_collision_without_force_errors() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();

        let dst = home.path().join(".bar");
        fs::write(&dst, b"new").unwrap();
        // Pre-existing file in repo.
        fs::write(repo.path().join(".bar"), b"old").unwrap();

        let err = adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: state.path().join("manifest.json"),
            force: false,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap_err();

        assert!(matches!(err, AdoptError::RepoCollision { .. }));
    }

    #[test]
    fn adopt_repo_collision_with_force_succeeds() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();

        let dst = home.path().join(".bar");
        fs::write(&dst, b"new content").unwrap();
        fs::write(repo.path().join(".bar"), b"old content").unwrap();

        adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: state.path().join("manifest.json"),
            force: true,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap();

        assert_eq!(fs::read(repo.path().join(".bar")).unwrap(), b"new content");
    }

    #[test]
    fn adopt_missing_dst_errors() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();

        let dst = home.path().join("nonexistent.cfg");

        let err = adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: state.path().join("manifest.json"),
            force: false,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap_err();

        assert!(matches!(err, AdoptError::DstMissing(_)));
    }

    #[test]
    fn adopt_dry_run_no_disk_writes() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();

        let dst = home.path().join(".cfg");
        fs::write(&dst, b"data").unwrap();
        let manifest_path = state.path().join("manifest.json");

        let report = adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: manifest_path.clone(),
            force: false,
            dry_run: true,
            resolver: linux_resolver(home.path()),
        })
        .unwrap();

        // Suggestion is still returned.
        assert!(report.link_suggestion.contains("src = \".cfg\""));

        // Nothing written to repo or manifest.
        assert!(!repo.path().join(".cfg").exists());
        assert!(!manifest_path.exists());
    }

    // ── adopt_edits ──────────────────────────────────────────────────────────

    #[test]
    fn adopt_edits_syncs_drifted_entries() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();

        let dst = home.path().join(".zshrc");
        fs::write(&dst, b"original").unwrap();
        let manifest_path = state.path().join("manifest.json");

        // First, adopt the file to get it into the manifest.
        adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: manifest_path.clone(),
            force: false,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap();

        // User edits dst in place.
        fs::write(&dst, b"edited content").unwrap();

        let report = adopt_edits(&AdoptEditsOpts {
            manifest_path: manifest_path.clone(),
            repo_path: repo.path().to_path_buf(),
            dry_run: false,
        })
        .unwrap();

        assert_eq!(report.adopted, 1);
        assert_eq!(report.clean, 0);
        assert_eq!(report.missing, 0);

        // Repo file now has the edited content.
        assert_eq!(
            fs::read(repo.path().join(".zshrc")).unwrap(),
            b"edited content"
        );

        // Manifest hashes updated.
        let manifest = Manifest::load(&manifest_path).unwrap().unwrap();
        let entry = &manifest.entries[&dst];
        assert_eq!(entry.hash_src, entry.hash_dst);
        let expected_hash = hash_file(&dst).unwrap();
        assert_eq!(entry.hash_src, expected_hash);
    }

    #[test]
    fn adopt_edits_no_drift_returns_zero_adopted() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();

        let dst = home.path().join(".tmux.conf");
        fs::write(&dst, b"clean").unwrap();
        let manifest_path = state.path().join("manifest.json");

        adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: manifest_path.clone(),
            force: false,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap();

        let report = adopt_edits(&AdoptEditsOpts {
            manifest_path: manifest_path.clone(),
            repo_path: repo.path().to_path_buf(),
            dry_run: false,
        })
        .unwrap();

        assert_eq!(report.adopted, 0);
        assert_eq!(report.clean, 1);
        assert_eq!(report.missing, 0);

        // Repo file unchanged.
        assert_eq!(fs::read(repo.path().join(".tmux.conf")).unwrap(), b"clean");
    }

    #[test]
    fn adopt_edits_dry_run_no_changes() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();

        let dst = home.path().join(".vimrc");
        fs::write(&dst, b"original").unwrap();
        let manifest_path = state.path().join("manifest.json");

        adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: manifest_path.clone(),
            force: false,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap();

        // Drift the dst.
        fs::write(&dst, b"drifted").unwrap();

        let report = adopt_edits(&AdoptEditsOpts {
            manifest_path: manifest_path.clone(),
            repo_path: repo.path().to_path_buf(),
            dry_run: true,
        })
        .unwrap();

        assert_eq!(report.adopted, 1);

        // Repo file still has original content.
        assert_eq!(fs::read(repo.path().join(".vimrc")).unwrap(), b"original");

        // Manifest hashes not updated.
        let manifest = Manifest::load(&manifest_path).unwrap().unwrap();
        let entry = &manifest.entries[&dst];
        assert_eq!(
            entry.hash_src,
            hash_file(&repo.path().join(".vimrc")).unwrap()
        );
    }

    #[test]
    fn adopt_edits_missing_dst_counted_and_warned() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let state = tempdir().unwrap();

        let dst = home.path().join(".missing");
        fs::write(&dst, b"data").unwrap();
        let manifest_path = state.path().join("manifest.json");

        adopt(&AdoptOpts {
            dst: dst.clone(),
            src_override: None,
            repo_path: repo.path().to_path_buf(),
            manifest_path: manifest_path.clone(),
            force: false,
            dry_run: false,
            resolver: linux_resolver(home.path()),
        })
        .unwrap();

        // Remove the dst to simulate DstMissing.
        fs::remove_file(&dst).unwrap();

        let report = adopt_edits(&AdoptEditsOpts {
            manifest_path: manifest_path.clone(),
            repo_path: repo.path().to_path_buf(),
            dry_run: false,
        })
        .unwrap();

        assert_eq!(report.missing, 1);
        assert_eq!(report.adopted, 0);

        // Manifest entry preserved.
        let manifest = Manifest::load(&manifest_path).unwrap().unwrap();
        assert_eq!(manifest.entries.len(), 1);
    }
}
