//! `krypt` — cross-platform dotfiles manager.
//!
//! This is the CLI entrypoint. Real logic lives in `krypt-core`. This crate
//! is intentionally thin: clap wiring + delegation.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::Result;
use krypt_core::adopt::{AdoptEditsOpts, AdoptError, AdoptOpts, adopt, adopt_edits};
use krypt_core::deploy::{DeployOpts, LinkReport, UnlinkReport, link, relink, unlink};
use krypt_core::doctor::{DoctorOpts, doctor};
use krypt_core::init::{InitError, InitOpts, init};
use krypt_core::manifest::{DriftStatus, Manifest, detect_drift};
use krypt_core::notify::{AutoNotifier, NotifyBackend, detect};
use krypt_core::paths::{Platform, Resolver};
use krypt_core::runner::Notifier as _;
use krypt_core::setup::{RealGitConfig, RealPrompter, SetupError, SetupOpts, YesPrompter};
use krypt_core::tool_config::ToolConfig;
use krypt_core::update::{UpdateError, UpdateOpts, update};
use krypt_pkg::deps::{DepsError, DepsOpts, install_deps};
use krypt_pkg::manager::RealRunner;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum PlatformArg {
    Linux,
    Macos,
    Windows,
}

impl From<PlatformArg> for Platform {
    fn from(p: PlatformArg) -> Self {
        match p {
            PlatformArg::Linux => Platform::Linux,
            PlatformArg::Macos => Platform::Macos,
            PlatformArg::Windows => Platform::Windows,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name    = "krypt",
    version,
    about   = "Cross-platform dotfiles manager.",
    long_about = None,
    propagate_version = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print version information and exit.
    Version,

    /// Parse and validate a `.krypt.toml` file.
    ///
    /// Exits 0 on success, non-zero with a pretty error on failure.
    Validate {
        /// Path to the config file to check. Defaults to `.krypt.toml` in
        /// the current directory.
        #[arg(default_value = ".krypt.toml")]
        path: PathBuf,
    },

    /// Resolve and print every known path variable.
    ///
    /// Useful for sanity-checking that XDG paths land where you expect on
    /// the current host, and for debugging `[paths]` overrides.
    Paths {
        /// Apply `[paths]` overrides from this config (defaults to
        /// `.krypt.toml` if present). Pass `--no-config` to skip.
        #[arg(long, default_value = ".krypt.toml")]
        config: PathBuf,

        /// Don't read overrides from any config file.
        #[arg(long, conflicts_with = "config")]
        no_config: bool,
    },

    /// Compare deployed files on disk against the manifest.
    ///
    /// Reports each entry as `clean`, `drifted` (hash mismatch), or
    /// `missing` (file gone). Exits 0 on clean, 1 if any drift found.
    Diff {
        /// Path to the manifest. Defaults to
        /// `${XDG_STATE}/krypt/manifest.json`.
        #[arg(long)]
        manifest: Option<PathBuf>,
    },

    /// Deploy every entry in `.krypt.toml`. Idempotent.
    ///
    /// Re-deploys files whose destination matches the manifest hash
    /// silently. Conflicts (destinations with content not tracked by the
    /// manifest) are skipped unless `--force` is set.
    Link(DeployArgs),

    /// Remove every file recorded in the manifest.
    ///
    /// Drifted destinations are kept by default — pass `--force` to
    /// delete them anyway.
    Unlink {
        /// Manifest path. Defaults to `${XDG_STATE}/krypt/manifest.json`.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Don't touch disk; print what would be removed.
        #[arg(long)]
        dry_run: bool,
        /// Delete drifted destinations too.
        #[arg(long)]
        force: bool,
    },

    /// `unlink` followed by `link`. Useful after big `.krypt.toml`
    /// edits where you want a clean re-deploy.
    Relink(DeployArgs),

    /// Clone a dotfiles repo and write the tool config.
    ///
    /// After `init`, run `krypt link --config <repo-path>/.krypt.toml` to
    /// deploy your dotfiles.
    Init(InitArgs),

    /// Pull the dotfiles repo and re-deploy.
    ///
    /// Reads the tool config to find the repo, fast-forward-pulls it via gix
    /// (HTTPS only — no SSH; see `krypt init --help`), then re-runs `link`.
    ///
    /// The working tree must be clean before running this command — commit,
    /// stash, or discard any changes first.  Auto-stash support is planned
    /// once gix gains a stash API.
    Update(UpdateArgs),

    /// Import an existing file into the dotfiles repo.
    ///
    /// Copies the file at `<dst>` into the repo (auto-derives the repo-relative
    /// path by stripping `$HOME`), records a manifest entry, and prints a
    /// `[[link]]` block to paste into `.krypt.toml`.
    ///
    /// The original file at `<dst>` is left in place — nothing is moved.
    #[command(name = "adopt")]
    Adopt(AdoptArgs),

    /// Sync in-place edits on deployed files back into the repo.
    ///
    /// For every drifted manifest entry, copies `dst` bytes back into
    /// `<repo>/<src>` and refreshes the manifest hashes.
    #[command(name = "adopt-edits")]
    AdoptEdits(AdoptEditsArgs),

    /// Run a full diagnostic health-check.
    ///
    /// Prints one status line per check. Use `--json` for machine-readable
    /// output suitable for bug reports or scripting. Exits 0 when all
    /// checks pass, 1 when one or more need attention.
    Doctor(DoctorArgs),

    /// Run the interactive setup wizard defined by `[prompts.*]` sections.
    ///
    /// Reads prompt sections from `.krypt.toml`, asks the user questions, and
    /// writes the collected values to destination files using the section's
    /// configured writer (gitconfig, hypr_vars, env, generic_template).
    Setup(SetupArgs),

    /// Install packages listed in `[[deps]]` using the appropriate package manager.
    ///
    /// Auto-detects the right manager for the current OS. Use `--manager` to
    /// override. Groups are filtered by `required_platforms`; use `--group` to
    /// target a single group. Use `--dry-run` to see what would be installed
    /// without touching the system.
    Deps(DepsArgs),

    /// Send a desktop notification.
    ///
    /// Dispatches a notification via the best available backend for the
    /// current platform. On Linux: `notify-send`. On macOS: `terminal-notifier`
    /// (if installed) or `osascript`. On Windows: PowerShell
    /// `System.Windows.Forms.MessageBox`. Falls back to stderr when nothing
    /// is available.
    ///
    /// Windows note: BurntToast is NOT used — `System.Windows.Forms.MessageBox`
    /// ships with every .NET install. BurntToast requires `Install-Module
    /// BurntToast` and is a third-party dependency.
    ///
    /// Backend values for `--backend`: auto, notify-send, osascript,
    /// terminal-notifier, powershell, stderr.
    Notify(NotifyArgs),
}

#[derive(clap::Args, Debug)]
struct InitArgs {
    /// Remote URL to clone (positional).
    #[arg(conflicts_with = "from")]
    url: Option<String>,

    /// Remote URL to clone (flag form — alias for the positional URL).
    #[arg(long, conflicts_with = "url")]
    from: Option<String>,

    /// Create an empty `.krypt.toml` stub without cloning. Mutually
    /// exclusive with providing a URL.
    #[arg(long, conflicts_with = "url", conflicts_with = "from")]
    bare: bool,

    /// Wipe the repo path if it already exists.
    #[arg(long)]
    force: bool,

    /// Override the default repo path (`${XDG_CONFIG}/krypt/repo`).
    #[arg(long)]
    repo_path: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct UpdateArgs {
    /// Override the path to `.krypt.toml` (defaults to `<repo_path>/.krypt.toml`).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Don't touch disk; pull the repo but pass dry_run to link.
    #[arg(long)]
    dry_run: bool,

    /// (No-op) Accept the flag for forward compatibility when hooks are implemented.
    #[arg(long)]
    skip_hooks: bool,

    /// On link: overwrite real conflicts.
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args, Debug)]
struct AdoptArgs {
    /// Absolute path to the file to import (typically under `$HOME`).
    dst: PathBuf,

    /// Override the auto-derived repo-relative source path.
    ///
    /// Required when `<dst>` is not under `$HOME`.
    #[arg(long)]
    src: Option<PathBuf>,

    /// Override the default repo path (`${XDG_CONFIG}/krypt/repo`).
    #[arg(long)]
    repo_path: Option<PathBuf>,

    /// Override the manifest path (`${XDG_STATE}/krypt/manifest.json`).
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Overwrite an existing file at `<repo>/<src>` without erroring.
    #[arg(long)]
    force: bool,

    /// Print the `[[link]]` suggestion without touching disk.
    #[arg(long)]
    dry_run: bool,
}

#[derive(clap::Args, Debug)]
struct AdoptEditsArgs {
    /// Override the manifest path (`${XDG_STATE}/krypt/manifest.json`).
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Override the default repo path (`${XDG_CONFIG}/krypt/repo`).
    #[arg(long)]
    repo_path: Option<PathBuf>,

    /// Print what would be synced without touching disk.
    #[arg(long)]
    dry_run: bool,
}

#[derive(clap::Args, Debug)]
struct DoctorArgs {
    /// Emit machine-readable JSON instead of the human-readable report.
    #[arg(long)]
    json: bool,

    /// Override the path to `.krypt.toml`.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Override the manifest path.
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Override the path to the tool config (`${XDG_CONFIG}/krypt/config.toml`).
    #[arg(long)]
    tool_config: Option<PathBuf>,

    /// Override the repo path.
    #[arg(long)]
    repo_path: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct DepsArgs {
    /// Path to `.krypt.toml`. Defaults to `.krypt.toml` in the current directory.
    #[arg(long, default_value = ".krypt.toml")]
    config: PathBuf,

    /// Override the detected package manager (e.g. `apt`, `pacman`).
    #[arg(long)]
    manager: Option<String>,

    /// Install only this dependency group.
    #[arg(long)]
    group: Option<String>,

    /// Print what would be installed without touching the system.
    #[arg(long)]
    dry_run: bool,
}

#[derive(clap::Args, Debug)]
struct NotifyArgs {
    /// Notification title.
    title: String,

    /// Notification body.
    body: String,

    /// Override the notification backend.
    ///
    /// Values: auto, notify-send, osascript, terminal-notifier, powershell, stderr.
    /// Overrides `[meta] notify_backend` from config.
    #[arg(long)]
    backend: Option<String>,

    /// Path to `.krypt.toml` to read `[meta] notify_backend` from.
    /// Defaults to `.krypt.toml` in the current directory; silently ignored
    /// if the file does not exist.
    #[arg(long, default_value = ".krypt.toml")]
    config: PathBuf,
}

#[derive(clap::Args, Debug)]
struct SetupArgs {
    /// Path to `.krypt.toml`. Defaults to `.krypt.toml` in CWD; falls back to
    /// the repo path from the tool config if present.
    #[arg(long, default_value = ".krypt.toml")]
    config: PathBuf,

    /// Run only the comma-separated list of `[prompts.<name>]` sections.
    /// If omitted, all sections are run in BTreeMap order.
    #[arg(long, value_delimiter = ',')]
    prompts: Option<Vec<String>>,

    /// Non-interactive: pre-fill every field with its computed default.
    /// Exits with an error if a required field has no default.
    #[arg(long)]
    yes: bool,

    /// Parse and collect values but do not write any destination files.
    #[arg(long)]
    dry_run: bool,
}

#[derive(clap::Args, Debug)]
struct DeployArgs {
    /// Path to `.krypt.toml`. Defaults to `.krypt.toml` in the cwd.
    #[arg(long, default_value = ".krypt.toml")]
    config: PathBuf,

    /// Manifest path. Defaults to `${XDG_STATE}/krypt/manifest.json`.
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Override the detected platform (testing escape hatch).
    #[arg(long, value_enum)]
    platform: Option<PlatformArg>,

    /// Don't touch disk; print what would happen.
    #[arg(long)]
    dry_run: bool,

    /// On `link`: overwrite real conflicts. On `relink`: overwrite + force-remove
    /// drifted destinations during the unlink half.
    #[arg(long)]
    force: bool,
}

fn main() -> Result<ExitCode> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Command::Version) | None => cmd_version(),
        Some(Command::Validate { path }) => cmd_validate(path),
        Some(Command::Paths { config, no_config }) => cmd_paths(config, no_config),
        Some(Command::Diff { manifest }) => cmd_diff(manifest),
        Some(Command::Link(args)) => cmd_link(args),
        Some(Command::Unlink {
            manifest,
            dry_run,
            force,
        }) => cmd_unlink(manifest, dry_run, force),
        Some(Command::Relink(args)) => cmd_relink(args),
        Some(Command::Init(args)) => cmd_init(args),
        Some(Command::Update(args)) => cmd_update(args),
        Some(Command::Adopt(args)) => cmd_adopt(args),
        Some(Command::AdoptEdits(args)) => cmd_adopt_edits(args),
        Some(Command::Setup(args)) => cmd_setup(args),
        Some(Command::Doctor(args)) => cmd_doctor(args),
        Some(Command::Deps(args)) => cmd_deps(args),
        Some(Command::Notify(args)) => cmd_notify(args),
    }
}

