//! `krypt doctor` — diagnostic health-check for an install.
//!
//! Collects a set of named checks into a [`DoctorReport`] struct, each
//! represented by a [`CheckStatus`] that can be `Ok`, `Warn`, `Fail`, or
//! `NotApplicable`.  The report is serde-serializable for `--json` output and
//! has a human-readable [`DoctorReport::render_text`] method.
//!
//! Entry point: [`doctor`].

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::manifest::{DriftStatus, Manifest, detect_drift};
use crate::paths::Platform;
use crate::tool_config::ToolConfig;

// ─── CheckStatus ────────────────────────────────────────────────────────────

/// Result of a single diagnostic check.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "detail", rename_all = "snake_case")]
pub enum CheckStatus<T: Serialize> {
    /// Check passed; `T` is the associated value.
    Ok(T),
    /// Check has a non-fatal concern; message explains what.
    Warn(String),
    /// Check failed; message explains what.
    Fail(String),
    /// Check does not apply in this context; reason explains why.
    NotApplicable(String),
}

impl<T: Serialize> CheckStatus<T> {
    /// Returns `true` only for `Ok`.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }

    /// Returns `true` for `Warn` or `Fail` (not `NotApplicable`).
    pub fn needs_attention(&self) -> bool {
        matches!(self, CheckStatus::Warn(_) | CheckStatus::Fail(_))
    }

    /// Single-character sigil for the text renderer.
    pub fn sigil(&self) -> char {
        match self {
            CheckStatus::Ok(_) => '✓',
            CheckStatus::Warn(_) => '!',
            CheckStatus::Fail(_) => '✗',
            CheckStatus::NotApplicable(_) => '-',
        }
    }
}

// ─── DoctorOpts ─────────────────────────────────────────────────────────────

/// Inputs to [`doctor`].
pub struct DoctorOpts {
    /// Path to `${XDG_CONFIG}/krypt/config.toml`.
    pub tool_config_path: PathBuf,
    /// Override the path to `.krypt.toml`. Derived from tool config when absent.
    pub config_path: Option<PathBuf>,
    /// Path to the deployment manifest JSON.
    pub manifest_path: PathBuf,
    /// Override the repo path. Derived from tool config when absent.
    pub repo_path: Option<PathBuf>,
    /// Detected package manager name, supplied by the CLI layer (krypt-pkg).
    /// `None` signals that detection was skipped or nothing was found.
    pub detected_manager: Option<String>,
}

// ─── DoctorReport ───────────────────────────────────────────────────────────

/// Summary of every diagnostic check.
///
/// Serialize this with `serde_json` for machine-readable output, or call
/// [`DoctorReport::render_text`] for a human-readable report.
#[derive(Debug, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Version of the `krypt` binary.
    pub tool_version: String,
    /// Tool config (`${XDG_CONFIG}/krypt/config.toml`) loaded status + path.
    pub tool_config: CheckStatus<String>,
    /// Whether the repo path exists on disk.
    pub repo_path: CheckStatus<String>,
    /// Whether the repo path is a git repository (via gix).
    pub repo_is_git: CheckStatus<String>,
    /// Whether the git working tree is clean.
    pub working_tree: CheckStatus<String>,
    /// Whether `.krypt.toml` parses and validates.
    pub krypt_config: CheckStatus<String>,
    /// Whether all `[[link]]` src files exist on disk.
    pub link_sources: CheckStatus<String>,
    /// Drift status of all deployed `[[link]]` destinations.
    pub link_destinations: CheckStatus<String>,
    /// Manifest load status + age.
    pub manifest: CheckStatus<String>,
    /// Detected platform.
    pub platform: CheckStatus<String>,
    /// Package manager detection (deferred to #19).
    pub package_manager: CheckStatus<String>,
    /// Hook runner status (deferred to #43).
    pub hooks: CheckStatus<String>,
}

