//! Orchestration for `krypt update`.
//!
//! Fetches the dotfiles repo from origin (HTTPS only — the fork does not yet
//! have an SSH transport; see the follow-up issue for the tracking item),
//! fast-forward-advances the local branch, updates the working tree, then
//! re-runs `link` to deploy any new files.
//!
//! By default a dirty working tree is auto-stashed before the pull and
//! restored afterwards.  Pass `no_stash: true` in [`UpdateOpts`] (or
//! `--no-stash` at the CLI) to skip auto-stash and error immediately on a
//! dirty tree (the old behaviour).
//!
//! # HTTPS-only note
//!
//! The gitoxide fork does not have an SSH transport, so only HTTPS URLs are
//! supported.  SSH-based remote URLs will fail with a connection error from
//! gix.  This limitation will be lifted once gitoxide ships SSH support.

// `UpdateError` wraps gix errors (already boxed) and `ToolConfigError`;
// on Windows the combined enum exceeds clippy's 128-byte threshold.
// The variants are already as compact as the upstream types allow.
#![allow(clippy::result_large_err)]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gix::hash::ObjectId;
use gix_actor::Signature as ActorSignature;

use thiserror::Error;

use crate::deploy::{DeployError, DeployOpts, LinkReport, link};
use crate::runner::RunnerError;
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

    /// The working tree has uncommitted changes and `--no-stash` was set.
    ///
    /// By default `krypt update` auto-stashes a dirty working tree and
    /// re-applies the changes after the pull.  This error only fires when
    /// `no_stash: true` is passed in [`UpdateOpts`] (CLI flag `--no-stash`).
    /// Without that flag, commit or discard changes if you do not want
    /// auto-stash.
    #[error(
        "working tree has uncommitted changes and --no-stash was set — \
         commit or discard changes and re-run `krypt update`, \
         or remove --no-stash to enable auto-stash"
    )]
    DirtyWorkingTree,

    /// Auto-stash succeeded but the pop after the pull produced merge conflicts.
    ///
    /// The pull completed successfully.  Your pre-update changes are still in
    /// the working tree as conflict markers, and `refs/stash` still holds the
    /// original stash so you can re-apply it after manual resolution.
    #[error(
        "auto-stash pop produced merge conflicts after the pull — \
         resolve the conflicts in the working tree, then drop the stash \
         with `git stash drop` (stash OID: {stash_oid})"
    )]
    AutoStashConflict {
        /// OID of the stash commit that was not dropped due to conflicts.
        stash_oid: ObjectId,
    },

    /// Auto-stash push failed.
    #[error("auto-stash push failed: {0}")]
    StashPush(#[source] Box<gix::stash::PushError>),

    /// Auto-stash pop failed.
    #[error("auto-stash pop failed: {0}")]
    StashPop(#[source] Box<gix::stash::PopError>),

    /// No committer identity configured in git config.
    #[error(
        "no committer identity configured — set user.name and user.email in git config \
         (needed for auto-stash commit)"
    )]
    NoCommitter,

    /// Committer identity has a bad timestamp in git config.
    #[error("could not determine committer identity for auto-stash: {0}")]
    Committer(#[source] gix::config::time::Error),

    /// Could not open the index for the auto-stash.
    #[error("opening git index for auto-stash: {0}")]
    OpenIndex(#[source] gix::worktree::open_index::Error),

    /// Could not build blob-merge platform needed by auto-stash pop.
    #[error("building merge resource cache for auto-stash pop: {0}")]
    MergeResourceCache(#[source] Box<gix::repository::merge_resource_cache::Error>),

    /// Could not build diff resource cache needed by auto-stash pop.
    #[error("building diff resource cache for auto-stash pop: {0}")]
    DiffResourceCache(#[source] gix::repository::diff_resource_cache::Error),

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

    /// A post-update hook failed and `ignore_failure` was not set.
    ///
    /// `RunnerError` is boxed to keep the enum variant ≤ 128 bytes on Windows.
    #[error("hook {name:?} failed: {source}")]
    Hook {
        /// The hook's `name` field.
        name: String,
        /// The underlying runner error.
        #[source]
        source: Box<RunnerError>,
    },
}

// ─── Options & report ───────────────────────────────────────────────────────

/// Inputs to [`update`].
///
/// By default a dirty working tree is auto-stashed before the pull and
/// restored afterwards.  Set `no_stash = true` to skip auto-stash and error
/// immediately on a dirty working tree (matches the old behaviour before gix
/// gained stash support).
pub struct UpdateOpts {
    /// Path to the tool config (`${XDG_CONFIG}/krypt/config.toml`).
    pub tool_config_path: PathBuf,

    /// Override the path to `.krypt.toml`. Defaults to `<repo_path>/.krypt.toml`.
    pub config_path: Option<PathBuf>,

    /// Path for the deployment manifest.
    pub manifest_path: PathBuf,

    /// Pass `dry_run = true` to the link step.
    pub dry_run: bool,

    /// Skip all post-update hooks.
    pub skip_hooks: bool,

    /// Pass `force = true` to the link step.
    pub force: bool,

    /// When `true`, skip auto-stash and error if the working tree is dirty
    /// (matches the prior behaviour before gix gained stash support).
    pub no_stash: bool,
}

pub use crate::hooks::HookSummary;

/// Summary returned by a successful [`update`].
#[derive(Debug)]
pub struct UpdateReport {
    /// Whether `git fetch` advanced the repo (i.e. there were new commits).
    pub pulled: bool,

    /// Report from the `link` step.
    pub link: LinkReport,

    /// Version warning if our binary is older than `[meta] krypt_min`.
    pub version_warning: Option<String>,

    /// Summary of `post-update` hook execution.
    pub hooks: HookSummary,

    /// Whether the working tree was auto-stashed before the pull and
    /// re-applied after. Under `dry_run`, whether it would have been: a dry
    /// run neither fetches nor stashes, so `pulled` is then always `false`.
    pub stashed: bool,
}

// ─── Implementation ──────────────────────────────────────────────────────────

/// Pull the dotfiles repo and re-deploy.
///
/// By default a dirty working tree is auto-stashed before the pull and
/// restored afterwards.  Set `opts.no_stash = true` to skip auto-stash and
/// error immediately on a dirty tree.
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

    let (pulled, stashed) = if opts.dry_run {
        (false, dry_run_pull(repo_path, opts.no_stash)?)
    } else {
        gix_ff_pull(repo_path, opts.no_stash)?
    };

    let krypt_cfg = crate::include::load_with_includes(&config_path).ok();

    let version_warning = krypt_cfg
        .as_ref()
        .and_then(|c| c.meta.krypt_min.as_deref())
        .and_then(version_warning_if_older);

    let link_report = link(&DeployOpts {
        config_path,
        manifest_path: opts.manifest_path.clone(),
        platform: None,
        dry_run: opts.dry_run,
        force: opts.force,
    })?;

    let hooks_summary = crate::hooks::run(
        krypt_cfg.as_ref(),
        crate::hooks::POST_UPDATE,
        opts.skip_hooks,
        opts.dry_run,
    )
    .map_err(|e| UpdateError::Hook {
        name: e.name,
        source: e.source,
    })?;

    Ok(UpdateReport {
        pulled,
        link: link_report,
        version_warning,
        hooks: hooks_summary,
        stashed,
    })
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// Open the repo, optionally auto-stash a dirty working tree, fetch from
/// origin, fast-forward the local branch, then restore the stash.
///
/// Returns `(pulled, stashed)` where:
/// * `pulled` — `true` if new commits were received, `false` if already up to date.
/// * `stashed` — `true` if the working tree was auto-stashed and restored.
///
/// # Why not shell out to `git pull --ff-only`?
///
/// We use gix as the sole git backend (no process spawning, no libgit2) so
/// the binary has zero runtime dependency on a system `git` and links only
/// rustls — no OpenSSL, no libssh2.  The trade-off is that we must implement
/// the pull logic ourselves:
///
/// 1. `repo.is_dirty()` — auto-stash if dirty (or bail with `DirtyWorkingTree`
///    when `no_stash` is true).
/// 2. `remote.connect(Fetch).prepare_fetch().receive()` — download new objects
///    and update `refs/remotes/origin/<branch>`.
/// 3. Confirm `merge_base(HEAD, remote_tracking) == HEAD` — i.e. remote is
///    strictly ahead (fast-forward safe).
/// 4. Advance the local branch ref and check out the new tree.
/// 5. Pop the stash if one was created.
fn gix_ff_pull(repo_path: &Path, no_stash: bool) -> Result<(bool, bool), UpdateError> {
    // ── 1. Dirty check / auto-stash ──────────────────────────────────────────
    let (mut repo, is_dirty) = open_and_check_dirty(repo_path)?;

    let mut stashed = false;
    let mut stash_oid: Option<ObjectId> = None;

    if is_dirty {
        if no_stash {
            return Err(UpdateError::DirtyWorkingTree);
        }
        // Auto-stash: capture current state and reset WT to HEAD.
        // `is_dirty()` can produce a false positive (stat-cache staleness).
        // When that happens, gix::stash::push returns NoLocalChanges — treat
        // it as a clean tree and skip the stash rather than erroring out.
        match stash_push(&mut repo, repo_path) {
            Ok(oid) => {
                stash_oid = Some(oid);
                stashed = true;
            }
            Err(UpdateError::StashPush(ref e))
                if matches!(**e, gix::stash::PushError::NoLocalChanges) => {}
            Err(e) => return Err(e),
        }
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
        // Nothing to pull — still pop the stash if we pushed one.
        if let Some(sid) = stash_oid {
            stash_pop(&mut repo, repo_path, head_oid, sid)?;
        }
        return Ok((false, stashed));
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
    //
    // All borrows of `repo` that hold references (objects, workdir) are
    // scoped here so they are dropped before `stash_pop` takes a `&mut repo`.
    {
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

        // Clone the workdir path so it doesn't borrow `repo` across the block.
        let workdir = repo.workdir().ok_or(UpdateError::DetachedHead)?.to_owned();

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
            &workdir,
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
    }

    // ── 8. Pop stash if we pushed one ────────────────────────────────────────
    //
    // The new HEAD tree is used as the "ours" side of the 3-way merge inside
    // pop.  We use `new_oid` (the updated HEAD) rather than the original
    // `head_oid` so that the merge base is the stash parent[0] and the
    // merge resolves against the pulled state.
    if let Some(sid) = stash_oid {
        stash_pop(&mut repo, repo_path, new_oid, sid)?;
    }

    Ok((true, stashed))
}

/// The pull step of a dry run: report whether [`gix_ff_pull`] would
/// auto-stash, and refuse as it would under `no_stash`, without fetching,
/// stashing, moving the branch or checking anything out.
fn dry_run_pull(repo_path: &Path, no_stash: bool) -> Result<bool, UpdateError> {
    let (_, is_dirty) = open_and_check_dirty(repo_path)?;
    if is_dirty && no_stash {
        return Err(UpdateError::DirtyWorkingTree);
    }
    Ok(is_dirty)
}

fn open_and_check_dirty(repo_path: &Path) -> Result<(gix::Repository, bool), UpdateError> {
    let repo = gix::open(repo_path).map_err(|e| UpdateError::OpenRepo {
        path: repo_path.to_path_buf(),
        source: Box::new(e),
    })?;
    let is_dirty = repo
        .is_dirty()
        .map_err(|e| UpdateError::GitStatus(Box::new(e)))?;
    Ok((repo, is_dirty))
}

// ─── Stash helpers ───────────────────────────────────────────────────────────

/// Capture the current working-tree state as a stash commit at `refs/stash`.
///
/// Returns the OID of the newly-created stash commit so the caller can pass it
/// to [`stash_pop`].
///
/// The stash message is `krypt-update-auto-stash` so it is easy to identify.
/// Untracked files are included (`include_untracked: true`) so that new,
/// unstaged files are not silently left behind after the pull.
fn stash_push(repo: &mut gix::Repository, repo_path: &Path) -> Result<ObjectId, UpdateError> {
    let head_oid = repo
        .head_id()
        .map_err(|_| UpdateError::DetachedHead)?
        .detach();

    let head_commit = repo
        .find_object(head_oid)
        .map_err(|_| UpdateError::DetachedHead)?;
    let head_tree_id = head_commit
        .peel_to_tree()
        .map_err(|_| UpdateError::DetachedHead)?
        .id;

    // Read the on-disk index.
    let index_file = repo.open_index().map_err(UpdateError::OpenIndex)?;
    let index_state = index_file.into();

    // Resolve committer identity — use the fallback path so we never panic on
    // an unconfigured repo (krypt sets up a sane user.name / user.email).
    let committer_sig = repo
        .committer()
        .ok_or(UpdateError::NoCommitter)?
        .map_err(UpdateError::Committer)?;
    // Materialize as owned so we can take a fresh `to_ref` with a local TimeBuf.
    let committer_owned: ActorSignature = committer_sig.into();

    let workdir = repo.workdir().ok_or(UpdateError::DetachedHead)?.to_owned();

    let head_branch: Option<gix::refs::FullName> =
        repo.head_ref().ok().flatten().map(|r| r.name().to_owned());

    let mut time_buf = gix::date::parse::TimeBuf::default();
    let committer_ref = committer_owned.to_ref(&mut time_buf);

    let outcome = gix::stash::push(
        gix::stash::PushContext {
            refs: &repo.refs,
            objects: &repo.objects,
            index: &index_state,
            worktree: &workdir,
            committer: committer_ref,
            // TODO(gix-stash): wire up smudge filters once gix-stash uses the
            // filter pipeline.  For krypt dotfile content the default (no
            // filters) is correct.
            checkout_options: gix_worktree_state::checkout::Options {
                overwrite_existing: true,
                ..Default::default()
            },
        },
        head_oid,
        head_tree_id,
        head_branch.as_ref().map(|n| n.as_ref()),
        gix::stash::PushOptions {
            include_untracked: true,
            keep_index: false,
            message: Some("krypt-update-auto-stash".into()),
            include_ignored: false,
        },
    )
    .map_err(|e| UpdateError::StashPush(Box::new(e)))?;

    tracing::info!(
        stash = %outcome.stash,
        repo = %repo_path.display(),
        "auto-stashed working tree before krypt update"
    );

    Ok(outcome.stash)
}

/// Apply the latest stash entry to the working tree (3-way merge) and drop it
/// from `refs/stash`.
///
/// `head_tree` is the OID of the root tree that `HEAD` currently points at
/// (the "ours" side of the 3-way merge).  `_stash_oid` is used only for
/// conflict-error reporting; the actual pop reads `refs/stash` directly.
fn stash_pop(
    repo: &mut gix::Repository,
    repo_path: &Path,
    head_tree_commit: ObjectId,
    stash_oid: ObjectId,
) -> Result<(), UpdateError> {
    // Resolve the HEAD tree OID from the commit.
    let head_tree_id = repo
        .find_object(head_tree_commit)
        .map_err(|_| UpdateError::DetachedHead)?
        .peel_to_tree()
        .map_err(|_| UpdateError::DetachedHead)?
        .id;

    let committer_sig = repo
        .committer()
        .ok_or(UpdateError::NoCommitter)?
        .map_err(UpdateError::Committer)?;
    let committer_owned: ActorSignature = committer_sig.into();

    let workdir = repo.workdir().ok_or(UpdateError::DetachedHead)?.to_owned();

    // Build the blob-merge and diff-resource platforms the pop context needs.
    let mut blob_merge = repo
        .merge_resource_cache(Default::default())
        .map_err(|e| UpdateError::MergeResourceCache(Box::new(e)))?;
    let mut diff_cache = repo
        .diff_resource_cache_for_tree_diff()
        .map_err(UpdateError::DiffResourceCache)?;

    let mut time_buf = gix::date::parse::TimeBuf::default();
    let committer_ref = committer_owned.to_ref(&mut time_buf);

    let outcome = gix::stash::pop(
        gix::stash::PopContext {
            refs: &repo.refs,
            objects: &repo.objects,
            committer: committer_ref,
            worktree: &workdir,
            blob_merge: &mut blob_merge,
            diff_cache: &mut diff_cache,
            checkout_options: gix_worktree_state::checkout::Options {
                overwrite_existing: true,
                ..Default::default()
            },
        },
        head_tree_id,
    )
    .map_err(|e| UpdateError::StashPop(Box::new(e)))?;

    if outcome.had_conflicts {
        tracing::warn!(
            stash = %stash_oid,
            repo = %repo_path.display(),
            "auto-stash pop produced merge conflicts after krypt update"
        );
        return Err(UpdateError::AutoStashConflict { stash_oid });
    }

    tracing::info!(
        stash = %stash_oid,
        repo = %repo_path.display(),
        "auto-stash restored cleanly after krypt update"
    );

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

    /// A repo whose `tracked.txt` is modified on disk after its commit, and
    /// which has no remote.
    ///
    /// gix's `is_dirty()` does not flag *untracked* files (matching git's
    /// `--ignore-untracked` semantics).  For a dotfiles repo this is correct:
    /// a stray untracked file in the repo root should not block a pull.
    fn dirty_repo() -> tempfile::TempDir {
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
        local
    }

    /// A modified index entry (tree-vs-index mismatch) causes `DirtyWorkingTree`.
    #[test]
    fn dirty_tree_always_errors() {
        let local = dirty_repo();
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
            no_stash: true,
        })
        .unwrap_err();

        assert!(
            matches!(err, UpdateError::DirtyWorkingTree),
            "expected DirtyWorkingTree, got {err:?}"
        );
    }

    /// `--dry-run` leaves a dirty tree as it is: no stash, no fetch (the repo
    /// has no remote, which a real pull would fail on), and reports that the
    /// real run would stash.
    #[test]
    fn dry_run_does_not_stash_or_pull() {
        let local = dirty_repo();
        fs::write(local.path().join(".krypt.toml"), "").unwrap();
        let tc_dir = tempdir().unwrap();
        let tc_path = make_tool_config(local.path(), &tc_dir);
        let state = tempdir().unwrap();

        let report = update(&UpdateOpts {
            tool_config_path: tc_path,
            config_path: Some(local.path().join(".krypt.toml")),
            manifest_path: state.path().join("manifest.json"),
            dry_run: true,
            skip_hooks: false,
            force: false,
            no_stash: false,
        })
        .expect("dry run");

        assert!(report.stashed, "a dirty tree would be stashed");
        assert!(!report.pulled);
        assert_eq!(
            fs::read(local.path().join("tracked.txt")).unwrap(),
            b"modified"
        );
        let repo = gix::open(local.path()).expect("open");
        assert!(
            repo.try_find_reference("refs/stash")
                .expect("lookup")
                .is_none(),
            "dry run created a stash"
        );
    }

    /// `--dry-run --no-stash` on a dirty tree refuses, as the real run would.
    #[test]
    fn dry_run_no_stash_dirty_errors() {
        let local = dirty_repo();
        let tc_dir = tempdir().unwrap();
        let tc_path = make_tool_config(local.path(), &tc_dir);
        let state = tempdir().unwrap();

        let err = update(&UpdateOpts {
            tool_config_path: tc_path,
            config_path: Some(local.path().join(".krypt.toml")),
            manifest_path: state.path().join("manifest.json"),
            dry_run: true,
            skip_hooks: false,
            force: false,
            no_stash: true,
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
            no_stash: false,
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
