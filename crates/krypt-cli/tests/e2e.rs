//! End-to-end integration tests for the `krypt` binary.
//!
//! Each test invokes the compiled binary against an isolated tempdir home so
//! that the host's real `~/.config/krypt/...` is never touched.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use assert_fs::TempDir;

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Sandbox environment bound to a temp home directory.
struct Env {
    home: TempDir,
}

impl Env {
    fn new() -> Self {
        Self {
            home: TempDir::new().expect("create temp home"),
        }
    }

    /// Absolute path to `<home>/<rel>`.
    fn path(&self, rel: &str) -> PathBuf {
        self.home.path().join(rel)
    }

    /// Pre-create XDG subdirectories.
    fn create_xdg_dirs(&self) {
        for rel in &[
            ".config",
            ".local/share",
            ".local/state",
            ".cache",
            ".config/krypt",
            ".local/state/krypt",
        ] {
            fs::create_dir_all(self.home.path().join(rel)).expect("create xdg dir");
        }
    }
}

/// Build a `Command` for the `krypt` binary with the sandbox environment
/// applied.  PATH is preserved so the binary can invoke sub-processes.
fn cmd(env: &Env) -> Command {
    let home = env.home.path();
    let mut c = Command::cargo_bin("krypt").expect("find krypt binary");
    c.env_clear();
    c.env("PATH", std::env::var("PATH").unwrap_or_default());
    // HOME: Unix primary; USERPROFILE: Windows primary (resolver reads it there).
    c.env("HOME", home);
    c.env("USERPROFILE", home);
    c.env("XDG_CONFIG_HOME", home.join(".config"));
    c.env("XDG_STATE_HOME", home.join(".local/state"));
    c.env("XDG_DATA_HOME", home.join(".local/share"));
    c.env("XDG_CACHE_HOME", home.join(".cache"));
    // Restore a TEMP env so XDG_RUNTIME has a fallback on Windows.
    if let Ok(tmp) = std::env::var("TEMP") {
        c.env("TEMP", tmp);
    }
    if let Ok(tmp) = std::env::var("TMP") {
        c.env("TMP", tmp);
    }
    // Windows also needs LOCALAPPDATA + APPDATA for WIN_* vars (not used in
    // these tests, but avoids resolver errors for the `paths` command).
    if let Ok(v) = std::env::var("LOCALAPPDATA") {
        c.env("LOCALAPPDATA", v);
    }
    if let Ok(v) = std::env::var("APPDATA") {
        c.env("APPDATA", v);
    }
    c
}

/// Standard repo path inside the sandbox.
fn repo_path(env: &Env) -> PathBuf {
    env.path(".config/krypt/repo")
}

/// Standard manifest path inside the sandbox.
fn manifest_path(env: &Env) -> PathBuf {
    env.path(".local/state/krypt/manifest.json")
}

/// Run `krypt init --bare` in the sandbox and return the env.
fn init_bare(env: &Env) {
    env.create_xdg_dirs();
    let rp = repo_path(env);
    cmd(env)
        .args(["init", "--bare", "--repo-path", &rp.to_string_lossy()])
        .assert()
        .success();
}

/// After `init --bare`, write `krypt.toml` + repo files, run `link`.
fn setup_linked(env: &Env, krypt_toml: &str, repo_files: &[(&str, &[u8])]) {
    init_bare(env);
    let rp = repo_path(env);
    // Write .krypt.toml into the repo.
    fs::write(rp.join(".krypt.toml"), krypt_toml).expect("write .krypt.toml");
    // Write additional repo source files.
    for (rel, bytes) in repo_files {
        let target = rp.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).expect("create repo subdir");
        }
        fs::write(target, bytes).expect("write repo file");
    }
    cmd(env)
        .args([
            "link",
            "--config",
            &rp.join(".krypt.toml").to_string_lossy(),
            "--manifest",
            &manifest_path(env).to_string_lossy(),
        ])
        .assert()
        .success();
}

