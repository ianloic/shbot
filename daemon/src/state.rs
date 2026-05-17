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

use crate::config::{Config, StateId};
use crate::runner::evaluate_scoped_state;
use crate::watcher::SyncWatcher;
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
    pub last_state: HashMap<StateId, String>,
}

#[derive(Debug)]
pub struct ScopedState {
    pub name: StateId,
    pub scope_root: PathBuf,
    pub subscribers: RwLock<HashSet<Pid>>,
    pub last_value: RwLock<Option<String>>,
}

pub struct DaemonState {
    pub clients: RwLock<HashMap<Pid, ClientInfo>>,
    pub git_latencies: RwLock<HashMap<PathBuf, u64>>,
    pub last_trigger_refresh_micros: AtomicU64,
    pub scoped_states: RwLock<HashMap<(StateId, PathBuf), Arc<ScopedState>>>,
    #[cfg(test)]
    pub test_events_tx: tokio::sync::broadcast::Sender<()>,
}

impl DaemonState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            clients: RwLock::new(HashMap::new()),
            git_latencies: RwLock::new(HashMap::new()),
            last_trigger_refresh_micros: AtomicU64::new(0),
            scoped_states: RwLock::new(HashMap::new()),
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
        {
            tracing::info!("TEST EVENT SENT from broadcast_to_cwd");
            let _ = self.test_events_tx.send(());
        }
    }

    pub async fn remove_client(&self, pid: Pid) {
        let mut clients = self.clients.write().await;
        clients.remove(&pid);
        #[cfg(test)]
        {
            tracing::info!("TEST EVENT SENT from broadcast_to_cwd");
            let _ = self.test_events_tx.send(());
        }
    }

    pub async fn update_cwd(&self, pid: Pid, cwd: PathBuf) {
        let mut clients = self.clients.write().await;
        if let Some(client) = clients.get_mut(&pid) {
            client.cwd = cwd;
        }
        #[cfg(test)]
        {
            tracing::info!("TEST EVENT SENT from broadcast_to_cwd");
            let _ = self.test_events_tx.send(());
        }
    }

    pub async fn get_cwd(&self, pid: Pid) -> Option<PathBuf> {
        self.clients.read().await.get(&pid).map(|c| c.cwd.clone())
    }

    pub async fn get_status_dump(
        &self,
        num_monitors: usize,
        config: &crate::config::Config,
    ) -> String {
        let clients = self.clients.read().await;
        let mut dump = "shbot daemon status:\n".to_string();
        let git_hash = option_env!("SHBOT_GIT_HASH").unwrap_or("unknown");
        dump.push_str(&format!("  Version / Git Hash: {}\n", git_hash));
        dump.push_str(&format!("  Clients connected: {}\n", clients.len()));

        for (pid, info) in clients.iter() {
            dump.push_str(&format!("    - PID: {}\n", pid));
            dump.push_str(&format!("      CWD: {}\n", info.cwd.display()));
            dump.push_str(&format!("      Cached States: {}\n", info.last_state.len()));
            for (state_id, _payload) in info.last_state.iter() {
                dump.push_str(&format!("        - {}\n", state_id));
            }
        }

        dump.push_str("Defined States:\n");
        let scoped_states = self.scoped_states.read().await;
        for (state_id, _) in &config.state {
            dump.push_str(&format!("  - {}\n", state_id));

            let mut active_scopes = Vec::new();
            for ((key_id, scope_root), state) in scoped_states.iter() {
                if key_id == state_id {
                    active_scopes.push((scope_root, state));
                }
            }

            if active_scopes.is_empty() {
                dump.push_str("      (No active scopes)\n");
            } else {
                for (scope_root, state) in active_scopes {
                    let subs = state.subscribers.read().await;
                    dump.push_str(&format!("      Scope: {}\n", scope_root.display()));
                    if subs.is_empty() {
                        dump.push_str("        Subscribers: none\n");
                    } else {
                        let mut subs_list: Vec<_> =
                            subs.iter().map(|pid| pid.to_string()).collect();
                        subs_list.sort();
                        dump.push_str(&format!(
                            "        Subscribers: [{}]\n",
                            subs_list.join(", ")
                        ));
                    }
                }
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
        {
            let mut clients = self.clients.write().await;
            for client in clients.values_mut() {
                if query == "*" || query == "all" {
                    client.last_state.clear();
                } else {
                    client.last_state.remove(query);
                }
            }
        }
        {
            let scoped_states = self.scoped_states.read().await;
            for (key, state) in scoped_states.iter() {
                if query == "*" || query == "all" || key.0.0 == query {
                    *state.last_value.write().await = None;
                }
            }
        }
        #[cfg(test)]
        {
            tracing::info!("TEST EVENT SENT from invalidate");
            let _ = self.test_events_tx.send(());
        }
    }

    pub async fn handle_cd(
        &self,
        pid: Pid,
        path: PathBuf,
        watcher: Arc<SyncWatcher>,
        config: &Config,
    ) {
        let old_cwd = self.get_cwd(pid).await;
        self.update_cwd(pid, path.clone()).await;

        let mut new_scopes = HashSet::new();
        for (name, state_config) in &config.state {
            if let Some(root) = state_config.scope.evaluate(&path) {
                new_scopes.insert((name.clone(), root));
            }
        }

        let mut old_scopes = HashSet::new();
        if let Some(old) = &old_cwd {
            for (name, state_config) in &config.state {
                if let Some(root) = state_config.scope.evaluate(old) {
                    old_scopes.insert((name.clone(), root));
                }
            }
        }

        let to_add: Vec<_> = new_scopes.difference(&old_scopes).cloned().collect();
        let to_remove: Vec<_> = old_scopes.difference(&new_scopes).cloned().collect();

        let mut newly_created_scopes = Vec::new();
        let mut initial_payloads = Vec::new();

        {
            let mut scoped_states = self.scoped_states.write().await;
            for key in to_add {
                let state = scoped_states.entry(key.clone()).or_insert_with(|| {
                    newly_created_scopes.push(key.clone());
                    Arc::new(ScopedState {
                        name: key.0.clone(),
                        scope_root: key.1.clone(),
                        subscribers: RwLock::new(HashSet::new()),
                        last_value: RwLock::new(None),
                    })
                });
                state.subscribers.write().await.insert(pid);

                // If the state already has a value, send it to the client immediately
                if let Some(val) = state.last_value.read().await.as_ref() {
                    initial_payloads.push((key.0.clone(), val.clone()));
                }
            }

            for key in to_remove {
                let mut is_orphaned = false;
                if let Some(state) = scoped_states.get(&key) {
                    let mut subs = state.subscribers.write().await;
                    subs.remove(&pid);
                    if subs.is_empty() {
                        is_orphaned = true;
                    }
                }
                if is_orphaned {
                    scoped_states.remove(&key);
                    // Also unwatch
                    if let Some(state_config) = config.state.get(&key.0) {
                        if state_config.invalidate.watch.is_some()
                            || state_config.invalidate.pwd.unwrap_or(false)
                        {
                            // Check if any OTHER active state is using this target before unwatching
                            let target = &key.1;
                            let mut keep = false;
                            for (other_key, _) in scoped_states.iter() {
                                if other_key.1 == *target {
                                    if let Some(sc) = config.state.get(&other_key.0) {
                                        if sc.invalidate.watch.is_some()
                                            || sc.invalidate.pwd.unwrap_or(false)
                                        {
                                            keep = true;
                                            break;
                                        }
                                    }
                                }
                            }
                            if !keep {
                                let watcher_clone = watcher.clone();
                                let target_clone = key.1.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    watcher_clone.unwatch(&target_clone);
                                })
                                .await;
                                self.git_latencies.write().await.remove(&key.1);
                            }
                        }
                    }
                }
            }
        }

        // Send any cached payloads
        if !initial_payloads.is_empty() {
            let mut clients = self.clients.write().await;
            if let Some(client) = clients.get_mut(&pid) {
                for (name, payload) in initial_payloads {
                    let msg = format!("update {} {}\n", name, payload);
                    client.last_state.insert(name, payload);
                    let _ = client.sender.try_send(msg);
                }
            }
        }

        // For newly created scopes, we need to watch them and evaluate them immediately
        for key in newly_created_scopes {
            if let Some(state_config) = config.state.get(&key.0) {
                if state_config.invalidate.watch.is_some()
                    || state_config.invalidate.pwd.unwrap_or(false)
                {
                    let watcher_clone = watcher.clone();
                    let target = key.1.clone();
                    let reasons = vec![key.0.0.clone()];
                    let _ = tokio::task::spawn_blocking(move || {
                        watcher_clone.watch(&target, &reasons);
                    })
                    .await;
                }

                // Trigger initial evaluation
                let start_trigger = std::time::Instant::now();
                evaluate_scoped_state(&key.0, &key.1, self, config).await;
                self.last_trigger_refresh_micros.store(
                    start_trigger.elapsed().as_micros() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        }
    }

    pub async fn handle_client_disconnect(
        &self,
        pid: Pid,
        watcher: Arc<SyncWatcher>,
        config: &Config,
    ) {
        let old_cwd = self.get_cwd(pid).await;
        self.remove_client(pid).await;
        if let Some(old) = old_cwd {
            let mut old_scopes = HashSet::new();
            for (name, state_config) in &config.state {
                if let Some(root) = state_config.scope.evaluate(&old) {
                    old_scopes.insert((name.clone(), root));
                }
            }

            let mut scoped_states = self.scoped_states.write().await;
            for key in old_scopes {
                let mut is_orphaned = false;
                if let Some(state) = scoped_states.get(&key) {
                    let mut subs = state.subscribers.write().await;
                    subs.remove(&pid);
                    if subs.is_empty() {
                        is_orphaned = true;
                    }
                }
                if is_orphaned {
                    scoped_states.remove(&key);
                    if let Some(state_config) = config.state.get(&key.0) {
                        if state_config.invalidate.watch.is_some()
                            || state_config.invalidate.pwd.unwrap_or(false)
                        {
                            let target = &key.1;
                            let mut keep = false;
                            for (other_key, _) in scoped_states.iter() {
                                if other_key.1 == *target {
                                    if let Some(sc) = config.state.get(&other_key.0) {
                                        if sc.invalidate.watch.is_some()
                                            || sc.invalidate.pwd.unwrap_or(false)
                                        {
                                            keep = true;
                                            break;
                                        }
                                    }
                                }
                            }
                            if !keep {
                                let watcher_clone = watcher.clone();
                                let target_clone = key.1.clone();
                                let _ = tokio::task::spawn_blocking(move || {
                                    watcher_clone.unwatch(&target_clone);
                                })
                                .await;
                                self.git_latencies.write().await.remove(&key.1);
                            }
                        }
                    }
                }
            }
        }
    }

    pub async fn update_scoped_state(&self, name: &StateId, scope_root: &Path, payload: &str) {
        let clients_to_notify = {
            let scoped_states = self.scoped_states.read().await;
            if let Some(state) = scoped_states.get(&(name.clone(), scope_root.to_path_buf())) {
                let mut last_val = state.last_value.write().await;
                if last_val.as_deref() == Some(payload) {
                    return;
                }
                *last_val = Some(payload.to_string());
                state.subscribers.read().await.clone()
            } else {
                return;
            }
        };

        let msg = format!("update {} {}\n", name, payload);
        let mut clients = self.clients.write().await;
        for pid in clients_to_notify {
            if let Some(client) = clients.get_mut(&pid) {
                client.last_state.insert(name.clone(), payload.to_string());
                let _ = client.sender.try_send(msg.clone());
            }
        }
        #[cfg(test)]
        {
            let _ = self.test_events_tx.send(());
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

        // manually set up a scoped state
        state.scoped_states.write().await.insert(
            (StateId::git(), path.clone()),
            Arc::new(ScopedState {
                name: StateId::git(),
                scope_root: path.clone(),
                subscribers: tokio::sync::RwLock::new([1].into()),
                last_value: tokio::sync::RwLock::new(None),
            }),
        );
        state.scoped_states.write().await.insert(
            (StateId::from("other"), path.clone()),
            Arc::new(ScopedState {
                name: StateId::from("other"),
                scope_root: path.clone(),
                subscribers: tokio::sync::RwLock::new([1].into()),
                last_value: tokio::sync::RwLock::new(None),
            }),
        );

        // 1. Initial send
        state
            .update_scoped_state(&StateId::git(), &path, "initial payload")
            .await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, "update git initial payload\n");

        // 2. Duplicate send (should be cached, not sent)
        state
            .update_scoped_state(&StateId::git(), &path, "initial payload")
            .await;
        assert!(
            rx.try_recv().is_err(),
            "Duplicate payload should not be sent"
        );

        // 3. Different payload (should be sent)
        state
            .update_scoped_state(&StateId::git(), &path, "new payload")
            .await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, "update git new payload\n");

        // 4. Invalidate specific query
        state.invalidate("git").await;
        state
            .update_scoped_state(&StateId::git(), &path, "new payload")
            .await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(
            msg, "update git new payload\n",
            "Payload should be sent again after invalidation"
        );

        // 5. Invalidate all
        state
            .update_scoped_state(&StateId::from("other"), &path, "something")
            .await;
        let _ = rx.recv().await.unwrap();
        state.invalidate("*").await;

        state
            .update_scoped_state(&StateId::from("other"), &path, "something")
            .await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(
            msg, "update other something\n",
            "Payload should be sent again after wildcard invalidation"
        );
    }

    #[tokio::test]
    async fn test_status_dump_with_latencies() {
        let state = DaemonState::new();
        state
            .git_latencies
            .write()
            .await
            .insert(PathBuf::from("/tmp/myrepo"), 1500);

        let config = crate::config::Config::default();
        let dump = state.get_status_dump(2, &config).await;
        assert!(dump.contains("Git Monitor Latencies:"));
        assert!(dump.contains("/tmp/myrepo"));
        assert!(dump.contains("1.5 ms"));
    }
}
