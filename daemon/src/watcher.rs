use crate::config::Config;
use crate::error::Result;
use crate::git::get_git_state;
use crate::runner::evaluate_file_triggers;
use crate::state::AppState;
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};

use std::sync::Mutex as StdMutex;
use tracing::{info, warn};

pub struct SyncWatcher {
    dirty_dirs: Arc<Mutex<HashSet<PathBuf>>>,
    watcher: StdMutex<Option<RecommendedWatcher>>,
}

impl SyncWatcher {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            dirty_dirs: Arc::new(Mutex::new(HashSet::new())),
            watcher: StdMutex::new(None),
        })
    }

    pub fn start(
        &self,
        state: Arc<AppState>,
        config_rx: tokio::sync::watch::Receiver<Config>,
    ) -> Result<()> {
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

        *self.watcher.lock().unwrap() = Some(watcher);

        let dirty_dirs = self.dirty_dirs.clone();
        tokio::spawn(async move {
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
                    let config = config_rx.borrow().clone();
                    for cwd in unique_cwds {
                        // Check if ANY dirty dir is the cwd itself or a subdirectory of it
                        let affected = to_process
                            .iter()
                            .any(|dirty_dir| dirty_dir.starts_with(&cwd));

                        if affected {
                            if let Ok(Some((_, git_state))) = get_git_state(&cwd) {
                                state
                                    .broadcast_to_cwd(&cwd, "git", &git_state.to_payload())
                                    .await;
                            }

                            evaluate_file_triggers(&cwd, &state, &config).await;
                        }
                    }
                }
            }
        });

        Ok(())
    }

    pub fn watch(&self, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }
        if let Some(w) = self.watcher.lock().unwrap().as_mut() {
            if let Err(e) = w.watch(path, notify::RecursiveMode::Recursive) {
                warn!("Failed to watch {:?}: {}", path, e);
            } else {
                info!("Watching path: {:?}", path);
            }
        }
    }

    pub fn unwatch(&self, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }
        if let Some(w) = self.watcher.lock().unwrap().as_mut() {
            if let Err(e) = w.unwatch(path) {
                // Ignore completely unwatched/not found errors safely without failing
                let e_str = e.to_string();
                if !e_str.contains("No watch found") && !e_str.contains("NotFound") {
                    warn!("Failed to unwatch {:?}: {}", path, e);
                }
            } else {
                info!("Unwatching path: {:?}", path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_sync_watcher_watch_unwatch() {
        // Here we test that the methods don't panic or crash when the inner
        // watcher isn't initialized yet (it should fail gracefully) or when
        // dummy paths are passed.
        let watcher = SyncWatcher::new();
        let path = PathBuf::from("/tmp");

        // Before start() is called, the watcher is None. It should not panic.
        watcher.watch(&path);
        watcher.unwatch(&path);

        // We can't trivially call start() here without a full Tokio runtime
        // and initialized configs, but ensuring watch/unwatch don't crash
        // in uninitialized states is critical for race condition safety!
    }
}
