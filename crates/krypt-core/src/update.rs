//! Orchestration for `krypt update`.
//!
//! Pulls the dotfiles repo (fast-forward only), optionally stashing local
//! changes before pulling and restoring them after, then re-runs `link` to
//! deploy any new files.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

use crate::deploy::{DeployError, DeployOpts, LinkReport, link};
use crate::tool_config::{ToolConfig, ToolConfigError};

// ─── Errors ─────────────────────────────────────────────────────────────────

/// Errors from [`update`].
#[derive(Debug, Error)]
pub enum UpdateError {
    /// Tool config is missing — the user needs to run `krypt init` first.
    #[error("tool config not found at {path:?} — run `krypt init` first")]
    ToolConfigMissing {
        /// Path that was checked.
        path: PathBuf,
    },

    /// Loading the tool config failed.
    #[error("loading tool config: {0}")]
    ToolConfig(#[from] ToolConfigError),

    /// Working tree is dirty and `--no-stash` was requested.
    #[error("working tree is dirty (use --no-stash=false to auto-stash)")]
    DirtyWorkingTree,

    /// `git stash push` failed.
    #[error("git stash push failed (exit {code})")]
    GitStashPush {
        /// Exit status code.
        code: i32,
    },

    /// `git stash pop` failed.
    #[error("git stash pop failed (exit {code})")]
    GitStashPop {
        /// Exit status code.
        code: i32,
    },

    /// `git pull --ff-only` failed.
    #[error("git pull failed (exit {code})")]
    GitPull {
        /// Exit status code.
        code: i32,
    },

    /// Spawning git failed entirely (e.g. git not on PATH).
    #[error("spawning git: {0}")]
    GitSpawn(#[source] io::Error),

    /// Running `git status --porcelain` to check for dirty tree failed.
    #[error("checking git status: {0}")]
    GitStatus(#[source] io::Error),

    /// `link` step failed.
    #[error("deploy link: {0}")]
    Deploy(#[from] DeployError),
}

// ─── Options & report ───────────────────────────────────────────────────────

/// Inputs to [`update`].
pub struct UpdateOpts {
    /// Path to the tool config (`${XDG_CONFIG}/krypt/config.toml`).
    pub tool_config_path: PathBuf,

    /// Override the path to `.krypt.toml`. Defaults to `<repo_path>/.krypt.toml`.
    pub config_path: Option<PathBuf>,

    /// Path for the deployment manifest.
    pub manifest_path: PathBuf,

    /// Pass `dry_run = true` to the link step.
    pub dry_run: bool,

    /// When true, abort instead of stashing if the working tree is dirty.
    pub no_stash: bool,

    /// Documented no-op for forward compatibility (hook runner not yet implemented).
    pub skip_hooks: bool,

    /// Pass `force = true` to the link step.
    pub force: bool,
}

/// Summary returned by a successful [`update`].
#[derive(Debug)]
pub struct UpdateReport {
    /// Whether local changes were stashed before pulling.
    pub stashed: bool,

    /// Whether `git pull --ff-only` advanced the repo.
    pub pulled: bool,

    /// Report from the `link` step.
    pub link: LinkReport,

    /// Version warning if our binary is older than `[meta] krypt_min`.
    pub version_warning: Option<String>,

    /// Number of `post-update` hooks skipped (not yet implemented).
    pub hooks_skipped: usize,
}

// ─── Implementation ──────────────────────────────────────────────────────────

/// Pull the dotfiles repo and re-deploy.
pub fn update(opts: &UpdateOpts) -> Result<UpdateReport, UpdateError> {
    let tool_cfg = ToolConfig::load(&opts.tool_config_path)?.ok_or_else(|| {
        UpdateError::ToolConfigMissing {
            path: opts.tool_config_path.clone(),
        }
    })?;

    let repo_path = &tool_cfg.repo.path;
    let config_path = opts
        .config_path
        .clone()
        .unwrap_or_else(|| repo_path.join(".krypt.toml"));

    let stashed = maybe_stash(repo_path, opts.no_stash)?;

    let pulled = git_pull(repo_path)?;

    if stashed {
        git_stash_pop(repo_path)?;
    }

    let krypt_cfg = crate::include::load_with_includes(&config_path).ok();

    let version_warning = krypt_cfg
        .as_ref()
        .and_then(|c| c.meta.krypt_min.as_deref())
        .and_then(version_warning_if_older);

    let hooks_skipped = krypt_cfg
        .as_ref()
        .map(|c| c.hooks.iter().filter(|h| h.when == "post-update").count())
        .unwrap_or(0);

    let link_report = link(&DeployOpts {
        config_path,
        manifest_path: opts.manifest_path.clone(),
        platform: None,
        dry_run: opts.dry_run,
        force: opts.force,
    })?;

    Ok(UpdateReport {
        stashed,
        pulled,
        link: link_report,
        version_warning,
        hooks_skipped,
    })
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// Check whether the working tree has uncommitted changes.
fn is_dirty(repo_path: &Path) -> Result<bool, UpdateError> {
    let out = Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap_or("."),
            "status",
            "--porcelain",
        ])
        .output()
        .map_err(UpdateError::GitStatus)?;
    Ok(!out.stdout.is_empty())
}

/// Stash local changes if the tree is dirty.
///
/// Returns `true` if a stash was created, `false` if the tree was clean.
fn maybe_stash(repo_path: &Path, no_stash: bool) -> Result<bool, UpdateError> {
    if !is_dirty(repo_path)? {
        return Ok(false);
    }
    if no_stash {
        return Err(UpdateError::DirtyWorkingTree);
    }
    let status = Command::new("git")
        .args([
            "-C",
            repo_path.to_str().unwrap_or("."),
            "stash",
            "push",
            "--include-untracked",
            "--message",
            "krypt-auto-stash",
        ])
        .status()
        .map_err(UpdateError::GitSpawn)?;

    if !status.success() {
        return Err(UpdateError::GitStashPush {
            code: status.code().unwrap_or(-1),
        });
    }
    Ok(true)
}

/// Run `git pull --ff-only`. Returns true if the pull was a no-op (already up
/// to date) or advanced HEAD. Passes stdio through so the user sees git output.
fn git_pull(repo_path: &Path) -> Result<bool, UpdateError> {
    let status = Command::new("git")
        .args(["-C", repo_path.to_str().unwrap_or("."), "pull", "--ff-only"])
        .status()
        .map_err(UpdateError::GitSpawn)?;

    if !status.success() {
        return Err(UpdateError::GitPull {
            code: status.code().unwrap_or(-1),
        });
    }
    Ok(true)
}

/// Restore the most recent stash entry.
fn git_stash_pop(repo_path: &Path) -> Result<(), UpdateError> {
    let status = Command::new("git")
        .args(["-C", repo_path.to_str().unwrap_or("."), "stash", "pop"])
        .status()
        .map_err(UpdateError::GitSpawn)?;

    if !status.success() {
        return Err(UpdateError::GitStashPop {
            code: status.code().unwrap_or(-1),
        });
    }
    Ok(())
}

/// Returns a warning string when our binary version is older than `min_version`.
fn version_warning_if_older(min_version: &str) -> Option<String> {
    let our_version = env!("CARGO_PKG_VERSION");
    if version_less_than(our_version, min_version) {
        Some(format!(
            "warning: this repo requires krypt >= {min_version}, but you have {our_version}; \
             please upgrade"
        ))
    } else {
        None
    }
}

/// Returns true if `a` is strictly less than `b` using semver-style comparison.
///
/// Parsing failures fall through to lexicographic comparison so the binary
/// never hard-fails on a malformed `krypt_min` value.
fn version_less_than(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(av), Some(bv)) => av < bv,
        _ => a < b,
    }
}

/// Parse a `MAJOR.MINOR.PATCH` string into a comparable tuple.
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()?
        .trim_end_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;

