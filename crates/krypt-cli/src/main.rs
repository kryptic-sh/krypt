//! `krypt` — cross-platform dotfiles manager.
//!
//! This is the CLI entrypoint. Real logic lives in `krypt-core`. This crate
//! is intentionally thin: clap wiring + delegation.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use color_eyre::Result;
use krypt_core::manifest::{DriftStatus, Manifest, detect_drift};
use krypt_core::paths::Resolver;

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
