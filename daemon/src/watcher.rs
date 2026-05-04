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
use crate::git::get_git_state;
use crate::runner::evaluate_file_triggers;
use crate::state::AppState;
use notify::{Config as NotifyConfig, Event, RecommendedWatcher, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::time::{Duration, sleep};

use std::sync::Mutex as StdMutex;
use tracing::{info, warn};

pub struct SyncWatcher {
    watcher: StdMutex<Option<RecommendedWatcher>>,
    watched_paths: StdMutex<HashSet<PathBuf>>,
}

impl SyncWatcher {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            watcher: StdMutex::new(None),
            watched_paths: StdMutex::new(HashSet::new()),
        })
    }

    pub fn start(
        &self,
        state: Arc<AppState>,
        config_rx: tokio::sync::watch::Receiver<Config>,
    ) -> Result<()> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();

        let watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res {
                    // Ignore file read access to prevent infinite loops where
                    // reading the git status triggers a notify event.
                    if matches!(event.kind, notify::EventKind::Access(_)) {
                        return;
                    }
                    for path in event.paths {
                        let _ = tx.send(path);
                    }
                }
            },
            NotifyConfig::default(),
        )?;

        *self.watcher.lock().unwrap() = Some(watcher);

        tokio::spawn(async move {
            loop {
                // Wait for the first event (no CPU polling)
                let Some(first_path) = rx.recv().await else {
                    break;
                };

                // Debounce: Wait a brief moment to collect bursty events
                sleep(Duration::from_millis(150)).await;

                let mut to_process = HashSet::new();
                to_process.insert(first_path);

                // Drain the channel of any additional events that arrived
                while let Ok(path) = rx.try_recv() {
                    to_process.insert(path);
                }

                let sample_paths: Vec<_> = to_process.iter().take(5).collect();
                tracing::debug!(
                    "Watcher awoken, processing {} file events. Sample paths: {:?}",
                    to_process.len(),
                    sample_paths
                );

                let unique_cwds = state.get_unique_cwds().await;
                let config = config_rx.borrow().clone();
                for cwd in unique_cwds {
                    let git_root = if let Ok(Some((root, _))) = get_git_state(&cwd) {
                        Some(PathBuf::from(root))
                    } else {
                        None
                    };

                    // Check if ANY dirty path is the cwd itself or a subdirectory of it,
                    // or within the git_root (if applicable)
                    let affected_paths: Vec<PathBuf> = to_process
                        .iter()
                        .filter(|dirty_path| {
                            let match_cwd = dirty_path.starts_with(&cwd);
                            let match_git = git_root
                                .as_ref()
                                .map_or(false, |r| dirty_path.starts_with(r));
                            if match_cwd || match_git {
                                tracing::trace!(
                                    "Event path {:?} matched cwd or git root {:?}",
                                    dirty_path,
                                    cwd
                                );
                                true
                            } else {
                                false
                            }
                        })
                        .cloned()
                        .collect();

                    if !affected_paths.is_empty() {
                        tracing::debug!(
                            "Triggering get_git_state for {:?} due to {} affected paths",
                            cwd,
                            affected_paths.len()
                        );
                        if let Ok(Some((_, git_state))) = get_git_state(&cwd) {
                            state
                                .broadcast_to_cwd(&cwd, "git", &git_state.to_payload())
                                .await;
                        }

                        evaluate_file_triggers(&cwd, &state, &config, Some(&affected_paths)).await;
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

        let mut paths = self.watched_paths.lock().unwrap();
        if paths.contains(path) {
            return;
        }

        if let Some(w) = self.watcher.lock().unwrap().as_mut() {
            if let Err(e) = w.watch(path, notify::RecursiveMode::Recursive) {
                warn!("Failed to watch {:?}: {}", path, e);
            } else {
                paths.insert(path.to_path_buf());
                info!("Watching path: {:?}", path);
            }
        }
    }

    pub fn unwatch(&self, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }

        let mut paths = self.watched_paths.lock().unwrap();
        if !paths.contains(path) {
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
                paths.remove(path);
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
