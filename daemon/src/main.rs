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
mod protocol;
mod runner;
mod scope;
mod server;
mod state;
mod watcher;

use crate::error::Result;
use crate::server::{LOG_RELOAD_HANDLE, ensure_owner_only_dir, restrict_to_owner};
use clap::Parser;
use cli::{Cli, Commands};
use std::env;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio, exit};
use std::thread;
use std::time::Duration;

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
            let git_hash = option_env!("SHBOT_GIT_HASH").unwrap_or("unknown");
            info!("Starting shbot daemon (git hash: {})", git_hash);
            handle_daemon()?;
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
    let socket_path = get_socket_path()?;
    if !socket_path.exists() {
        println!("Daemon is not running (socket empty).");
        return Ok(());
    }

    let mut stream = StdUnixStream::connect(&socket_path)?;
    stream.write_all(b"status\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    println!("{}", response.trim());
    Ok(())
}

fn handle_reload() -> Result<()> {
    let socket_path = get_socket_path()?;
    if !socket_path.exists() {
        println!("Daemon is not running (socket empty).");
        return Ok(());
    }

    let mut stream = StdUnixStream::connect(&socket_path)?;
    stream.write_all(b"reload\n")?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    println!("{}", response.trim());
    Ok(())
}

fn handle_loglevel(level: &str) -> Result<()> {
    let socket_path = get_socket_path()?;
    if !socket_path.exists() {
        println!("Daemon is not running (socket empty).");
        return Ok(());
    }

    let mut stream = StdUnixStream::connect(&socket_path)?;
    stream.write_all(format!("loglevel {}\n", level).as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    println!("{}", response.trim());
    Ok(())
}

fn handle_daemon() -> Result<()> {
    // Load config
    let config = config::Config::load()?;
    let socket_path = get_socket_path()?;

    // Spin up the async tokio runtime *after* any potential forks
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        info!("Async runtime initialized!");
        let state = state::DaemonState::new();

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
            if let Ok(mut sig_term) = signal(SignalKind::terminate())
                && let Some(_) = sig_term.recv().await
            {
                info!("Received SIGTERM, shutting down cleanly to flush coverage");
                exit(0);
            }
        });

        runner::start_timeout_tasks(state.clone(), config_rx.clone());

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
            None,
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
            ensure_owner_only_dir(p)?;
        }
        let file = File::create(path)?;
        restrict_to_owner(path)?;
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(format_layer.with_ansi(false).with_writer(file))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(format_layer.with_ansi(io::stdout().is_terminal()))
            .init();
    }

    let _ = LOG_RELOAD_HANDLE.set(reload_handle);

    Ok(())
}

fn handle_connect() -> Result<()> {
    let socket_path = get_socket_path()?;

    // Quick check if socket is already responsive
    if socket_path.exists() && StdUnixStream::connect(&socket_path).is_ok() {
        return Ok(());
    }

    let state_dir = socket_path.parent().unwrap().to_path_buf();
    ensure_owner_only_dir(&state_dir)?;

    // Clean up stale socket before binding
    if socket_path.exists() {
        fs::remove_file(&socket_path)?;
    }

    let hostname = get_hostname();
    let log_file = state_dir.join(format!("{}.log", hostname));

    let stdout = fs::File::create(&log_file)?;
    restrict_to_owner(&log_file)?;
    let stderr = stdout.try_clone()?;

    StdCommand::new(env::current_exe()?)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .current_dir(&state_dir)
        .spawn()?;

    // Wait briefly for the daemon to start and bind the socket
    for _ in 0..50 {
        if socket_path.exists() && StdUnixStream::connect(&socket_path).is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    Ok(())
}

fn get_hostname() -> String {
    env::var("HOST")
        .or_else(|_| env::var("HOSTNAME"))
        .unwrap_or_else(|_| {
            fs::read_to_string("/proc/sys/kernel/hostname")
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "localhost".to_string())
        })
}

// Socket path convention: ${SHBOT_SOCKET:-${XDG_STATE_HOME:-$HOME/.local/state}/shbot/$HOST.socket}
pub fn get_socket_path() -> Result<PathBuf> {
    if let Ok(sock) = env::var("SHBOT_SOCKET")
        && !sock.is_empty()
    {
        return Ok(PathBuf::from(sock));
    }

    let state_home = env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = env::var("HOME").expect("HOME env var not set");
            PathBuf::from(home).join(".local").join("state")
        });
    let host = get_hostname();

    Ok(state_home.join("shbot").join(format!("{}.socket", host)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener as StdUnixListener;

    #[test]
    fn test_get_hostname_from_env() {
        temp_env::with_vars([("HOST", Some("testhost-1")), ("HOSTNAME", None)], || {
            assert_eq!(get_hostname(), "testhost-1");
        });

        temp_env::with_vars([("HOST", None), ("HOSTNAME", Some("testhost-2"))], || {
            assert_eq!(get_hostname(), "testhost-2");
        });
    }

    #[test]
    fn test_get_socket_path() {
        temp_env::with_vars(
            [
                ("HOME", Some("/tmp/testhome")),
                ("HOST", Some("testhost")),
                ("SHBOT_SOCKET", None),
                ("XDG_STATE_HOME", None),
            ],
            || {
                let path = get_socket_path().unwrap();
                assert_eq!(
                    path,
                    PathBuf::from("/tmp/testhome/.local/state/shbot/testhost.socket")
                );
            },
        );

        temp_env::with_vars(
            [
                ("HOME", Some("/tmp/testhome")),
                ("HOST", Some("testhost")),
                ("SHBOT_SOCKET", None),
                ("XDG_STATE_HOME", Some("/tmp/custom_state")),
            ],
            || {
                let path = get_socket_path().unwrap();
                assert_eq!(
                    path,
                    PathBuf::from("/tmp/custom_state/shbot/testhost.socket")
                );
            },
        );

        temp_env::with_vars([("SHBOT_SOCKET", Some("/tmp/explicit.socket"))], || {
            let path = get_socket_path().unwrap();
            assert_eq!(path, PathBuf::from("/tmp/explicit.socket"));
        });
    }

    #[test]
    fn test_handle_commands() {
        let dir = tempfile::tempdir().unwrap();
        temp_env::with_vars(
            [
                ("HOME", Some(dir.path().to_str().unwrap())),
                ("HOST", Some("testhost")),
                ("SHBOT_SOCKET", None),
                ("XDG_STATE_HOME", None),
            ],
            || {
                let socket_path = get_socket_path().unwrap();
                fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

                // If daemon is not running (socket empty)
                assert!(handle_status().is_ok());
                assert!(handle_reload().is_ok());
                assert!(handle_loglevel("debug").is_ok());

                // Now mock the daemon
                let listener = StdUnixListener::bind(&socket_path).unwrap();

                thread::spawn(move || {
                    for mut stream in listener.incoming().flatten() {
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
                });

                assert!(handle_status().is_ok());
                assert!(handle_reload().is_ok());
                assert!(handle_loglevel("debug").is_ok());
            },
        );
    }
}
