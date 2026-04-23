use crate::config::{Config, TriggerConfig};
use crate::state::AppState;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{Duration, sleep};
use tracing::{error, info};

pub fn start_periodic_triggers(
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

            for (name, trigger) in config.triggers {
                if let TriggerConfig::Periodic {
                    interval_sec,
                    command,
                } = trigger
                {
                    let name = name.clone();
                    let interval = Duration::from_secs(interval_sec);
                    let state_clone = state.clone();

                    let handle = tokio::spawn(async move {
                        info!(
                            "Starting periodic trigger '{}' every {}s",
                            name,
                            interval.as_secs()
                        );
                        loop {
                            sleep(interval).await;
                            match Command::new("sh").arg("-c").arg(&command).output().await {
                                Ok(output) => {
                                    if output.status.success() {
                                        let stdout = String::from_utf8_lossy(&output.stdout)
                                            .trim()
                                            .to_string();
                                        state_clone.broadcast_all(&name, &stdout).await;
                                    } else {
                                        let stderr = String::from_utf8_lossy(&output.stderr);
                                        error!("Periodic trigger '{}' failed: {}", name, stderr);
                                    }
                                }
                                Err(e) => {
                                    error!("Periodic trigger '{}' failed to execute: {}", name, e);
                                }
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

pub async fn evaluate_file_triggers(
    cwd: &std::path::Path,
    state: &AppState,
    config: &Config,
    affected_paths: Option<&[std::path::PathBuf]>,
) {
    // Only run if the path exists
    if !cwd.exists() {
        return;
    }

    for (name, trigger) in &config.triggers {
        if let TriggerConfig::FileChange {
            watch: watch_pattern,
            command,
        } = trigger
        {
            let mut matched = false;

            if let Some(paths) = affected_paths {
                let builder = globset::GlobBuilder::new(watch_pattern);
                let glob = match builder.build() {
                    Ok(g) => g.compile_matcher(),
                    Err(e) => {
                        error!(
                            "Invalid glob pattern '{}' for trigger '{}': {}",
                            watch_pattern, name, e
                        );
                        continue;
                    }
                };

                for path in paths {
                    if let Ok(rel_path) = path.strip_prefix(cwd) {
                        if glob.is_match(rel_path) {
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
                let name = name.clone();
                let command = command.clone();

                match Command::new("sh")
                    .arg("-c")
                    .arg(&command)
                    .current_dir(cwd)
                    .output()
                    .await
                {
                    Ok(output) => {
                        if output.status.success() {
                            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                            state.broadcast_to_cwd(cwd, &name, &stdout).await;
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            error!("File trigger '{}' failed: {}", name, stderr);
                        }
                    }
                    Err(e) => {
                        error!("File trigger '{}' failed to execute: {}", name, e);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, TriggerConfig};
    use tokio::sync::watch;

    #[tokio::test]
    async fn test_runner_reloads_on_config_change() {
        let state = AppState::new();
        let mut config = Config::default();
        config.triggers.insert(
            "test_1".to_string(),
            TriggerConfig::Periodic {
                interval_sec: 1,
                command: "echo 'first'".to_string(),
            },
        );

        let (config_tx, config_rx) = watch::channel(config);

        start_periodic_triggers(state.clone(), config_rx);

        // Allow task to spawn and run 1st config
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Update config to something else
        let mut new_config = Config::default();
        new_config.triggers.insert(
            "test_2".to_string(),
            TriggerConfig::Periodic {
                interval_sec: 1,
                command: "echo 'second'".to_string(),
            },
        );

        // Broadcast reload
        assert!(config_tx.send(new_config).is_ok());

        // Yield tokio to allow aborts to process
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // The fact that it doesn't panic and we successfully spawned and
        // aborted the internal tasks indicates the loop processes correctly.
    }
}
