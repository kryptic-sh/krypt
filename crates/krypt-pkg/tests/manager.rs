//! Integration tests for package manager impls and orchestration.

use std::collections::BTreeMap;

use krypt_pkg::apt::Apt;
use krypt_pkg::brew::Brew;
use krypt_pkg::cargo::Cargo;
use krypt_pkg::deps::{DepGroup, DepsOpts, check_deps, install_deps};
use krypt_pkg::detect::{detect_all, pick_by_name};
use krypt_pkg::dnf::Dnf;
use krypt_pkg::manager::{MockResponse, MockRunner, PackageError, PackageManager, root_invocation};
use krypt_pkg::pacman::Pacman;
use krypt_pkg::scoop::{Scoop, add_buckets};
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
    let runner = MockRunner::new().with(
        "rpm",
        &["-q", "--whatprovides", "git"],
        MockResponse::success(),
    );
    assert!(Dnf.is_installed(&runner, "git").unwrap());
}

#[test]
fn dnf_is_installed_exit1() {
    let runner = MockRunner::new().with(
        "rpm",
        &["-q", "--whatprovides", "git"],
        MockResponse::failure(),
    );
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

/// A `scoop export` response listing `apps` as `(name, info)` and `buckets`.
fn scoop_export(apps: &[(&str, &str)], buckets: &[&str]) -> MockResponse {
    let apps: Vec<String> = apps
        .iter()
        .map(|(name, info)| {
            format!(r#"{{"Name":"{name}","Info":"{info}","Source":"main","Version":"1.0"}}"#)
        })
        .collect();
    let buckets: Vec<String> = buckets
        .iter()
        .map(|name| format!(r#"{{"Name":"{name}","Source":"https://example.invalid"}}"#))
        .collect();
    scoop_says(&format!(
        r#"{{"apps":[{}],"buckets":[{}]}}"#,
        apps.join(","),
        buckets.join(",")
    ))
}

/// A scoop response printing `stdout`. Scoop exits 0 whatever happened (an
/// unknown app included), so failures are mocked with exit 0 too.
fn scoop_says(stdout: &str) -> MockResponse {
    MockResponse {
        status: 0,
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

#[test]
fn scoop_is_installed_reads_the_export() {
    let runner = MockRunner::new().with(
        "scoop",
        &["export"],
        scoop_export(
            &[("jq", ""), ("Alacritty", ""), ("broken", "Install failed")],
            &["main"],
        ),
    );
    assert!(Scoop.is_installed(&runner, "jq").unwrap());
    assert!(Scoop.is_installed(&runner, "extras/alacritty").unwrap());
    // `scoop list j` matches jq; an installed-check must not.
    assert!(!Scoop.is_installed(&runner, "j").unwrap());
    assert!(!Scoop.is_installed(&runner, "broken").unwrap());
}

#[test]
fn scoop_is_installed_errors_on_unreadable_export() {
    let runner = MockRunner::new().with("scoop", &["export"], scoop_says("not json"));
    assert!(Scoop.is_installed(&runner, "jq").is_err());
}

#[test]
fn scoop_install_one_call_per_package_confirmed_from_the_export() {
    let runner = MockRunner::new().with(
        "scoop",
        &["export"],
        scoop_export(&[("foo", ""), ("bar", "")], &["main"]),
    );
    Scoop
        .install(&runner, &["foo".to_string(), "extras/bar".to_string()])
        .unwrap();
    let calls = runner.calls();
    assert_eq!(
        calls[0],
        ("scoop".to_string(), vec!["install".into(), "foo".into()]),
        "no sudo"
    );
    assert_eq!(calls[1].1, ["install", "extras/bar"]);
    assert_eq!(calls[2].1, ["export"]);
}

#[test]
fn scoop_install_reports_what_scoop_did_not_install() {
    let runner = MockRunner::new()
        .with(
            "scoop",
            &["install", "foo"],
            scoop_says("'foo' (1.0) was installed successfully!"),
        )
        .with(
            "scoop",
            &["install", "no-such"],
            scoop_says("Couldn't find manifest for 'no-such'."),
        )
        .with(
            "scoop",
            &["export"],
            scoop_export(&[("foo", "")], &["main"]),
        );
    let err = Scoop
        .install(&runner, &["foo".to_string(), "no-such".to_string()])
        .unwrap_err();
    match err {
        PackageError::NotInstalled { packages, output } => {
            assert_eq!(packages, ["no-such"]);
            assert_eq!(output, "Couldn't find manifest for 'no-such'.");
        }
        other => panic!("expected NotInstalled, got {other:?}"),
    }
}

#[test]
fn scoop_exists_needs_a_json_manifest() {
    let runner = MockRunner::new()
        .with(
            "scoop",
            &["cat", "jq"],
            scoop_says(r#"{"version":"1.8.2"}"#),
        )
        .with(
            "scoop",
            &["cat", "no-such"],
            scoop_says("Couldn't find manifest for 'no-such'."),
        );
    assert!(Scoop.exists(&runner, "jq").unwrap());
    assert!(!Scoop.exists(&runner, "no-such").unwrap());
}

#[test]
fn scoop_add_buckets_adds_only_missing_buckets() {
    let urls = BTreeMap::from([(
        "kryptic-sh".to_string(),
        "https://github.com/kryptic-sh/scoop-bucket".to_string(),
    )]);
    let runner = MockRunner::new()
        .with("scoop", &["export"], scoop_export(&[], &["main", "Extras"]))
        .with(
            "scoop",
            &["export"],
            scoop_export(&[], &["main", "extras", "nerd-fonts", "kryptic-sh"]),
        );
    let missing = add_buckets(
        &runner,
        &[
            "git",
            "extras/alacritty",
            "nerd-fonts/Hack-NF",
            "kryptic-sh/pikr",
            "kryptic-sh/hrdr",
        ],
        &urls,
    )
    .unwrap();
    assert!(missing.is_empty(), "{missing:?}");

    let adds: Vec<Vec<String>> = runner
        .calls()
        .into_iter()
        .filter(|(_, args)| args.first().is_some_and(|a| a == "bucket"))
        .map(|(_, args)| args)
        .collect();
    assert_eq!(
        adds,
        [
            vec!["bucket", "add", "nerd-fonts"],
            vec![
                "bucket",
                "add",
                "kryptic-sh",
                "https://github.com/kryptic-sh/scoop-bucket"
            ],
        ]
    );
}

#[test]
fn scoop_add_buckets_returns_buckets_scoop_did_not_add() {
    let runner = MockRunner::new().with("scoop", &["export"], scoop_export(&[], &["main"]));
    let missing = add_buckets(&runner, &["no-such/app"], &BTreeMap::new()).unwrap();
    assert_eq!(missing, ["no-such"]);
}

#[test]
fn scoop_add_buckets_is_a_no_op_without_qualified_entries() {
    let runner = MockRunner::new().with("scoop", &["export"], scoop_export(&[], &["main"]));
    assert!(
        add_buckets(&runner, &["git", "jq"], &BTreeMap::new())
            .unwrap()
            .is_empty()
    );
    assert_eq!(runner.calls().len(), 1, "only the export is read");
}

#[test]
fn install_deps_with_scoop_adds_buckets_and_reports_per_package() {
    let groups = vec![DepGroup {
        group: "core".into(),
        scoop: vec![
            "git".into(),
            "extras/alacritty".into(),
            "gone/app".into(),
            "no-such".into(),
        ],
        ..Default::default()
    }];
    let buckets_added = scoop_export(&[], &["main", "extras"]);
    let runner = MockRunner::new()
        // Bucket check, then the re-read after `bucket add`.
        .with("scoop", &["export"], scoop_export(&[], &["main"]))
        .with("scoop", &["export"], buckets_added.clone())
        // One is_installed per ready entry.
        .with("scoop", &["export"], buckets_added.clone())
        .with("scoop", &["export"], buckets_added.clone())
        .with("scoop", &["export"], buckets_added)
        // Read back after the install.
        .with(
            "scoop",
            &["export"],
            scoop_export(&[("git", ""), ("alacritty", "")], &["main", "extras"]),
        );

    let opts = DepsOpts {
        groups,
        manager: Some("scoop".into()),
        group_filter: None,
        dry_run: false,
    };
    let report = install_deps(&opts, &runner).unwrap();

    assert_eq!(report.installed, ["git", "extras/alacritty"]);
    let failed: Vec<&str> = report.failed.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(failed, ["gone/app", "no-such"]);
    assert!(
        report.failed[0].1.contains("bucket `gone`"),
        "{:?}",
        report.failed
    );
    let installs: Vec<String> = runner
        .calls()
        .into_iter()
        .filter(|(_, args)| args.len() == 2 && args[0] == "install")
        .map(|(_, mut args)| args.remove(1))
        .collect();
    assert_eq!(
        installs,
        ["git", "extras/alacritty", "no-such"],
        "the entry whose bucket is missing is never passed to scoop"
    );
}

#[test]
fn install_deps_dry_run_with_scoop_adds_no_buckets() {
    let groups = vec![DepGroup {
        group: "core".into(),
        scoop: vec!["extras/alacritty".into()],
        ..Default::default()
    }];
    let runner = MockRunner::new();
    let opts = DepsOpts {
        groups,
        manager: Some("scoop".into()),
        group_filter: None,
        dry_run: true,
    };
    let report = install_deps(&opts, &runner).unwrap();
    assert_eq!(report.installed, ["extras/alacritty"]);
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn check_deps_with_scoop_counts_a_bucket_it_cannot_add_as_missing() {
    let groups = vec![DepGroup {
        group: "core".into(),
        scoop: vec!["jq".into(), "gone/app".into()],
        ..Default::default()
    }];
    let runner = MockRunner::new()
        .with("scoop", &["export"], scoop_export(&[], &["main"]))
        .with(
            "scoop",
            &["cat", "jq"],
            scoop_says(r#"{"version":"1.8.2"}"#),
        );
    let opts = DepsOpts {
        groups,
        manager: Some("scoop".into()),
        group_filter: None,
        dry_run: false,
    };
    let report = check_deps(&opts, &runner).unwrap();
    assert_eq!(report.found, ["jq"]);
    assert_eq!(report.missing, ["gone/app"]);
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

fn brew_info(pkg: &str, json: &str) -> MockRunner {
    MockRunner::new().with(
        "brew",
        &["info", "--json=v2", pkg],
        MockResponse {
            status: 0,
            stdout: json.into(),
            stderr: String::new(),
        },
    )
}

#[test]
fn brew_exists_taps_a_tap_qualified_name_first() {
    let runner = brew_info(
        "kryptic-sh/tap/hjkl",
        r#"{"formulae":[{"name":"hjkl","disabled":false}],"casks":[]}"#,
    );
    assert!(Brew.exists(&runner, "kryptic-sh/tap/hjkl").unwrap());
    assert_eq!(
        runner.calls()[0],
        (
            "brew".to_string(),
            vec!["tap".into(), "kryptic-sh/tap".into()]
        )
    );

    let no_tap = MockRunner::new().with("brew", &["tap", "nobody/tap"], MockResponse::failure());
    assert!(!Brew.exists(&no_tap, "nobody/tap/x").unwrap());
    assert_eq!(no_tap.calls().len(), 1, "no info lookup without the tap");
}

#[test]
fn brew_exists_rejects_disabled_and_unknown_packages() {
    let cask = brew_info(
        "firefox",
        r#"{"formulae":[],"casks":[{"token":"firefox","disabled":false}]}"#,
    );
    assert!(Brew.exists(&cask, "firefox").unwrap());
    assert_eq!(cask.calls().len(), 1, "a plain name is not tapped");

    let disabled = brew_info(
        "alacritty",
        r#"{"formulae":[],"casks":[{"token":"alacritty","disabled":true}]}"#,
    );
    assert!(!Brew.exists(&disabled, "alacritty").unwrap());

    let unknown = MockRunner::new().with(
        "brew",
        &["info", "--json=v2", "no-such"],
        MockResponse::failure(),
    );
    assert!(!Brew.exists(&unknown, "no-such").unwrap());
}

#[test]
fn winget_exists_follows_exit_status() {
    let runner = MockRunner::new().with(
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

/// The `cargo:` entries run cargo, which the group's own install may have just
/// put on PATH (rustup), so the PATH is refreshed between the two.
#[test]
fn install_deps_refreshes_path_before_cargo_entries() {
    let groups = vec![DepGroup {
        group: "dev".into(),
        apt: vec!["rustup".into(), "cargo:hjkl".into()],
        ..Default::default()
    }];
    let runner = cargo_list("").with("dpkg", &["-s", "rustup"], MockResponse::failure());
    let opts = DepsOpts {
        groups,
        manager: Some("apt".into()),
        group_filter: None,
        dry_run: false,
    };
    install_deps(&opts, &runner).unwrap();

    let calls = runner.calls();
    let position = |cmd: &str, args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        calls
            .iter()
            .position(|(c, a)| c == cmd && *a == args)
            .unwrap_or_else(|| panic!("no call {cmd} {args:?} in {calls:?}"))
    };
    let apt = position("sudo", &["apt-get", "install", "-y", "rustup"]);
    let refresh = position(MockRunner::REFRESH_PATH, &[]);
    let cargo = position("cargo", &["install", "--locked", "hjkl"]);
    assert!(apt < refresh && refresh < cargo, "calls: {calls:?}");
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
