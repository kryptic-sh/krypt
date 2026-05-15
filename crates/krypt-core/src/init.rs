//! Orchestration for `krypt init`.
//!
//! Clones a remote dotfiles repo (or creates an empty local stub) into
//! the configured repo path, then writes the tool config.
//!
//! # HTTPS-only note
//!
//! Cloning uses gix's blocking HTTP transport with rustls — no system `git`
//! required, no OpenSSL, no libssh2.  The trade-off is that **only HTTPS
//! URLs are supported** (gix 0.83 has no SSH transport).  If your remote is
//! SSH-only, clone manually with `git clone` first and then run
//! `krypt init --repo-path <path>` without a URL to write the tool config.

// `InitError` wraps gix clone/checkout errors (already boxed) and
// `ToolConfigError`; on Windows the combined enum exceeds clippy's 128-byte
// threshold.  The variants are already as compact as the upstream types allow.
#![allow(clippy::result_large_err)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

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

    /// `gix::prepare_clone` failed.
    #[error("preparing clone of {url:?}: {source}")]
    GixClonePrepare {
        /// The URL that was cloned.
        url: String,
        /// Underlying error (boxed to keep the enum variant small).
        #[source]
        source: Box<gix::clone::Error>,
    },

    /// The fetch step of the clone failed.
    #[error("fetching during clone of {url:?}: {source}")]
    GixCloneFetch {
        /// The URL that was cloned.
        url: String,
        /// Underlying error (boxed to keep the enum variant small).
        #[source]
        source: Box<gix::clone::fetch::Error>,
    },

    /// The checkout step after cloning failed.
    #[error("checking out clone of {url:?}: {source}")]
    GixCloneCheckout {
        /// The URL that was cloned.
        url: String,
        /// Underlying error (boxed to keep the enum variant small).
        #[source]
        source: Box<gix::clone::checkout::main_worktree::Error>,
    },

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
    /// Remote HTTPS URL to clone from.  `None` when `--bare`.
    ///
    /// Only HTTPS URLs are supported by the gix transport used here.
    /// SSH URLs will fail — see the module-level note.
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
        gix_clone(opts.url.as_deref().unwrap(), &opts.repo_path)?;
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

/// Clone `url` into `dest` using gix's blocking HTTP/rustls transport.
///
/// No system `git` binary is required.  Only HTTPS URLs are supported —
/// gix 0.83 has no SSH transport.
fn gix_clone(url: &str, dest: &Path) -> Result<(), InitError> {
    let interrupt = AtomicBool::new(false);

    let (mut checkout, _fetch_outcome) = gix::prepare_clone(url, dest)
        .map_err(|e| InitError::GixClonePrepare {
            url: url.to_owned(),
            source: Box::new(e),
        })?
        .fetch_then_checkout(gix::progress::Discard, &interrupt)
        .map_err(|e| InitError::GixCloneFetch {
            url: url.to_owned(),
            source: Box::new(e),
        })?;

    checkout
        .main_worktree(gix::progress::Discard, &interrupt)
        .map_err(|e| InitError::GixCloneCheckout {
            url: url.to_owned(),
            source: Box::new(e),
        })?;
    // (Repository and checkout outcome returned; we only need the side-effect.)

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

    /// Clone from a local file-path URL using gix.
    ///
    /// We need a non-empty commit in the origin so that gix has something to
    /// check out — an empty bare repo will succeed but produce an empty clone.
    #[test]
    fn clone_from_local_file_url() {
        let origin_dir = tempdir().unwrap();
        let repo_dir = tempdir().unwrap();
        let cfg_dir = tempdir().unwrap();

        // Create a local non-bare repo with an initial commit using gix APIs.
        let origin_repo = gix::init(origin_dir.path()).expect("gix::init origin");
        write_initial_gix_commit(&origin_repo);

        let url = format!("file://{}", origin_dir.path().display());
        let repo_path = repo_dir.path().join("repo");
        let tc_path = tool_config_path(&cfg_dir);

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

    /// Write an initial empty commit to a freshly initialised gix repository.
    fn write_initial_gix_commit(repo: &gix::Repository) {
        let sig = gix::actor::SignatureRef::from_bytes(b"Test <test@test.test> 0 +0000")
            .expect("valid sig");

        let empty_tree = gix::objs::Tree::empty();
        let tree_id = repo.write_object(&empty_tree).expect("write tree").detach();

        // commit_as creates the commit and advances HEAD.
        let parents: Vec<gix::hash::ObjectId> = vec![];
        repo.commit_as(sig, sig, "HEAD", "initial", tree_id, parents)
            .expect("write initial commit");
    }
}
