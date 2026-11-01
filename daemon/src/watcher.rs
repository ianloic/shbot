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
use notify::{
    Config as NotifyConfig, ErrorKind as NotifyErrorKind, Event, EventKind, RecommendedWatcher,
    RecursiveMode, Watcher,
};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, sleep};
use tracing::{debug, info, warn};

const MAX_WATCHED_PATHS: usize = 256;
const EVENT_CHANNEL_CAPACITY: usize = 1024;

const UNWATCHABLE_PREFIXES: [&str; 4] = ["/proc", "/sys", "/dev", "/run"];

fn is_watchable_root(path: &Path) -> bool {
    if path == Path::new("/") {
        return false;
    }
    !UNWATCHABLE_PREFIXES
        .iter()
        .any(|root| path.starts_with(root))
}

fn is_noisy_git_internal_path(path: &Path) -> bool {
    let mut saw_git = false;
    for comp in path.components() {
        if let Component::Normal(os) = comp {
            if saw_git && os == "objects" {
                return true;
            }
            if os == ".git" {
                saw_git = true;
            }
        }
    }
    false
}

fn create_recommended_watcher(tx: mpsc::Sender<PathBuf>) -> Result<RecommendedWatcher> {
    let watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                // Ignore file read access to prevent infinite loops where
                // reading the git status triggers a notify event.
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                for path in event.paths {
                    if is_noisy_git_internal_path(&path) {
                        continue;
                    }
                    // Drop events when the queue is full rather than
                    // growing without bound during event storms.
                    if tx.try_send(path).is_err() {
                        debug!("Watcher event queue full; dropping event");
                    }
                }
            }
        },
        NotifyConfig::default(),
    )?;
    Ok(watcher)
}

pub struct SyncWatcher {
    watcher: StdMutex<Option<RecommendedWatcher>>,
    watched_paths: StdMutex<HashSet<PathBuf>>,
    event_tx: StdMutex<Option<mpsc::WeakSender<PathBuf>>>,
}

