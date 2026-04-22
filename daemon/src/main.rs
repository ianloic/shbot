mod cli;
mod config;
mod git;
mod server;
mod state;
mod watcher;

use clap::Parser;
use cli::{Cli, Commands};
use daemonize::Daemonize;
use std::fs::File;
use std::path::{Path, PathBuf};
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

fn main() -> anyhow::Result<()> {
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
            // handle_status().await?;
            println!("Status stub...");
        }
    }

    Ok(())
}

fn handle_daemon() -> anyhow::Result<()> {
    // Spin up the async tokio runtime *after* any potential forks
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    
    rt.block_on(async {
        println!("Async runtime initialized!");
        // TODO: Start async socket server and tasks here
    });
    
    Ok(())
}

fn setup_logging(log_file: Option<&Path>) -> anyhow::Result<()> {
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

fn handle_connect() -> anyhow::Result<()> {
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
pub fn get_socket_path() -> anyhow::Result<PathBuf> {
    let base_dirs = directories::ProjectDirs::from("", "", "shbot")
        .ok_or_else(|| anyhow::anyhow!("Could not find valid home directory"))?;
    
    let state_dir = base_dirs.data_local_dir(); // Usually ~/.local/share/shbot
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
