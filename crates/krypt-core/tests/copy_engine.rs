//! Integration tests for [`krypt_core::copy`].
//!
//! Each test spins up a `tempfile::TempDir` as the synthetic dotfiles
//! repo + another as `$HOME`, drops real files in them, builds a
//! [`Plan`] via the planner, runs the executor, and asserts the
//! filesystem state matches.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use krypt_core::config::{Config, parse_str};
use krypt_core::copy::{Action, EntryKind, ExecOpts, PlanError, execute, plan};
use krypt_core::paths::{Platform, Resolver};
use tempfile::TempDir;

/// Build a fresh resolver pinned to Linux with `HOME` set to `home_dir`.
fn make_resolver(home_dir: &Path) -> Resolver {
    let mut env = std::collections::HashMap::new();
    env.insert("HOME".into(), home_dir.to_string_lossy().to_string());
    Resolver::for_platform(Platform::Linux).with_env(env)
}

/// Parse `cfg_str` and return the resulting Config. Panics on error —
/// these tests should always have well-formed input.
fn parse(cfg_str: &str) -> Config {
    parse_str(cfg_str, "test.toml").expect("fixture should parse")
}

#[test]
fn simple_link_copies_file_to_resolved_dst() {
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(repo.path().join("gitconfig"), b"[user]\nname = X\n").unwrap();

    let cfg = parse(
        r#"
[[link]]
src = "gitconfig"
dst = "${HOME}/.gitconfig"
"#,
    );
    let resolver = make_resolver(home.path());
    let p = plan(&cfg, repo.path(), &resolver).unwrap();
    assert_eq!(p.actions.len(), 1);
    assert!(matches!(p.actions[0], Action::Copy { .. }));

    let report = execute(&p, ExecOpts::default()).unwrap();
    assert_eq!(report.written, 1);
    assert_eq!(report.skipped_conflicts, 0);

    let deployed = fs::read(home.path().join(".gitconfig")).unwrap();
    assert_eq!(deployed, b"[user]\nname = X\n");
}

