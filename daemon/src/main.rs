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

mod cli;
mod config;
mod error;
mod git;
mod runner;
mod server;
mod state;
mod watcher;

use crate::error::Result;
use clap::Parser;
use cli::{Cli, Commands};
use daemonize::Daemonize;
use std::fs::File;
use std::path::{Path, PathBuf};

use tracing::{error, info};

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Setup logging
    setup_logging(cli.log_file.as_deref())?;

    match cli.command {
        Commands::Connect => {
            handle_connect()?;
        }
        Commands::Daemon => {
            info!("Starting shbot daemon");
            handle_daemon(None)?;
        }
        Commands::Status => {
            if let Err(e) = handle_status() {
                eprintln!("Failed to get status: {}", e);
            }
        }
        Commands::Reload => {
            if let Err(e) = handle_reload() {
                eprintln!("Failed to reload: {}", e);
            }
        }
        Commands::Loglevel { level } => {
            if let Err(e) = handle_loglevel(&level) {
                eprintln!("Failed to set log level: {}", e);
            }
        }
    }

    Ok(())
}

fn handle_status() -> Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let socket_path = get_socket_path()?;
    if !socket_path.exists() {
        println!("Daemon is not running (socket empty).");
        return Ok(());
    }

    let mut stream = UnixStream::connect(&socket_path)?;
    stream.write_all(b"status\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    println!("{}", response.trim());
    Ok(())
}

fn handle_reload() -> Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let socket_path = get_socket_path()?;
    if !socket_path.exists() {
        println!("Daemon is not running (socket empty).");
        return Ok(());
    }

    let mut stream = UnixStream::connect(&socket_path)?;
    stream.write_all(b"reload\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    println!("{}", response.trim());
    Ok(())
}

fn handle_loglevel(level: &str) -> Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    let socket_path = get_socket_path()?;
    if !socket_path.exists() {
        println!("Daemon is not running (socket empty).");
        return Ok(());
    }

    let mut stream = UnixStream::connect(&socket_path)?;
    stream.write_all(format!("loglevel {}\n", level).as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    println!("{}", response.trim());
    Ok(())
}

fn handle_daemon(pre_bound_listener: Option<std::os::unix::net::UnixListener>) -> Result<()> {
    // Load config
    let config = config::Config::load()?;
    let socket_path = get_socket_path()?;

    // Spin up the async tokio runtime *after* any potential forks
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        info!("Async runtime initialized!");
        let state = state::AppState::new();

        let (config_tx, config_rx) = tokio::sync::watch::channel(config);

        let sighup_tx = config_tx.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            if let Ok(mut sig) = signal(SignalKind::hangup()) {
                while let Some(_) = sig.recv().await {
                    info!("Received SIGHUP, reloading configuration");
                    if let Ok(new_config) = config::Config::load() {
                        if let Err(e) = sighup_tx.send(new_config) {
                            error!("Failed to broadcast config on SIGHUP: {}", e);
                        }
                    } else {
                        error!("Failed to parse shbot.toml config on SIGHUP");
                    }
                }
            } else {
                error!("Failed to register SIGHUP signal handler");
            }
        });

        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            if let Ok(mut sig_term) = signal(SignalKind::terminate()) {
                if let Some(_) = sig_term.recv().await {
                    info!("Received SIGTERM, shutting down cleanly to flush coverage");
                    std::process::exit(0);
                }
            }
        });

        runner::start_periodic_triggers(state.clone(), config_rx.clone());

        let watcher = watcher::SyncWatcher::new();
        if let Err(e) = watcher.start(state.clone(), config_rx.clone()) {
            error!("Failed to start SyncWatcher: {}", e);
        }

        if let Err(e) = server::run_server(
            state.clone(),
            config_rx.clone(),
            config_tx.clone(),
            watcher.clone(),
            socket_path,
            pre_bound_listener,
        )
        .await
        {
            error!("Server loop exited: {}", e);
        }
    });

    Ok(())
}

