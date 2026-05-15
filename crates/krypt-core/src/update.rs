//! Orchestration for `krypt update`.
//!
//! Fetches the dotfiles repo from origin (HTTPS only — gix 0.83 has no SSH
//! transport; see the follow-up issue for the tracking item), fast-forward-
//! advances the local branch, updates the working tree, then re-runs `link`
//! to deploy any new files.
//!
//! A dirty working tree is always an error: commit, stash, or discard changes
//! before running `krypt update`.  Auto-stash was removed pending gix gaining
//! stash support; see the follow-up issue for re-adding it.
//!
//! # HTTPS-only note
//!
//! gix 0.83 does not have an SSH transport, so only HTTPS URLs are supported.
//! SSH-based remote URLs will fail with a connection error from gix.  This
//! limitation will be lifted once gitoxide ships SSH support.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

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

    /// The working tree has uncommitted changes.
    ///
    /// A dirty dotfiles repo before `krypt update` is a smell, not a normal
    /// state.  The right answer is to commit, stash, or discard changes first.
    /// Auto-stash was removed while gix lacks a stash API; when gix ships
    /// stash support the auto-stash flow with a `--no-stash` opt-out will be
    /// restored.
    #[error(
        "working tree has uncommitted changes — commit, stash, or discard them \
         and re-run `krypt update`"
    )]
    DirtyWorkingTree,

    /// Opening the git repository failed.
    #[error("opening git repo at {path:?}: {source}")]
    OpenRepo {
        /// Path that was opened.
        path: PathBuf,
        /// Underlying gix error (boxed to keep the enum variant small).
        #[source]
        source: Box<gix::open::Error>,
    },

    /// Checking dirty status failed.
    #[error("checking git status: {0}")]
    GitStatus(#[source] Box<gix::status::is_dirty::Error>),

    /// No default remote found.
    #[error("no default fetch remote configured in {path:?}")]
    NoRemote {
        /// The repo path.
        path: PathBuf,
    },

    /// Connecting to the remote failed.
    #[error("connecting to remote: {0}")]
    Connect(#[source] Box<gix::remote::connect::Error>),

    /// Preparing the fetch failed.
    #[error("preparing fetch: {0}")]
    PrepareFetch(#[source] Box<gix::remote::fetch::prepare::Error>),

    /// The fetch itself failed.
    #[error("fetching from remote: {0}")]
    Fetch(#[source] Box<gix::remote::fetch::Error>),

    /// HEAD is detached (or the operation that needs HEAD failed).
    #[error("HEAD is detached or could not be resolved — cannot fast-forward")]
    DetachedHead,

    /// No remote-tracking ref found for the local branch.
    #[error("no remote-tracking ref for branch {branch:?}")]
    NoTrackingRef {
        /// The local branch name.
        branch: String,
    },

    /// Computing the merge-base failed (needed for FF check).
    #[error("merge-base computation: {0}")]
    MergeBase(#[source] gix::repository::merge_base::Error),

    /// The remote has commits that are not a fast-forward of the local HEAD.
    #[error("remote is not a fast-forward of local HEAD — cannot pull without merging")]
    NotFastForward,

    /// Advancing the local branch reference failed.
    #[error("advancing local branch ref: {0}")]
    RefEdit(#[source] gix::reference::edit::Error),

    /// Rebuilding the index from the new tree failed.
    #[error("rebuilding index from new commit tree: {0}")]
    IndexFromTree(#[source] gix::repository::index_from_tree::Error),

    /// Checking out the new working-tree state failed.
    #[error("checking out new working tree: {0}")]
    Checkout(#[source] Box<gix::worktree::state::checkout::Error>),

    /// Writing the updated index to disk failed.
    #[error("writing index: {0}")]
    WriteIndex(#[source] gix::index::file::write::Error),

    /// Resolving the checkout options failed.
    #[error("checkout options: {0}")]
    CheckoutOptions(#[source] Box<gix::config::checkout_options::Error>),

    /// Converting the object store to an `Arc` failed.
    #[error("converting object store to Arc: {0}")]
    OdbArc(#[source] std::io::Error),

    /// Peeling a reference to its target OID failed.
    #[error("looking up ref OID: {0}")]
    PeelRef(#[source] gix::reference::peel::Error),

    /// `link` step failed.
    #[error("deploy link: {0}")]
    Deploy(#[from] DeployError),
}

// ─── Options & report ───────────────────────────────────────────────────────

/// Inputs to [`update`].
///
/// The working tree **must** be clean before calling `update`.  If it is not,
/// [`update`] returns [`UpdateError::DirtyWorkingTree`] immediately.
/// There is no auto-stash option; commit, stash, or discard changes first.
/// Auto-stash will be re-added once gix gains stash support.
pub struct UpdateOpts {
    /// Path to the tool config (`${XDG_CONFIG}/krypt/config.toml`).
    pub tool_config_path: PathBuf,

    /// Override the path to `.krypt.toml`. Defaults to `<repo_path>/.krypt.toml`.
    pub config_path: Option<PathBuf>,

    /// Path for the deployment manifest.
    pub manifest_path: PathBuf,

    /// Pass `dry_run = true` to the link step.
    pub dry_run: bool,

    /// Documented no-op for forward compatibility (hook runner not yet implemented).
    pub skip_hooks: bool,

    /// Pass `force = true` to the link step.
    pub force: bool,
}

/// Summary returned by a successful [`update`].
#[derive(Debug)]
pub struct UpdateReport {
    /// Whether `git fetch` advanced the repo (i.e. there were new commits).
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
///
/// Errors immediately if the working tree is dirty.  There is no auto-stash;
/// that feature was removed pending gix gaining stash support.
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

    let pulled = gix_ff_pull(repo_path)?;

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
        pulled,
        link: link_report,
        version_warning,
        hooks_skipped,
    })
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// Open the repo, check it is clean, fetch from origin, and fast-forward the
/// local branch to the remote-tracking commit.
///
/// Returns `true` if new commits were received, `false` if already up to date.
///
/// # Why not shell out to `git pull --ff-only`?
///
/// We use gix as the sole git backend (no process spawning, no libgit2) so
/// the binary has zero runtime dependency on a system `git` and links only
/// rustls — no OpenSSL, no libssh2.  The trade-off is that we must implement
/// the pull logic ourselves:
///
/// 1. `repo.is_dirty()` — bail if uncommitted changes exist.
/// 2. `remote.connect(Fetch).prepare_fetch().receive()` — download new objects
///    and update `refs/remotes/origin/<branch>`.
/// 3. Confirm `merge_base(HEAD, remote_tracking) == HEAD` — i.e. remote is
///    strictly ahead (fast-forward safe).
/// 4. Advance the local branch ref and check out the new tree.
///
/// gix 0.83 has no stash API, so auto-stash was removed; see the follow-up
/// issue to restore it once gitoxide ships stash support.
fn gix_ff_pull(repo_path: &Path) -> Result<bool, UpdateError> {
    let repo = gix::open(repo_path).map_err(|e| UpdateError::OpenRepo {
        path: repo_path.to_path_buf(),
        source: Box::new(e),
    })?;

    // ── 1. Dirty check ───────────────────────────────────────────────────────
    if repo
        .is_dirty()
        .map_err(|e| UpdateError::GitStatus(Box::new(e)))?
    {
        return Err(UpdateError::DirtyWorkingTree);
    }

    // ── 2. Fetch from the default remote ────────────────────────────────────
    let interrupt = AtomicBool::new(false);

    let remote = repo
        .find_default_remote(gix::remote::Direction::Fetch)
        .ok_or_else(|| UpdateError::NoRemote {
            path: repo_path.to_path_buf(),
        })?
        .map_err(|_| UpdateError::NoRemote {
            path: repo_path.to_path_buf(),
        })?;

    remote
        .connect(gix::remote::Direction::Fetch)
        .map_err(|e| UpdateError::Connect(Box::new(e)))?
        .prepare_fetch(gix::progress::Discard, Default::default())
        .map_err(|e| UpdateError::PrepareFetch(Box::new(e)))?
        .receive(gix::progress::Discard, &interrupt)
        .map_err(|e| UpdateError::Fetch(Box::new(e)))?;

    // ── 3. Resolve local branch and remote-tracking ref ──────────────────────
    let head_ref = repo
        .head_ref()
        .map_err(|_| UpdateError::DetachedHead)?
        .ok_or(UpdateError::DetachedHead)?;

    let tracking_name = repo
        .branch_remote_tracking_ref_name(head_ref.name(), gix::remote::Direction::Fetch)
        .ok_or_else(|| UpdateError::NoTrackingRef {
            branch: head_ref.name().shorten().to_string(),
        })?
        .map_err(|_| UpdateError::NoTrackingRef {
            branch: head_ref.name().shorten().to_string(),
        })?;

    let mut tracking_ref =
        repo.find_reference(tracking_name.as_ref())
            .map_err(|_| UpdateError::NoTrackingRef {
                branch: head_ref.name().shorten().to_string(),
            })?;

    let new_oid = tracking_ref
        .peel_to_id()
        .map_err(UpdateError::PeelRef)?
        .detach();

    // ── 4. Already up to date? ───────────────────────────────────────────────
    let head_oid = repo
        .head_id()
        .map_err(|_| UpdateError::DetachedHead)?
        .detach();

    if head_oid == new_oid {
        return Ok(false);
    }

    // ── 5. Fast-forward check ────────────────────────────────────────────────
    //
    // A fast-forward is safe iff the current HEAD is an ancestor of the new
    // remote commit, i.e. merge_base(HEAD, new) == HEAD.
    let base = repo
        .merge_base(head_oid, new_oid)
        .map_err(UpdateError::MergeBase)?
        .detach();

    if base != head_oid {
        return Err(UpdateError::NotFastForward);
    }

    // ── 6. Advance the local branch ref ──────────────────────────────────────
    use gix::refs::{
        Target,
        transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog},
    };

    repo.edit_reference(RefEdit {
        change: Change::Update {
            log: LogChange {
                mode: RefLog::AndReference,
                force_create_reflog: false,
                message: "krypt update: fast-forward".into(),
            },
            expected: PreviousValue::MustExistAndMatch(Target::Object(head_oid)),
            new: Target::Object(new_oid),
        },
        name: head_ref.name().to_owned(),
        deref: false,
    })
    .map_err(UpdateError::RefEdit)?;

    // ── 7. Update the working tree to match the new commit ───────────────────
    //
    // The working tree is guaranteed clean (step 1), so rebuilding the index
    // from the new tree and checking out is equivalent to `git reset --hard`.
    // Files removed from the new tree must be explicitly unlinked: we compare
    // the old and new indices and delete anything that disappeared.
    let new_commit = repo
        .find_object(new_oid)
        .map_err(|_| UpdateError::DetachedHead)?;
    let new_tree = new_commit
        .peel_to_tree()
        .map_err(|_| UpdateError::DetachedHead)?;
    let new_tree_id = new_tree.id;

    // Build new index from new tree (high-level helper on Repository).
    let mut new_index = repo
        .index_from_tree(new_tree_id.as_ref())
        .map_err(UpdateError::IndexFromTree)?;

    let new_paths: std::collections::HashSet<Vec<u8>> = new_index
        .entries()
        .iter()
        .map(|e| {
            let p: &[u8] = e.path(&new_index);
            p.to_vec()
        })
        .collect();

    // Load the previous index to discover deleted files.
    let old_index = repo
        .index_or_load_from_head()
        .map_err(|_| UpdateError::DetachedHead)?;

    let workdir = repo.workdir().ok_or(UpdateError::DetachedHead)?;

    for entry in old_index.entries() {
        let rel: &[u8] = entry.path(&old_index);
        if !new_paths.contains(rel)
            && let Ok(rel_str) = std::str::from_utf8(rel)
        {
            let _ = std::fs::remove_file(workdir.join(std::path::Path::new(rel_str)));
        }
    }

    // Check out the new index into the working directory.
    let checkout_opts = repo
        .checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)
        .map_err(|e| UpdateError::CheckoutOptions(Box::new(e)))?;

    let interrupt2 = AtomicBool::new(false);
    let files = gix::progress::Discard;
    let bytes = gix::progress::Discard;

    gix::worktree::state::checkout(
        &mut new_index,
        workdir,
        repo.objects
            .clone()
            .into_arc()
            .map_err(UpdateError::OdbArc)?,
        &files,
        &bytes,
        &interrupt2,
        checkout_opts,
    )
    .map_err(|e| UpdateError::Checkout(Box::new(e)))?;

    new_index
        .write(Default::default())
        .map_err(UpdateError::WriteIndex)?;

    Ok(true)
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
    use tempfile::tempdir;

    // ── gix test helpers ────────────────────────────────────────────────────

    fn test_sig_raw() -> &'static str {
        // Raw git signature format: "Name <email> seconds tz"
        "Test <test@test.test> 0 +0000"
    }

    /// Write a commit directly via gix's high-level `commit_as` API.
    fn write_commit(repo: &gix::Repository, message: &str, files: &[(&str, &[u8])]) {
        // Build and write blob objects, then build tree.
        let mut tree_entries: Vec<gix::objs::tree::Entry> = files
            .iter()
            .map(|(name, content)| {
                let blob_id = repo.write_blob(content).expect("write blob").detach();
                gix::objs::tree::Entry {
                    mode: gix::objs::tree::EntryKind::Blob.into(),
                    filename: (*name).into(),
                    oid: blob_id,
                }
            })
            .collect();
        tree_entries.sort_by(|a, b| a.filename.cmp(&b.filename));

        let tree = gix::objs::Tree {
            entries: tree_entries,
        };
        let tree_id = repo.write_object(&tree).expect("write tree").detach();

        let sig = gix::actor::SignatureRef::from_bytes(test_sig_raw().as_bytes())
            .expect("valid test sig");
        let parent: Vec<gix::hash::ObjectId> = repo
            .head_id()
            .ok()
            .map(|id| id.detach())
            .into_iter()
            .collect();

        // commit_as updates HEAD automatically (deref through symbolic HEAD).
        repo.commit_as(sig, sig, "HEAD", message, tree_id, parent)
            .expect("write commit");
    }

    /// Init a new repo with a single empty commit.
    fn init_with_commit(dir: &Path) -> gix::Repository {
        let repo = gix::init(dir).expect("gix::init");
        write_commit(&repo, "initial", &[]);
        repo
    }

    fn make_tool_config(repo_path: &Path, tc_dir: &tempfile::TempDir) -> PathBuf {
        let tc_path = tc_dir.path().join("krypt").join("config.toml");
        let cfg = crate::tool_config::ToolConfig {
            repo: crate::tool_config::RepoConfig {
                path: repo_path.to_path_buf(),
                url: None,
            },
        };
        cfg.save(&tc_path).unwrap();
        tc_path
    }

    // ── Tests ────────────────────────────────────────────────────────────────

    /// A modified index entry (tree-vs-index mismatch) causes `DirtyWorkingTree`.
    ///
    /// gix's `is_dirty()` does not flag *untracked* files (matching git's
    /// `--ignore-untracked` semantics).  For a dotfiles repo this is correct:
    /// a stray untracked file in the repo root should not block a pull.
    ///
    /// We trigger a tree-vs-index mismatch by staging a blob that is different
    /// from what the HEAD commit contains.
    #[test]
    fn dirty_tree_always_errors() {
        let local = tempdir().unwrap();

        // Commit a tracked file.
        write_commit(
            &init_with_commit(local.path()),
            "add file",
            &[("tracked.txt", b"original")],
        );

        // Make the index dirty: write the file with different content to disk
        // AND update the index to point to a blob with different content than
        // the HEAD tree has.  We do this by staging via gix's index APIs.
        //
        // The simplest approach: after commit, the index (if it exists on disk)
        // should match HEAD.  We rebuild it from the current HEAD tree, then
        // write different content to disk so that the index SHA != worktree SHA.
        {
            let repo = gix::open(local.path()).expect("open");
            let head_tree_id = repo
                .head_commit()
                .expect("head commit")
                .tree_id()
                .expect("tree");
            let mut idx = repo
                .index_from_tree(head_tree_id.as_ref())
                .expect("index from tree");
            // Write the index to disk so gix can compare it with the worktree.
            idx.write(Default::default()).expect("write index");
        }
        // Now modify the file on disk so it differs from what the index records.
        fs::write(local.path().join("tracked.txt"), b"modified").unwrap();

        let tc_dir = tempdir().unwrap();
        let tc_path = make_tool_config(local.path(), &tc_dir);
        let state = tempdir().unwrap();

        let err = update(&UpdateOpts {
            tool_config_path: tc_path,
            config_path: Some(local.path().join(".krypt.toml")),
            manifest_path: state.path().join("manifest.json"),
            dry_run: false,
            skip_hooks: false,
            force: false,
        })
        .unwrap_err();

        assert!(
            matches!(err, UpdateError::DirtyWorkingTree),
            "expected DirtyWorkingTree, got {err:?}"
        );
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
            skip_hooks: false,
            force: false,
        })
        .unwrap_err();

        assert!(
            matches!(err, UpdateError::ToolConfigMissing { ref path } if path == &tc_path),
            "expected ToolConfigMissing, got {err:?}"
        );
    }

    #[test]
    fn version_warning_fires_when_older() {
        assert!(version_less_than("0.0.2", "99.0.0"));
        let warn = version_warning_if_older("99.0.0");
        assert!(warn.is_some());
        assert!(warn.unwrap().contains("99.0.0"));
    }

    #[test]
    fn version_warning_absent_when_current() {
        let our = env!("CARGO_PKG_VERSION");
        assert!(version_warning_if_older(our).is_none());
    }

    #[test]
    fn parse_version_basic() {
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.0.0"), Some((0, 0, 0)));
        assert!(parse_version("bad").is_none());
    }
}
