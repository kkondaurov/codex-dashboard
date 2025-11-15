use clap::Parser;
use std::path::PathBuf;

/// Command-line interface for configuring the proxy binary.
#[derive(Debug, Parser)]
#[command(author, version, about = "Local Codex usage proxy and TUI", long_about = None)]
pub struct Cli {
    /// Path to a TOML configuration file (defaults to ./codex-usage.toml if present).
    #[arg(long, value_name = "FILE")]
    pub config_path: Option<PathBuf>,
}