/// Forward-slash path string safe for TOML basic strings.
fn toml_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Escape a literal string for use as a regex pattern.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for ch in s.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// insta filter settings that redact variable content from snapshots.
fn snapshot_settings(env: &Env) -> insta::Settings {
    let mut settings = insta::Settings::clone_current();
    let home_str = toml_path(env.home.path());
    // Redact the exact temp home path first (most specific).
    settings.add_filter(&regex_escape(&home_str), "[TEMP]");
    // Redact any remaining /tmp/.../... style paths.
    settings.add_filter(r"/tmp/[^\s]+", "[TEMP]");
    settings.add_filter(r"C:\\[^\s]+", "[TEMP]");
    // Redact version strings like "0.0.2" or "1.2.3".
    settings.add_filter(r"\d+\.\d+\.\d+", "[VERSION]");
    // Redact git commit short hashes (7 hex chars preceded by "HEAD ").
    settings.add_filter(r"HEAD [0-9a-f]{7}", "HEAD [HASH]");
    // Redact age suffixes like "3s ago", "5m ago", "2h ago", "1d ago".
    settings.add_filter(r"\d+[smhd] ago", "[AGE]");
    settings
}

// ─── Tests ────────────────────────────────────────────────────────────────────

/// `krypt version` — exits 0, snapshot stdout.
#[test]
fn test_version() {
    let env = Env::new();
    let output = cmd(&env).arg("version").output().expect("run version");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let settings = snapshot_settings(&env);
    settings.bind(|| {
        insta::assert_snapshot!("version", stdout);
    });
}

/// `krypt validate <path>` — parse a valid `.krypt.toml`, exit 0, snapshot stdout.
#[test]
fn test_validate() {
    let env = Env::new();
    env.create_xdg_dirs();
    let cfg_file = env.path(".krypt.toml");
    fs::write(
        &cfg_file,
        "[[link]]\nsrc = \"gitconfig\"\ndst = \"${HOME}/.gitconfig\"\n",
    )
    .expect("write cfg");

    let output = cmd(&env)
        .args(["validate", &cfg_file.to_string_lossy()])
        .output()
        .expect("run validate");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // Structural check: success sigil and the filename must appear.
    assert!(
        stdout.contains('✓'),
        "validate should print ✓ on success: {stdout}"
    );
    assert!(
        stdout.contains("parsed and validated successfully"),
        "validate success message missing: {stdout}"
    );
}

/// `krypt paths` — exit 0, all XDG vars and HOME resolve under the temp sandbox.
#[test]
fn test_paths() {
    let env = Env::new();
    env.create_xdg_dirs();

    let output = cmd(&env)
        .args(["paths", "--no-config"])
        .output()
        .expect("run paths");
    assert!(output.status.success());
    // Normalise both stdout and home path to forward slashes so the check
    // works on Windows where path display uses backslashes.
    let stdout = String::from_utf8_lossy(&output.stdout)
        .replace('\\', "/")
        .to_lowercase();
    let home_str = env
        .home
        .path()
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    // Every XDG var must resolve under our sandbox home.
    for var in &["home", "xdg_config", "xdg_data", "xdg_state", "xdg_cache"] {
        assert!(stdout.contains(var), "paths output missing {var}: {stdout}");
    }
    // HOME line must point at our sandbox.
    assert!(
        stdout.contains(&home_str),
        "paths output must contain sandbox home {home_str}: {stdout}"
    );
}

/// `krypt diff` — after link with no edits, exit 0, "all clean".
#[test]
fn test_diff_clean() {
    let env = Env::new();
    let home_str = toml_path(env.home.path());
    let krypt_toml = format!("[[link]]\nsrc = \"gitconfig\"\ndst = \"{home_str}/.gitconfig\"\n");
    setup_linked(&env, &krypt_toml, &[("gitconfig", b"[user]\n")]);

    let output = cmd(&env)
        .args(["diff", "--manifest", &manifest_path(&env).to_string_lossy()])
        .output()
        .expect("run diff");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("all clean"),
        "expected 'all clean', got: {stdout}"
    );
}

