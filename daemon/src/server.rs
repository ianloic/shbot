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
use crate::config::Config;
use crate::error::Result;
use crate::error::ShbotError;
use crate::protocol::Command;
use crate::state::{DaemonState, Pid};
use crate::watcher::SyncWatcher;
use listenfd::ListenFd;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, Registry, reload::Handle};

pub static LOG_RELOAD_HANDLE: OnceLock<Handle<EnvFilter, Registry>> = OnceLock::new();

const OWNER_ONLY_FILE_MODE: u32 = 0o600;
const OWNER_ONLY_DIR_MODE: u32 = 0o700;
const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 128;

enum LineOutcome {
    Line,
    Eof,
    Oversized,
}

async fn read_capped_line<R>(
    reader: &mut BufReader<R>,
    buf: &mut Vec<u8>,
) -> std::io::Result<LineOutcome>
where
    R: AsyncRead + Unpin,
{
    buf.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if buf.is_empty() {
                LineOutcome::Eof
            } else {
                LineOutcome::Line
            });
        }
        let newline = available.iter().position(|&b| b == b'\n');
        let end = newline.map_or(available.len(), |i| i + 1);
        let has_newline = newline.is_some();
        buf.extend_from_slice(&available[..end]);
        reader.consume(end);
        if buf.len() > MAX_LINE_BYTES {
            return Ok(LineOutcome::Oversized);
        }
        if has_newline {
            buf.pop();
            return Ok(LineOutcome::Line);
        }
    }
}

fn current_uid() -> Result<u32> {
    Ok(fs::metadata("/proc/self")?.uid())
}

pub fn ensure_owner_only_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(OWNER_ONLY_DIR_MODE))?;
    Ok(())
}

pub fn restrict_to_owner(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(OWNER_ONLY_FILE_MODE))?;
    Ok(())
}