impl DoctorReport {
    /// Returns `true` when every applicable check is `Ok`.
    pub fn is_all_green(&self) -> bool {
        self.tool_config.is_ok()
            && self.repo_path.is_ok()
            && self.repo_is_git.is_ok()
            && self.working_tree.is_ok()
            && self.krypt_config.is_ok()
            && self.link_sources.is_ok()
            && self.link_destinations.is_ok()
            && self.manifest.is_ok()
            && self.platform.is_ok()
    }

    /// Render a single-column human-readable report.
    pub fn render_text(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("krypt {}", self.tool_version));
        lines.push(String::new());

        let rows: Vec<(&str, char, String)> = vec![
            check_row("tool config", &self.tool_config),
            check_row("repo path", &self.repo_path),
            check_row("repo is git", &self.repo_is_git),
            check_row("working tree", &self.working_tree),
            check_row("config", &self.krypt_config),
            check_row("link sources", &self.link_sources),
            check_row("link destinations", &self.link_destinations),
            check_row("manifest", &self.manifest),
            check_row("platform", &self.platform),
            check_row("package manager", &self.package_manager),
            check_row("hooks", &self.hooks),
        ];

        let label_width = rows.iter().map(|(l, _, _)| l.len()).max().unwrap_or(0);
        let mut attention = 0usize;
        let applicable = rows.len();

        for (label, sigil, detail) in &rows {
            lines.push(format!("{sigil} {label:<label_width$}  {detail}"));
            if *sigil != '✓' && *sigil != '-' {
                attention += 1;
            }
        }

        lines.push(String::new());
        if attention == 0 {
            lines.push(format!("all {applicable} checks passed."));
        } else {
            lines.push(format!("{attention}/{applicable} checks need attention."));
        }

        lines.join("\n")
    }
}

fn check_row<'a, T: Serialize>(label: &'a str, status: &CheckStatus<T>) -> (&'a str, char, String) {
    let (sigil, detail) = render_check(status);
    (label, sigil, detail)
}

