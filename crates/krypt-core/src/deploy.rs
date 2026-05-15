//! High-level deploy orchestration — the engine behind
//! `krypt link` / `krypt unlink` / `krypt relink`.
//!
//! Stages of a `link`:
//!
//! 1. Load `.krypt.toml` (with `include = [...]` expansion).
//! 2. Build a [`Resolver`] (optionally pinned to a non-host
//!    [`Platform`] for testing).
//! 3. Apply `[paths]` overrides from the config.
//! 4. Build a [`Plan`].
//! 5. Load the existing [`Manifest`] (if any).
//! 6. **Narrow conflicts**: for each [`Action::Conflict`] in the plan,
//!    check the manifest — if the recorded `hash_dst` matches the
//!    current file on disk, the destination is "ours" and a re-deploy
//!    is safe; promote to a [`Action::Copy`]. If `force` is set,
//!    promote everything.
//! 7. Execute the plan.
//! 8. Update the manifest with what was written, save atomically.
//!
//! `unlink` is the inverse: iterate the manifest, hash each `dst`,
//! delete only entries that still match the recorded hash (i.e.
//! haven't drifted). With `force`, delete regardless of drift.
//!
//! `relink` = unlink + link.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::{Config, ConfigError};
use crate::copy::{Action, ExecError, ExecOpts, PlanError, Report, execute as execute_plan, plan};
use crate::manifest::{
    DriftStatus, Manifest, ManifestEntry, ManifestError, detect_drift, hash_file,
};
use crate::paths::{Platform, Resolver};

// ─── Errors ─────────────────────────────────────────────────────────────────