fn setup_logging(log_file: Option<&Path>) -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt, reload};

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter_layer, reload_handle) = reload::Layer::new(env_filter);

    let format_layer = fmt::layer();
    if let Some(path) = log_file {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let file = File::create(path)?;
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(format_layer.with_writer(file))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(format_layer)
            .init();
    }

    // Store reload handle for dynamic updates
    let _ = crate::server::LOG_RELOAD_HANDLE.set(reload_handle);

    Ok(())
}

fn handle_connect() -> Result<()> {
    let socket_path = get_socket_path()?;

    // Quick check if socket is already responsive
    if socket_path.exists() && std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
        return Ok(());
    }

    let state_dir = socket_path.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&state_dir)?;

    // Clean up stale socket before binding
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    // Bind listener BEFORE forking. Child will inherit this working listener.
    let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;

    let hostname = get_hostname();
    let pid_file = state_dir.join(format!("{}.pid", hostname));
    let log_file = state_dir.join(format!("{}.log", hostname));

    let stdout = std::fs::File::create(&log_file)?;
    let stderr = std::fs::File::create(&log_file)?;

    let daemonize = Daemonize::new()
        .pid_file(&pid_file)
        .working_directory(&state_dir)
        .stdout(stdout)
        .stderr(stderr);

    match daemonize.start() {
        Ok(_) => {
            // We are now in the child (daemonized) process!
            // Execute handle_daemon pushing the inherited listener.
            if let Err(e) = handle_daemon(Some(listener)) {
                eprintln!("Daemon crashed: {}", e);
                std::process::exit(1);
            }
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("Failed to run daemonize: {}", e);
            std::process::exit(1);
        }
    }
}

fn get_hostname() -> String {
    std::env::var("HOST")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "localhost".to_string())
        })
}

// Socket path convention: ~/.local/state/shbot/socket/$HOST.socket
pub fn get_socket_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").expect("HOME env var not set");
    let host = get_hostname();

    let path = PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("shbot")
        .join(format!("{}.socket", host));

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_get_hostname_from_env() {
        unsafe {
            std::env::set_var("HOST", "testhost-1");
            std::env::remove_var("HOSTNAME");
        }
        assert_eq!(get_hostname(), "testhost-1");

        unsafe {
            std::env::remove_var("HOST");
            std::env::set_var("HOSTNAME", "testhost-2");
        }
        assert_eq!(get_hostname(), "testhost-2");
    }

    #[test]
    fn test_get_socket_path() {
        unsafe {
            std::env::set_var("HOME", "/tmp/testhome");
            std::env::set_var("HOST", "testhost");
        }

        let path = get_socket_path().unwrap();
        assert_eq!(
            path,
            PathBuf::from("/tmp/testhome/.local/state/shbot/testhost.socket")
        );
    }

    #[test]
    fn test_handle_commands() {
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", dir.path().to_str().unwrap());
            std::env::set_var("HOST", "testhost");
        }

        let socket_path = get_socket_path().unwrap();
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        // If daemon is not running (socket empty)
        assert!(handle_status().is_ok());
        assert!(handle_reload().is_ok());
        assert!(handle_loglevel("debug").is_ok());

        // Now mock the daemon
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if let Ok(mut stream) = stream {
                    use std::io::{Read, Write};
                    let mut buf = [0; 1024];
                    if let Ok(n) = stream.read(&mut buf) {
                        let req = String::from_utf8_lossy(&buf[..n]);
                        if req.trim() == "status" {
                            stream.write_all(b"status ok\n").unwrap();
                        } else if req.trim() == "reload" {
                            stream.write_all(b"reload ok\n").unwrap();
                        } else if req.starts_with("loglevel") {
                            stream.write_all(b"loglevel ok\n").unwrap();
                        }
                    }
                }
            }
        });

        assert!(handle_status().is_ok());
        assert!(handle_reload().is_ok());
        assert!(handle_loglevel("debug").is_ok());
    }
}
