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

use crate::runner::evaluate_watch_tasks;
use crate::state::DaemonState;
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
        state: Arc<DaemonState>,
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

                let config = config_rx.borrow().clone();
                let affected_paths: Vec<PathBuf> = to_process.into_iter().collect();

                if !affected_paths.is_empty() {
                    let start_trigger = std::time::Instant::now();
                    evaluate_watch_tasks(&state, &config, Some(&affected_paths)).await;
                    state.last_trigger_refresh_micros.store(
                        start_trigger.elapsed().as_micros() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
            }
        });

        Ok(())
    }

    pub fn get_watch_count(&self) -> usize {
        self.watched_paths.lock().unwrap().len()
    }

    pub fn watch(&self, path: &Path, reasons: &[String]) {
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
                info!(
                    "Started monitoring directory: {:?} (Reasons: {:?})",
                    path, reasons
                );
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
                info!("Stopped monitoring directory: {:?}", path);
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
        watcher.watch(&path, &[]);
        watcher.unwatch(&path);

        // We can't trivially call start() here without a full Tokio runtime
        // and initialized configs, but ensuring watch/unwatch don't crash
        // in uninitialized states is critical for race condition safety!
    }

    #[tokio::test]
    async fn test_unwatch_never_watched() {
        use crate::config::Config;
        use crate::state::DaemonState;

        let watcher = SyncWatcher::new();
        let state = DaemonState::new();
        let (_config_tx, config_rx) = tokio::sync::watch::channel(Config::default());

        watcher.start(state, config_rx).unwrap();

        let path = PathBuf::from("/tmp/nonexistent_shbot_test_path");
        // Try unwatching before watching
        watcher.unwatch(&path);

        assert_eq!(watcher.get_watch_count(), 0);
    }

    #[tokio::test]
    async fn test_watcher_channel_disconnect() {
        use crate::config::Config;
        use crate::state::DaemonState;

        let watcher = SyncWatcher::new();
        let state = DaemonState::new();
        let (_config_tx, config_rx) = tokio::sync::watch::channel(Config::default());

        watcher.start(state, config_rx).unwrap();

        // Let it start
        tokio::task::yield_now().await;

        // Force the inner watcher to drop by replacing it.
        // This will drop the `notify` watcher, which drops the `tx` sender,
        // causing the `rx.recv()` loop in the spawned task to return None and break cleanly.
        *watcher.watcher.lock().unwrap() = None;

        // Yield to allow the rx loop to exit
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }
}