/// Anything that can go wrong during a deploy.
#[derive(Debug, Error)]
pub enum DeployError {
    /// `.krypt.toml` failed to load or include-expand.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// `include = [...]` expansion failed.
    #[error("include expansion: {0}")]
    Include(String),
    /// Building the plan failed.
    #[error(transparent)]
    Plan(#[from] PlanError),
    /// Executing the plan failed.
    #[error(transparent)]
    Exec(#[from] ExecError),
    /// Reading or writing the manifest failed.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// I/O failure outside the planner/executor path (e.g. unlink remove).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

// ─── Options ────────────────────────────────────────────────────────────────

/// Knobs for [`link`] / [`unlink`] / [`relink`].
#[derive(Debug, Clone)]
pub struct DeployOpts {
    /// Absolute path to the `.krypt.toml` file driving the deploy. The
    /// directory it lives in is treated as `repo_root` for source-path
    /// resolution.
    pub config_path: PathBuf,
    /// Where to read + persist the deployment manifest. Defaults to
    /// `${XDG_STATE}/krypt/manifest.json`; tests pass an explicit path.
    pub manifest_path: PathBuf,
    /// Override the auto-detected platform. `None` = use `cfg!(target_os)`.
    pub platform: Option<Platform>,
    /// If true, no filesystem mutation occurs. The returned report still
    /// describes what *would* have happened.
    pub dry_run: bool,
    /// On `link`, overwrite real conflicts (destinations with content
    /// that doesn't match any prior manifest entry). On `unlink`,
    /// delete destinations even if they've drifted from the recorded
    /// hash.
    pub force: bool,
}

// ─── Reports ────────────────────────────────────────────────────────────────

/// Summary returned by [`link`].
#[derive(Debug, Default, Clone)]
pub struct LinkReport {
    /// Files actually copied (count). Includes safe re-deploys + forced.
    pub written: usize,
    /// Conflicts surfaced + skipped because `force` was off.
    pub conflicts_skipped: usize,
    /// Manifest-tracked re-deploys whose dst hash matched the recorded
    /// one (so we overwrote silently with the same bytes — true
    /// idempotency).
    pub idempotent_rewrites: usize,
    /// Conflicts skipped because the user-supplied `--platform` filter
    /// excluded them. Always 0 today (the planner already filters); kept
    /// for forward compat with cross-platform planning.
    pub platform_skipped: usize,
}

/// Summary returned by [`unlink`].
#[derive(Debug, Default, Clone)]
pub struct UnlinkReport {
    /// Files removed from disk.
    pub removed: usize,
    /// Entries skipped because the destination drifted (use `force` to
    /// delete anyway).
    pub drift_skipped: usize,
    /// Entries skipped because the destination is already gone.
    pub already_missing: usize,
}

// ─── link / unlink / relink ────────────────────────────────────────────────

/// Deploy every entry in the config. Idempotent — re-runs over a clean
/// state make no changes.
pub fn link(opts: &DeployOpts) -> Result<LinkReport, DeployError> {
    let (cfg, repo_root) = load_config(&opts.config_path)?;
    let resolver = build_resolver(opts.platform, &cfg);

    let raw_plan = plan(&cfg, &repo_root, &resolver)?;
    let mut manifest =
        Manifest::load(&opts.manifest_path)?.unwrap_or_else(|| Manifest::new(repo_root.clone()));
    manifest.repo_path = repo_root.clone();

    let (narrowed, idempotent) = narrow_conflicts(&raw_plan, &manifest, opts.force);

    let report = execute_plan(
        &narrowed,
        ExecOpts {
            dry_run: opts.dry_run,
            // narrow_conflicts already promoted everything we want to
            // overwrite into Copy; anything still Conflict here is a
            // real, unresolved conflict — leave it skipped.
            overwrite_conflicts: false,
        },
    )?;

    let mut conflicts_skipped = 0usize;
    for a in &narrowed.actions {
        if matches!(a, Action::Conflict { .. }) {
            conflicts_skipped += 1;
        }
    }

    if !opts.dry_run {
        for w in &report.written {
            if let (Some(hash_src), Some(hash_dst)) = (&w.hash_src, &w.hash_dst) {
                let src_rel = w
                    .src
                    .strip_prefix(&repo_root)
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|_| w.src.clone());
                manifest.record(ManifestEntry {
                    src: src_rel,
                    dst: w.dst.clone(),
                    kind: w.kind,
                    hash_src: hash_src.clone(),
                    hash_dst: hash_dst.clone(),
                    deployed_at: now_unix(),
                });
            }
        }
        manifest.save(&opts.manifest_path)?;
    }

    Ok(LinkReport {
        written: report.written.len(),
        conflicts_skipped,
        idempotent_rewrites: idempotent,
        platform_skipped: 0,
    })
}

/// Remove every entry recorded in the manifest. Drifted destinations are
/// skipped unless `force` is set.
pub fn unlink(opts: &DeployOpts) -> Result<UnlinkReport, DeployError> {
    let Some(mut manifest) = Manifest::load(&opts.manifest_path)? else {
        // Nothing to do; manifest doesn't exist yet.
        return Ok(UnlinkReport::default());
    };

    let mut report = UnlinkReport::default();
    let drift = detect_drift(&manifest);
    let mut to_forget: Vec<PathBuf> = Vec::new();

    for d in drift {
        match d.status {
            DriftStatus::Clean => {
                if !opts.dry_run {
                    fs::remove_file(&d.dst)?;
                }
                report.removed += 1;
                to_forget.push(d.dst);
            }
            DriftStatus::DstMissing => {
                report.already_missing += 1;
                to_forget.push(d.dst);
            }
            DriftStatus::Drifted => {
                if opts.force {
                    if !opts.dry_run {
                        fs::remove_file(&d.dst)?;
                    }
                    report.removed += 1;
                    to_forget.push(d.dst);
                } else {
                    report.drift_skipped += 1;
                }
            }
        }
    }

    if !opts.dry_run {
        for dst in &to_forget {
            manifest.forget(dst);
        }
        manifest.save(&opts.manifest_path)?;
    }

    Ok(report)
}

/// Convenience: [`unlink`] followed by [`link`]. Useful after large
/// config edits where you want a clean redeploy.
pub fn relink(opts: &DeployOpts) -> Result<(UnlinkReport, LinkReport), DeployError> {
    let u = unlink(opts)?;
    let l = link(opts)?;
    Ok((u, l))
}

// ─── Internals ──────────────────────────────────────────────────────────────

/// Walk `plan.actions` and, for each [`Action::Conflict`], either promote
/// to [`Action::Copy`] (safe to overwrite) or leave it alone (real
/// conflict). Returns `(narrowed_plan, idempotent_count)`.
fn narrow_conflicts(
    plan: &crate::copy::Plan,
    manifest: &Manifest,
    force: bool,
) -> (crate::copy::Plan, usize) {
    let mut out = Vec::with_capacity(plan.actions.len());
    let mut idempotent = 0usize;
    for action in &plan.actions {
        match action {
            Action::Copy { .. } => out.push(action.clone()),
            Action::Conflict { src, dst, kind } => {
                if force {
                    out.push(Action::Copy {
                        src: src.clone(),
                        dst: dst.clone(),
                        kind: *kind,
                    });
                    continue;
                }
                if let Some(entry) = manifest.entries.get(dst)
                    && hash_matches_recorded(dst, &entry.hash_dst)
                {
                    out.push(Action::Copy {
                        src: src.clone(),
                        dst: dst.clone(),
                        kind: *kind,
                    });
                    idempotent += 1;
                    continue;
                }
                out.push(action.clone());
            }
        }
    }
    (crate::copy::Plan { actions: out }, idempotent)
}

fn hash_matches_recorded(dst: &Path, recorded: &str) -> bool {
    match hash_file(dst) {
        Ok(actual) => actual == recorded,
        Err(_) => false,
    }
}

fn load_config(config_path: &Path) -> Result<(Config, PathBuf), DeployError> {
    let cfg = crate::include::load_with_includes(config_path).map_err(|e| match e {
        crate::include::IncludeError::Config(c) => DeployError::Config(c),
        other => DeployError::Include(other.to_string()),
    })?;
    let repo_root = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok((cfg, repo_root))
}

fn build_resolver(platform: Option<Platform>, cfg: &Config) -> Resolver {
    let r = match platform {
        Some(p) => Resolver::for_platform(p),
        None => Resolver::new(),
    };
    let overrides: BTreeMap<String, String> = cfg.paths.clone().into_iter().collect();
    r.with_overrides(overrides)
}

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// (Suppress dead-code lint on `Report` — used through `execute_plan`.)
#[allow(dead_code)]
fn _ensure_report_in_scope(_: Report) {}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Build a tiny synthetic repo + return its `.krypt.toml` path.
    fn synth_repo(root: &Path, files: &[(&str, &[u8])]) -> PathBuf {
        for (rel, bytes) in files {
            let p = root.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(p, bytes).unwrap();
        }
        root.join(".krypt.toml")
    }

    /// Return the path as a forward-slash string safe to embed in a TOML
    /// basic string. On Windows, `Path::display` emits backslashes which
    /// TOML treats as escape sequences and rejects.
    fn toml_path(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    fn opts(cfg: PathBuf, manifest: PathBuf, force: bool) -> DeployOpts {
        DeployOpts {
            config_path: cfg,
            manifest_path: manifest,
            platform: Some(Platform::Linux),
            dry_run: false,
            force,
        }
    }

    #[test]
    fn link_writes_files_and_manifest() {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let state = tempdir().unwrap();

        let cfg_text = format!(
            r#"
[paths]
HOME = "{home}"

[[link]]
src = "gitconfig"
dst = "${{HOME}}/.gitconfig"
"#,
            home = toml_path(home.path())
        );
        let cfg_path = synth_repo(repo.path(), &[("gitconfig", b"[user]\n")]);
        fs::write(&cfg_path, cfg_text).unwrap();

        let manifest_path = state.path().join("manifest.json");
        let r = link(&opts(cfg_path, manifest_path.clone(), false)).unwrap();
        assert_eq!(r.written, 1);
        assert!(home.path().join(".gitconfig").exists());

        let m = Manifest::load(&manifest_path).unwrap().unwrap();
        assert_eq!(m.entries.len(), 1);
        let entry = &m.entries[&home.path().join(".gitconfig")];
        assert_eq!(entry.src, PathBuf::from("gitconfig"));
        assert!(entry.hash_dst.starts_with("sha256:"));
    }

    #[test]
    fn link_idempotent_when_manifest_agrees() {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let state = tempdir().unwrap();

        let cfg_text = format!(
            r#"
[paths]
HOME = "{home}"

[[link]]
src = "a"
dst = "${{HOME}}/a"
"#,
            home = toml_path(home.path())
        );
        let cfg_path = synth_repo(repo.path(), &[("a", b"v1")]);
        fs::write(&cfg_path, cfg_text).unwrap();
        let manifest_path = state.path().join("manifest.json");

        link(&opts(cfg_path.clone(), manifest_path.clone(), false)).unwrap();
        let r = link(&opts(cfg_path, manifest_path, false)).unwrap();
        // Plan sees dst exists → Conflict. Manifest matches → narrowed to
        // Copy and silently rewritten.
        assert_eq!(r.idempotent_rewrites, 1);
        assert_eq!(r.conflicts_skipped, 0);
        assert_eq!(r.written, 1);
    }

    #[test]
    fn link_untracked_conflict_skipped_without_force() {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let state = tempdir().unwrap();

        // Pre-existing file at dst, NOT in our manifest.
        fs::write(home.path().join("a"), b"user wrote this").unwrap();

        let cfg_text = format!(
            r#"
[paths]
HOME = "{home}"

[[link]]
src = "a"
dst = "${{HOME}}/a"
"#,
            home = toml_path(home.path())
        );
        let cfg_path = synth_repo(repo.path(), &[("a", b"repo wrote this")]);
        fs::write(&cfg_path, cfg_text).unwrap();

        let manifest_path = state.path().join("manifest.json");
        let r = link(&opts(cfg_path.clone(), manifest_path.clone(), false)).unwrap();
        assert_eq!(r.conflicts_skipped, 1);
        assert_eq!(r.written, 0);
        assert_eq!(fs::read(home.path().join("a")).unwrap(), b"user wrote this");

        // With force, it overwrites.
        let r = link(&opts(cfg_path, manifest_path, true)).unwrap();
        assert_eq!(r.written, 1);
        assert_eq!(fs::read(home.path().join("a")).unwrap(), b"repo wrote this");
    }

    #[test]
    fn unlink_removes_clean_entries_only() {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let state = tempdir().unwrap();

        let cfg_text = format!(
            r#"
[paths]
HOME = "{home}"

[[link]]
src = "a"
dst = "${{HOME}}/a"

[[link]]
src = "b"
dst = "${{HOME}}/b"
"#,
            home = toml_path(home.path())
        );
        let cfg_path = synth_repo(repo.path(), &[("a", b"a1"), ("b", b"b1")]);
        fs::write(&cfg_path, cfg_text).unwrap();
        let manifest_path = state.path().join("manifest.json");

        link(&opts(cfg_path, manifest_path.clone(), false)).unwrap();

        // User modifies one deployed file.
        fs::write(home.path().join("b"), b"USER EDITED").unwrap();

        let r = unlink(&DeployOpts {
            config_path: PathBuf::new(),
            manifest_path: manifest_path.clone(),
            platform: Some(Platform::Linux),
            dry_run: false,
            force: false,
        })
        .unwrap();
        assert_eq!(r.removed, 1);
        assert_eq!(r.drift_skipped, 1);
        assert!(!home.path().join("a").exists());
        assert!(home.path().join("b").exists(), "drifted file kept");

        let m = Manifest::load(&manifest_path).unwrap().unwrap();
        assert_eq!(m.entries.len(), 1, "drifted entry still tracked");
    }

    #[test]
    fn unlink_force_removes_drifted() {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let state = tempdir().unwrap();

        let cfg_text = format!(
            r#"
[paths]
HOME = "{home}"

[[link]]
src = "a"
dst = "${{HOME}}/a"
"#,
            home = toml_path(home.path())
        );
        let cfg_path = synth_repo(repo.path(), &[("a", b"a1")]);
        fs::write(&cfg_path, cfg_text).unwrap();
        let manifest_path = state.path().join("manifest.json");

        link(&opts(cfg_path, manifest_path.clone(), false)).unwrap();
        fs::write(home.path().join("a"), b"DRIFT").unwrap();

        let r = unlink(&DeployOpts {
            config_path: PathBuf::new(),
            manifest_path: manifest_path.clone(),
            platform: Some(Platform::Linux),
            dry_run: false,
            force: true,
        })
        .unwrap();
        assert_eq!(r.removed, 1);
        assert!(!home.path().join("a").exists());
    }

    #[test]
    fn link_unlink_link_round_trips() {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let state = tempdir().unwrap();

        let cfg_text = format!(
            r#"
[paths]
HOME = "{home}"

[[link]]
src = "x"
dst = "${{HOME}}/x"

[[link]]
src = "y/y"
dst = "${{HOME}}/.config/y/y"
"#,
            home = toml_path(home.path())
        );
        let cfg_path = synth_repo(repo.path(), &[("x", b"X"), ("y/y", b"Y")]);
        fs::write(&cfg_path, cfg_text).unwrap();
        let manifest_path = state.path().join("manifest.json");

        let dopts = opts(cfg_path, manifest_path.clone(), false);

        // First link.
        link(&dopts).unwrap();
        let snapshot_x = fs::read(home.path().join("x")).unwrap();
        let snapshot_y = fs::read(home.path().join(".config/y/y")).unwrap();
        let snapshot_manifest =
            serde_json::to_string(&Manifest::load(&manifest_path).unwrap()).unwrap();

        // Unlink.
        unlink(&dopts).unwrap();
        assert!(!home.path().join("x").exists());
        assert!(!home.path().join(".config/y/y").exists());
        let m = Manifest::load(&manifest_path).unwrap().unwrap();
        assert_eq!(m.entries.len(), 0);

        // Re-link. State must match.
        link(&dopts).unwrap();
        assert_eq!(fs::read(home.path().join("x")).unwrap(), snapshot_x);
        assert_eq!(
            fs::read(home.path().join(".config/y/y")).unwrap(),
            snapshot_y
        );

        let after = serde_json::to_string(&Manifest::load(&manifest_path).unwrap()).unwrap();
        // Timestamps will differ but entries set must match.
        let snap_m: Manifest = serde_json::from_str(&snapshot_manifest).unwrap();
        let after_m: Manifest = serde_json::from_str(&after).unwrap();
        assert_eq!(snap_m.entries.len(), after_m.entries.len());
        for (k, snap_entry) in &snap_m.entries {
            let after_entry = &after_m.entries[k];
            assert_eq!(snap_entry.src, after_entry.src);
            assert_eq!(snap_entry.hash_src, after_entry.hash_src);
            assert_eq!(snap_entry.hash_dst, after_entry.hash_dst);
            assert_eq!(snap_entry.kind, after_entry.kind);
        }
    }

    #[test]
    fn relink_runs_unlink_then_link() {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let state = tempdir().unwrap();

        let cfg_text = format!(
            r#"
[paths]
HOME = "{home}"

[[link]]
src = "a"
dst = "${{HOME}}/a"
"#,
            home = toml_path(home.path())
        );
        let cfg_path = synth_repo(repo.path(), &[("a", b"v1")]);
        fs::write(&cfg_path, cfg_text).unwrap();
        let manifest_path = state.path().join("manifest.json");

        let dopts = opts(cfg_path, manifest_path, false);
        link(&dopts).unwrap();
        let (u, l) = relink(&dopts).unwrap();
        assert_eq!(u.removed, 1);
        assert_eq!(l.written, 1);
        assert!(home.path().join("a").exists());
    }

    #[test]
    fn dry_run_writes_nothing() {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let state = tempdir().unwrap();

        let cfg_text = format!(
            r#"
[paths]
HOME = "{home}"

[[link]]
src = "a"
dst = "${{HOME}}/a"
"#,
            home = toml_path(home.path())
        );
        let cfg_path = synth_repo(repo.path(), &[("a", b"v1")]);
        fs::write(&cfg_path, cfg_text).unwrap();
        let manifest_path = state.path().join("manifest.json");

        let r = link(&DeployOpts {
            config_path: cfg_path,
            manifest_path: manifest_path.clone(),
            platform: Some(Platform::Linux),
            dry_run: true,
            force: false,
        })
        .unwrap();
        assert_eq!(r.written, 1);
        assert!(!home.path().join("a").exists());
        assert!(!manifest_path.exists());
    }
}
