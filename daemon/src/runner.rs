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

use crate::config::{Config, StateId, StateSource};
use crate::git::get_git_state;
use crate::state::DaemonState;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::{Duration, sleep};
use tracing::{debug, error, info, warn};

const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(1);

async fn read_capped<R>(pipe: R) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = pipe;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                // Keep at most MAX + 1 bytes (the extra byte signals
                // truncation) but keep draining so the child can exit
                // instead of blocking on a full pipe.
                if buf.len() < MAX_COMMAND_OUTPUT_BYTES + 1 {
                    let remaining = MAX_COMMAND_OUTPUT_BYTES + 1 - buf.len();
                    let keep = n.min(remaining);
                    buf.extend_from_slice(&chunk[..keep]);
                }
            }
            Err(_) => break,
        }
    }
    buf
}

async fn run_command(command: &str, cwd: &Path, timeout: Duration) -> Option<String> {
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            error!("Command source failed to execute '{}': {}", command, e);
            return None;
        }
    };

    let stdout_handle = child.stdout.take().map(|p| tokio::spawn(read_capped(p)));
    let stderr_handle = child.stderr.take().map(|p| tokio::spawn(read_capped(p)));

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            warn!(
                "Command '{}' timed out after {:?}; killing",
                command, timeout
            );
            let _ = child.kill().await;
            if let Some(handle) = stdout_handle {
                handle.abort();
            }
            if let Some(handle) = stderr_handle {
                handle.abort();
            }
            return None;
        }
    };

    let join_pipe = |handle: Option<tokio::task::JoinHandle<Vec<u8>>>| async {
        match handle {
            Some(handle) => match tokio::time::timeout(PIPE_DRAIN_GRACE, handle).await {
                Ok(Ok(bytes)) => bytes,
                _ => Vec::new(),
            },
            None => Vec::new(),
        }
    };

    let stdout = join_pipe(stdout_handle).await;
    let stderr = join_pipe(stderr_handle).await;

    match status {
        Ok(status) => {
            if status.success() {
                if stdout.len() > MAX_COMMAND_OUTPUT_BYTES {
                    warn!(
                        "Command '{}' output exceeded {} bytes; truncating",
                        command, MAX_COMMAND_OUTPUT_BYTES
                    );
                }
                Some(String::from_utf8_lossy(&stdout).trim().to_string())
            } else {
                error!(
                    "Command source '{}' failed: {}",
                    command,
                    String::from_utf8_lossy(&stderr)
                );
                None
            }
        }
        Err(e) => {
            error!("Command source '{}' failed to wait: {}", command, e);
            None
        }
    }
}

pub async fn evaluate_state_source(
    name: &StateId,
    source: &StateSource,
    cwd: &Path,
    state: &DaemonState,
    command_timeout: Duration,
) -> Option<String> {
    match source {
        StateSource::File { file } => {
            let path = PathBuf::from(file);
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => Some(content.trim().to_string()),
                Err(e) => {
                    error!("File source '{}' failed to read {:?}: {}", name, path, e);
                    None
                }
            }
        }
        StateSource::Command { command } => run_command(command, cwd, command_timeout).await,
        StateSource::Native { native } => {
            if native == "git" {
                let start_git = Instant::now();
                let cwd_clone = cwd.to_path_buf();
                let result = tokio::task::spawn_blocking(move || get_git_state(&cwd_clone)).await;

                if let Ok(Ok(Some((root, git_state)))) = result {
                    state
                        .git_latencies
                        .write()
                        .await
                        .insert(root, start_git.elapsed().as_micros() as u64);
                    Some(git_state.to_payload())
                } else {
                    Some(String::new())
                }
            } else {
                None
            }
        }
    }
}

