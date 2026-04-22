use crate::config::{Config, TriggerConfig};
use crate::state::AppState;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{Duration, sleep};
use tracing::{error, info};

pub fn start_periodic_triggers(state: Arc<AppState>, config: Arc<Config>) {
    for (name, trigger) in &config.triggers {
        if let TriggerConfig::Periodic {
            interval_sec,
            command,
        } = trigger
        {
            let name = name.clone();
            let command = command.clone();
            let interval = Duration::from_secs(*interval_sec);
            let state = state.clone();

            tokio::spawn(async move {
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
                                let stdout =
                                    String::from_utf8_lossy(&output.stdout).trim().to_string();
                                state.broadcast_all(&name, &stdout).await;
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
        }
    }
}

pub async fn evaluate_file_triggers(cwd: &std::path::Path, state: &AppState, config: &Config) {
    // Only run if the path exists
    if !cwd.exists() {
        return;
    }

    for (name, trigger) in &config.triggers {
        if let TriggerConfig::FileChange {
            watch: _watch_pattern,
            command,
        } = trigger
        {
            // For now, naive execute in the given CWD
            let name = name.clone();
            let command = command.clone();
            // Actually, we can just broadcast_to_cwd

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