    // ── Git test helpers ────────────────────────────────────────────────────

    struct GitEnv {
        /// Bare remote acting as "origin". Kept alive so the directory is not dropped.
        #[allow(dead_code)]
        pub origin: tempfile::TempDir,
        /// Working clone used to push new commits through.
        pub upstream: tempfile::TempDir,
        /// The "local" repo that `update()` operates on.
        pub local: tempfile::TempDir,
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git must be available");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    fn git_with_identity(dir: &Path, args: &[&str]) {
        let status = StdCommand::new("git")
            .args(["-c", "user.email=test@test.test", "-c", "user.name=Test"])
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git must be available");
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    /// Build a three-tempdir git setup.
    ///
    /// - `origin/`   — bare repo (acts as remote)
    /// - `upstream/` — regular clone used to push commits
    /// - `local/`    — clone that `update()` will pull in
    ///
    /// An initial empty commit is pushed so both working dirs are non-empty.
    fn setup_git_env() -> GitEnv {
        let origin = tempdir().unwrap();
        let upstream = tempdir().unwrap();
        let local = tempdir().unwrap();

        git(origin.path(), &["init", "--bare"]);

        git(upstream.path(), &["init"]);
        git(
            upstream.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git_with_identity(
            upstream.path(),
            &["commit", "--allow-empty", "-m", "initial"],
        );
        git(upstream.path(), &["push", "origin", "HEAD:main"]);

        let local_path = local.path();
        git(local_path, &["clone", origin.path().to_str().unwrap(), "."]);

        GitEnv {
            origin,
            upstream,
            local,
        }
    }

    /// Push a new file from `upstream` to `origin`, so `local` can pull it.
    fn push_new_file(env: &GitEnv, rel_path: &str, content: &[u8]) {
        let dest = env.upstream.path().join(rel_path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&dest, content).unwrap();
        git(env.upstream.path(), &["add", rel_path]);
        git_with_identity(
            env.upstream.path(),
            &["commit", "-m", &format!("add {rel_path}")],
        );
        git(env.upstream.path(), &["push", "origin", "HEAD:main"]);
    }

    // ── Helper: build UpdateOpts pointing at a tempdir ──────────────────────

    fn make_tool_config(local: &tempfile::TempDir, tc_dir: &tempfile::TempDir) -> PathBuf {
        let tc_path = tc_dir.path().join("krypt").join("config.toml");
        let cfg = crate::tool_config::ToolConfig {
            repo: crate::tool_config::RepoConfig {
                path: local.path().to_path_buf(),
                url: None,
            },
        };
        cfg.save(&tc_path).unwrap();
        tc_path
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    #[test]
    fn update_pulls_new_file_and_deploys() {
        let env = setup_git_env();
        let home = tempdir().unwrap();
        let state = tempdir().unwrap();
        let tc_dir = tempdir().unwrap();

        // Write a minimal .krypt.toml into upstream then push it.
        let krypt_toml_content = format!(
            "[paths]\nHOME = \"{home}\"\n\n[[link]]\nsrc = \"gitconfig\"\ndst = \"${{HOME}}/.gitconfig\"\n",
            home = home.path().display()
        );
        push_new_file(&env, ".krypt.toml", krypt_toml_content.as_bytes());
        push_new_file(&env, "gitconfig", b"[user]\n\tname = Test\n");

        let tc_path = make_tool_config(&env.local, &tc_dir);
        let manifest_path = state.path().join("manifest.json");

        let report = update(&UpdateOpts {
            tool_config_path: tc_path,
            config_path: None,
            manifest_path,
            dry_run: false,
            no_stash: false,
            skip_hooks: false,
            force: false,
        })
        .unwrap();

        assert!(report.pulled);
        assert!(!report.stashed);
        assert_eq!(report.link.written, 1);
        assert!(home.path().join(".gitconfig").exists());
    }

    #[test]
    fn no_stash_with_dirty_tree_errors() {
        let env = setup_git_env();
        let state = tempdir().unwrap();
        let tc_dir = tempdir().unwrap();

        // Dirty the working tree.
        fs::write(env.local.path().join("dirty_file"), b"dirty").unwrap();

        let tc_path = make_tool_config(&env.local, &tc_dir);
        let manifest_path = state.path().join("manifest.json");

        let err = update(&UpdateOpts {
            tool_config_path: tc_path,
            config_path: None,
            manifest_path,
            dry_run: false,
            no_stash: true,
            skip_hooks: false,
            force: false,
        })
        .unwrap_err();

        assert!(matches!(err, UpdateError::DirtyWorkingTree));
    }

    #[test]
    fn auto_stash_roundtrip_leaves_tree_intact() {
        let env = setup_git_env();
        let state = tempdir().unwrap();
        let tc_dir = tempdir().unwrap();

        // Put a minimal .krypt.toml in the local repo so link doesn't fail.
        let krypt_toml_content = "# empty\n";
        fs::write(
            env.local.path().join(".krypt.toml"),
            krypt_toml_content.as_bytes(),
        )
        .unwrap();

        // Commit it so it won't be stashed and the working tree can still have
        // untracked changes.
        git(env.local.path(), &["add", ".krypt.toml"]);
        git_with_identity(env.local.path(), &["commit", "-m", "add krypt.toml"]);

        // Now add an untracked file that should be stashed.
        let dirty_path = env.local.path().join("local_change");
        fs::write(&dirty_path, b"local edit").unwrap();

        let tc_path = make_tool_config(&env.local, &tc_dir);
        let manifest_path = state.path().join("manifest.json");

        update(&UpdateOpts {
            tool_config_path: tc_path,
            config_path: None,
            manifest_path,
            dry_run: false,
            no_stash: false,
            skip_hooks: false,
            force: false,
        })
        .unwrap();

        // The previously-stashed untracked file should be restored.
        assert!(
            dirty_path.exists(),
            "stashed file should be restored after pull"
        );
        assert_eq!(fs::read(&dirty_path).unwrap(), b"local edit");
    }

    #[test]
    fn version_warning_fires_when_older() {
        // Our binary is "0.0.2"; simulate a repo requiring "99.0.0".
        assert!(version_less_than("0.0.2", "99.0.0"));
        let warn = version_warning_if_older("99.0.0");
        assert!(warn.is_some());
        assert!(warn.unwrap().contains("99.0.0"));
    }

    #[test]
    fn version_warning_absent_when_current() {
        // Require the same version we are — no warning.
        let our = env!("CARGO_PKG_VERSION");
        assert!(version_warning_if_older(our).is_none());
    }

    #[test]
    fn parse_version_basic() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.0.0"), Some((0, 0, 0)));
        assert!(parse_version("bad").is_none());
    }

    #[test]
    fn tool_config_missing_gives_clear_error() {
        let tc_dir = tempdir().unwrap();
        let tc_path = tc_dir.path().join("nonexistent.toml");
        let state = tempdir().unwrap();

        let err = update(&UpdateOpts {
            tool_config_path: tc_path.clone(),
            config_path: None,
            manifest_path: state.path().join("manifest.json"),
            dry_run: false,
            no_stash: false,
            skip_hooks: false,
            force: false,
        })
        .unwrap_err();

        assert!(
            matches!(err, UpdateError::ToolConfigMissing { ref path } if path == &tc_path),
            "expected ToolConfigMissing, got {err:?}"
        );
    }
}
