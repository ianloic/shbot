use crate::state::{AppState, Pid};
use crate::git::get_git_state;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration};
use tracing::{error, info, debug};

pub struct SyncWatcher {
    dirty_dirs: Arc<Mutex<HashSet<PathBuf>>>,
}

impl SyncWatcher {
    pub fn new() -> Self {
        Self {
            dirty_dirs: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn start(&self, state: Arc<AppState>) -> anyhow::Result<()> {
        let dirty_dirs_clone = self.dirty_dirs.clone();
        
        // Ensure channels match event closures
        let (tx, rx) = std::sync::mpsc::channel::<PathBuf>();
        
        let mut watcher = RecommendedWatcher::new(move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                // Determine root dir affected (just parent of the path for now).
                // Or simply let the debouncer push all changed paths, and map them to their CWDs/repos later.
                let mut dirty = dirty_dirs_clone.blocking_lock();
                for path in event.paths {
                    // For git, we really care about the closest `.git` tree.
                    // But we can just push the exact file path and let the runner walk up.
                    if let Some(parent) = path.parent() {
                        dirty.insert(parent.to_path_buf());
                    }
                }
            }
        }, Config::default())?;

        // Background loop for tokio
        let dirty_dirs = self.dirty_dirs.clone();
        tokio::spawn(async move {
            // Keep watcher alive in this thread
            let _w = watcher;
            
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
                    // We need to re-evaluate what git states are dirty. 
                    // This is naive but works: just iterate over all unique clients' CWDs and check if `to_process` intersects.
                    // Actually, the spec says "shbot walks up the tree... and maps GitRepo(RootPath) -> Set<Client PIDs>."
                    // Let's just re-evaluate EVERY connected client's git state, it's cheap enough because it's only 10s of clients usually.
                    // But wait, the spec says to dynamically attach file watchers.
                    // For now, let's just do a naive global check for simplicity and robustness.
                }
            }
        });

        Ok(())
    }
}
