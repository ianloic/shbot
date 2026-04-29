use crate::config::Config;
use crate::error::Result;
use crate::git::get_git_state;
use crate::runner::evaluate_file_triggers;
use crate::state::{AppState, Pid};
use crate::watcher::SyncWatcher;
use listenfd::ListenFd;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, Registry, reload::Handle};

pub static LOG_RELOAD_HANDLE: std::sync::OnceLock<Handle<EnvFilter, Registry>> =
    std::sync::OnceLock::new();

pub async fn run_server(
    state: Arc<AppState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    config_tx: tokio::sync::watch::Sender<Config>,
    watcher: Arc<SyncWatcher>,
    socket_path: PathBuf,
    pre_bound_listener: Option<std::os::unix::net::UnixListener>,
) -> Result<()> {
    let mut listenfd = ListenFd::from_env();

    // Check if we received a pre-bound listener from the parent or systemd
    let listener = if let Some(std_listener) =
        pre_bound_listener.or_else(|| listenfd.take_unix_listener(0).unwrap_or(None))
    {
        info!("Using inherited or systemd socket activation");
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
                let config_rx_clone = config_rx.clone();
                let config_tx_clone = config_tx.clone();
                let watcher_clone = watcher.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(
                        stream,
                        state_clone,
                        config_rx_clone,
                        config_tx_clone,
                        watcher_clone,
                    )
                    .await
                    {
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

async fn handle_connection(
    stream: UnixStream,
    state: Arc<AppState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    config_tx: tokio::sync::watch::Sender<Config>,
    watcher: Arc<SyncWatcher>,
) -> Result<()> {
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
                    state.add_client(pid, PathBuf::new(), tx.clone()).await;
                    info!("Client connected: PID {}", pid);
                } else {
                    warn!("Invalid hello PID: {}", arg);
                }
            }
            "cd" => {
                if let Some(pid) = client_pid {
                    let old_cwd = state.get_cwd(pid).await;
                    let path = PathBuf::from(arg);
                    state.update_cwd(pid, path.clone()).await;

                    let watch_target = if let Ok(Some((root, _))) = get_git_state(&path) {
                        PathBuf::from(root)
                    } else {
                        path.clone()
                    };
                    tracing::debug!(
                        "Handling cd to {:?}, evaluating watch target: {:?}",
                        path,
                        watch_target
                    );
                    watcher.watch(&watch_target);

                    if let Some(old) = old_cwd {
                        if old != path {
                            let old_watch_target = if let Ok(Some((root, _))) = get_git_state(&old)
                            {
                                PathBuf::from(root)
                            } else {
                                old.clone()
                            };

                            if old_watch_target != watch_target {
                                let active_cwds = state.get_unique_cwds().await;
                                let mut keep = false;
                                for active in active_cwds {
                                    let active_watch =
                                        if let Ok(Some((root, _))) = get_git_state(&active) {
                                            PathBuf::from(root)
                                        } else {
                                            active.clone()
                                        };
                                    if active_watch == old_watch_target {
                                        keep = true;
                                        break;
                                    }
                                }
                                if !keep {
                                    tracing::debug!(
                                        "Unwatching target {:?} as no active cwds use it",
                                        old_watch_target
                                    );
                                    watcher.unwatch(&old_watch_target);
                                } else {
                                    tracing::debug!(
                                        "Keeping watch on target {:?} as it is still used by another active cwd",
                                        old_watch_target
                                    );
                                }
                            }
                        }
                    }

                    if let Ok(Some((_, git_state))) = get_git_state(&path) {
                        state.send_to(pid, "git", &git_state.to_payload()).await;
                    } else {
                        state.send_to(pid, "git", "").await;
                    }
                    let config = config_rx.borrow().clone();
                    evaluate_file_triggers(&path, &state, &config, None).await;
                } else {
                    warn!("Received cd before hello");
                }
            }
            "reload" => {
                info!("Reload request received via socket");
                match Config::load() {
                    Ok(new_config) => {
                        if let Err(e) = config_tx.send(new_config) {
                            error!("Failed to broadcast config on reload: {}", e);
                            let _ =
                                tx.try_send("error: failed to broadcast new config\n".to_string());
                        } else {
                            let _ = tx.try_send(
                                "status: configuration reloaded successfully\n".to_string(),
                            );
                        }
                    }
                    Err(e) => {
                        error!("Failed to reload config: {}", e);
                        let _ = tx.try_send(format!("error: failed to load config - {}\n", e));
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                break;
            }
            "invalidate" => {
                info!("Invalidate request for: {}", arg);
                state.invalidate(arg).await;
            }
            "loglevel" => {
                if let Some(handle) = LOG_RELOAD_HANDLE.get() {
                    match EnvFilter::try_new(arg) {
                        Ok(new_filter) => {
                            if let Err(e) = handle.modify(|filter| *filter = new_filter) {
                                error!("Failed to update log level: {}", e);
                                let _ = tx.try_send(format!(
                                    "error: failed to update log level - {}\n",
                                    e
                                ));
                            } else {
                                info!("Log level dynamically updated to: {}", arg);
                                let _ = tx.try_send(
                                    "status: log level updated successfully\n".to_string(),
                                );
                            }
                        }
                        Err(e) => {
                            warn!("Invalid log level provided: {}", arg);
                            let _ = tx
                                .try_send(format!("error: invalid log level '{}' - {}\n", arg, e));
                        }
                    }
                } else {
                    let _ = tx.try_send("error: log reload handle not initialized\n".to_string());
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                break;
            }
            "status" => {
                // Send a status dump to this unique connection, then close
                let count = state.get_client_count().await;
                let dump = format!("status: {} clients connected\n", count);
                let _ = tx.try_send(dump);

                // Yield to ensure the writer has a chance to write before we break and close the socket.
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                break;
            }
            _ => {
                warn!("Unknown command: {}", cmd);
            }
        }
    }

    if let Some(pid) = client_pid {
        info!("Client disconnected: PID {}", pid);
        let old_cwd = state.get_cwd(pid).await;
        state.remove_client(pid).await;
        if let Some(old) = old_cwd {
            let old_watch_target = if let Ok(Some((root, _))) = get_git_state(&old) {
                PathBuf::from(root)
            } else {
                old.clone()
            };

            let active_cwds = state.get_unique_cwds().await;
            let mut keep = false;
            for active in active_cwds {
                let active_watch = if let Ok(Some((root, _))) = get_git_state(&active) {
                    PathBuf::from(root)
                } else {
                    active.clone()
                };
                if active_watch == old_watch_target {
                    keep = true;
                    break;
                }
            }
            if !keep {
                tracing::debug!(
                    "Client {} disconnected, unwatching target {:?} as no other clients need it",
                    pid,
                    old_watch_target
                );
                watcher.unwatch(&old_watch_target);
            } else {
                tracing::debug!(
                    "Client {} disconnected, keeping watch on target {:?} as it is still used by another active cwd",
                    pid,
                    old_watch_target
                );
            }
        }
    }

    writer_task.abort();
    Ok(())
}
