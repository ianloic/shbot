// Copyright (C) 2026 Ian McKellar
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

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
