use crate::config::Config;
use crate::git::get_git_state;
use crate::runner::evaluate_file_triggers;
use crate::state::AppState;
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};

pub struct SyncWatcher {
    dirty_dirs: Arc<Mutex<HashSet<PathBuf>>>,
}

impl SyncWatcher {
    pub fn new() -> Self {
        Self {
            dirty_dirs: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn start(&self, state: Arc<AppState>, config: Arc<Config>) -> anyhow::Result<()> {
        let dirty_dirs_clone = self.dirty_dirs.clone();

        let watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res {
                    let mut dirty = dirty_dirs_clone.blocking_lock();
                    for path in event.paths {
                        if let Some(parent) = path.parent() {
                            dirty.insert(parent.to_path_buf());
                        }
                    }
                }
            },
            NotifyConfig::default(),
        )?;

        let dirty_dirs = self.dirty_dirs.clone();
        tokio::spawn(async move {
            // Keep watcher alive in this thread
            let _w = watcher; // TODO: Dynamically update watcher rules instead of global watch

            loop {
                sleep(Duration::from_millis(150)).await;
                let mut to_process = Vec::new();
                {
                    let mut dirty = dirty_dirs.lock().await;
                    if !dirty.is_empty() {
                        to_process = dirty.drain().collect();
                    }
                }

                if !to_process.is_empty() {
                    let unique_cwds = state.get_unique_cwds().await;
                    for cwd in unique_cwds {
                        if let Ok(Some((_, git_state))) = get_git_state(&cwd) {
                            state
                                .broadcast_to_cwd(&cwd, "git", &git_state.to_payload())
                                .await;
                        }

                        evaluate_file_triggers(&cwd, &state, &config).await;
                    }
                }
            }
        });

        Ok(())
    }
}