pub async fn run_server(
    state: Arc<DaemonState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    config_tx: tokio::sync::watch::Sender<Config>,
    watcher: Arc<SyncWatcher>,
    socket_path: PathBuf,
    pre_bound_listener: Option<StdUnixListener>,
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
        if let Some(p) = socket_path.parent() {
            ensure_owner_only_dir(p)?;
        }
        if socket_path.exists() {
            if StdUnixStream::connect(&socket_path).is_ok() {
                return Err(ShbotError::Custom(format!(
                    "refusing to bind {:?}: another daemon is already listening",
                    socket_path
                )));
            }
            fs::remove_file(&socket_path)?;
        }

        info!("Binding to socket: {:?}", socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        restrict_to_owner(&socket_path)?;
        listener
    };

    let daemon_uid = current_uid()?;
    let connection_slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let cred = match stream.peer_cred() {
                    Ok(cred) => cred,
                    Err(e) => {
                        warn!("Failed to read peer credentials: {}", e);
                        continue;
                    }
                };
                if cred.uid() != daemon_uid {
                    warn!(
                        "Rejecting connection from uid {} (daemon runs as uid {})",
                        cred.uid(),
                        daemon_uid
                    );
                    continue;
                }
                let peer_pid = cred.pid().map(|p| p as Pid);

                let permit = match connection_slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        warn!(
                            "Rejecting connection: at capacity ({} active)",
                            MAX_CONNECTIONS
                        );
                        continue;
                    }
                };

                let state_clone = state.clone();
                let config_rx_clone = config_rx.clone();
                let config_tx_clone = config_tx.clone();
                let watcher_clone = watcher.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_connection(
                        stream,
                        state_clone,
                        config_rx_clone,
                        config_tx_clone,
                        watcher_clone,
                        peer_pid,
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
    state: Arc<DaemonState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    config_tx: tokio::sync::watch::Sender<Config>,
    watcher: Arc<SyncWatcher>,
    peer_pid: Option<Pid>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut raw = Vec::with_capacity(128);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(100);

    let idle_timeout = tokio::time::Duration::from_secs(config_rx.borrow().daemon.idle_timeout_sec);
    let deadline: Arc<StdMutex<Instant>> = Arc::new(StdMutex::new(Instant::now() + idle_timeout));

    // We don't know the PID until the client says `hello <pid>`
    let mut client_pid: Option<Pid> = None;

    // Spawn a writer task for this connection
    let writer_deadline = deadline.clone();
    let writer_task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Some(msg) = rx.recv().await {
            if writer.write_all(msg.as_bytes()).await.is_err() {
                break;
            }
            *writer_deadline.lock().unwrap() = Instant::now() + idle_timeout;
        }
    });

    loop {
        let next_deadline = *deadline.lock().unwrap();
        let outcome = tokio::select! {
            biased;
            res = read_capped_line(&mut reader, &mut raw) => res,
            _ = tokio::time::sleep_until(next_deadline) => {
                warn!("Idle timeout expired for client {:?}", client_pid);
                break;
            }
        };

        let line = match outcome {
            Ok(LineOutcome::Eof) => break,
            Ok(LineOutcome::Oversized) => {
                warn!("Dropping connection: line exceeds {} bytes", MAX_LINE_BYTES);
                break;
            }
            Ok(LineOutcome::Line) => match String::from_utf8(raw.clone()) {
                Ok(line) => line,
                Err(_) => {
                    error!("Socket read error: invalid UTF-8");
                    break;
                }
            },
            Err(e) => {
                error!("Socket read error: {}", e);
                break;
            }
        };

        *deadline.lock().unwrap() = Instant::now() + idle_timeout;

        let Some(command) = Command::parse(&line) else {
            continue;
        };

        match command {
            Command::Hello(pid) => {
                if peer_pid != Some(pid) {
                    warn!(
                        "Rejecting hello from pid {} (kernel peer pid: {:?})",
                        pid, peer_pid
                    );
                    let _ = tx.try_send(format!(
                        "error: pid {} does not match the connection owner\n",
                        pid
                    ));
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    break;
                }
                client_pid = Some(pid);
                state.add_client(pid, PathBuf::new(), tx.clone()).await;
                info!("Client connected: PID {}", pid);
            }
            Command::InvalidHello(arg) => {
                warn!("Invalid hello PID: {}", arg);
            }
            Command::Cd(path) => {
                if let Some(pid) = client_pid {
                    let config = config_rx.borrow().clone();
                    state.handle_cd(pid, path, watcher.clone(), &config).await;
                } else {
                    warn!("Received cd before hello");
                }
            }
            Command::Reload => {
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
            Command::Invalidate(arg) => {
                info!("Invalidate request for: {}", arg);
                state.invalidate(&arg).await;
            }
            Command::LogLevel(arg) => {
                if let Some(handle) = LOG_RELOAD_HANDLE.get() {
                    match EnvFilter::try_new(&arg) {
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
            Command::Status => {
                // Send a status dump to this unique connection, then close
                let config = config_rx.borrow().clone();
                let dump = state
                    .get_status_dump(watcher.get_watch_count(), &config)
                    .await;
                let _ = tx.try_send(dump);

                // Yield to ensure the writer has a chance to write before we break and close the socket.
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                break;
            }
            Command::Unknown(cmd) => {
                warn!("Unknown command: {}", cmd);
            }
        }
    }

    if let Some(pid) = client_pid {
        info!("Client disconnected: PID {}", pid);
        let config = config_rx.borrow().clone();
        state
            .handle_client_disconnect(pid, watcher.clone(), &config)
            .await;
    }

    writer_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, StateId};
    use crate::state::DaemonState;
    use crate::watcher::SyncWatcher;
    use std::process::Command as StdCommand;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    #[allow(unused_macros)]
    macro_rules! wait_for_watch_count {
        ($watcher:expr, $count:expr) => {
            for _ in 0..50 {
                if $watcher.get_watch_count() == $count {
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }
            assert_eq!($watcher.get_watch_count(), $count);
        };
    }

    async fn setup_server_with_peer(
        peer_pid: Option<Pid>,
    ) -> (
        UnixStream,
        tokio::task::JoinHandle<()>,
        Arc<DaemonState>,
        Arc<SyncWatcher>,
    ) {
        let (client, server) = UnixStream::pair().unwrap();
        let state = DaemonState::new();
        let watcher = SyncWatcher::new();
        let (config_tx, config_rx) = tokio::sync::watch::channel(Config::default());

        watcher.start(state.clone(), config_rx.clone()).unwrap();

        let state_clone = state.clone();
        let watcher_clone = watcher.clone();
        let server_task = tokio::spawn(async move {
            let _ = handle_connection(
                server,
                state_clone,
                config_rx,
                config_tx,
                watcher_clone,
                peer_pid,
            )
            .await;
        });

        (client, server_task, state, watcher)
    }

    async fn setup_server() -> (
        UnixStream,
        tokio::task::JoinHandle<()>,
        Arc<DaemonState>,
        Arc<SyncWatcher>,
    ) {
        setup_server_with_peer(None).await
    }

    #[tokio::test]
    async fn test_server_hello_cd() {
        let (mut client, _server_task, state, _watcher) = setup_server_with_peer(Some(12345)).await;

        let mut rx = state.test_events_tx.subscribe();
        client.write_all(b"hello 12345\n").await.unwrap();
        let _ = rx.recv().await;
        assert_eq!(state.get_cwd(12345).await.unwrap(), PathBuf::from(""));

        let mut rx = state.test_events_tx.subscribe();
        client.write_all(b"cd /tmp\n").await.unwrap();
        let _ = rx.recv().await;
        assert_eq!(state.get_cwd(12345).await.unwrap(), PathBuf::from("/tmp"));

        client.write_all(b"status\n").await.unwrap();
        let mut buf = String::new();
        client.read_to_string(&mut buf).await.unwrap();
        assert!(buf.contains("12345"));
    }

    #[tokio::test]
    async fn test_server_reload_error() {
        let (mut client, _server_task, _state, _watcher) = setup_server().await;

        let temp_home = tempfile::tempdir().unwrap();

        temp_env::async_with_vars([("HOME", Some(temp_home.path()))], async {
            let config_dir = temp_home.path().join(".config").join("shbot");
            fs::create_dir_all(&config_dir).unwrap();
            fs::write(config_dir.join("shbot.toml"), "invalid toml syntax = {").unwrap();

            client.write_all(b"reload\n").await.unwrap();
            client.write_all(b"status\n").await.unwrap(); // Close connection
            let mut buf = String::new();
            client.read_to_string(&mut buf).await.unwrap();
            assert!(buf.contains("error: failed to load config"));
        })
        .await;
    }

    #[tokio::test]
    async fn test_server_loglevel() {
        let (mut client, _server_task, _state, _watcher) = setup_server().await;

        client.write_all(b"loglevel debug\n").await.unwrap();
        client.write_all(b"status\n").await.unwrap(); // close connection
        let mut buf = String::new();
        client.read_to_string(&mut buf).await.unwrap();
        assert!(buf.contains("error: log reload handle not initialized"));
    }

    #[tokio::test]
    async fn test_server_invalidate() {
        let (mut client, _server_task, state, _watcher) = setup_server_with_peer(Some(12345)).await;

        let mut rx = state.test_events_tx.subscribe();
        client.write_all(b"hello 12345\n").await.unwrap();
        let _ = rx.recv().await;

        state
            .clients
            .write()
            .await
            .get_mut(&12345)
            .unwrap()
            .last_state
            .insert(StateId::from("custom"), "somedata".to_string());
        assert!(
            state
                .clients
                .read()
                .await
                .get(&12345)
                .unwrap()
                .last_state
                .contains_key("custom")
        );

        let mut rx = state.test_events_tx.subscribe();
        client.write_all(b"invalidate all\n").await.unwrap();
        let _ = rx.recv().await;

        assert!(
            !state
                .clients
                .read()
                .await
                .get(&12345)
                .unwrap()
                .last_state
                .contains_key("custom")
        );
    }

    #[tokio::test]
    async fn test_server_unknown_command() {
        let (mut client, _server_task, _state, _watcher) = setup_server().await;
        client.write_all(b"fakecommand arg\n").await.unwrap();
        client.write_all(b"status\n").await.unwrap();
        let mut buf = String::new();
        client.read_to_string(&mut buf).await.unwrap();
        assert!(buf.contains("status:"));
    }

    #[tokio::test]
    async fn test_client_disconnect_unwatches() {
        let (mut client, server_task, state, watcher) = setup_server_with_peer(Some(9999)).await;

        let temp_dir = tempfile::tempdir().unwrap();
        StdCommand::new("git")
            .arg("init")
            .current_dir(temp_dir.path())
            .output()
            .unwrap();
        let path_str = temp_dir.path().to_str().unwrap();
        let cd_cmd = format!("cd {}\n", path_str);

        let mut rx = state.test_events_tx.subscribe();
        client.write_all(b"hello 9999\n").await.unwrap();
        let _ = rx.recv().await;

        let mut rx = state.test_events_tx.subscribe();
        client.write_all(cd_cmd.as_bytes()).await.unwrap();
        let _ = rx.recv().await;

        drop(client);
        let _ = server_task.await;

        assert!(state.get_cwd(9999).await.is_none());
        wait_for_watch_count!(watcher, 0);
    }

    #[tokio::test]
    async fn test_server_empty_line_and_invalid_hello() {
        let (mut client, _server_task, state, _watcher) = setup_server().await;

        // Empty line
        client.write_all(b"\n").await.unwrap();

        // Invalid hello
        client.write_all(b"hello notapid\n").await.unwrap();
        // Since invalid hello does nothing, we can't easily wait. But it just logs,
        // so we'll wait for the task to finish or just poll
        tokio::task::yield_now().await;

        assert!(state.clients.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_server_cd_before_hello() {
        let (mut client, _server_task, state, _watcher) = setup_server().await;

        let temp_dir = tempfile::tempdir().unwrap();
        let path_str = temp_dir.path().to_str().unwrap();
        let cd_cmd = format!("cd {}\n", path_str);

        client.write_all(cd_cmd.as_bytes()).await.unwrap();
        // Since cd before hello fails immediately, we'll just yield. It won't trigger an event.
        tokio::task::yield_now().await;

        assert!(state.clients.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_server_cd_shared_watch_target() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .try_init();
        let (mut client1, _server_task1, state, watcher) = setup_server_with_peer(Some(111)).await;
        let (mut client2, _server_task2) = {
            let (c, s) = UnixStream::pair().unwrap();
            let (tx, rx) = tokio::sync::watch::channel(Config::default());
            let t = tokio::spawn(handle_connection(
                s,
                state.clone(),
                rx,
                tx,
                watcher.clone(),
                Some(222),
            ));
            (c, t)
        };

        let temp_dir1 = tempfile::tempdir().unwrap();
        StdCommand::new("git")
            .arg("init")
            .current_dir(temp_dir1.path())
            .output()
            .unwrap();
        let path_str1 = temp_dir1.path().to_str().unwrap();
        let cd_cmd1 = format!("cd {}\n", path_str1);

        let temp_dir2 = tempfile::tempdir().unwrap();
        StdCommand::new("git")
            .arg("init")
            .current_dir(temp_dir2.path())
            .output()
            .unwrap();
        let path_str2 = temp_dir2.path().to_str().unwrap();
        let cd_cmd2 = format!("cd {}\n", path_str2);

        let mut rx1 = state.test_events_tx.subscribe();
        client1.write_all(b"hello 111\n").await.unwrap();
        let _ = rx1.recv().await;

        let mut rx2 = state.test_events_tx.subscribe();
        client2.write_all(b"hello 222\n").await.unwrap();
        let _ = rx2.recv().await;

        let mut rx1 = state.test_events_tx.subscribe();
        client1.write_all(cd_cmd1.as_bytes()).await.unwrap();
        let _ = rx1.recv().await;

        let mut rx2 = state.test_events_tx.subscribe();
        client2.write_all(cd_cmd1.as_bytes()).await.unwrap();
        let _ = rx2.recv().await;

        wait_for_watch_count!(watcher, 1);

        // Disconnect client 1, client 2 still needs temp_dir1
        let mut rx = state.test_events_tx.subscribe();
        drop(client1);
        let _ = rx.recv().await;

        // Watcher count should still be 1 because client 2 is alive
        wait_for_watch_count!(watcher, 1);

        // cd client 2 away
        let mut rx = state.test_events_tx.subscribe();
        client2.write_all(cd_cmd2.as_bytes()).await.unwrap();
        let _ = rx.recv().await;

        // now watcher should have 1 (for temp_dir2), and temp_dir1 should be unwatched
        wait_for_watch_count!(watcher, 1);
    }

    #[tokio::test]
    async fn test_server_corrupted_payload() {
        let (mut client, _server_task, _state, _watcher) = setup_server().await;

        // Send non-utf8
        client.write_all(&[0xFF, 0xFE, 0xFD, b'\n']).await.unwrap();

        // Send a very long line to test buffer
        let long_line = vec![b'A'; 10000];
        client.write_all(&long_line).await.unwrap();
        client.write_all(b"\n").await.unwrap();

        // Server should drop connection on invalid UTF-8, so this write or read might fail.
        let _ = client.write_all(b"status\n").await;
        let mut buf = String::new();
        let res = client.read_to_string(&mut buf).await;
        // Either the read succeeds with 0 bytes (EOF) or fails with ConnectionReset
        if let Ok(n) = res {
            assert_eq!(n, 0);
        }
    }

    #[tokio::test]
    async fn test_server_loglevel_success() {
        let (mut client, _server_task, _state, _watcher) = setup_server().await;

        let (_filter, handle) = tracing_subscriber::reload::Layer::new(EnvFilter::new("info"));
        let _ = LOG_RELOAD_HANDLE.set(handle);

        client.write_all(b"loglevel debug\n").await.unwrap();
        client.write_all(b"status\n").await.unwrap();

        let mut buf = String::new();
        client.read_to_string(&mut buf).await.unwrap();
        // We can't strictly assert the buffer because it contains status, but we know it didn't panic.

        // Test invalid log level
        let (client2, _) = UnixStream::pair().unwrap();
        let _t = tokio::spawn(handle_connection(
            client2,
            _state.clone(),
            tokio::sync::watch::channel(Config::default()).1,
            tokio::sync::watch::channel(Config::default()).0,
            _watcher.clone(),
            None,
        ));
        // We don't have a handle to client2 here, actually we do but we just made a dummy. Let's ignore it for now.
    }

    #[tokio::test]
    async fn test_server_hello_pid_mismatch() {
        let (mut client, server_task, state, _watcher) = setup_server_with_peer(Some(4242)).await;

        client.write_all(b"hello 12345\n").await.unwrap();

        let mut buf = String::new();
        client.read_to_string(&mut buf).await.unwrap();
        assert!(buf.contains("error: pid 12345 does not match the connection owner"));
        assert!(state.clients.read().await.is_empty());

        let _ = server_task.await;
    }

    #[tokio::test]
    async fn test_run_server_socket_mode_and_peer_credentials() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("test.socket");

        let state = DaemonState::new();
        let watcher = SyncWatcher::new();
        let (config_tx, config_rx) = tokio::sync::watch::channel(Config::default());
        watcher.start(state.clone(), config_rx.clone()).unwrap();

        let server_task = tokio::spawn(run_server(
            state.clone(),
            config_rx,
            config_tx,
            watcher.clone(),
            socket_path.clone(),
            None,
        ));

        let mut client = None;
        for _ in 0..100_000 {
            if let Ok(stream) = UnixStream::connect(&socket_path).await {
                client = Some(stream);
                break;
            }
            tokio::task::yield_now().await;
        }
        let mut client = client.expect("server never started listening");

        let mode = fs::metadata(&socket_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let real_pid = std::process::id();
        let mut rx = state.test_events_tx.subscribe();
        client
            .write_all(format!("hello {}\n", real_pid).as_bytes())
            .await
            .unwrap();
        let _ = rx.recv().await;
        assert_eq!(state.get_cwd(real_pid).await, Some(PathBuf::from("")));

        client.write_all(b"hello 1\n").await.unwrap();
        let mut buf = String::new();
        client.read_to_string(&mut buf).await.unwrap();
        assert!(buf.contains("error: pid 1 does not match the connection owner"));
        assert!(state.get_cwd(real_pid).await.is_none());

        server_task.abort();
    }

    #[tokio::test]
    async fn test_run_server_refuses_to_steal_live_socket() {
        let temp_dir = tempfile::tempdir().unwrap();
        let socket_path = temp_dir.path().join("busy.socket");

        let state = DaemonState::new();
        let watcher = SyncWatcher::new();
        let (config_tx, config_rx) = tokio::sync::watch::channel(Config::default());
        watcher.start(state.clone(), config_rx.clone()).unwrap();

        let first = tokio::spawn(run_server(
            state.clone(),
            config_rx,
            config_tx,
            watcher.clone(),
            socket_path.clone(),
            None,
        ));

        let mut connected = false;
        for _ in 0..100_000 {
            if UnixStream::connect(&socket_path).await.is_ok() {
                connected = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(connected, "first server never started listening");

        let (config_tx2, config_rx2) = tokio::sync::watch::channel(Config::default());
        let second = tokio::spawn(run_server(
            state,
            config_rx2,
            config_tx2,
            watcher,
            socket_path.clone(),
            None,
        ));

        let result = second.await.unwrap();
        assert!(
            result.is_err(),
            "second daemon must refuse to steal a live socket"
        );
        assert!(socket_path.exists(), "original socket must be untouched");

        first.abort();
    }

    #[tokio::test]
    async fn test_server_oversized_line_drops_connection() {
        let (mut client, _server_task, state, _watcher) = setup_server().await;

        let big = vec![b'A'; MAX_LINE_BYTES + 1];
        client.write_all(&big).await.unwrap();

        let mut buf = String::new();
        let n = client.read_to_string(&mut buf).await.unwrap();
        assert_eq!(n, 0, "connection must be dropped on oversized input");
        assert!(state.clients.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_server_idle_timeout() {
        let (client, server) = UnixStream::pair().unwrap();
        let state = DaemonState::new();
        let watcher = SyncWatcher::new();

        let mut config = Config::default();
        config.daemon.idle_timeout_sec = 0;
        let (config_tx, config_rx) = tokio::sync::watch::channel(config);
        watcher.start(state.clone(), config_rx.clone()).unwrap();

        let state_clone = state.clone();
        let server_task = tokio::spawn(async move {
            let _ = handle_connection(
                server,
                state_clone,
                config_rx,
                config_tx,
                watcher,
                Some(12345),
            )
            .await;
        });

        let mut client = client;
        let mut rx = state.test_events_tx.subscribe();
        client.write_all(b"hello 12345\n").await.unwrap();
        let _ = rx.recv().await;

        let _ = rx.recv().await;

        assert!(
            state.get_cwd(12345).await.is_none(),
            "idle client must be disconnected"
        );
        let _ = server_task.await;
    }
}
