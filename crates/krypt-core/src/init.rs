//! Orchestration for `krypt init`.
//!
//! Clones a remote dotfiles repo (or creates an empty local stub) into
//! the configured repo path, then writes the tool config.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

use crate::tool_config::{RepoConfig, ToolConfig, ToolConfigError};

// ─── Errors ─────────────────────────────────────────────────────────────────

/// Errors from [`init`].
#[derive(Debug, Error)]
pub enum InitError {
    /// No URL supplied and `--bare` not set.
    #[error("must provide URL or --bare")]
    MissingUrl,

    /// Repo path already exists (non-empty) and `--force` was not set.
    #[error("repo path already exists: {path:?} (use --force to overwrite)")]
    RepoExists {
        /// The conflicting path.
        path: PathBuf,
    },

    /// `git clone` exited non-zero. Output is inherited from the parent,
    /// so the user has already seen git's error on stderr — we just
    /// surface the exit code.
    #[error("git clone failed (exit {code})")]
    GitClone {
        /// Exit status code.
        code: i32,
    },

    /// Spawning git failed entirely (e.g. git not on PATH).
    #[error("spawning git: {0}")]
    GitSpawn(#[source] io::Error),

    /// I/O failure outside git (directory creation, file write, …).
    #[error("io: {0}")]
    Io(#[from] io::Error),

    /// Writing the tool config failed.
    #[error("writing tool config: {0}")]
    ToolConfig(#[from] ToolConfigError),
}

// ─── Options & report ───────────────────────────────────────────────────────

/// Inputs to [`init`].
pub struct InitOpts {
    /// Remote URL to clone from. `None` when `--bare`.
    pub url: Option<String>,
    /// Where to put the repo on disk.
    pub repo_path: PathBuf,
    /// Where to write `config.toml`.
    pub tool_config_path: PathBuf,
    /// Create an empty stub instead of cloning.
    pub bare: bool,
    /// Wipe an existing repo path before proceeding.
    pub force: bool,
}

/// Summary returned by a successful [`init`].
#[derive(Debug)]
pub struct InitReport {
    /// Absolute path to the repo on disk.
    pub repo_path: PathBuf,
    /// Absolute path to the tool config written.
    pub tool_config_path: PathBuf,
}

// ─── Implementation ──────────────────────────────────────────────────────────

/// Initialise a dotfiles repo and write the tool config.
pub fn init(opts: &InitOpts) -> Result<InitReport, InitError> {
    if opts.url.is_none() && !opts.bare {
        return Err(InitError::MissingUrl);
    }

    prepare_repo_path(&opts.repo_path, opts.force)?;

    if opts.bare {
        init_bare(&opts.repo_path)?;
    } else {
        git_clone(opts.url.as_deref().unwrap(), &opts.repo_path)?;
    }

    let cfg = ToolConfig {
        repo: RepoConfig {
            path: opts.repo_path.clone(),
            url: if opts.bare { None } else { opts.url.clone() },
        },
    };
    cfg.save(&opts.tool_config_path)?;

    Ok(InitReport {
        repo_path: opts.repo_path.clone(),
        tool_config_path: opts.tool_config_path.clone(),
    })
}

// ─── Internals ───────────────────────────────────────────────────────────────

fn prepare_repo_path(path: &Path, force: bool) -> Result<(), InitError> {
    if path.exists() {
        let non_empty = path
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
        if non_empty {
            if force {
                fs::remove_dir_all(path)?;
            } else {
                return Err(InitError::RepoExists {
                    path: path.to_path_buf(),
                });
            }
        }
    }
    Ok(())
}

fn git_clone(url: &str, dest: &Path) -> Result<(), InitError> {
    let status = Command::new("git")
        .arg("clone")
        .arg(url)
        .arg(dest)
        .status()
        .map_err(InitError::GitSpawn)?;

    if !status.success() {
        return Err(InitError::GitClone {
            code: status.code().unwrap_or(-1),
        });
    }
    Ok(())
}

fn init_bare(path: &Path) -> Result<(), InitError> {
    fs::create_dir_all(path)?;
    let stub = path.join(".krypt.toml");
    fs::write(
        &stub,
        concat!(
            "# krypt dotfiles manifest\n",
            "# See https://github.com/kryptic-sh/krypt for schema reference.\n",
            "\n",
            "# [[link]]\n",
            "# src = \".gitconfig\"\n",
            "# dst = \"${HOME}/.gitconfig\"\n",
        ),
    )?;
    Ok(())
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;

    fn tool_config_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("krypt").join("config.toml")
    }

    #[test]
    fn missing_url_no_bare_errors() {
        let repo_dir = tempdir().unwrap();
        let cfg_dir = tempdir().unwrap();
        let err = init(&InitOpts {
            url: None,
            repo_path: repo_dir.path().join("repo"),
            tool_config_path: tool_config_path(&cfg_dir),
            bare: false,
            force: false,
        })
        .unwrap_err();
        assert!(matches!(err, InitError::MissingUrl));
    }

    #[test]
    fn bare_creates_stub_and_tool_config() {
        let repo_dir = tempdir().unwrap();
        let cfg_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo");
        let tc_path = tool_config_path(&cfg_dir);

        init(&InitOpts {
            url: None,
            repo_path: repo_path.clone(),
            tool_config_path: tc_path.clone(),
            bare: true,
            force: false,
        })
        .unwrap();

        assert!(repo_path.join(".krypt.toml").exists());
        let tc = ToolConfig::load(&tc_path).unwrap().unwrap();
        assert_eq!(tc.repo.path, repo_path);
        assert!(tc.repo.url.is_none());
    }

    #[test]
    fn existing_repo_without_force_errors() {
        let repo_dir = tempdir().unwrap();
        let cfg_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo");
        fs::create_dir_all(&repo_path).unwrap();
        fs::write(repo_path.join("existing"), b"data").unwrap();

        let err = init(&InitOpts {
            url: None,
            repo_path,
            tool_config_path: tool_config_path(&cfg_dir),
            bare: true,
            force: false,
        })
        .unwrap_err();
        assert!(matches!(err, InitError::RepoExists { .. }));
    }

    #[test]
    fn existing_repo_with_force_succeeds() {
        let repo_dir = tempdir().unwrap();
        let cfg_dir = tempdir().unwrap();
        let repo_path = repo_dir.path().join("repo");
        fs::create_dir_all(&repo_path).unwrap();
        fs::write(repo_path.join("old_file"), b"old").unwrap();

        init(&InitOpts {
            url: None,
            repo_path: repo_path.clone(),
            tool_config_path: tool_config_path(&cfg_dir),
            bare: true,
            force: true,
        })
        .unwrap();

        assert!(!repo_path.join("old_file").exists());
        assert!(repo_path.join(".krypt.toml").exists());
    }

    #[test]
    fn clone_from_local_bare_repo() {
        let origin_dir = tempdir().unwrap();
        let repo_dir = tempdir().unwrap();
        let cfg_dir = tempdir().unwrap();

        // Create a local bare repo to clone from.
        let status = StdCommand::new("git")
            .args(["init", "--bare", origin_dir.path().to_str().unwrap()])
            .output()
            .expect("git must be available");
        assert!(status.status.success(), "git init --bare failed");

        let repo_path = repo_dir.path().join("repo");
        let tc_path = tool_config_path(&cfg_dir);
        let url = origin_dir.path().to_str().unwrap().to_string();

        init(&InitOpts {
            url: Some(url.clone()),
            repo_path: repo_path.clone(),
            tool_config_path: tc_path.clone(),
            bare: false,
            force: false,
        })
        .unwrap();

        assert!(repo_path.exists());
        let tc = ToolConfig::load(&tc_path).unwrap().unwrap();
        assert_eq!(tc.repo.path, repo_path);
        assert_eq!(tc.repo.url.as_deref(), Some(url.as_str()));
    }
}