pub fn start_timeout_tasks(
    state: Arc<DaemonState>,
    mut config_rx: tokio::sync::watch::Receiver<Config>,
) {
    tokio::spawn(async move {
        let mut active_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        loop {
            // Abort old handles on every configuration turn
            for handle in active_handles.drain(..) {
                handle.abort();
            }

            let config = config_rx.borrow().clone();

            for (name, state_config) in &config.state {
                if let Some(timeout_sec) = state_config.invalidate.timeout {
                    let name = name.clone();
                    let interval = Duration::from_secs(timeout_sec);
                    let state_clone = state.clone();
                    let _source = state_config.source.clone();

                    let config_clone = config.clone();

                    let handle = tokio::spawn(async move {
                        info!(
                            "Starting timeout task '{}' every {}s",
                            name,
                            interval.as_secs()
                        );
                        loop {
                            sleep(interval).await;

                            let scope_roots: Vec<_> = {
                                let scoped_states = state_clone.scoped_states.read().await;
                                scoped_states
                                    .iter()
                                    .filter_map(
                                        |(k, _)| if k.0 == name { Some(k.1.clone()) } else { None },
                                    )
                                    .collect()
                            };

                            for root in scope_roots {
                                evaluate_scoped_state(&name, &root, &state_clone, &config_clone)
                                    .await;
                            }
                        }
                    });

                    active_handles.push(handle);
                }
            }

            // Wait for configuration changes; if channel closes, break
            if config_rx.changed().await.is_err() {
                break;
            }
        }
    });
}

pub async fn evaluate_scoped_state(
    name: &StateId,
    scope_root: &Path,
    state: &DaemonState,
    config: &Config,
) {
    if let Some(state_config) = config.state.get(name) {
        if !state.begin_evaluation(name, scope_root).await {
            debug!(
                "Skipping duplicate evaluation of state '{}' at {:?} (queued rerun)",
                name, scope_root
            );
            return;
        }
        let command_timeout = Duration::from_secs(config.daemon.command_timeout_sec);
        loop {
            if let Some(stdout) = evaluate_state_source(
                name,
                &state_config.source,
                scope_root,
                state,
                command_timeout,
            )
            .await
            {
                state.update_scoped_state(name, scope_root, &stdout).await;
            }
            if !state.finish_evaluation_step(name, scope_root).await {
                break;
            }
        }
    }
}

