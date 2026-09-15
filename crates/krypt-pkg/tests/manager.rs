//! Integration tests for package manager impls and orchestration.

use krypt_pkg::apt::Apt;
use krypt_pkg::brew::Brew;
use krypt_pkg::cargo::Cargo;
use krypt_pkg::deps::{DepGroup, DepsOpts, check_deps, install_deps};
use krypt_pkg::detect::{detect_all, pick_by_name};
use krypt_pkg::dnf::Dnf;
use krypt_pkg::manager::{MockResponse, MockRunner, PackageManager, root_invocation};
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
        &["list", "--versions", "git"],
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
        &["list", "--versions", "git"],
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
    for ((cmd, args), pkg) in calls.iter().zip(["foo", "bar"]) {
        assert_eq!(cmd, "winget");
        assert_eq!(
            args,
            &[
                "install",
                "--id",
                pkg,
                "--exact",
                "--silent",
                "--accept-package-agreements",
                "--accept-source-agreements"
            ]
        );
    }
}

#[test]
fn winget_install_treats_no_applicable_upgrade_as_installed() {
    let runner = MockRunner::new().with(
        "winget",
        &[
            "install",
            "--id",
            "Git.Git",
            "--exact",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ],
        MockResponse {
            status: krypt_pkg::winget::UPDATE_NOT_APPLICABLE,
            stdout: "No available upgrade found.".into(),
            stderr: String::new(),
        },
    );
    Winget.install(&runner, &["Git.Git".to_string()]).unwrap();
}

#[test]
fn winget_install_other_failure_is_error() {
    let runner = MockRunner::new().with(
        "winget",
        &[
            "install",
            "--id",
            "Git.Git",
            "--exact",
            "--silent",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ],
        MockResponse::failure(),
    );
    assert!(Winget.install(&runner, &["Git.Git".to_string()]).is_err());
}

