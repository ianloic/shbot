mod cli;
mod config;
mod error;
mod git;
mod runner;
mod server;
mod state;
mod watcher;

use crate::error::{Result, ShbotError};
use clap::Parser;
use cli::{Cli, Commands};
use daemonize::Daemonize;
use std::fs::File;
use std::path::{Path, PathBuf};

use tracing::{Level, error, info};
use tracing_subscriber::FmtSubscriber;

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
        )
        .await
        {
            error!("Server loop exited: {}", e);
        }
    });

    Ok(())
}

fn setup_logging(log_file: Option<&Path>) -> Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env());

    if let Some(path) = log_file {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let file = File::create(path)?;
        let subscriber = subscriber.with_writer(file).finish();
        tracing::subscriber::set_global_default(subscriber)?;
    } else {
        let subscriber = subscriber.finish();
        tracing::subscriber::set_global_default(subscriber)?;
    }

    Ok(())
}

fn handle_connect() -> Result<()> {
    let state_dir = get_socket_path()?.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&state_dir)?;

    let pid_file = state_dir.join("shbot.pid");
    let log_file = state_dir.join("shbot.log");

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
            if let Err(e) = handle_daemon() {
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

// Socket path convention: ~/.local/state/shbot/socket/$HOST.socket
pub fn get_socket_path() -> Result<PathBuf> {
    let base_dirs = directories::ProjectDirs::from("", "", "shbot")
        .ok_or_else(|| ShbotError::Custom("Could not find valid home directory".into()))?;

    let _state_dir = base_dirs.data_local_dir(); // Usually ~/.local/share/shbot
    // But spec explicitly says ~/.local/state/shbot/socket/${HOSTNAME}.socket
    // We can use std::env::var("HOME").unwrap() + "/.local/state/shbot/socket/..."

    let home = std::env::var("HOME").expect("HOME env var not set");
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".to_string());

    let path = PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("shbot")
        .join("socket")
        .join(format!("{}.socket", host));

    Ok(path)
}
