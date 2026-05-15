//! Integration tests for package manager impls and orchestration.

use krypt_pkg::apt::Apt;
use krypt_pkg::brew::Brew;
use krypt_pkg::deps::{DepGroup, DepsOpts, install_deps};
use krypt_pkg::detect::{detect_all, pick_by_name};
use krypt_pkg::dnf::Dnf;
use krypt_pkg::manager::{MockResponse, MockRunner, PackageManager};
use krypt_pkg::pacman::Pacman;
use krypt_pkg::scoop::Scoop;
use krypt_pkg::winget::Winget;

// ─── pacman ───────────────────────────────────────────────────────────────────

#[test]
fn pacman_install_batches_with_sudo() {
    let runner = MockRunner::new();
    Pacman
        .install(&runner, &["foo".to_string(), "bar".to_string()])
        .unwrap();
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    let (cmd, args) = &calls[0];
    assert_eq!(cmd, "sudo");
    // args[0] is either "pacman" or "paru"; args[1..] is fixed
    assert!(args[0] == "pacman" || args[0] == "paru");
    assert_eq!(&args[1..], &["-S", "--noconfirm", "foo", "bar"]);
}

#[test]
fn pacman_is_installed_exit0() {
    let runner = MockRunner::new().with(
        "pacman",
        &["-Q", "git"],
        MockResponse {
            status: 0,
            stdout: "git 2.44.0-1".into(),
            stderr: String::new(),
        },
    );
    assert!(Pacman.is_installed(&runner, "git").unwrap());
}

#[test]
fn pacman_is_installed_exit1() {
    let runner = MockRunner::new().with("pacman", &["-Q", "git"], MockResponse::failure());
    assert!(!Pacman.is_installed(&runner, "git").unwrap());
}

// ─── apt ──────────────────────────────────────────────────────────────────────

#[test]
fn apt_install_batches_with_sudo() {
    let runner = MockRunner::new();
    Apt.install(&runner, &["foo".to_string(), "bar".to_string()])
        .unwrap();
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    let (cmd, args) = &calls[0];
    assert_eq!(cmd, "sudo");
    assert_eq!(args, &["apt-get", "install", "-y", "foo", "bar"]);
}

#[test]
fn apt_is_installed_exit0() {
    let runner = MockRunner::new().with("dpkg", &["-s", "git"], MockResponse::success());
    assert!(Apt.is_installed(&runner, "git").unwrap());
}

#[test]
fn apt_is_installed_exit1() {
    let runner = MockRunner::new().with("dpkg", &["-s", "git"], MockResponse::failure());
    assert!(!Apt.is_installed(&runner, "git").unwrap());
}

// ─── dnf ──────────────────────────────────────────────────────────────────────

#[test]
fn dnf_install_batches_with_sudo() {
    let runner = MockRunner::new();
    Dnf.install(&runner, &["foo".to_string(), "bar".to_string()])
        .unwrap();
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    let (cmd, args) = &calls[0];
    assert_eq!(cmd, "sudo");
    assert_eq!(args, &["dnf", "install", "-y", "foo", "bar"]);
}

#[test]
fn dnf_is_installed_exit0() {
    let runner = MockRunner::new().with("rpm", &["-q", "git"], MockResponse::success());
    assert!(Dnf.is_installed(&runner, "git").unwrap());
}

#[test]
fn dnf_is_installed_exit1() {
    let runner = MockRunner::new().with("rpm", &["-q", "git"], MockResponse::failure());
    assert!(!Dnf.is_installed(&runner, "git").unwrap());
}

// ─── brew ─────────────────────────────────────────────────────────────────────

#[test]
fn brew_install_no_sudo() {
    let runner = MockRunner::new();
    Brew.install(&runner, &["foo".to_string(), "bar".to_string()])
        .unwrap();
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    let (cmd, args) = &calls[0];
    assert_eq!(cmd, "brew");
    assert_eq!(args, &["install", "foo", "bar"]);
}

#[test]
fn brew_is_installed_non_empty_stdout() {
    let runner = MockRunner::new().with(
        "brew",
        &["list", "--formula", "--versions", "git"],
        MockResponse {
            status: 0,
            stdout: "git 2.44.0".into(),
            stderr: String::new(),
        },
    );
    assert!(Brew.is_installed(&runner, "git").unwrap());
}

#[test]
fn brew_is_installed_empty_stdout() {
    let runner = MockRunner::new().with(
        "brew",
        &["list", "--formula", "--versions", "git"],
        MockResponse {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        },
    );
    assert!(!Brew.is_installed(&runner, "git").unwrap());
}

// ─── scoop ────────────────────────────────────────────────────────────────────

#[test]
fn scoop_install_no_sudo() {
    let runner = MockRunner::new();
    Scoop
        .install(&runner, &["foo".to_string(), "bar".to_string()])
        .unwrap();
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    let (cmd, args) = &calls[0];
    assert_eq!(cmd, "scoop");
    assert_eq!(args, &["install", "foo", "bar"]);
}

