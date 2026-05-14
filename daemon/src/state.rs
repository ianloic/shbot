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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{RwLock, mpsc};

pub type Pid = u32;

#[derive(Debug)]
pub struct ClientInfo {
    pub cwd: PathBuf,
    pub sender: mpsc::Sender<String>,
    pub last_state: HashMap<String, String>,
}

pub struct DaemonState {
    pub clients: RwLock<HashMap<Pid, ClientInfo>>,
    pub git_latencies: RwLock<HashMap<PathBuf, u64>>,
    pub last_trigger_refresh_micros: AtomicU64,
    #[cfg(test)]
    pub test_events_tx: tokio::sync::broadcast::Sender<()>,
}

impl DaemonState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            clients: RwLock::new(HashMap::new()),
            git_latencies: RwLock::new(HashMap::new()),
            last_trigger_refresh_micros: AtomicU64::new(0),
            #[cfg(test)]
            test_events_tx: tokio::sync::broadcast::channel(100).0,
        })
    }

    pub async fn add_client(&self, pid: Pid, cwd: PathBuf, sender: mpsc::Sender<String>) {
        let mut clients = self.clients.write().await;
        clients.insert(
            pid,
            ClientInfo {
                cwd,
                sender,
                last_state: HashMap::new(),
            },
        );
        #[cfg(test)]
        let _ = self.test_events_tx.send(());
    }

    pub async fn remove_client(&self, pid: Pid) {
        let mut clients = self.clients.write().await;
        clients.remove(&pid);
        #[cfg(test)]
        let _ = self.test_events_tx.send(());
    }

    pub async fn update_cwd(&self, pid: Pid, cwd: PathBuf) {
        let mut clients = self.clients.write().await;
        if let Some(client) = clients.get_mut(&pid) {
            client.cwd = cwd;
        }
        #[cfg(test)]
        let _ = self.test_events_tx.send(());
    }

    pub async fn get_cwd(&self, pid: Pid) -> Option<PathBuf> {
        self.clients.read().await.get(&pid).map(|c| c.cwd.clone())
    }

    #[allow(dead_code)]
    pub async fn broadcast(&self, state_id: &str, payload: &str) {
        let msg = format!("update {} {}\n", state_id, payload);
        let mut clients = self.clients.write().await;
        for client in clients.values_mut() {
            if client.last_state.get(state_id) == Some(&payload.to_string()) {
                continue;
            }
            client
                .last_state
                .insert(state_id.to_string(), payload.to_string());
            let _ = client.sender.try_send(msg.clone());
        }
        #[cfg(test)]
        let _ = self.test_events_tx.send(());
    }

    pub async fn broadcast_all(&self, state_id: &str, payload: &str) {
        let mut clients = self.clients.write().await;
        let msg = format!("update {} {}\n", state_id, payload);
        for client in clients.values_mut() {
            if client.last_state.get(state_id) == Some(&payload.to_string()) {
                continue;
            }
            client
                .last_state
                .insert(state_id.to_string(), payload.to_string());
            let _ = client.sender.try_send(msg.clone());
        }
        #[cfg(test)]
        let _ = self.test_events_tx.send(());
    }
    pub async fn send_to(&self, pid: Pid, state_id: &str, payload: &str) {
        let msg = format!("update {} {}\n", state_id, payload);
        let mut clients = self.clients.write().await;
        if let Some(client) = clients.get_mut(&pid) {
            if client.last_state.get(state_id) == Some(&payload.to_string()) {
                return;
            }
            client
                .last_state
                .insert(state_id.to_string(), payload.to_string());
            let _ = client.sender.try_send(msg);
        }
        #[cfg(test)]
        let _ = self.test_events_tx.send(());
    }

    pub async fn broadcast_to_cwd(&self, cwd: &Path, state_id: &str, payload: &str) {
        let msg = format!("update {} {}\n", state_id, payload);
        let mut clients = self.clients.write().await;
        for client in clients.values_mut() {
            if client.cwd == cwd {
                if client.last_state.get(state_id) == Some(&payload.to_string()) {
                    continue;
                }
                client
                    .last_state
                    .insert(state_id.to_string(), payload.to_string());
                let _ = client.sender.try_send(msg.clone());
            }
        }
        #[cfg(test)]
        let _ = self.test_events_tx.send(());
    }

    pub async fn get_unique_cwds(&self) -> HashSet<PathBuf> {
        let clients = self.clients.read().await;
        clients
            .values()
            .map(|c| c.cwd.clone())
            .filter(|p| p.components().count() > 0)
            .collect()
    }

    pub async fn get_status_dump(&self, num_monitors: usize) -> String {
        let clients = self.clients.read().await;
        let mut dump = "shbot daemon status:\n".to_string();
        dump.push_str(&format!("  Clients connected: {}\n", clients.len()));

        for (pid, info) in clients.iter() {
            dump.push_str(&format!("    - PID: {}\n", pid));
            dump.push_str(&format!("      CWD: {}\n", info.cwd.display()));
            dump.push_str(&format!("      Cached States: {}\n", info.last_state.len()));
            for (state_id, _payload) in info.last_state.iter() {
                dump.push_str(&format!("        - {}\n", state_id));
            }
        }

        dump.push_str("State Collection:\n");
        dump.push_str(&format!("  Active File Monitors: {}\n", num_monitors));
        dump.push_str(&format!(
            "  Last File Trigger Refresh: {} ms\n",
            self.last_trigger_refresh_micros.load(Ordering::Relaxed) as f64 / 1000.0
        ));

        let git_latencies = self.git_latencies.read().await;
        if !git_latencies.is_empty() {
            dump.push_str("  Git Monitor Latencies:\n");
            for (path, micros) in git_latencies.iter() {
                dump.push_str(&format!(
                    "    - {}: {} ms\n",
                    path.display(),
                    *micros as f64 / 1000.0
                ));
            }
        }

        dump
    }

    pub async fn invalidate(&self, query: &str) {
        let mut clients = self.clients.write().await;
        for client in clients.values_mut() {
            if query == "*" || query == "all" {
                client.last_state.clear();
            } else {
                client.last_state.remove(query);
            }
        }
        #[cfg(test)]
        let _ = self.test_events_tx.send(());
    }

    async fn check_and_unwatch(
        &self,
        target: &PathBuf,
        watcher: &Arc<crate::watcher::SyncWatcher>,
        config: &crate::config::Config,
    ) {
        let active_cwds = self.get_unique_cwds().await;
        let mut keep = false;
        for active in active_cwds {
            for (_, state_config) in &config.state {
                if state_config.invalidate.watch.is_some()
                    || state_config.invalidate.pwd.unwrap_or(false)
                {
                    if let Some(scope_root) = state_config.scope.evaluate(&active) {
                        if scope_root == *target {
                            keep = true;
                            break;
                        }
                    }
                }
            }
            if keep {
                break;
            }
        }
        if !keep {
            tracing::debug!("Unwatching target {:?} as no active cwds use it", target);
            let watcher_clone = watcher.clone();
            let unwatch_target = target.clone();
            tokio::task::spawn_blocking(move || {
                watcher_clone.unwatch(&unwatch_target);
            });
            self.git_latencies.write().await.remove(target);
        } else {
            tracing::debug!(
                "Keeping watch on target {:?} as it is still used by another active cwd",
                target
            );
        }
    }

    pub async fn handle_cd(
        &self,
        pid: Pid,
        path: PathBuf,
        watcher: Arc<crate::watcher::SyncWatcher>,
        config: &crate::config::Config,
    ) {
        let old_cwd = self.get_cwd(pid).await;
        self.update_cwd(pid, path.clone()).await;

        let mut watch_targets = HashSet::new();
        for (_, state_config) in &config.state {
            if state_config.invalidate.watch.is_some()
                || state_config.invalidate.pwd.unwrap_or(false)
            {
                if let Some(root) = state_config.scope.evaluate(&path) {
                    watch_targets.insert(root);
                }
            }
        }

        for target in &watch_targets {
            let watcher_clone = watcher.clone();
            let watch_target_clone = target.clone();
            tokio::task::spawn_blocking(move || {
                watcher_clone.watch(&watch_target_clone);
            });
        }

        if let Some(old) = old_cwd {
            if old != path {
                let mut old_watch_targets = HashSet::new();
                for (_, state_config) in &config.state {
                    if state_config.invalidate.watch.is_some()
                        || state_config.invalidate.pwd.unwrap_or(false)
                    {
                        if let Some(root) = state_config.scope.evaluate(&old) {
                            old_watch_targets.insert(root);
                        }
                    }
                }

                for old_target in old_watch_targets {
                    if !watch_targets.contains(&old_target) {
                        self.check_and_unwatch(&old_target, &watcher, config).await;
                    }
                }
            }
        }

        let start_trigger = std::time::Instant::now();
        crate::runner::evaluate_watch_tasks(&path, self, config, None).await;
        self.last_trigger_refresh_micros.store(
            start_trigger.elapsed().as_micros() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub async fn handle_client_disconnect(
        &self,
        pid: Pid,
        watcher: Arc<crate::watcher::SyncWatcher>,
        config: &crate::config::Config,
    ) {
        let old_cwd = self.get_cwd(pid).await;
        self.remove_client(pid).await;
        if let Some(old) = old_cwd {
            let mut old_watch_targets = HashSet::new();
            for (_, state_config) in &config.state {
                if state_config.invalidate.watch.is_some()
                    || state_config.invalidate.pwd.unwrap_or(false)
                {
                    if let Some(root) = state_config.scope.evaluate(&old) {
                        old_watch_targets.insert(root);
                    }
                }
            }

            for old_target in old_watch_targets {
                self.check_and_unwatch(&old_target, &watcher, config).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_cwd_tracking() {
        let state = DaemonState::new();
        let (tx, _rx) = mpsc::channel(1);

        let path1 = PathBuf::from("/tmp/foo");
        let path2 = PathBuf::from("/tmp/bar");

        // Add two clients
        state.add_client(100, path1.clone(), tx.clone()).await;
        state.add_client(200, path2.clone(), tx.clone()).await;

        assert_eq!(state.get_cwd(100).await, Some(path1.clone()));
        assert_eq!(state.get_cwd(200).await, Some(path2.clone()));
        assert_eq!(state.get_cwd(999).await, None);

        // Client 1 moves to path 2
        state.update_cwd(100, path2.clone()).await;
        assert_eq!(state.get_cwd(100).await, Some(path2.clone()));

        // Client 1 disconnects
        state.remove_client(100).await;
        assert_eq!(state.get_cwd(100).await, None); // Should be gone

        // Client 2 disconnects
        state.remove_client(200).await;
    }

    #[tokio::test]
    async fn test_state_caching_and_invalidation() {
        let state = DaemonState::new();
        let (tx, mut rx) = mpsc::channel(10);
        let path = PathBuf::from("/tmp/test");

        state.add_client(1, path.clone(), tx).await;

        // 1. Initial send
        state.send_to(1, "git", "initial payload").await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, "update git initial payload\n");

        // 2. Duplicate send (should be cached, not sent)
        state.send_to(1, "git", "initial payload").await;
        assert!(
            rx.try_recv().is_err(),
            "Duplicate payload should not be sent"
        );

        // 3. Different payload (should be sent)
        state.send_to(1, "git", "new payload").await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, "update git new payload\n");

        // 4. Invalidate specific query
        state.invalidate("git").await;
        state.send_to(1, "git", "new payload").await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(
            msg, "update git new payload\n",
            "Payload should be sent again after invalidation"
        );

        // 5. Invalidate all
        state.send_to(1, "other", "something").await;
        let _ = rx.recv().await.unwrap();
        state.invalidate("*").await;

        state.send_to(1, "other", "something").await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(
            msg, "update other something\n",
            "Payload should be sent again after wildcard invalidation"
        );
    }

    #[tokio::test]
    async fn test_broadcast_functions() {
        let state = DaemonState::new();
        let (tx1, mut rx1) = mpsc::channel(10);
        let (tx2, mut rx2) = mpsc::channel(10);

        let path1 = PathBuf::from("/tmp/foo");
        let path2 = PathBuf::from("/tmp/bar");

        state.add_client(1, path1.clone(), tx1).await;
        state.add_client(2, path2.clone(), tx2).await;

        // test broadcast
        state.broadcast("msg1", "data1").await;
        assert_eq!(rx1.recv().await.unwrap(), "update msg1 data1\n");
        assert_eq!(rx2.recv().await.unwrap(), "update msg1 data1\n");

        // test broadcast cache hit
        state.broadcast("msg1", "data1").await;
        assert!(rx1.try_recv().is_err());
        assert!(rx2.try_recv().is_err());

        // test broadcast_all
        state.broadcast_all("msg2", "data2").await;
        assert_eq!(rx1.recv().await.unwrap(), "update msg2 data2\n");
        assert_eq!(rx2.recv().await.unwrap(), "update msg2 data2\n");

        // test broadcast_all cache hit
        state.broadcast_all("msg2", "data2").await;
        assert!(rx1.try_recv().is_err());
        assert!(rx2.try_recv().is_err());

        // test broadcast_to_cwd
        state.broadcast_to_cwd(&path1, "msg3", "data3").await;
        assert_eq!(rx1.recv().await.unwrap(), "update msg3 data3\n");
        assert!(rx2.try_recv().is_err());

        // test broadcast_to_cwd cache hit
        state.broadcast_to_cwd(&path1, "msg3", "data3").await;
        assert!(rx1.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_status_dump_with_latencies() {
        let state = DaemonState::new();
        state
            .git_latencies
            .write()
            .await
            .insert(PathBuf::from("/tmp/myrepo"), 1500);

        let dump = state.get_status_dump(2).await;
        assert!(dump.contains("Git Monitor Latencies:"));
        assert!(dump.contains("/tmp/myrepo"));
        assert!(dump.contains("1.5 ms"));
    }
}
