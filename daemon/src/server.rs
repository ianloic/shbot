use crate::state::{AppState, Pid};
use anyhow::{Context, Result};
use listenfd::ListenFd;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info, warn};

pub async fn run_server(state: Arc<AppState>, socket_path: PathBuf) -> Result<()> {
    let mut listenfd = ListenFd::from_env();
    
    // Check if systemd passed us a listener
    let listener = if let Some(std_listener) = listenfd.take_unix_listener(0)? {
        info!("Using systemd socket activation");
        std_listener.set_nonblocking(true)?;
        UnixListener::from_std(std_listener)?
    } else {
        // Unbind previous socket if it exists
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)?;
        } else if let Some(p) = socket_path.parent() {
            std::fs::create_dir_all(p)?;
        }
        
        info!("Binding to socket: {:?}", socket_path);
        UnixListener::bind(&socket_path)?
    };

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state_clone = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, state_clone).await {
                        error!("Connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("Accept failed: {}", e);
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, state: Arc<AppState>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(100);
    
    // We don't know the PID until the client says `hello <pid>`
    let mut client_pid: Option<Pid> = None;

    // Spawn a writer task for this connection
    let writer_task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Some(msg) = rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break; // EOF
        }

        let line_trimmed = line.trim_end();
        if line_trimmed.is_empty() {
            continue;
        }

        let mut parts = line_trimmed.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("");

        match cmd {
            "hello" => {
                if let Ok(pid) = arg.parse::<Pid>() {
                    client_pid = Some(pid);
                    state.add_client(pid, tx.clone()).await;
                    info!("Client connected: PID {}", pid);
                } else {
                    warn!("Invalid hello PID: {}", arg);
                }
            }
            "cd" => {
                if let Some(pid) = client_pid {
                    let path = PathBuf::from(arg);
                    state.update_cwd(pid, path).await;
                    // Trigger a re-evaluation of git/watcher logic here (handled by a separate module usually polling the state)
                } else {
                    warn!("Received cd before hello");
                }
            }
            "invalidate" => {
                info!("Invalidate request for: {}", arg);
                // Handle manual invalidate
            }
            "status" => {
                // Send a status dump to this unique connection, then close
                let count = state.get_client_count().await;
                let dump = format!("status: {} clients connected\n", count);
                let _ = tx.try_send(dump);
                // After status is satisfied we usually decouple or close.
            }
            _ => {
                warn!("Unknown command: {}", cmd);
            }
        }
    }

    if let Some(pid) = client_pid {
        info!("Client disconnected: PID {}", pid);
        state.remove_client(pid).await;
    }

    writer_task.abort();
    Ok(())
}