/// `krypt link` — fresh env + `.krypt.toml` + one file, dest & manifest exist.
#[test]
fn test_link() {
    let env = Env::new();
    env.create_xdg_dirs();
    init_bare(&env);

    let rp = repo_path(&env);
    let home_str = toml_path(env.home.path());
    let krypt_toml = format!("[[link]]\nsrc = \"gitconfig\"\ndst = \"{home_str}/.gitconfig\"\n");
    fs::write(rp.join(".krypt.toml"), &krypt_toml).expect("write toml");
    fs::write(rp.join("gitconfig"), b"[user]\n").expect("write src");

    let mp = manifest_path(&env);
    cmd(&env)
        .args([
            "link",
            "--config",
            &rp.join(".krypt.toml").to_string_lossy(),
            "--manifest",
            &mp.to_string_lossy(),
        ])
        .assert()
        .success();

    assert!(env.path(".gitconfig").exists(), "deployed file missing");
    assert!(mp.exists(), "manifest missing");
}

/// `krypt unlink` — after link, unlink removes the deployed file.
#[test]
fn test_unlink() {
    let env = Env::new();
    let home_str = toml_path(env.home.path());
    let krypt_toml = format!("[[link]]\nsrc = \"gitconfig\"\ndst = \"{home_str}/.gitconfig\"\n");
    setup_linked(&env, &krypt_toml, &[("gitconfig", b"[user]\n")]);

    let dst = env.path(".gitconfig");
    assert!(dst.exists(), "file should exist after link");

    cmd(&env)
        .args([
            "unlink",
            "--manifest",
            &manifest_path(&env).to_string_lossy(),
        ])
        .assert()
        .success();

    assert!(!dst.exists(), "file should be gone after unlink");
}

/// `krypt relink` — after link, relink exits 0 and dest still present.
#[test]
fn test_relink() {
    let env = Env::new();
    let home_str = toml_path(env.home.path());
    let krypt_toml = format!("[[link]]\nsrc = \"gitconfig\"\ndst = \"{home_str}/.gitconfig\"\n");
    setup_linked(&env, &krypt_toml, &[("gitconfig", b"[user]\n")]);

    let rp = repo_path(&env);
    cmd(&env)
        .args([
            "relink",
            "--config",
            &rp.join(".krypt.toml").to_string_lossy(),
            "--manifest",
            &manifest_path(&env).to_string_lossy(),
        ])
        .assert()
        .success();

    assert!(
        env.path(".gitconfig").exists(),
        "file should still exist after relink"
    );
}

/// `krypt init --bare` — repo stub + tool config written.
#[test]
fn test_init_bare() {
    let env = Env::new();
    env.create_xdg_dirs();

    let rp = repo_path(&env);
    cmd(&env)
        .args(["init", "--bare", "--repo-path", &rp.to_string_lossy()])
        .assert()
        .success();

    assert!(rp.join(".krypt.toml").exists(), "repo stub missing");
    let tc_path = env.path(".config/krypt/config.toml");
    assert!(tc_path.exists(), "tool config missing");
}

/// `krypt update` without init — exits non-zero, stderr mentions `krypt init`.
#[test]
fn test_update_no_init() {
    let env = Env::new();
    env.create_xdg_dirs();

    let output = cmd(&env).arg("update").output().expect("run update");
    assert!(!output.status.success(), "update should fail without init");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("init"),
        "stderr should mention 'init', got: {stderr}"
    );
}