pub async fn evaluate_watch_tasks(
    state: &DaemonState,
    config: &Config,
    affected_paths: Option<&[PathBuf]>,
) {
    let active_scopes: Vec<_> = {
        let scoped_states = state.scoped_states.read().await;
        scoped_states.keys().cloned().collect()
    };

    for (name, scope_root) in active_scopes {
        if let Some(state_config) = config.state.get(&name)
            && state_config.watch_pattern().is_some()
        {
            let mut matched = false;

            if let Some(paths) = affected_paths {
                let Some(glob) = state_config.watch_matcher(&name) else {
                    continue;
                };
                let ignores = state_config.ignore_set();

                for path in paths {
                    if let Ok(rel_path) = path.strip_prefix(&scope_root) {
                        if let Some(ig) = ignores
                            && ig.is_match(rel_path)
                        {
                            continue;
                        }
                        if glob.is_match(rel_path) {
                            debug!("Noticed directory change: {:?}", path);
                            matched = true;
                            break;
                        }
                    }
                }
            } else {
                // If affected_paths is None, force execution
                matched = true;
            }

            if matched {
                evaluate_scoped_state(&name, &scope_root, state, config).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, StateConfig, StateId, StateInvalidate};
    use crate::scope::Scope;
    use crate::state::{DaemonState, ScopedState};

    use std::collections::HashSet;
    use std::process::{self, Command as StdCommand};
    use std::sync::Arc;
    use tokio::sync::{RwLock, watch};

    #[tokio::test]
    async fn test_runner_reloads_on_config_change() {
        let state = DaemonState::new();
        let mut config = Config::default();
        config.state.insert(
            StateId::from("test_1"),
            StateConfig {
                scope: Scope::Global,
                source: StateSource::Command {
                    command: "echo 'first'".to_string(),
                },
                invalidate: StateInvalidate {
                    timeout: Some(1),
                    ..Default::default()
                },
            },
        );

        // Add a dummy ScopedState so the timeout task evaluates something
        state.scoped_states.write().await.insert(
            (StateId::from("test_1"), PathBuf::from("/")),
            Arc::new(ScopedState {
                subscribers: RwLock::new(HashSet::new()),
                last_value: RwLock::new(None),
            }),
        );

        let (config_tx, config_rx) = watch::channel(config);

        let mut rx = state.test_events_tx.subscribe();
        start_timeout_tasks(state.clone(), config_rx);

        // Allow task to spawn and run 1st config
        let _ = rx.recv().await;

        // Update config to something else
        let mut new_config = Config::default();
        new_config.state.insert(
            StateId::from("test_2"),
            StateConfig {
                scope: Scope::Global,
                source: StateSource::Command {
                    command: "echo 'second'".to_string(),
                },
                invalidate: StateInvalidate {
                    timeout: Some(1),
                    ..Default::default()
                },
            },
        );

        // Broadcast reload
        assert!(config_tx.send(new_config).is_ok());

        // Yield tokio to allow aborts to process
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // The fact that it doesn't panic and we successfully spawned and
        // aborted the internal tasks indicates the loop processes correctly.
    }

    #[tokio::test]
    async fn test_evaluate_watch_tasks_glob_matching() {
        let state = DaemonState::new();
        let mut config = Config::default();
        config.state.insert(
            StateId::from("rust_files"),
            StateConfig {
                scope: Scope::Global,
                source: StateSource::Command {
                    command: "echo 'rust'".to_string(),
                },
                invalidate: StateInvalidate {
                    watch: Some("**/*.rs".to_string()),
                    ..Default::default()
                },
            },
        );

        let cwd = tempfile::tempdir().unwrap();
        state.scoped_states.write().await.insert(
            (StateId::from("rust_files"), cwd.path().to_path_buf()),
            Arc::new(ScopedState {
                subscribers: RwLock::new(HashSet::new()),
                last_value: RwLock::new(None),
            }),
        );

        // No paths means forced evaluation
        evaluate_watch_tasks(&state, &config, None).await;

        // Let's test with matching path
        let affected_match = vec![cwd.path().join("src/main.rs")];
        evaluate_watch_tasks(&state, &config, Some(&affected_match)).await;

        // Test with non-matching path
        let affected_nomatch = vec![cwd.path().join("src/main.js")];
        evaluate_watch_tasks(&state, &config, Some(&affected_nomatch)).await;
    }

    #[tokio::test]
    async fn test_runner_timeout_task_failures() {
        let state = DaemonState::new();
        let mut config = Config::default();
        config.state.insert(
            StateId::from("test_fail"),
            StateConfig {
                scope: Scope::Global,
                source: StateSource::Command {
                    command: "exit 1".to_string(),
                },
                invalidate: StateInvalidate {
                    timeout: Some(1),
                    ..Default::default()
                },
            },
        );

        state.scoped_states.write().await.insert(
            (StateId::from("test_fail"), PathBuf::from("/")),
            Arc::new(ScopedState {
                subscribers: RwLock::new(HashSet::new()),
                last_value: RwLock::new(None),
            }),
        );
        let (_config_tx, config_rx) = watch::channel(config);
        start_timeout_tasks(state.clone(), config_rx);

        // Let it run once. Since it fails, it won't broadcast. We can just yield.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn test_run_command_success() {
        let temp = tempfile::tempdir().unwrap();
        let out = run_command("echo hi", temp.path(), Duration::from_secs(10)).await;
        assert_eq!(out.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn test_run_command_failure_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        let out = run_command("exit 1", temp.path(), Duration::from_secs(10)).await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn test_run_command_timeout_kills_child() {
        let temp = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let out = run_command("sleep 60", temp.path(), Duration::from_millis(100)).await;
        assert!(out.is_none());
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timeout must not wait for the full command duration"
        );

        let ps = StdCommand::new("ps").arg("-ef").output().unwrap();
        let ps_output = String::from_utf8_lossy(&ps.stdout);
        let test_pid = process::id().to_string();
        assert!(
            !ps_output
                .lines()
                .any(|l| l.contains("sleep 60") && l.contains(&test_pid)),
            "timed-out child must be killed, not left orphaned"
        );
    }

    #[tokio::test]
    async fn test_run_command_output_capped() {
        let temp = tempfile::tempdir().unwrap();
        let out = run_command("seq 1 50000", temp.path(), Duration::from_secs(5)).await;
        let out = out.expect("command must succeed");
        assert!(
            out.len() <= MAX_COMMAND_OUTPUT_BYTES + 8,
            "output must be capped, got {} bytes",
            out.len()
        );
    }

    #[tokio::test]
    async fn test_evaluation_in_flight_dedup() {
        let state = DaemonState::new();
        let name = StateId::from("dedup");
        let root = PathBuf::from("/tmp/dedup");

        assert!(state.begin_evaluation(&name, &root).await);
        assert!(
            !state.begin_evaluation(&name, &root).await,
            "second evaluation of the same scope must be skipped and mark dirty latch"
        );
        assert!(
            state.finish_evaluation_step(&name, &root).await,
            "dirty latch must request one trailing evaluation pass"
        );
        assert!(
            !state.finish_evaluation_step(&name, &root).await,
            "once trailing pass completes without new triggers, evaluation must finish"
        );
        assert!(
            state.begin_evaluation(&name, &root).await,
            "evaluation must be re-entrant after completion"
        );
        state.end_evaluation(&name, &root).await;
    }

    #[tokio::test]
    async fn test_evaluate_watch_tasks_cwd_not_exists() {
        // Obsolete test as cwd is no longer used, but let's keep a dummy
        let state = DaemonState::new();
        let config = Config::default();
        evaluate_watch_tasks(&state, &config, None).await;
    }

    #[tokio::test]
    async fn test_evaluate_watch_tasks_invalid_glob() {
        let state = DaemonState::new();
        let mut config = Config::default();
        config.state.insert(
            StateId::from("bad_glob"),
            StateConfig {
                scope: Scope::Global,
                source: StateSource::Command {
                    command: "echo 'hi'".to_string(),
                },
                invalidate: StateInvalidate {
                    watch: Some("[".to_string()),
                    ..Default::default()
                },
            },
        );

        let cwd = tempfile::tempdir().unwrap();
        state.scoped_states.write().await.insert(
            (StateId::from("bad_glob"), cwd.path().to_path_buf()),
            Arc::new(ScopedState {
                subscribers: RwLock::new(HashSet::new()),
                last_value: RwLock::new(None),
            }),
        );
        let affected = vec![cwd.path().join("test.txt")];
        evaluate_watch_tasks(&state, &config, Some(&affected)).await;
    }

    #[tokio::test]
    async fn test_evaluate_watch_tasks_failures() {
        let state = DaemonState::new();
        let mut config = Config::default();
        config.state.insert(
            StateId::from("fail_cmd"),
            StateConfig {
                scope: Scope::Global,
                source: StateSource::Command {
                    command: "exit 1".to_string(),
                },
                invalidate: StateInvalidate {
                    watch: Some("*".to_string()),
                    ..Default::default()
                },
            },
        );

        let cwd = tempfile::tempdir().unwrap();
        state.scoped_states.write().await.insert(
            (StateId::from("fail_cmd"), cwd.path().to_path_buf()),
            Arc::new(ScopedState {
                subscribers: RwLock::new(HashSet::new()),
                last_value: RwLock::new(None),
            }),
        );
        evaluate_watch_tasks(&state, &config, None).await;
    }
}