#[test]
fn dry_run_writes_nothing() {
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(repo.path().join("foo"), b"hi").unwrap();

    let cfg = parse(
        r#"
[[link]]
src = "foo"
dst = "${HOME}/foo"
"#,
    );
    let resolver = make_resolver(home.path());
    let p = plan(&cfg, repo.path(), &resolver).unwrap();
    let report = execute(
        &p,
        ExecOpts {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    // Report counts the *planned* writes even in dry-run, so callers can
    // print "would write N files".
    assert_eq!(report.written, 1);
    assert!(!home.path().join("foo").exists());
}

#[test]
fn existing_dst_yields_conflict_action() {
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(repo.path().join("a"), b"new").unwrap();
    fs::write(home.path().join("a"), b"existing").unwrap();

    let cfg = parse(
        r#"
[[link]]
src = "a"
dst = "${HOME}/a"
"#,
    );
    let resolver = make_resolver(home.path());
    let p = plan(&cfg, repo.path(), &resolver).unwrap();
    assert!(matches!(p.actions[0], Action::Conflict { .. }));

    // Default: don't overwrite.
    let report = execute(&p, ExecOpts::default()).unwrap();
    assert_eq!(report.written, 0);
    assert_eq!(report.skipped_conflicts, 1);
    assert_eq!(fs::read(home.path().join("a")).unwrap(), b"existing");

    // overwrite_conflicts = true: write.
    let report = execute(
        &p,
        ExecOpts {
            overwrite_conflicts: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.written, 1);
    assert_eq!(fs::read(home.path().join("a")).unwrap(), b"new");
}

#[test]
fn parent_dirs_are_created() {
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(repo.path().join("x"), b"x").unwrap();

    let cfg = parse(
        r#"
[[link]]
src = "x"
dst = "${HOME}/a/b/c/x"
"#,
    );
    let resolver = make_resolver(home.path());
    let p = plan(&cfg, repo.path(), &resolver).unwrap();
    execute(&p, ExecOpts::default()).unwrap();
    assert!(home.path().join("a/b/c/x").exists());
}

#[test]
fn glob_expands_and_strips_prefix_into_dst_dir() {
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::create_dir_all(repo.path().join(".config/nvim/lua")).unwrap();
    fs::write(repo.path().join(".config/nvim/init.lua"), b"-- init\n").unwrap();
    fs::write(repo.path().join(".config/nvim/lua/plug.lua"), b"-- plug\n").unwrap();

    let cfg = parse(
        r#"
[[link]]
src_glob = ".config/nvim/**/*"
dst = "${HOME}/.config/nvim/"
"#,
    );
    let resolver = make_resolver(home.path());
    let p = plan(&cfg, repo.path(), &resolver).unwrap();
    // Two regular files matched. Directories the glob picks up are
    // filtered out by the planner.
    let files: Vec<_> = p
        .actions
        .iter()
        .map(|a| a.dst().strip_prefix(home.path()).unwrap().to_path_buf())
        .collect();
    assert!(
        files
            .iter()
            .any(|p| p == Path::new(".config/nvim/init.lua"))
    );
    assert!(
        files
            .iter()
            .any(|p| p == Path::new(".config/nvim/lua/plug.lua"))
    );

    execute(&p, ExecOpts::default()).unwrap();
    assert_eq!(
        fs::read(home.path().join(".config/nvim/init.lua")).unwrap(),
        b"-- init\n"
    );
    assert_eq!(
        fs::read(home.path().join(".config/nvim/lua/plug.lua")).unwrap(),
        b"-- plug\n"
    );
}

#[test]
fn platform_filter_skips_other_os() {
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(repo.path().join("mac"), b"x").unwrap();
    fs::write(repo.path().join("win"), b"x").unwrap();

    // We always run with the test's current platform via the planner's
    // cfg!() check. Use macos + windows entries — both should be skipped
    // on Linux CI, leaving zero actions.
    let cfg = parse(
        r#"
[[link]]
src = "mac"
dst = "${HOME}/mac"
platform = "macos"

[[link]]
src = "win"
dst = "${HOME}/win"
platform = "windows"
"#,
    );
    let resolver = make_resolver(home.path());
    let p = plan(&cfg, repo.path(), &resolver).unwrap();
    // On CI we run on linux. If you're running the test on macOS or
    // Windows locally, this test asserts the *opposite* platform's
    // entry is skipped — still zero kept on a single-platform run.
    let kept: Vec<_> = p.actions.iter().collect();
    let current = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "windows"
    };
    let expected = match current {
        "linux" => 0,
        _ => 1,
    };
    assert_eq!(kept.len(), expected);
}

#[test]
fn template_entries_produce_actions() {
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(
        repo.path().join("gitconfig.template"),
        b"[user]\nname = ?\n",
    )
    .unwrap();

    let cfg = parse(
        r#"
[[template]]
src = "gitconfig.template"
dst = "${HOME}/.gitconfig.local"
prompts = ["git"]
"#,
    );
    let resolver = make_resolver(home.path());
    let p = plan(&cfg, repo.path(), &resolver).unwrap();
    assert_eq!(p.actions.len(), 1);
    assert_eq!(p.actions[0].kind(), EntryKind::Template);
    execute(&p, ExecOpts::default()).unwrap();
    assert!(home.path().join(".gitconfig.local").exists());
}

#[test]
fn mtime_is_preserved_within_filesystem_resolution() {
    use std::time::{Duration, SystemTime};
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let src = repo.path().join("ts");
    fs::write(&src, b"x").unwrap();
    // Backdate the source mtime by 1 hour to make the difference obvious.
    let backdated = SystemTime::now() - Duration::from_secs(3600);
    let f = std::fs::OpenOptions::new().write(true).open(&src).unwrap();
    f.set_modified(backdated).unwrap();

    let cfg = parse(
        r#"
[[link]]
src = "ts"
dst = "${HOME}/ts"
"#,
    );
    let resolver = make_resolver(home.path());
    let p = plan(&cfg, repo.path(), &resolver).unwrap();
    execute(&p, ExecOpts::default()).unwrap();

    let src_mtime = fs::metadata(&src).unwrap().modified().unwrap();
    let dst_mtime = fs::metadata(home.path().join("ts"))
        .unwrap()
        .modified()
        .unwrap();
    // Allow 1s slop for filesystems with coarse mtime resolution.
    let diff = src_mtime
        .duration_since(dst_mtime)
        .unwrap_or_else(|e| e.duration());
    assert!(
        diff < Duration::from_secs(1),
        "mtime drift: {diff:?} (src={src_mtime:?}, dst={dst_mtime:?})"
    );
}

#[test]
#[cfg(unix)]
fn unix_mode_is_preserved() {
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let src = repo.path().join("exec");
    fs::write(&src, b"#!/bin/sh\n").unwrap();
    let mut perms = fs::metadata(&src).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&src, perms).unwrap();

    let cfg = parse(
        r#"
[[link]]
src = "exec"
dst = "${HOME}/exec"
"#,
    );
    let resolver = make_resolver(home.path());
    let p = plan(&cfg, repo.path(), &resolver).unwrap();
    execute(&p, ExecOpts::default()).unwrap();

    let dst_mode = fs::metadata(home.path().join("exec"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dst_mode, 0o755);
}

#[test]
fn unknown_platform_string_errors() {
    let repo = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    // The schema parser already rejects unknown platform strings, so we
    // hand-build a Config to exercise the planner's defence-in-depth check.
    let cfg = parse(
        r#"
[[link]]
src = "x"
dst = "${HOME}/x"
"#,
    );
    let mut cfg = cfg;
    cfg.links[0].platform = Some("plan9".into());
    let resolver = make_resolver(home.path());
    let err = plan(&cfg, repo.path(), &resolver).unwrap_err();
    assert!(matches!(err, PlanError::UnknownPlatform { .. }));
}

#[test]
fn missing_source_during_execute_errors() {
    let home = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    // Plan refers to a file that doesn't exist on disk.
    let p = krypt_core::copy::Plan {
        actions: vec![Action::Copy {
            src: repo.path().join("missing"),
            dst: home.path().join("x"),
            kind: EntryKind::Link,
        }],
    };
    let err = execute(&p, ExecOpts::default()).unwrap_err();
    assert!(matches!(err, krypt_core::copy::ExecError::SourceMissing(_)));
    // BTreeMap import is just to silence the unused warning when this
    // file ever drops the `use` above.
    let _: BTreeMap<String, String> = BTreeMap::new();
}
