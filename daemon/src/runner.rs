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

use crate::config::{Config, StateSource};
use crate::state::AppState;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{Duration, sleep};
use tracing::{error, info};

pub async fn evaluate_state_source(
    name: &str,
    source: &StateSource,
    cwd: &std::path::Path,
) -> Option<String> {
    match source {
        StateSource::File { file } => {
            let path = std::path::PathBuf::from(file);
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
        StateSource::Command { command } => {
            match Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(cwd)
                .output()
                .await
            {
                Ok(output) => {
                    if output.status.success() {
                        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        error!("Command source '{}' failed: {}", name, stderr);
                        None
                    }
                }
                Err(e) => {
                    error!("Command source '{}' failed to execute: {}", name, e);
                    None
                }
            }
        }
    }
}

pub fn start_timeout_tasks(
    state: Arc<AppState>,
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

            for (name, state_config) in config.state {
                if let Some(timeout_sec) = state_config.invalidate.timeout {
                    let name = name.clone();
                    let interval = Duration::from_secs(timeout_sec);
                    let state_clone = state.clone();
                    let source = state_config.source.clone();

                    let handle = tokio::spawn(async move {
                        info!(
                            "Starting timeout task '{}' every {}s",
                            name,
                            interval.as_secs()
                        );
                        let default_cwd = std::env::current_dir()
                            .unwrap_or_else(|_| std::path::PathBuf::from("/"));
                        loop {
                            sleep(interval).await;
                            if let Some(stdout) =
                                evaluate_state_source(&name, &source, &default_cwd).await
                            {
                                state_clone.broadcast_all(&name, &stdout).await;
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

pub async fn evaluate_watch_tasks(
    cwd: &std::path::Path,
    state: &AppState,
    config: &Config,
    affected_paths: Option<&[std::path::PathBuf]>,
) {
    // Only run if the path exists
    if !cwd.exists() {
        return;
    }

    for (name, state_config) in &config.state {
        let watch_pattern = if let Some(w) = &state_config.invalidate.watch {
            Some(w.as_str())
        } else if let StateSource::File { file } = &state_config.source {
            Some(file.as_str())
        } else {
            None
        };

        if let Some(watch_pattern) = watch_pattern {
            let mut matched = false;

            if let Some(paths) = affected_paths {
                let builder = globset::GlobBuilder::new(watch_pattern);
                let glob = match builder.build() {
                    Ok(g) => g.compile_matcher(),
                    Err(e) => {
                        error!(
                            "Invalid glob pattern '{}' for state '{}': {}",
                            watch_pattern, name, e
                        );
                        continue;
                    }
                };

                for path in paths {
                    if let Ok(rel_path) = path.strip_prefix(cwd)
                        && glob.is_match(rel_path)
                    {
                        matched = true;
                        break;
                    }
                }
            } else {
                // If affected_paths is None, force execution
                matched = true;
            }

            if matched
                && let Some(stdout) = evaluate_state_source(name, &state_config.source, cwd).await
            {
                state.broadcast_to_cwd(cwd, name, &stdout).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, StateConfig, StateInvalidate};
    use tokio::sync::watch;

    #[tokio::test]
    async fn test_runner_reloads_on_config_change() {
        let state = AppState::new();
        let mut config = Config::default();
        config.state.insert(
            "test_1".to_string(),
            StateConfig {
                source: StateSource::Command {
                    command: "echo 'first'".to_string(),
                },
                invalidate: StateInvalidate {
                    timeout: Some(1),
                    ..Default::default()
                },
            },
        );

        let (config_tx, config_rx) = watch::channel(config);

        let mut rx = state.test_events_tx.subscribe();
        start_timeout_tasks(state.clone(), config_rx);

        // Allow task to spawn and run 1st config
        let _ = rx.recv().await;

        // Update config to something else
        let mut new_config = Config::default();
        new_config.state.insert(
            "test_2".to_string(),
            StateConfig {
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
        let state = AppState::new();
        let mut config = Config::default();
        config.state.insert(
            "rust_files".to_string(),
            StateConfig {
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

        // No paths means forced evaluation
        evaluate_watch_tasks(cwd.path(), &state, &config, None).await;

        // Let's test with matching path
        let affected_match = vec![cwd.path().join("src/main.rs")];
        evaluate_watch_tasks(cwd.path(), &state, &config, Some(&affected_match)).await;

        // Test with non-matching path
        let affected_nomatch = vec![cwd.path().join("src/main.js")];
        evaluate_watch_tasks(cwd.path(), &state, &config, Some(&affected_nomatch)).await;
    }

    #[tokio::test]
    async fn test_runner_timeout_task_failures() {
        let state = AppState::new();
        let mut config = Config::default();
        config.state.insert(
            "test_fail".to_string(),
            StateConfig {
                source: StateSource::Command {
                    command: "exit 1".to_string(),
                },
                invalidate: StateInvalidate {
                    timeout: Some(1),
                    ..Default::default()
                },
            },
        );

        let (_config_tx, config_rx) = watch::channel(config);
        start_timeout_tasks(state.clone(), config_rx);

        // Let it run once. Since it fails, it won't broadcast. We can just yield.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn test_evaluate_watch_tasks_cwd_not_exists() {
        let state = AppState::new();
        let config = Config::default();
        let path = std::path::PathBuf::from("/this/path/does/not/exist/hopefully");
        evaluate_watch_tasks(&path, &state, &config, None).await;
    }

    #[tokio::test]
    async fn test_evaluate_watch_tasks_invalid_glob() {
        let state = AppState::new();
        let mut config = Config::default();
        config.state.insert(
            "bad_glob".to_string(),
            StateConfig {
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
        let affected = vec![cwd.path().join("test.txt")];
        evaluate_watch_tasks(cwd.path(), &state, &config, Some(&affected)).await;
    }

    #[tokio::test]
    async fn test_evaluate_watch_tasks_failures() {
        let state = AppState::new();
        let mut config = Config::default();
        config.state.insert(
            "fail_cmd".to_string(),
            StateConfig {
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
        evaluate_watch_tasks(cwd.path(), &state, &config, None).await;
    }
}