fn cmd_version() -> Result<ExitCode> {
    println!("krypt {}", env!("CARGO_PKG_VERSION"));
    println!("  core:     {}", krypt_core::VERSION);
    println!("  pkg:      {}", krypt_pkg::VERSION);
    println!("  platform: {}", krypt_platform::VERSION);
    Ok(ExitCode::SUCCESS)
}

fn cmd_setup(args: SetupArgs) -> Result<ExitCode> {
    // Resolve config path: CLI arg first, then tool config repo, then CWD default.
    let config_path = if args.config.exists() {
        args.config.clone()
    } else {
        let tc_path = ToolConfig::default_path()
            .map_err(|e| color_eyre::eyre::eyre!("resolving tool config path: {e}"))?;
        if let Ok(Some(tc)) = ToolConfig::load(&tc_path) {
            let repo_cfg = tc.repo.path.join(".krypt.toml");
            if repo_cfg.exists() {
                repo_cfg
            } else {
                args.config.clone()
            }
        } else {
            args.config.clone()
        }
    };

    let cfg = krypt_core::config::parse_file(&config_path)
        .map_err(|e| color_eyre::eyre::eyre!("loading config: {e}"))?;

    let sections = args.prompts.unwrap_or_default();

    let opts = SetupOpts {
        sections: sections.clone(),
        yes: args.yes,
        prompt_sections: cfg.prompts.clone(),
    };

    // Build per-section destination and source paths from [[template]] entries.
    // A template's `prompts` list names the sections that write to its `dst`.
    let mut dsts = std::collections::BTreeMap::new();
    let mut srcs = std::collections::BTreeMap::new();
    let resolver = Resolver::new();
    for tmpl in &cfg.templates {
        for section_name in &tmpl.prompts {
            let dst_str = resolver
                .resolve(&tmpl.dst)
                .unwrap_or_else(|_| tmpl.dst.clone());
            if !args.dry_run {
                dsts.insert(section_name.clone(), PathBuf::from(dst_str));
                srcs.insert(section_name.clone(), PathBuf::from(&tmpl.src));
            }
        }
    }

    let result = if args.yes {
        let mut p = YesPrompter;
        let git = RealGitConfig;
        krypt_core::setup::setup_with_destinations_and_srcs(&opts, &dsts, &srcs, &mut p, &git)
    } else {
        let mut p = RealPrompter;
        let git = RealGitConfig;
        krypt_core::setup::setup_with_destinations_and_srcs(&opts, &dsts, &srcs, &mut p, &git)
    };

    match result {
        Ok(report) => {
            let dry_label = if args.dry_run { " (dry-run)" } else { "" };
            println!("\nsetup complete{dry_label}:");
            println!("  sections: {}", report.sections_run.join(", "));
            println!("  fields:   {}", report.fields_collected);
            if !args.dry_run {
                for f in &report.files_written {
                    println!("  wrote:    {}", f.display());
                }
            }
            if !report.skipped_by_requires.is_empty() {
                println!(
                    "  skipped (requires):  {}",
                    report.skipped_by_requires.join(", ")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(SetupError::UnknownPromptSection(name)) => {
            eprintln!("error: unknown prompt section {name:?}");
            Ok(ExitCode::from(2))
        }
        Err(SetupError::UnknownWriter(w)) => {
            eprintln!("error: unknown writer {w:?}");
            Ok(ExitCode::from(2))
        }
        Err(SetupError::RequiredFieldHasNoDefault { key }) => {
            eprintln!(
                "error: required field {key:?} has no default; cannot run unattended (--yes)"
            );
            Ok(ExitCode::from(2))
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(ExitCode::from(1))
        }
    }
}

fn cmd_validate(path: PathBuf) -> Result<ExitCode> {
    match krypt_core::config::parse_file(&path) {
        Ok(_) => {
            println!("✓ {} parsed and validated successfully", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("✗ {e}");
            Ok(ExitCode::from(2))
        }
    }
}

fn cmd_paths(config: PathBuf, no_config: bool) -> Result<ExitCode> {
    let mut resolver = Resolver::new();

    if !no_config && config.exists() {
        match krypt_core::config::parse_file(&config) {
            Ok(cfg) => {
                resolver = resolver.with_overrides(cfg.paths.into_iter().collect());
                println!("# Overrides loaded from: {}", config.display());
            }
            Err(e) => {
                eprintln!("warning: ignoring {} ({e})", config.display());
            }
        }
    }

    let names = resolver.known_vars();
    let width = names.iter().map(|n| n.len()).max().unwrap_or(0);
    let mut had_error = false;

    for name in names {
        match resolver.resolve_var(&name) {
            Ok(v) => println!("{name:width$}  {v}"),
            Err(e) => {
                eprintln!("{name:width$}  <error: {e}>");
                had_error = true;
            }
        }
    }

    Ok(if had_error {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_diff(manifest_path: Option<PathBuf>) -> Result<ExitCode> {
    let path = match manifest_path {
        Some(p) => p,
        None => default_manifest_path()?,
    };

    let Some(manifest) = Manifest::load(&path).map_err(color_eyre::eyre::Report::msg)? else {
        eprintln!("no manifest at {} — nothing deployed yet", path.display());
        return Ok(ExitCode::SUCCESS);
    };

    let drift = detect_drift(&manifest);
    let mut dirty = 0usize;
    let width = drift
        .iter()
        .map(|d| d.dst.to_string_lossy().len())
        .max()
        .unwrap_or(0);

    for d in &drift {
        let dst = d.dst.to_string_lossy();
        match d.status {
            DriftStatus::Clean => println!("clean    {dst:width$}"),
            DriftStatus::Drifted => {
                println!("drifted  {dst:width$}");
                dirty += 1;
            }
            DriftStatus::DstMissing => {
                println!("missing  {dst:width$}");
                dirty += 1;
            }
        }
    }

    if dirty == 0 {
        println!("\n{} entries, all clean", drift.len());
        Ok(ExitCode::SUCCESS)
    } else {
        println!("\n{}/{} entries dirty", dirty, drift.len());
        Ok(ExitCode::from(1))
    }
}

fn default_manifest_path() -> Result<PathBuf> {
    let r = Resolver::new();
    let state = r
        .resolve_var("XDG_STATE")
        .map_err(|e| color_eyre::eyre::eyre!("resolving XDG_STATE: {e}"))?;
    Ok(PathBuf::from(state).join("krypt").join("manifest.json"))
}

fn default_repo_path() -> Result<PathBuf> {
    let r = Resolver::new();
    let cfg = r
        .resolve_var("XDG_CONFIG")
        .map_err(|e| color_eyre::eyre::eyre!("resolving XDG_CONFIG: {e}"))?;
    Ok(PathBuf::from(cfg).join("krypt").join("repo"))
}

fn cmd_init(args: InitArgs) -> Result<ExitCode> {
    let url = args.from.or(args.url);

    let repo_path = match args.repo_path {
        Some(p) => p,
        None => default_repo_path()?,
    };

    let tool_config_path = ToolConfig::default_path()
        .map_err(|e| color_eyre::eyre::eyre!("resolving tool config path: {e}"))?;

    let opts = InitOpts {
        url,
        repo_path,
        tool_config_path,
        bare: args.bare,
        force: args.force,
    };

    match init(&opts) {
        Ok(report) => {
            println!("initialized repo at {}", report.repo_path.display());
            println!(
                "tool config written to {}",
                report.tool_config_path.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(InitError::MissingUrl) => {
            eprintln!("error: must provide URL or --bare");
            Ok(ExitCode::from(2))
        }
        Err(InitError::RepoExists { path }) => {
            eprintln!(
                "error: {} already exists (use --force to overwrite)",
                path.display()
            );
            Ok(ExitCode::from(1))
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(ExitCode::from(1))
        }
    }
}

fn cmd_update(args: UpdateArgs) -> Result<ExitCode> {
    let tool_config_path = ToolConfig::default_path()
        .map_err(|e| color_eyre::eyre::eyre!("resolving tool config path: {e}"))?;

    let manifest_path = default_manifest_path()?;

    let opts = UpdateOpts {
        tool_config_path,
        config_path: args.config,
        manifest_path,
        dry_run: args.dry_run,
        skip_hooks: args.skip_hooks,
        force: args.force,
    };

    match update(&opts) {
        Ok(report) => {
            if let Some(warn) = &report.version_warning {
                eprintln!("{warn}");
            }
            if report.hooks_skipped > 0 {
                eprintln!(
                    "warning: {n} post-update hook(s) configured but hook execution not yet \
                     implemented — see #43",
                    n = report.hooks_skipped,
                );
            }
            println!("pull:  {}", if report.pulled { "ok" } else { "up to date" });
            println!("link:");
            print_link_report(&report.link, opts.dry_run);
            Ok(if report.link.conflicts_skipped > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        Err(UpdateError::ToolConfigMissing { .. }) => {
            eprintln!(
                "error: {}",
                UpdateError::ToolConfigMissing {
                    path: opts.tool_config_path
                }
            );
            eprintln!("hint:  run `krypt init` to set up your dotfiles repo first");
            Ok(ExitCode::from(2))
        }
        Err(UpdateError::DirtyWorkingTree) => {
            eprintln!("error: {}", UpdateError::DirtyWorkingTree);
            Ok(ExitCode::from(1))
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(ExitCode::from(1))
        }
    }
}

fn deploy_opts_from(args: &DeployArgs) -> Result<DeployOpts> {
    let manifest_path = match &args.manifest {
        Some(p) => p.clone(),
        None => default_manifest_path()?,
    };
    Ok(DeployOpts {
        config_path: args.config.clone(),
        manifest_path,
        platform: args.platform.map(Into::into),
        dry_run: args.dry_run,
        force: args.force,
    })
}

fn cmd_link(args: DeployArgs) -> Result<ExitCode> {
    let opts = deploy_opts_from(&args)?;
    let r = link(&opts).map_err(color_eyre::eyre::Report::msg)?;
    print_link_report(&r, opts.dry_run);
    Ok(if r.conflicts_skipped > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_unlink(manifest: Option<PathBuf>, dry_run: bool, force: bool) -> Result<ExitCode> {
    let manifest_path = match manifest {
        Some(p) => p,
        None => default_manifest_path()?,
    };
    let opts = DeployOpts {
        config_path: PathBuf::new(),
        manifest_path,
        platform: None,
        dry_run,
        force,
    };
    let r = unlink(&opts).map_err(color_eyre::eyre::Report::msg)?;
    print_unlink_report(&r, dry_run);
    Ok(if r.drift_skipped > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_relink(args: DeployArgs) -> Result<ExitCode> {
    let opts = deploy_opts_from(&args)?;
    let (u, l) = relink(&opts).map_err(color_eyre::eyre::Report::msg)?;
    println!("unlink:");
    print_unlink_report(&u, opts.dry_run);
    println!("link:");
    print_link_report(&l, opts.dry_run);
    Ok(if l.conflicts_skipped > 0 || u.drift_skipped > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn print_link_report(r: &LinkReport, dry_run: bool) {
    let verb = if dry_run { "would write" } else { "wrote" };
    println!("  {verb}: {}", r.written);
    if r.idempotent_rewrites > 0 {
        println!("  idempotent re-deploys: {}", r.idempotent_rewrites);
    }
    if r.conflicts_skipped > 0 {
        println!(
            "  conflicts skipped: {} (re-run with --force to overwrite)",
            r.conflicts_skipped
        );
    }
}

fn print_unlink_report(r: &UnlinkReport, dry_run: bool) {
    let verb = if dry_run { "would remove" } else { "removed" };
    println!("  {verb}: {}", r.removed);
    if r.already_missing > 0 {
        println!("  already missing: {}", r.already_missing);
    }
    if r.drift_skipped > 0 {
        println!(
            "  drifted (kept): {} (re-run with --force to delete)",
            r.drift_skipped
        );
    }
}

fn cmd_adopt(args: AdoptArgs) -> Result<ExitCode> {
    let repo_path = match args.repo_path {
        Some(p) => p,
        None => default_repo_path()?,
    };
    let manifest_path = match args.manifest {
        Some(p) => p,
        None => default_manifest_path()?,
    };

    let opts = AdoptOpts {
        dst: args.dst,
        src_override: args.src,
        repo_path,
        manifest_path,
        force: args.force,
        dry_run: args.dry_run,
        resolver: Resolver::new(),
    };

    match adopt(&opts) {
        Ok(report) => {
            println!("{}", report.link_suggestion);
            if args.dry_run {
                println!("\n(dry-run: no files written)");
            } else {
                println!("\nadopted: {:?} -> repo:{:?}", report.dst, report.src);
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(AdoptError::DstMissing(p)) => {
            eprintln!("error: dst does not exist: {}", p.display());
            Ok(ExitCode::from(1))
        }
        Err(AdoptError::OutsideHome { dst }) => {
            eprintln!(
                "error: {} is outside $HOME; provide --src <rel> to name the repo-relative path",
                dst.display()
            );
            Ok(ExitCode::from(2))
        }
        Err(AdoptError::RepoCollision { src }) => {
            eprintln!(
                "error: repo already has {}; use --force to overwrite, or --src to pick a different name",
                src.display()
            );
            Ok(ExitCode::from(1))
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(ExitCode::from(1))
        }
    }
}

fn cmd_adopt_edits(args: AdoptEditsArgs) -> Result<ExitCode> {
    let repo_path = match args.repo_path {
        Some(p) => p,
        None => default_repo_path()?,
    };
    let manifest_path = match args.manifest {
        Some(p) => p,
        None => default_manifest_path()?,
    };

    let opts = AdoptEditsOpts {
        manifest_path,
        repo_path,
        dry_run: args.dry_run,
    };

    match adopt_edits(&opts) {
        Ok(report) => {
            println!(
                "adopted edits for {} entries ({} clean, {} missing)",
                report.adopted, report.clean, report.missing
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(ExitCode::from(1))
        }
    }
}

fn cmd_deps(args: DepsArgs) -> Result<ExitCode> {
    let config = krypt_core::config::parse_file(&args.config)
        .map_err(|e| color_eyre::eyre::eyre!("loading config: {e}"))?;

    let current_platform = Platform::current().as_str();
    let groups: Vec<krypt_pkg::deps::DepGroup> = config
        .deps
        .into_iter()
        .filter(|g| {
            let rp = &g.required_platforms;
            rp.is_empty() || rp.iter().any(|p| p == "all" || p == current_platform)
        })
        .map(|g| krypt_pkg::deps::DepGroup {
            group: g.group,
            pacman: g.pacman,
            apt: g.apt,
            dnf: g.dnf,
            brew: g.brew,
            scoop: g.scoop,
            winget: g.winget,
        })
        .collect();

    let opts = DepsOpts {
        groups,
        manager: args.manager,
        group_filter: args.group,
        dry_run: args.dry_run,
    };
    let runner = RealRunner;

    let report = match install_deps(&opts, &runner) {
        Ok(r) => r,
        Err(DepsError::NoManagerDetected) => {
            eprintln!("error: no package manager detected; install one or use --manager <name>");
            return Ok(ExitCode::from(2));
        }
        Err(DepsError::UnknownManager(name)) => {
            eprintln!("error: unknown package manager '{name}'");
            return Ok(ExitCode::from(2));
        }
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(ExitCode::from(1));
        }
    };

    let dry_label = if args.dry_run { " (dry-run)" } else { "" };
    println!("manager: {}{}", report.manager_used, dry_label);

    if !report.already_installed.is_empty() {
        println!("already installed: {}", report.already_installed.join(", "));
    }
    if !report.installed.is_empty() {
        let verb = if args.dry_run {
            "would install"
        } else {
            "installed"
        };
        println!("{}: {}", verb, report.installed.join(", "));
    }
    if !report.skipped_unavailable.is_empty() {
        println!(
            "skipped (no packages for this manager): {}",
            report.skipped_unavailable.join(", ")
        );
    }
    if !report.failed.is_empty() {
        for (pkg, err) in &report.failed {
            eprintln!("failed: {pkg}: {err}");
        }
        return Ok(ExitCode::from(1));
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_doctor(args: DoctorArgs) -> Result<ExitCode> {
    let tool_config_path = match args.tool_config {
        Some(p) => p,
        None => ToolConfig::default_path()
            .map_err(|e| color_eyre::eyre::eyre!("resolving tool config path: {e}"))?,
    };
    let manifest_path = match args.manifest {
        Some(p) => p,
        None => default_manifest_path()?,
    };

    let detected_manager = krypt_pkg::detect::pick_default().map(|m| m.name().to_owned());

    let opts = DoctorOpts {
        tool_config_path,
        config_path: args.config,
        manifest_path,
        repo_path: args.repo_path,
        detected_manager,
    };

    let report = doctor(&opts);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|e| color_eyre::eyre::eyre!("serializing report: {e}"))?
        );
    } else {
        println!("{}", report.render_text());
    }

    Ok(if report.is_all_green() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn cmd_notify(args: NotifyArgs) -> Result<ExitCode> {
    // Precedence: --backend flag > [meta] notify_backend > auto-detect.
    let override_name: Option<String> = if args.backend.is_some() {
        args.backend.clone()
    } else if args.config.exists() {
        krypt_core::config::parse_file(&args.config)
            .ok()
            .and_then(|c| c.meta.notify_backend)
    } else {
        None
    };

    let backend: NotifyBackend = detect(override_name.as_deref());
    let notifier = AutoNotifier::with_backend(backend);

    match notifier.notify(&args.title, &args.body) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("error: {e}");
            Ok(ExitCode::from(1))
        }
    }
}
