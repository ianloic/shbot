use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about = "shbot - Async Zsh state daemon", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Optional log file to redirect output (instead of stdout)
    #[arg(long, global = true)]
    pub log_file: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Bootstraps the daemon (spawns in background if not running)
    Connect,
    /// Runs the long-lived daemon server
    Daemon,
    /// Requests the current internal state from the running daemon
    Status,
    /// Reloads the configuration file dynamically
    Reload,
    /// Sets the daemon's runtime log level
    Loglevel { level: String },
}