#[test]
fn winget_is_installed_non_empty() {
    let runner = MockRunner::new().with(
        "winget",
        &["list", "--id", "Git.Git", "--exact"],
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
        &["list", "--id", "Git.Git", "--exact"],
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

/// In non-dry-run mode, is_installed is called and already-installed packages are skipped.
#[test]
fn install_deps_skips_already_installed() {
    let groups = vec![DepGroup {
        group: "core".into(),
        apt: vec!["curl".into(), "git".into()],
        ..Default::default()
    }];

    // curl not installed, git installed
    let runner = MockRunner::new()
        .with("dpkg", &["-s", "curl"], MockResponse::failure())
        .with("dpkg", &["-s", "git"], MockResponse::success())
        // install call for curl only
        .with("sudo", &["apt-get", "install", "-y", "curl"], MockResponse::success());

    let opts = DepsOpts {
        groups,
        manager: Some("apt".into()),
        group_filter: None,
        dry_run: false,
    };

    let report = install_deps(&opts, &runner).unwrap();
    assert_eq!(report.managers_used, ["apt"]);
    assert!(report.installed.contains(&"curl".to_string()));
    assert!(report.already_installed.contains(&"git".to_string()));
}

/// In dry-run mode, is_installed is skipped and all packages are reported as would-install.
#[test]
fn install_deps_dry_run_skips_is_installed() {
    let groups = vec![DepGroup {
        group: "core".into(),
        apt: vec!["curl".into(), "git".into()],
        ..Default::default()
    }];

    // No mock responses needed — is_installed should not be called in dry-run.
    let runner = MockRunner::new();

    let opts = DepsOpts {
        groups,
        manager: Some("apt".into()),
        group_filter: None,
        dry_run: true,
    };

    let report = install_deps(&opts, &runner).unwrap();
    assert_eq!(report.managers_used, ["apt"]);
    assert!(report.installed.contains(&"curl".to_string()));
    assert!(report.installed.contains(&"git".to_string()));
    assert!(report.already_installed.is_empty());
    // No dpkg calls should have been made.
    let calls = runner.calls();
    assert!(
        calls.is_empty(),
        "is_installed should not be called in dry-run mode"
    );
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

    let runner = MockRunner::new();

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

// ─── root privileges ──────────────────────────────────────────────────────────

#[test]
fn root_invocation_prefixes_sudo_only_when_present() {
    assert_eq!(
        root_invocation(true, "apt-get", &["install", "-y", "git"]),
        ("sudo", vec!["apt-get", "install", "-y", "git"])
    );
    assert_eq!(
        root_invocation(false, "apt-get", &["install", "-y", "git"]),
        ("apt-get", vec!["install", "-y", "git"])
    );
}

#[test]
fn system_managers_run_directly_without_sudo() {
    let pkgs = ["git".to_string()];

    let runner = MockRunner::new().without_sudo();
    Apt.install(&runner, &pkgs).unwrap();
    Dnf.install(&runner, &pkgs).unwrap();
    Pacman.install(&runner, &pkgs).unwrap();

    let calls = runner.calls();
    assert_eq!(
        calls[0],
        (
            "apt-get".into(),
            vec!["install".into(), "-y".into(), "git".into()]
        )
    );
    assert_eq!(
        calls[1],
        (
            "dnf".into(),
            vec!["install".into(), "-y".into(), "git".into()]
        )
    );
    assert!(calls[2].0 == "pacman" || calls[2].0 == "paru");
    assert_eq!(calls[2].1, ["-S", "--noconfirm", "git"]);
}

// ─── exists ───────────────────────────────────────────────────────────────────

#[test]
fn apt_exists_simulates_the_install() {
    let args = ["install", "--simulate", "--quiet", "bat"];
    let runner = MockRunner::new().with("apt-get", &args, MockResponse::success());
    assert!(Apt.exists(&runner, "bat").unwrap());
    let runner = MockRunner::new().with(
        "apt-get",
        &args,
        MockResponse {
            status: 100,
            stdout: String::new(),
            stderr: "E: Unable to locate package bat".into(),
        },
    );
    assert!(!Apt.exists(&runner, "bat").unwrap());
}

#[test]
fn dnf_exists_needs_repoquery_output() {
    let args = ["repoquery", "--quiet", "--whatprovides", "bat"];
    let runner = MockRunner::new().with(
        "dnf",
        &args,
        MockResponse {
            status: 0,
            stdout: "bat-0:0.26.1-1.fc44.x86_64\n".into(),
            stderr: String::new(),
        },
    );
    assert!(Dnf.exists(&runner, "bat").unwrap());
    // dnf5 exits 0 with no output when nothing matches.
    let runner = MockRunner::new().with("dnf", &args, MockResponse::success());
    assert!(!Dnf.exists(&runner, "bat").unwrap());
}

#[test]
fn dnf_exists_looks_groups_up_as_groups() {
    let runner = MockRunner::new().with(
        "dnf",
        &["group", "info", "development-tools"],
        MockResponse::failure(),
    );
    assert!(!Dnf.exists(&runner, "@development-tools").unwrap());
    assert_eq!(runner.calls()[0].1, ["group", "info", "development-tools"]);
}

#[test]
fn pacman_exists_finds_sync_packages_without_asking_the_aur() {
    let runner = MockRunner::new();
    assert!(Pacman.exists(&runner, "bat").unwrap());
    assert_eq!(
        runner.calls(),
        [("pacman".to_string(), vec!["-Si".into(), "bat".into()])]
    );
}

fn aur_reply(pkg: &str, body: &str) -> MockRunner {
    let url = format!("{}?arg[]={pkg}", krypt_pkg::pacman::AUR_INFO_URL);
    MockRunner::new()
        .with("pacman", &["-Si", pkg], MockResponse::failure())
        .with(
            "curl",
            &["-fsSL", &url],
            MockResponse {
                status: 0,
                stdout: body.into(),
                stderr: String::new(),
            },
        )
}

#[test]
fn pacman_exists_falls_back_to_the_aur() {
    let found = aur_reply(
        "hjkl-bin",
        r#"{"resultcount":1,"results":[{"Name":"hjkl-bin"}],"type":"multiinfo","version":5}"#,
    );
    assert!(Pacman.exists(&found, "hjkl-bin").unwrap());

    let missing = aur_reply(
        "no-such",
        r#"{"resultcount":0,"results":[],"type":"multiinfo","version":5}"#,
    );
    assert!(!Pacman.exists(&missing, "no-such").unwrap());

    let garbage = aur_reply("no-such", "<html>rate limited</html>");
    assert!(Pacman.exists(&garbage, "no-such").is_err());
}

#[test]
fn brew_exists_taps_a_tap_qualified_name_first() {
    let runner = MockRunner::new();
    assert!(Brew.exists(&runner, "kryptic-sh/tap/hjkl").unwrap());
    assert_eq!(
        runner.calls(),
        [
            (
                "brew".to_string(),
                vec!["tap".into(), "kryptic-sh/tap".into()]
            ),
            (
                "brew".to_string(),
                vec!["info".into(), "kryptic-sh/tap/hjkl".into()]
            ),
        ]
    );

    let no_tap = MockRunner::new().with("brew", &["tap", "nobody/tap"], MockResponse::failure());
    assert!(!Brew.exists(&no_tap, "nobody/tap/x").unwrap());
    assert_eq!(no_tap.calls().len(), 1, "no info lookup without the tap");

    let plain = MockRunner::new();
    assert!(Brew.exists(&plain, "git").unwrap());
    assert_eq!(
        plain.calls(),
        [("brew".to_string(), vec!["info".into(), "git".into()])]
    );
}

#[test]
fn brew_scoop_winget_exists_follow_exit_status() {
    let runner = MockRunner::new()
        .with(
            "brew",
            &["info", "kryptic-sh/tap/hjkl"],
            MockResponse::success(),
        )
        .with("scoop", &["info", "pikr"], MockResponse::failure())
        .with(
            "winget",
            &[
                "show",
                "--id",
                "Git.Git",
                "--exact",
                "--accept-source-agreements",
            ],
            MockResponse::success(),
        );
    assert!(Brew.exists(&runner, "kryptic-sh/tap/hjkl").unwrap());
    assert!(!Scoop.exists(&runner, "pikr").unwrap());
    assert!(Winget.exists(&runner, "Git.Git").unwrap());
}

// ─── cargo ────────────────────────────────────────────────────────────────────

fn cargo_list(stdout: &str) -> MockRunner {
    MockRunner::new().with(
        "cargo",
        &["install", "--list"],
        MockResponse {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        },
    )
}

#[test]
fn cargo_is_installed_matches_crate_lines_only() {
    let runner = cargo_list("hjkl v0.41.6:\n    hjkl\nripgrep v14.1.1:\n    rg\n");
    assert!(Cargo.is_installed(&runner, "hjkl").unwrap());
    assert!(Cargo.is_installed(&runner, "ripgrep@14").unwrap());
    // `rg` is a binary of ripgrep, not an installed crate.
    assert!(!Cargo.is_installed(&runner, "rg").unwrap());
    assert!(!Cargo.is_installed(&runner, "hj").unwrap());
}

#[test]
fn cargo_install_is_locked_and_unprivileged() {
    let runner = MockRunner::new();
    Cargo
        .install(&runner, &["hjkl".to_string(), "sqeel".to_string()])
        .unwrap();
    assert_eq!(
        runner.calls(),
        [(
            "cargo".to_string(),
            vec![
                "install".into(),
                "--locked".into(),
                "hjkl".into(),
                "sqeel".into()
            ]
        )]
    );
}

#[test]
fn install_deps_routes_cargo_entries_to_cargo() {
    let groups = vec![DepGroup {
        group: "tools".into(),
        apt: vec!["curl".into(), "cargo:hjkl".into(), "cargo:quoty".into()],
        ..Default::default()
    }];
    let runner = cargo_list("quoty v0.2.10:\n    quoty\n").with(
        "dpkg",
        &["-s", "curl"],
        MockResponse::failure(),
    );

    let opts = DepsOpts {
        groups,
        manager: Some("apt".into()),
        group_filter: None,
        dry_run: false,
    };
    let report = install_deps(&opts, &runner).unwrap();

    assert_eq!(report.managers_used, ["apt", "cargo"]);
    assert_eq!(report.installed, ["curl", "cargo:hjkl"]);
    assert_eq!(report.already_installed, ["cargo:quoty"]);
    let calls = runner.calls();
    assert!(calls.contains(&(
        "sudo".to_string(),
        vec![
            "apt-get".into(),
            "install".into(),
            "-y".into(),
            "curl".into()
        ]
    )));
    assert!(calls.contains(&(
        "cargo".to_string(),
        vec!["install".into(), "--locked".into(), "hjkl".into()]
    )));
}

#[test]
fn check_deps_sorts_found_and_missing_without_installing() {
    let groups = vec![DepGroup {
        group: "tools".into(),
        apt: vec!["bat".into(), "no-such".into(), "cargo:hjkl".into()],
        ..Default::default()
    }];
    let runner = MockRunner::new()
        .with(
            "apt-get",
            &["install", "--simulate", "--quiet", "no-such"],
            MockResponse::failure(),
        )
        .with("cargo", &["info", "hjkl"], MockResponse::success());

    let opts = DepsOpts {
        groups,
        manager: Some("apt".into()),
        group_filter: None,
        dry_run: false,
    };
    let report = check_deps(&opts, &runner).unwrap();

    assert_eq!(report.managers_used, ["apt", "cargo"]);
    assert_eq!(report.found, ["bat", "cargo:hjkl"]);
    assert_eq!(report.missing, ["no-such"]);
    assert!(
        runner
            .calls()
            .iter()
            .all(|(cmd, args)| cmd != "sudo" && !args.contains(&"-y".to_string())),
        "a check never installs: {:?}",
        runner.calls()
    );
}