impl SyncWatcher {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            watcher: StdMutex::new(None),
            watched_paths: StdMutex::new(HashSet::new()),
            event_tx: StdMutex::new(None),
        })
    }

    pub fn start(&self, state: Arc<DaemonState>, config_rx: watch::Receiver<Config>) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<PathBuf>(EVENT_CHANNEL_CAPACITY);
        *self.event_tx.lock().unwrap() = Some(tx.downgrade());

        let watcher = create_recommended_watcher(tx)?;

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
                debug!(
                    "Watcher awoken, processing {} file events. Sample paths: {:?}",
                    to_process.len(),
                    sample_paths
                );

                let config = config_rx.borrow().clone();
                let affected_paths: Vec<PathBuf> = to_process.into_iter().collect();

                if !affected_paths.is_empty() {
                    let start_trigger = Instant::now();
                    evaluate_watch_tasks(&state, &config, Some(&affected_paths)).await;
                    state.last_trigger_refresh_micros.store(
                        start_trigger.elapsed().as_micros() as u64,
                        Ordering::Relaxed,
                    );
                }
            }
        });

        Ok(())
    }

    pub fn get_watch_count(&self) -> usize {
        self.watched_paths.lock().unwrap().len()
    }

    pub fn is_watching(&self, path: &Path) -> bool {
        self.watched_paths.lock().unwrap().contains(path)
    }

    pub fn watch(&self, path: &Path, reasons: &[String]) {
        if path.as_os_str().is_empty() {
            return;
        }

        if !is_watchable_root(path) {
            warn!("Refusing to recursively watch sensitive root {:?}", path);
            return;
        }

        let mut paths = self.watched_paths.lock().unwrap();
        if paths.contains(path) {
            return;
        }

        if paths.len() >= MAX_WATCHED_PATHS {
            warn!(
                "Watch limit of {} paths reached; not watching {:?}",
                MAX_WATCHED_PATHS, path
            );
            return;
        }

        if let Some(w) = self.watcher.lock().unwrap().as_mut() {
            if let Err(e) = w.watch(path, RecursiveMode::Recursive) {
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
        if !paths.remove(path) {
            return;
        }

        let mut watcher_guard = self.watcher.lock().unwrap();
        let mut needs_rebuild = false;
        if let Some(w) = watcher_guard.as_mut() {
            if let Err(e) = w.unwatch(path) {
                // notify's inotify backend removes the top-level watch on `path` first and then
                // iterates child watches; if any child directory under `path` was deleted while
                // watched, inotify_rm_watch returns EINVAL (os error 22) and aborts the loop,
                // leaking the remaining child watches and poisoning notify's internal descriptor
                // map. We always remove `path` from `watched_paths` so future `watch(path)` calls
                // succeed, and rebuild the watcher to reclaim any leaked child descriptors.
                let is_not_found = matches!(
                    e.kind,
                    NotifyErrorKind::WatchNotFound | NotifyErrorKind::PathNotFound
                );
                if !is_not_found {
                    debug!(
                        "Partial unwatch for {:?} ({}); rebuilding watcher for {} active paths",
                        path,
                        e,
                        paths.len()
                    );
                    needs_rebuild = true;
                }
            }
            info!("Stopped monitoring directory: {:?}", path);
        }

        if needs_rebuild
            && let Some(weak_tx) = self.event_tx.lock().unwrap().as_ref()
            && let Some(tx) = weak_tx.upgrade()
        {
            // Drop the old watcher first so its kernel inotify fd is closed immediately,
            // releasing all leaked child watch descriptors.
            *watcher_guard = None;
            match create_recommended_watcher(tx) {
                Ok(mut new_watcher) => {
                    for active_path in paths.iter() {
                        if let Err(e) = new_watcher.watch(active_path, RecursiveMode::Recursive) {
                            warn!(
                                "Failed to re-watch {:?} after watcher rebuild: {}",
                                active_path, e
                            );
                        }
                    }
                    *watcher_guard = Some(new_watcher);
                }
                Err(e) => {
                    warn!("Failed to rebuild watcher after unwatch error: {}", e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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

    #[test]
    fn test_noisy_git_internal_path_filter() {
        assert!(is_noisy_git_internal_path(Path::new(
            "/repo/.git/objects/ab/cd1234"
        )));
        assert!(!is_noisy_git_internal_path(Path::new(
            "/repo/.git/index.lock"
        )));
        assert!(!is_noisy_git_internal_path(Path::new(
            "/repo/.git/refs/heads/main.lock"
        )));
        assert!(!is_noisy_git_internal_path(Path::new("/repo/.git/index")));
        assert!(!is_noisy_git_internal_path(Path::new("/repo/.git/HEAD")));
        assert!(!is_noisy_git_internal_path(Path::new("/repo/src/main.rs")));
        assert!(!is_noisy_git_internal_path(Path::new("/repo/Cargo.lock")));
    }

    #[tokio::test]
    async fn test_unwatch_never_watched() {
        let watcher = SyncWatcher::new();
        let state = DaemonState::new();
        let (_config_tx, config_rx) = watch::channel(Config::default());

        watcher.start(state, config_rx).unwrap();

        let path = PathBuf::from("/tmp/nonexistent_shbot_test_path");
        // Try unwatching before watching
        watcher.unwatch(&path);

        assert_eq!(watcher.get_watch_count(), 0);
    }

    #[tokio::test]
    async fn test_watcher_channel_disconnect() {
        let watcher = SyncWatcher::new();
        let state = DaemonState::new();
        let (_config_tx, config_rx) = watch::channel(Config::default());

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

    #[tokio::test]
    async fn test_watcher_refuses_sensitive_roots() {
        let watcher = SyncWatcher::new();
        let state = DaemonState::new();
        let (_config_tx, config_rx) = watch::channel(Config::default());

        watcher.start(state, config_rx).unwrap();

        for root in [
            "/",
            "/proc",
            "/proc/self",
            "/sys",
            "/dev",
            "/dev/null",
            "/run",
        ] {
            watcher.watch(Path::new(root), &[]);
        }

        assert_eq!(watcher.get_watch_count(), 0);
    }

    #[tokio::test]
    async fn test_watcher_watch_limit() {
        let watcher = SyncWatcher::new();
        let state = DaemonState::new();
        let (_config_tx, config_rx) = watch::channel(Config::default());

        watcher.start(state, config_rx).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let mut paths = watcher.watched_paths.lock().unwrap();
        for i in 0..MAX_WATCHED_PATHS {
            paths.insert(temp.path().join(format!("dummy_{}", i)));
        }
        drop(paths);

        let extra = temp.path().join("extra");
        watcher.watch(&extra, &[]);
        assert_eq!(watcher.get_watch_count(), MAX_WATCHED_PATHS);
    }

    #[tokio::test]
    async fn test_unwatch_after_deleted_subdir_allows_rewatch() {
        let watcher = SyncWatcher::new();
        let state = DaemonState::new();
        let (_config_tx, config_rx) = watch::channel(Config::default());

        watcher.start(state, config_rx).unwrap();

        let temp = tempfile::tempdir().unwrap();
        let sub = temp.path().join("sub");
        fs::create_dir_all(&sub).unwrap();

        let temp2 = tempfile::tempdir().unwrap();
        let temp3 = tempfile::tempdir().unwrap();
        let temp3_path = temp3.path().to_path_buf();

        // Empty path and nonexistent path edge cases
        watcher.watch(Path::new(""), &[]);
        watcher.unwatch(Path::new(""));
        watcher.watch(&temp.path().join("does_not_exist"), &[]);

        watcher.watch(temp.path(), &["git".to_string()]);
        // Duplicate watch on same path should be a no-op
        watcher.watch(temp.path(), &["git".to_string()]);
        watcher.watch(temp2.path(), &["git".to_string()]);
        watcher.watch(&temp3_path, &["git".to_string()]);
        assert_eq!(watcher.get_watch_count(), 3);

        // Remove temp3 on disk while still in watched_paths so rebuild also exercises the re-watch error branch
        drop(temp3);

        // Delete the watched subdirectory so the kernel auto-removes its inotify watch
        // and notify's internal paths map retains a stale WatchDescriptor.
        fs::remove_dir_all(&sub).unwrap();

        // Unwatching the parent directory must still remove it from watched_paths
        // even if notify returns EINVAL (os error 22) for the deleted subdirectory,
        // and rebuild the watcher for remaining active paths.
        watcher.unwatch(temp.path());
        assert_eq!(
            watcher.get_watch_count(),
            2,
            "watched_paths must remove temp.path() while keeping other paths"
        );

        // Re-watching the parent directory must succeed again.
        watcher.watch(temp.path(), &["git".to_string()]);
        assert_eq!(watcher.get_watch_count(), 3);
    }
}
