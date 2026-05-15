//! `krypt` — cross-platform dotfiles manager.
//!
//! This is the CLI entrypoint. Real logic lives in `krypt-core`. This crate
//! is intentionally thin: clap wiring + delegation.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::Result;
use krypt_core::deploy::{DeployOpts, LinkReport, UnlinkReport, link, relink, unlink};
use krypt_core::init::{InitError, InitOpts, init};
use krypt_core::manifest::{DriftStatus, Manifest, detect_drift};
use krypt_core::paths::{Platform, Resolver};
use krypt_core::tool_config::ToolConfig;
use krypt_core::update::{UpdateError, UpdateOpts, update};

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
    }
}

fn cmd_version() -> Result<ExitCode> {
    println!("krypt {}", env!("CARGO_PKG_VERSION"));
    println!("  core:     {}", krypt_core::VERSION);
    println!("  pkg:      {}", krypt_pkg::VERSION);
    println!("  platform: {}", krypt_platform::VERSION);
    Ok(ExitCode::SUCCESS)
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
