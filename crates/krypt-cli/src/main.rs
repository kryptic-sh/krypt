//! `krypt` — cross-platform dotfiles manager.
//!
//! This is the CLI entrypoint. Real logic lives in `krypt-core`. This crate
//! is intentionally thin: clap wiring + delegation.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use color_eyre::Result;

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
        Some(Command::Version) | None => {
            println!("krypt {}", env!("CARGO_PKG_VERSION"));
            println!("  core:     {}", krypt_core::VERSION);
            println!("  pkg:      {}", krypt_pkg::VERSION);
            println!("  platform: {}", krypt_platform::VERSION);
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Validate { path }) => match krypt_core::config::parse_file(&path) {
            Ok(_) => {
                println!("✓ {} parsed and validated successfully", path.display());
                Ok(ExitCode::SUCCESS)
            }
            Err(e) => {
                eprintln!("✗ {e}");
                Ok(ExitCode::from(2))
            }
        },
    }
}
