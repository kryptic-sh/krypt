//! `files` — cross-platform dotfiles manager.
//!
//! This is the CLI entrypoint. Real logic lives in `files-core`. This crate
//! is intentionally thin: clap wiring + delegation.

use clap::{Parser, Subcommand};
use color_eyre::Result;

#[derive(Parser, Debug)]
#[command(
    name    = "files",
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
}

fn main() -> Result<()> {
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
            println!("files {}", env!("CARGO_PKG_VERSION"));
            println!("  core:     {}", files_core::VERSION);
            println!("  pkg:      {}", files_pkg::VERSION);
            println!("  platform: {}", files_platform::VERSION);
        }
    }

    Ok(())
}