#[test]
fn scoop_is_installed_non_empty() {
    let runner = MockRunner::new().with(
        "scoop",
        &["list", "git"],
        MockResponse {
            status: 0,
            stdout: "git".into(),
            stderr: String::new(),
        },
    );
    assert!(Scoop.is_installed(&runner, "git").unwrap());
}

#[test]
fn scoop_is_installed_empty() {
    let runner = MockRunner::new().with(
        "scoop",
        &["list", "git"],
        MockResponse {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        },
    );
    assert!(!Scoop.is_installed(&runner, "git").unwrap());
}

// ─── winget ───────────────────────────────────────────────────────────────────

#[test]
fn winget_install_one_call_per_package() {
    let runner = MockRunner::new();
    Winget
        .install(&runner, &["foo".to_string(), "bar".to_string()])
        .unwrap();
    let calls = runner.calls();
    assert_eq!(
        calls.len(),
        2,
        "winget should invoke one process per package"
    );
    for (cmd, args) in &calls {
        assert_eq!(cmd, "winget");
        assert_eq!(
            &args[..4],
            &[
                "install",
                "--silent",
                "--accept-package-agreements",
                "--accept-source-agreements"
            ]
        );
    }
    assert_eq!(calls[0].1[4], "foo");
    assert_eq!(calls[1].1[4], "bar");
}

#[test]
fn winget_is_installed_non_empty() {
    let runner = MockRunner::new().with(
        "winget",
        &["list", "--id", "Git.Git"],
        MockResponse {
            status: 0,
            stdout: "Git.Git  2.44.0".into(),
            stderr: String::new(),
        },
    );
    assert!(Winget.is_installed(&runner, "Git.Git").unwrap());
}

#[test]
fn winget_is_installed_empty() {
    let runner = MockRunner::new().with(
        "winget",
        &["list", "--id", "Git.Git"],
        MockResponse {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        },
    );
    assert!(!Winget.is_installed(&runner, "Git.Git").unwrap());
}

// ─── auto-detection ───────────────────────────────────────────────────────────

#[test]
fn detect_all_returns_at_least_one_on_host() {
    let all = detect_all();
    assert!(
        !all.is_empty(),
        "expected at least one manager available on the test host"
    );
}

#[test]
fn pick_by_name_returns_none_for_unknown() {
    assert!(pick_by_name("nonexistent").is_none());
}

#[test]
fn pick_by_name_returns_manager_by_name() {
    let mgr = pick_by_name("apt").expect("apt should always be registered");
    assert_eq!(mgr.name(), "apt");
}

// ─── install_deps orchestration ───────────────────────────────────────────────

#[test]
fn install_deps_installs_and_skips_already_present() {
    let groups = vec![DepGroup {
        group: "core".into(),
        apt: vec!["curl".into(), "git".into()],
        ..Default::default()
    }];

    // curl not installed, git installed
    let runner = MockRunner::new()
        .with("dpkg", &["-s", "curl"], MockResponse::failure())
        .with("dpkg", &["-s", "git"], MockResponse::success());

    let opts = DepsOpts {
        groups,
        manager: Some("apt".into()),
        group_filter: None,
        dry_run: true,
    };

    let report = install_deps(&opts, &runner).unwrap();
    assert_eq!(report.manager_used, "apt");
    assert!(report.installed.contains(&"curl".to_string()));
    assert!(report.already_installed.contains(&"git".to_string()));
}

#[test]
fn install_deps_group_filter_works() {
    let groups = vec![
        DepGroup {
            group: "a".into(),
            apt: vec!["pkg-a".into()],
            ..Default::default()
        },
        DepGroup {
            group: "b".into(),
            apt: vec!["pkg-b".into()],
            ..Default::default()
        },
    ];

    let runner = MockRunner::new().with("dpkg", &["-s", "pkg-b"], MockResponse::failure());

    let opts = DepsOpts {
        groups,
        manager: Some("apt".into()),
        group_filter: Some("b".into()),
        dry_run: true,
    };

    let report = install_deps(&opts, &runner).unwrap();
    assert!(report.installed.contains(&"pkg-b".to_string()));
    assert!(!report.installed.contains(&"pkg-a".to_string()));
}

#[test]
fn install_deps_skips_empty_package_list() {
    let groups = vec![DepGroup {
        group: "fonts".into(),
        // apt list is empty — only brew packages defined
        brew: vec!["font-hack".into()],
        ..Default::default()
    }];

    let runner = MockRunner::new();

    let opts = DepsOpts {
        groups,
        manager: Some("apt".into()),
        group_filter: None,
        dry_run: true,
    };

    let report = install_deps(&opts, &runner).unwrap();
    assert!(report.skipped_unavailable.contains(&"fonts".to_string()));
    assert!(report.installed.is_empty());
}