/// `krypt adopt <file>` — after init, file appears in repo, manifest updated.
#[test]
fn test_adopt() {
    let env = Env::new();
    init_bare(&env);

    let dst = env.path(".gitconfig");
    fs::write(&dst, b"[user]\n").expect("write file to adopt");

    let rp = repo_path(&env);
    let mp = manifest_path(&env);

    cmd(&env)
        .args([
            "adopt",
            &dst.to_string_lossy(),
            "--repo-path",
            &rp.to_string_lossy(),
            "--manifest",
            &mp.to_string_lossy(),
        ])
        .assert()
        .success();

    assert!(rp.join(".gitconfig").exists(), "repo missing adopted file");
    assert!(mp.exists(), "manifest not created");
}

/// `krypt adopt-edits` — after link + edit deployed file, adopt-edits syncs repo.
#[test]
fn test_adopt_edits() {
    let env = Env::new();
    let home_str = toml_path(env.home.path());
    let krypt_toml = format!("[[link]]\nsrc = \"gitconfig\"\ndst = \"{home_str}/.gitconfig\"\n");
    setup_linked(&env, &krypt_toml, &[("gitconfig", b"original content\n")]);

    let deployed = env.path(".gitconfig");
    fs::write(&deployed, b"edited content\n").expect("edit deployed file");

    let rp = repo_path(&env);
    let mp = manifest_path(&env);

    cmd(&env)
        .args([
            "adopt-edits",
            "--repo-path",
            &rp.to_string_lossy(),
            "--manifest",
            &mp.to_string_lossy(),
        ])
        .assert()
        .success();

    let repo_content = fs::read(rp.join("gitconfig")).expect("read repo file");
    assert_eq!(
        repo_content, b"edited content\n",
        "repo should have edited content"
    );
}

/// `krypt doctor` — after link, text output contains key check labels; `--json`
/// produces parseable JSON with expected keys.
#[test]
fn test_doctor() {
    let env = Env::new();
    let home_str = toml_path(env.home.path());
    let krypt_toml = format!("[[link]]\nsrc = \"gitconfig\"\ndst = \"{home_str}/.gitconfig\"\n");
    setup_linked(&env, &krypt_toml, &[("gitconfig", b"[user]\n")]);

    let rp = repo_path(&env);
    let mp = manifest_path(&env);
    let tc_path = env.path(".config/krypt/config.toml");

    let config_arg = rp.join(".krypt.toml");
    let config_str = config_arg.to_string_lossy();
    let mp_str = mp.to_string_lossy();
    let tc_str = tc_path.to_string_lossy();
    let rp_str = rp.to_string_lossy();

    let text_output = cmd(&env)
        .args([
            "doctor",
            "--config",
            &config_str,
            "--manifest",
            &mp_str,
            "--tool-config",
            &tc_str,
            "--repo-path",
            &rp_str,
        ])
        .output()
        .expect("run doctor");
    let text_stdout = String::from_utf8_lossy(&text_output.stdout).into_owned();
    // Key check labels must appear in the text report regardless of platform.
    for label in &[
        "tool config",
        "repo path",
        "repo is git",
        "manifest",
        "platform",
    ] {
        assert!(
            text_stdout.contains(label),
            "doctor text missing label '{label}': {text_stdout}"
        );
    }

    let json_output = cmd(&env)
        .args([
            "doctor",
            "--json",
            "--config",
            &config_str,
            "--manifest",
            &mp_str,
            "--tool-config",
            &tc_str,
            "--repo-path",
            &rp_str,
        ])
        .output()
        .expect("run doctor --json");

    // doctor exits 1 when any check needs attention; that's expected since
    // bare init doesn't create a git repo.  We only care about valid JSON.
    let json_stdout = String::from_utf8_lossy(&json_output.stdout).into_owned();
    let parsed: serde_json::Value =
        serde_json::from_str(&json_stdout).expect("doctor --json output is not valid JSON");
    assert!(parsed.is_object());
    assert!(parsed["tool_version"].is_string());
    assert!(parsed["tool_config"].is_object());
    assert!(parsed["platform"].is_object());
    assert!(parsed["manifest"].is_object());
}
