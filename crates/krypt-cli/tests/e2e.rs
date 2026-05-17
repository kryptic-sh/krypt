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

/// `krypt deps --dry-run` — reads a synthetic config with one `[[deps]]` group,
/// exits 0, and prints the expected packages without touching the system.
#[test]
fn test_deps_dry_run() {
    let env = Env::new();
    env.create_xdg_dirs();
    init_bare(&env);

    let rp = repo_path(&env);

    // Write a .krypt.toml with a [[deps]] group that has packages for every manager.
    let krypt_toml = concat!(
        "[[deps]]\n",
        "group = \"core\"\n",
        "required_platforms = [\"all\"]\n",
        "pacman = [\"base-devel\"]\n",
        "apt = [\"build-essential\"]\n",
        "dnf = [\"@development-tools\"]\n",
        "brew = [\"coreutils\"]\n",
        "scoop = [\"git\"]\n",
        "winget = [\"Git.Git\"]\n",
    );
    fs::write(rp.join(".krypt.toml"), krypt_toml).expect("write .krypt.toml");

    let config_path = rp.join(".krypt.toml");

    // Use an explicit manager so the test is deterministic on all CI platforms
    // (auto-detection would fail on Windows runners where no manager is installed).
    // pacman is always registered by pick_by_name regardless of availability.
    let output = cmd(&env)
        .args([
            "deps",
            "--config",
            &config_path.to_string_lossy(),
            "--manager",
            "pacman",
            "--dry-run",
        ])
        .output()
        .expect("run deps --dry-run");

    // --dry-run should always exit 0 (no real install to fail).
    assert!(
        output.status.success(),
        "deps --dry-run should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    // The output should mention the manager used and "(dry-run)".
    assert!(
        stdout.contains("dry-run"),
        "output should mention dry-run: {stdout}"
    );
    assert!(
        stdout.contains("manager:"),
        "output should name the manager: {stdout}"
    );
    // The pacman package from the test config should appear in the "would install" line.
    assert!(
        stdout.contains("base-devel"),
        "output should mention the queued package: {stdout}"
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

/// `krypt setup --dry-run` — with a config that has a `[prompts.*]` section,
/// exits 0, prints "dry-run", and does NOT write any destination files.
#[test]
fn test_setup_dry_run() {
    let env = Env::new();
    init_bare(&env);

    let rp = repo_path(&env);
    let home_str = toml_path(env.home.path());

    // A minimal config with one prompts section and a corresponding [[template]].
    // The field has a literal default so --yes / dry-run works without interaction.
    let krypt_toml = format!(
        concat!(
            "[[template]]\n",
            "src = \"env.template\"\n",
            "dst = \"{home}/.env_out\"\n",
            "prompts = [\"myenv\"]\n",
            "\n",
            "[prompts.myenv]\n",
            "heading = \"Test env\"\n",
            "writer = \"env\"\n",
            "fields = [\n",
            "  {{ key = \"GREETING\", prompt = \"Greeting\", default = \"hello\" }},\n",
            "]\n",
        ),
        home = home_str,
    );
    fs::write(rp.join(".krypt.toml"), &krypt_toml).expect("write .krypt.toml");
    fs::write(rp.join("env.template"), b"GREETING=hello\n").expect("write template");

    let dst = env.path(".env_out");
    let config_path = rp.join(".krypt.toml");

    let output = cmd(&env)
        .args([
            "setup",
            "--config",
            &config_path.to_string_lossy(),
            "--dry-run",
            "--yes",
        ])
        .output()
        .expect("run setup --dry-run");

    assert!(
        output.status.success(),
        "setup --dry-run should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("dry-run"),
        "output should mention dry-run: {stdout}"
    );
    assert!(!dst.exists(), "dry-run must not write destination file");
}

/// `krypt notify hello world --backend stderr` — exits 0, stderr contains both words.
#[test]
fn test_notify_stderr() {
    let env = Env::new();

    let output = cmd(&env)
        .args(["notify", "hello", "world", "--backend", "stderr"])
        .output()
        .expect("run notify");

    assert!(
        output.status.success(),
        "notify --backend stderr should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("hello"),
        "stderr should contain title 'hello': {stderr}"
    );
    assert!(
        stderr.contains("world"),
        "stderr should contain body 'world': {stderr}"
    );
}

/// `krypt setup --yes` — with all fields having resolvable defaults, exits 0
/// and writes destination files.
#[test]
fn test_setup_yes() {
    let env = Env::new();
    init_bare(&env);

    let rp = repo_path(&env);
    let home_str = toml_path(env.home.path());

    // Config with two prompt sections: one env writer, one hypr_vars writer.
    // All fields have literal defaults so --yes can proceed without TTY.
    let krypt_toml = format!(
        concat!(
            "[[template]]\n",
            "src = \"env.template\"\n",
            "dst = \"{home}/.env_setup_out\"\n",
            "prompts = [\"myenv\"]\n",
            "\n",
            "[[template]]\n",
            "src = \"hypr.template.conf\"\n",
            "dst = \"{home}/.hypr_setup_out\"\n",
            "prompts = [\"myhypr\"]\n",
            "\n",
            "[prompts.myenv]\n",
            "heading = \"Env section\"\n",
            "writer = \"env\"\n",
            "fields = [\n",
            "  {{ key = \"EDITOR\", prompt = \"Editor\", default = \"nvim\" }},\n",
            "]\n",
            "\n",
            "[prompts.myhypr]\n",
            "heading = \"Hypr section\"\n",
            "writer = \"hypr_vars\"\n",
            "fields = [\n",
            "  {{ key = \"terminal\", prompt = \"Terminal\", default = \"alacritty\" }},\n",
            "]\n",
        ),
        home = home_str,
    );
    fs::write(rp.join(".krypt.toml"), &krypt_toml).expect("write .krypt.toml");
    fs::write(rp.join("env.template"), b"").expect("write env template");
    fs::write(rp.join("hypr.template.conf"), b"").expect("write hypr template");

    let config_path = rp.join(".krypt.toml");
    let env_dst = env.path(".env_setup_out");
    let hypr_dst = env.path(".hypr_setup_out");

    let output = cmd(&env)
        .args(["setup", "--config", &config_path.to_string_lossy(), "--yes"])
        .output()
        .expect("run setup --yes");

    assert!(
        output.status.success(),
        "setup --yes should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("setup complete"),
        "output should confirm setup complete: {stdout}"
    );

    // Destination files must exist and contain expected content.
    assert!(env_dst.exists(), "env destination file should be written");
    let env_content = fs::read_to_string(&env_dst).expect("read env output");
    assert!(
        env_content.contains("export EDITOR=nvim"),
        "env file should contain EDITOR: {env_content}"
    );

    assert!(hypr_dst.exists(), "hypr destination file should be written");
    let hypr_content = fs::read_to_string(&hypr_dst).expect("read hypr output");
    assert!(
        hypr_content.contains("$terminal = alacritty"),
        "hypr file should contain terminal: {hypr_content}"
    );
}

/// `krypt menu` with no `[[command]] group = "menu"` entries → exit 0, output
/// mentions "no menus".
#[test]
fn test_menu_list_empty() {
    let env = Env::new();
    init_bare(&env);

    let rp = repo_path(&env);
    // Write a config with no [[command]] entries at all.
    fs::write(rp.join(".krypt.toml"), "[meta]\nname = \"test\"\n").expect("write .krypt.toml");

    let config_path = rp.join(".krypt.toml");

    let output = cmd(&env)
        .args(["menu", "--config", &config_path.to_string_lossy()])
        .output()
        .expect("run menu");

    assert!(
        output.status.success(),
        "krypt menu on empty config should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("no menus"),
        "output should mention 'no menus': {stdout}"
    );
}

/// `krypt menu my-menu --dry-run` → exit 0, stdout contains the step listing.
#[test]
fn test_menu_dry_run() {
    let env = Env::new();
    init_bare(&env);

    let rp = repo_path(&env);
    let krypt_toml = concat!(
        "[[command]]\n",
        "group = \"menu\"\n",
        "name = \"my-menu\"\n",
        "description = \"Test menu\"\n",
        "steps = [\n",
        "  { run = [\"echo\", \"hello\"] },\n",
        "  { notify = [\"Done\", \"All steps complete\"] },\n",
        "]\n",
    );
    fs::write(rp.join(".krypt.toml"), krypt_toml).expect("write .krypt.toml");

    let config_path = rp.join(".krypt.toml");

    let output = cmd(&env)
        .args([
            "menu",
            "my-menu",
            "--dry-run",
            "--config",
            &config_path.to_string_lossy(),
        ])
        .output()
        .expect("run menu --dry-run");

    assert!(
        output.status.success(),
        "krypt menu my-menu --dry-run should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("dry-run"),
        "output should mention 'dry-run': {stdout}"
    );
    assert!(
        stdout.contains("echo"),
        "output should list the echo step: {stdout}"
    );
}

/// `krypt unknown-group` — exit 1, stderr mentions "unknown group" and lists available groups.
#[test]
fn test_external_unknown_group() {
    let env = Env::new();
    init_bare(&env);

    let rp = repo_path(&env);
    let krypt_toml = concat!(
        "[[command]]\n",
        "group = \"menu\"\n",
        "name = \"wifi\"\n",
        "description = \"WiFi\"\n",
        "steps = [{ run = [\"echo\", \"wifi\"] }]\n",
    );
    fs::write(rp.join(".krypt.toml"), krypt_toml).expect("write .krypt.toml");

    let config_path = rp.join(".krypt.toml");

    let output = cmd(&env)
        .args(["unknown-group", "--config", &config_path.to_string_lossy()])
        .output()
        .expect("run unknown-group");

    assert!(
        !output.status.success(),
        "unknown-group should exit non-zero"
    );
    assert_eq!(output.status.code(), Some(1));

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("unknown group"),
        "stderr should mention 'unknown group': {stderr}"
    );
    assert!(
        stderr.contains("menu"),
        "stderr should list available group 'menu': {stderr}"
    );
}

/// `krypt my-group` with a fixture config lists commands in that group.
#[test]
fn test_external_group_list() {
    let env = Env::new();
    init_bare(&env);

    let rp = repo_path(&env);
    let krypt_toml = concat!(
        "[[command]]\n",
        "group = \"my-group\"\n",
        "name = \"do-thing\"\n",
        "description = \"Does a thing\"\n",
        "steps = [{ run = [\"echo\", \"hi\"] }]\n",
    );
    fs::write(rp.join(".krypt.toml"), krypt_toml).expect("write .krypt.toml");

    let config_path = rp.join(".krypt.toml");

    let output = cmd(&env)
        .args(["my-group", "--config", &config_path.to_string_lossy()])
        .output()
        .expect("run my-group");

    assert!(
        output.status.success(),
        "krypt my-group should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("do-thing"),
        "output should list the command 'do-thing': {stdout}"
    );
}

/// `krypt my-group my-cmd --dry-run` → exit 0, prints dry-run plan.
#[test]
fn test_external_group_dry_run() {
    let env = Env::new();
    init_bare(&env);

    let rp = repo_path(&env);
    let krypt_toml = concat!(
        "[[command]]\n",
        "group = \"my-group\"\n",
        "name = \"my-cmd\"\n",
        "description = \"My command\"\n",
        "steps = [\n",
        "  { run = [\"echo\", \"hello\"] },\n",
        "]\n",
    );
    fs::write(rp.join(".krypt.toml"), krypt_toml).expect("write .krypt.toml");

    let config_path = rp.join(".krypt.toml");

    let output = cmd(&env)
        .args([
            "my-group",
            "my-cmd",
            "--dry-run",
            "--config",
            &config_path.to_string_lossy(),
        ])
        .output()
        .expect("run my-group my-cmd --dry-run");

    assert!(
        output.status.success(),
        "krypt my-group my-cmd --dry-run should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("dry-run"),
        "output should mention 'dry-run': {stdout}"
    );
    assert!(
        stdout.contains("echo"),
        "output should show the echo step: {stdout}"
    );
}

// ─── Battery tests ────────────────────────────────────────────────────────────

/// `krypt battery clear --log-file <path>` — log file exists → deleted,
/// stdout mentions path and "Clearing".
#[test]
fn test_battery_clear_existing_file() {
    let env = Env::new();
    env.create_xdg_dirs();

    // Create a temporary log file with some dummy content.
    let log_dir = env.path(".local/log");
    fs::create_dir_all(&log_dir).expect("create log dir");
    let log_file = log_dir.join("bathist.log");
    fs::write(
        &log_file,
        b"2026-01-01 00:00:00, 1700000000, 80%, Discharging\n",
    )
    .expect("write log file");
    assert!(log_file.exists(), "log file must exist before clear");

    let output = cmd(&env)
        .args([
            "battery",
            "clear",
            "--log-file",
            &log_file.to_string_lossy(),
        ])
        .output()
        .expect("run battery clear");

    assert!(
        output.status.success(),
        "battery clear should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("Clearing"),
        "stdout should mention 'Clearing': {stdout}"
    );
    assert!(
        stdout.contains(&log_file.to_string_lossy().to_string()),
        "stdout should print the log path: {stdout}"
    );
    assert!(!log_file.exists(), "log file should be deleted after clear");
}

/// `krypt battery clear --log-file <path>` — log file absent → exit 0, no error.
#[test]
fn test_battery_clear_missing_file() {
    let env = Env::new();
    env.create_xdg_dirs();

    let log_file = env.path(".local/log/bathist.log");
    // Deliberately do NOT create the file.
    assert!(!log_file.exists(), "log file should not exist");

    let output = cmd(&env)
        .args([
            "battery",
            "clear",
            "--log-file",
            &log_file.to_string_lossy(),
        ])
        .output()
        .expect("run battery clear");

    assert!(
        output.status.success(),
        "battery clear on missing file should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("Clearing"),
        "stdout should mention 'Clearing' even for missing file: {stdout}"
    );
}

/// `krypt battery log --log-file <path>` — appends a CSV row; file is created
/// when absent.
#[test]
fn test_battery_log_creates_file() {
    let env = Env::new();
    env.create_xdg_dirs();

    let log_dir = env.path(".local/log");
    let log_file = log_dir.join("bathist.log");
    assert!(!log_file.exists(), "log file should not pre-exist");

    let output = cmd(&env)
        .args(["battery", "log", "--log-file", &log_file.to_string_lossy()])
        .output()
        .expect("run battery log");

    // Always exits 0 (bash script behaviour — errors are logged, not raised).
    assert!(
        output.status.success(),
        "battery log should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The file must now exist (either a real reading or an error row).
    assert!(
        log_file.exists(),
        "log file should be created by battery log"
    );

    let content = fs::read_to_string(&log_file).expect("read log file");
    // At minimum the row must have a date-like prefix.
    assert!(
        content.contains('-'),
        "log row should contain a date: {content}"
    );
    assert!(
        content.contains(','),
        "log row should be CSV (comma-separated): {content}"
    );
}