fn render_check<T: Serialize>(status: &CheckStatus<T>) -> (char, String) {
    let sigil = status.sigil();
    let detail = match status {
        CheckStatus::Ok(v) => serde_json::to_value(v)
            .ok()
            .and_then(|j| j.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{}", serde_json::to_value(v).unwrap_or_default())),
        CheckStatus::Warn(m) | CheckStatus::Fail(m) => m.clone(),
        CheckStatus::NotApplicable(r) => r.clone(),
    };
    (sigil, detail)
}

// ─── doctor ─────────────────────────────────────────────────────────────────

/// Run all diagnostic checks and return a [`DoctorReport`].
///
/// This function never panics and never returns `Err`; individual check
/// failures are captured inside the report.  The caller decides the process
/// exit code via [`DoctorReport::is_all_green`].
pub fn doctor(opts: &DoctorOpts) -> DoctorReport {
    let tool_version = env!("CARGO_PKG_VERSION").to_owned();

    // ── tool config ──────────────────────────────────────────────────────────
    let (tool_config_check, tool_cfg) = check_tool_config(&opts.tool_config_path);

    // ── derive repo path ─────────────────────────────────────────────────────
    let resolved_repo = opts
        .repo_path
        .clone()
        .or_else(|| tool_cfg.as_ref().map(|tc| tc.repo.path.clone()));

    // ── repo path exists ─────────────────────────────────────────────────────
    let (repo_path_check, repo_path_ok) = match &resolved_repo {
        None => (
            CheckStatus::Fail("cannot determine repo path — tool config missing".into()),
            false,
        ),
        Some(rp) => {
            if rp.exists() {
                (CheckStatus::Ok(rp.display().to_string()), true)
            } else {
                (
                    CheckStatus::Fail(format!("{} does not exist", rp.display())),
                    false,
                )
            }
        }
    };

    // ── repo is git ──────────────────────────────────────────────────────────
    let (repo_is_git_check, gix_repo) = if repo_path_ok {
        let rp = resolved_repo.as_deref().unwrap();
        check_git_repo(rp)
    } else {
        (
            CheckStatus::Fail("skipped — repo path not available".into()),
            None,
        )
    };

    // ── working tree clean ───────────────────────────────────────────────────
    let working_tree_check = check_working_tree(gix_repo.as_ref());

    // ── .krypt.toml parses ───────────────────────────────────────────────────
    let config_path = opts
        .config_path
        .clone()
        .or_else(|| resolved_repo.as_ref().map(|rp| rp.join(".krypt.toml")));

    let (krypt_config_check, krypt_cfg) = check_krypt_config(config_path.as_deref());

    // ── link sources exist ───────────────────────────────────────────────────
    let link_sources_check = check_link_sources(krypt_cfg.as_ref(), resolved_repo.as_deref());

    // ── link destinations (drift) ────────────────────────────────────────────
    let link_destinations_check = check_link_destinations(&opts.manifest_path);

    // ── manifest ─────────────────────────────────────────────────────────────
    let manifest_check = check_manifest(&opts.manifest_path);

    // ── platform ─────────────────────────────────────────────────────────────
    let platform_check = CheckStatus::Ok(Platform::current().as_str().to_owned());

    // ── package manager ──────────────────────────────────────────────────────
    let package_manager_check = match &opts.detected_manager {
        Some(name) => CheckStatus::Ok(name.clone()),
        None => CheckStatus::Warn("no package manager detected on PATH".into()),
    };

    DoctorReport {
        tool_version,
        tool_config: tool_config_check,
        repo_path: repo_path_check,
        repo_is_git: repo_is_git_check,
        working_tree: working_tree_check,
        krypt_config: krypt_config_check,
        link_sources: link_sources_check,
        link_destinations: link_destinations_check,
        manifest: manifest_check,
        platform: platform_check,
        package_manager: package_manager_check,
        hooks: CheckStatus::NotApplicable("pending #43".into()),
    }
}

// ─── individual checks ────────────────────────────────────────────────────

fn check_tool_config(path: &Path) -> (CheckStatus<String>, Option<ToolConfig>) {
    match ToolConfig::load(path) {
        Ok(Some(cfg)) => (CheckStatus::Ok(path.display().to_string()), Some(cfg)),
        Ok(None) => (
            CheckStatus::Fail(format!("not found at {}", path.display())),
            None,
        ),
        Err(e) => (CheckStatus::Fail(format!("load error: {e}")), None),
    }
}

fn check_git_repo(repo_path: &Path) -> (CheckStatus<String>, Option<gix::Repository>) {
    match gix::open(repo_path) {
        Ok(repo) => {
            let head = repo
                .head_commit()
                .ok()
                .map(|c| {
                    let id = c.id;
                    format!("HEAD {}", &id.to_hex_with_len(7))
                })
                .unwrap_or_else(|| "HEAD <unknown>".into());
            (CheckStatus::Ok(head), Some(repo))
        }
        Err(e) => (CheckStatus::Fail(format!("not a git repo: {e}")), None),
    }
}

fn check_working_tree(repo: Option<&gix::Repository>) -> CheckStatus<String> {
    let Some(repo) = repo else {
        return CheckStatus::Fail("skipped — repo not available".into());
    };
    match repo.is_dirty() {
        Ok(true) => CheckStatus::Warn("uncommitted changes present".into()),
        Ok(false) => CheckStatus::Ok("clean".into()),
        Err(e) => CheckStatus::Warn(format!("status check failed: {e}")),
    }
}

fn check_krypt_config(
    config_path: Option<&Path>,
) -> (CheckStatus<String>, Option<crate::config::Config>) {
    let Some(path) = config_path else {
        return (
            CheckStatus::Fail(
                "cannot determine config path — tool config and repo path both missing".into(),
            ),
            None,
        );
    };

    if !path.exists() {
        return (
            CheckStatus::Fail(format!("{} not found", path.display())),
            None,
        );
    }

    match crate::include::load_with_includes(path) {
        Ok(cfg) => {
            let links = cfg.links.len();
            let templates = cfg.templates.len();
            let detail = if templates == 0 {
                format!("parses, {links} links")
            } else {
                format!("parses, {links} links + {templates} templates")
            };
            (CheckStatus::Ok(detail), Some(cfg))
        }
        Err(e) => (CheckStatus::Fail(format!("parse error: {e}")), None),
    }
}

fn check_link_sources(
    cfg: Option<&crate::config::Config>,
    repo_path: Option<&Path>,
) -> CheckStatus<String> {
    let Some(cfg) = cfg else {
        return CheckStatus::Fail("skipped — config not loaded".into());
    };
    let Some(repo) = repo_path else {
        return CheckStatus::Fail("skipped — repo path not available".into());
    };

    let mut missing: Vec<String> = Vec::new();
    for link in &cfg.links {
        if let Some(src) = &link.src {
            let full = repo.join(src);
            if !full.exists() {
                missing.push(src.clone());
            }
        }
    }

    let total = cfg.links.iter().filter(|l| l.src.is_some()).count();

    if missing.is_empty() {
        CheckStatus::Ok(format!("all {total} exist"))
    } else {
        CheckStatus::Fail(format!("{} missing: {}", missing.len(), missing.join(", ")))
    }
}

fn check_link_destinations(manifest_path: &Path) -> CheckStatus<String> {
    match Manifest::load(manifest_path) {
        Ok(None) => CheckStatus::NotApplicable("no manifest — nothing deployed yet".into()),
        Ok(Some(manifest)) => {
            let drift = detect_drift(&manifest);
            let total = drift.len();
            let drifted = drift
                .iter()
                .filter(|d| d.status == DriftStatus::Drifted)
                .count();
            let missing = drift
                .iter()
                .filter(|d| d.status == DriftStatus::DstMissing)
                .count();
            let clean = total - drifted - missing;

            if drifted == 0 && missing == 0 {
                CheckStatus::Ok(format!("{clean} clean"))
            } else {
                let mut parts = Vec::new();
                if clean > 0 {
                    parts.push(format!("{clean} clean"));
                }
                if drifted > 0 {
                    parts.push(format!("{drifted} drifted"));
                }
                if missing > 0 {
                    parts.push(format!("{missing} missing"));
                }
                CheckStatus::Warn(format!(
                    "{} (run `krypt diff` for details)",
                    parts.join(", ")
                ))
            }
        }
        Err(e) => CheckStatus::Fail(format!("manifest load error: {e}")),
    }
}

fn check_manifest(manifest_path: &Path) -> CheckStatus<String> {
    match Manifest::load(manifest_path) {
        Ok(None) => CheckStatus::Fail(format!("not found at {}", manifest_path.display())),
        Ok(Some(manifest)) => {
            let entries = manifest.entries.len();
            let age_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .saturating_sub(manifest.deployed_at);
            let age = humanish_age(age_secs);
            CheckStatus::Ok(format!("{entries} entries, last deploy {age}"))
        }
        Err(e) => CheckStatus::Fail(format!("load error: {e}")),
    }
}

/// Convert seconds into a simple human-readable string.
fn humanish_age(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::copy::EntryKind;
    use crate::manifest::ManifestEntry;
    use crate::tool_config::RepoConfig;
    use std::fs;
    use tempfile::tempdir;

    fn write_commit(repo: &gix::Repository, message: &str, files: &[(&str, &[u8])]) {
        let mut entries: Vec<gix::objs::tree::Entry> = files
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
        entries.sort_by(|a, b| a.filename.cmp(&b.filename));

        let tree = gix::objs::Tree { entries };
        let tree_id = repo.write_object(&tree).expect("write tree").detach();
        let sig = gix::actor::SignatureRef::from_bytes(b"T <t@t> 0 +0000").unwrap();
        let parent: Vec<gix::hash::ObjectId> = repo
            .head_id()
            .ok()
            .map(|id| id.detach())
            .into_iter()
            .collect();
        repo.commit_as(sig, sig, "HEAD", message, tree_id, parent)
            .expect("commit");
    }

    fn init_git_repo(dir: &Path) -> gix::Repository {
        let repo = gix::init(dir).expect("gix::init");
        write_commit(&repo, "initial", &[]);
        repo
    }

    fn make_tool_config(repo_path: &Path, tc_path: &Path) {
        let cfg = ToolConfig {
            repo: RepoConfig {
                path: repo_path.to_path_buf(),
                url: None,
            },
        };
        cfg.save(tc_path).unwrap();
    }

    fn make_krypt_toml(repo: &Path, content: &str) {
        fs::write(repo.join(".krypt.toml"), content).unwrap();
    }

    fn fake_manifest_entry(src: &str, dst: PathBuf) -> ManifestEntry {
        ManifestEntry {
            src: src.into(),
            dst,
            kind: EntryKind::Link,
            hash_src: "sha256:aa".into(),
            hash_dst: "sha256:aa".into(),
            deployed_at: 0,
        }
    }

    // ── 1. Healthy synthetic install ─────────────────────────────────────────

    #[test]
    fn healthy_install_all_green() {
        let repo_dir = tempdir().unwrap();
        let tc_dir = tempdir().unwrap();
        let state_dir = tempdir().unwrap();

        init_git_repo(repo_dir.path());

        let tc_path = tc_dir.path().join("config.toml");
        make_tool_config(repo_dir.path(), &tc_path);

        let src_file = repo_dir.path().join("dot_gitconfig");
        fs::write(&src_file, b"[user]").unwrap();

        let dst_file = state_dir.path().join("deployed_gitconfig");
        fs::write(&dst_file, b"[user]").unwrap();
        let hash = crate::manifest::hash_file(&dst_file).unwrap();

        let manifest_path = state_dir.path().join("manifest.json");
        let mut m = Manifest::new(repo_dir.path().to_path_buf());
        m.record(ManifestEntry {
            src: "dot_gitconfig".into(),
            dst: dst_file.clone(),
            kind: EntryKind::Link,
            hash_src: hash.clone(),
            hash_dst: hash,
            deployed_at: m.deployed_at,
        });
        m.save(&manifest_path).unwrap();

        let src_name = src_file
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let dst_str = dst_file.to_string_lossy().replace('\\', "/");
        let toml_content = format!("[[link]]\nsrc = \"{src_name}\"\ndst = \"{dst_str}\"\n");
        make_krypt_toml(repo_dir.path(), &toml_content);

        let report = doctor(&DoctorOpts {
            tool_config_path: tc_path,
            config_path: None,
            manifest_path,
            repo_path: None,
            detected_manager: Some("pacman".into()),
        });

        assert!(
            report.tool_config.is_ok(),
            "tool_config: {:?}",
            report.tool_config
        );
        assert!(
            report.repo_path.is_ok(),
            "repo_path: {:?}",
            report.repo_path
        );
        assert!(
            report.repo_is_git.is_ok(),
            "repo_is_git: {:?}",
            report.repo_is_git
        );
        assert!(
            report.working_tree.is_ok(),
            "working_tree: {:?}",
            report.working_tree
        );
        assert!(
            report.krypt_config.is_ok(),
            "krypt_config: {:?}",
            report.krypt_config
        );
        assert!(
            report.link_sources.is_ok(),
            "link_sources: {:?}",
            report.link_sources
        );
        assert!(
            report.link_destinations.is_ok(),
            "link_destinations: {:?}",
            report.link_destinations
        );
        assert!(report.manifest.is_ok(), "manifest: {:?}", report.manifest);
        assert!(report.is_all_green());
    }

    // ── 2. Missing tool config ───────────────────────────────────────────────

    #[test]
    fn missing_tool_config_fails() {
        let tc_dir = tempdir().unwrap();
        let state_dir = tempdir().unwrap();

        let report = doctor(&DoctorOpts {
            tool_config_path: tc_dir.path().join("nonexistent.toml"),
            config_path: None,
            manifest_path: state_dir.path().join("manifest.json"),
            repo_path: None,
            detected_manager: None,
        });

        assert!(report.tool_config.needs_attention());
        assert!(!report.is_all_green());
    }

    // ── 3. Repo path does not exist ──────────────────────────────────────────

    #[test]
    fn missing_repo_path_fails() {
        let tc_dir = tempdir().unwrap();
        let state_dir = tempdir().unwrap();

        let tc_path = tc_dir.path().join("config.toml");
        let bogus_repo = tc_dir.path().join("nonexistent_repo");
        make_tool_config(&bogus_repo, &tc_path);

        let report = doctor(&DoctorOpts {
            tool_config_path: tc_path,
            config_path: None,
            manifest_path: state_dir.path().join("manifest.json"),
            repo_path: None,
            detected_manager: None,
        });

        assert!(report.repo_path.needs_attention());
        assert!(!report.is_all_green());
    }

    // ── 4. Repo path exists but is not a git repo ────────────────────────────

    #[test]
    fn non_git_repo_fails() {
        let repo_dir = tempdir().unwrap();
        let tc_dir = tempdir().unwrap();
        let state_dir = tempdir().unwrap();

        let tc_path = tc_dir.path().join("config.toml");
        make_tool_config(repo_dir.path(), &tc_path);

        let report = doctor(&DoctorOpts {
            tool_config_path: tc_path,
            config_path: None,
            manifest_path: state_dir.path().join("manifest.json"),
            repo_path: None,
            detected_manager: None,
        });

        assert!(report.repo_path.is_ok());
        assert!(report.repo_is_git.needs_attention());
        assert!(!report.is_all_green());
    }

    // ── 5. Link src missing on disk ──────────────────────────────────────────

    #[test]
    fn missing_link_src_fails() {
        let repo_dir = tempdir().unwrap();
        let tc_dir = tempdir().unwrap();
        let state_dir = tempdir().unwrap();

        init_git_repo(repo_dir.path());
        let tc_path = tc_dir.path().join("config.toml");
        make_tool_config(repo_dir.path(), &tc_path);

        make_krypt_toml(
            repo_dir.path(),
            "[[link]]\nsrc = \"does_not_exist\"\ndst = \"/tmp/x\"\n",
        );

        let report = doctor(&DoctorOpts {
            tool_config_path: tc_path,
            config_path: None,
            manifest_path: state_dir.path().join("manifest.json"),
            repo_path: None,
            detected_manager: None,
        });

        assert!(report.link_sources.needs_attention());
        if let CheckStatus::Fail(msg) = &report.link_sources {
            assert!(msg.contains("does_not_exist"), "msg: {msg}");
        }
        assert!(!report.is_all_green());
    }

    // ── 6. Manifest has drifted entry ────────────────────────────────────────

    #[test]
    fn drifted_manifest_entry_reported() {
        let repo_dir = tempdir().unwrap();
        let tc_dir = tempdir().unwrap();
        let state_dir = tempdir().unwrap();

        init_git_repo(repo_dir.path());
        let tc_path = tc_dir.path().join("config.toml");
        make_tool_config(repo_dir.path(), &tc_path);
        make_krypt_toml(repo_dir.path(), "");

        let dst = state_dir.path().join("deployed.txt");
        fs::write(&dst, b"changed").unwrap();

        let manifest_path = state_dir.path().join("manifest.json");
        let mut m = Manifest::new(repo_dir.path().to_path_buf());
        m.record(ManifestEntry {
            hash_dst: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
            ..fake_manifest_entry("dot", dst)
        });
        m.save(&manifest_path).unwrap();

        let report = doctor(&DoctorOpts {
            tool_config_path: tc_path,
            config_path: None,
            manifest_path,
            repo_path: None,
            detected_manager: None,
        });

        assert!(report.link_destinations.needs_attention());
        if let CheckStatus::Warn(msg) = &report.link_destinations {
            assert!(msg.contains("drifted"), "msg: {msg}");
        }
    }

    // ── 7. JSON output is valid JSON ─────────────────────────────────────────

    #[test]
    fn json_output_is_valid() {
        let tc_dir = tempdir().unwrap();
        let state_dir = tempdir().unwrap();

        let report = doctor(&DoctorOpts {
            tool_config_path: tc_dir.path().join("config.toml"),
            config_path: None,
            manifest_path: state_dir.path().join("manifest.json"),
            repo_path: None,
            detected_manager: None,
        });

        let json = serde_json::to_string_pretty(&report).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse back");
        assert!(parsed.is_object());
        assert!(parsed["tool_version"].is_string());
    }
}
