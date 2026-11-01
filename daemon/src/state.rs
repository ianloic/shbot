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

use crate::config::{Config, StateId, StateSource};
use crate::protocol::format_update_message;
use crate::runner::evaluate_scoped_state;
use crate::watcher::SyncWatcher;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
#[cfg(test)]
use tokio::sync::broadcast;
use tokio::sync::{RwLock, mpsc, watch};
use tracing::debug;
#[cfg(test)]
use tracing::info;

pub type Pid = u32;

#[derive(Debug)]
pub struct ClientInfo {
    pub cwd: PathBuf,
    pub sender: mpsc::Sender<String>,
    pub last_state: HashMap<StateId, String>,
}

#[derive(Debug)]
pub struct ScopedState {
    pub subscribers: RwLock<HashSet<Pid>>,
    pub last_value: RwLock<Option<String>>,
    pub cancel_tx: watch::Sender<bool>,
}

impl ScopedState {
    pub fn new(subscribers: HashSet<Pid>) -> Arc<Self> {
        let (cancel_tx, _) = watch::channel(false);
        Arc::new(Self {
            subscribers: RwLock::new(subscribers),
            last_value: RwLock::new(None),
            cancel_tx,
        })
    }

    pub fn subscribe_cancel(&self) -> watch::Receiver<bool> {
        self.cancel_tx.subscribe()
    }

    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }
}

pub struct DaemonState {
    pub clients: RwLock<HashMap<Pid, ClientInfo>>,
    pub git_latencies: RwLock<HashMap<PathBuf, u64>>,
    pub last_trigger_refresh_micros: AtomicU64,
    pub scoped_states: RwLock<HashMap<(StateId, PathBuf), Arc<ScopedState>>>,
    pub in_flight: RwLock<HashMap<(StateId, PathBuf), bool>>,
    #[cfg(test)]
    pub test_events_tx: broadcast::Sender<()>,
}

impl DaemonState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            clients: RwLock::new(HashMap::new()),
            git_latencies: RwLock::new(HashMap::new()),
            last_trigger_refresh_micros: AtomicU64::new(0),
            scoped_states: RwLock::new(HashMap::new()),
            in_flight: RwLock::new(HashMap::new()),
            #[cfg(test)]
            test_events_tx: broadcast::channel(100).0,
        })
    }

    pub async fn begin_evaluation(&self, name: &StateId, root: &Path) -> bool {
        let mut in_flight = self.in_flight.write().await;
        let key = (name.clone(), root.to_path_buf());
        if let Some(rerun_pending) = in_flight.get_mut(&key) {
            *rerun_pending = true;
            false
        } else {
            in_flight.insert(key, false);
            true
        }
    }

    pub async fn finish_evaluation_step(&self, name: &StateId, root: &Path) -> bool {
        let mut in_flight = self.in_flight.write().await;
        let key = (name.clone(), root.to_path_buf());
        if let Some(rerun_pending) = in_flight.get_mut(&key) {
            if *rerun_pending {
                *rerun_pending = false;
                return true;
            }
            in_flight.remove(&key);
        }
        false
    }

    pub async fn end_evaluation(&self, name: &StateId, root: &Path) {
        self.in_flight
            .write()
            .await
            .remove(&(name.clone(), root.to_path_buf()));
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
            info!("TEST EVENT SENT from broadcast_to_cwd");
            let _ = self.test_events_tx.send(());
        }
    }

    pub async fn remove_client(&self, pid: Pid) {
        let mut clients = self.clients.write().await;
        clients.remove(&pid);
        #[cfg(test)]
        {
            info!("TEST EVENT SENT from broadcast_to_cwd");
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
            info!("TEST EVENT SENT from broadcast_to_cwd");
            let _ = self.test_events_tx.send(());
        }
    }

    #[cfg(test)]
    pub async fn get_cwd(&self, pid: Pid) -> Option<PathBuf> {
        self.clients.read().await.get(&pid).map(|c| c.cwd.clone())
    }

    pub async fn get_status_dump(&self, num_monitors: usize, config: &Config) -> String {
        let clients = self.clients.read().await;
        let mut dump = "shbot daemon status:\n".to_string();
        let version = env!("CARGO_PKG_VERSION");
        let git_hash = option_env!("SHBOT_GIT_HASH").unwrap_or("unknown");
        dump.push_str(&format!("  Daemon Version: {}\n", version));
        dump.push_str(&format!("  Version / Git Hash: {}\n", git_hash));
        dump.push_str(&format!("  Daemon PID: {}\n", process::id()));
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
                if query == "*"
                    || query == "all"
                    || key.0.0 == query
                    || key.1.to_string_lossy() == query
                {
                    *state.last_value.write().await = None;
                }
            }
        }
        #[cfg(test)]
        {
            info!("TEST EVENT SENT from invalidate");
            let _ = self.test_events_tx.send(());
        }
    }

    pub async fn handle_config_reload(
        self: &Arc<Self>,
        watcher: Arc<SyncWatcher>,
        config: &Config,
    ) {
        let client_cwds: Vec<(Pid, PathBuf)> = {
            let clients = self.clients.read().await;
            clients
                .iter()
                .filter(|(_, info)| !info.cwd.as_os_str().is_empty())
                .map(|(&pid, info)| (pid, info.cwd.clone()))
                .collect()
        };
        for (pid, cwd) in client_cwds {
            self.handle_cd(pid, cwd, watcher.clone(), config).await;
        }
    }

    pub async fn handle_cd(
        self: &Arc<Self>,
        pid: Pid,
        path: PathBuf,
        watcher: Arc<SyncWatcher>,
        config: &Config,
    ) {
        debug!("Handling cd to {:?} for PID {}", path, pid);
        self.update_cwd(pid, path.clone()).await;

        let mut new_scopes = HashSet::new();
        for (name, state_config) in &config.state {
            if let Some(root) = state_config.evaluate_scope(&path) {
                new_scopes.insert((name.clone(), root));
            }
        }

        let mut old_scopes = HashSet::new();
        {
            let scoped_states = self.scoped_states.read().await;
            for (key, state) in scoped_states.iter() {
                if state.subscribers.read().await.contains(&pid) {
                    old_scopes.insert(key.clone());
                }
            }
        }

        let to_add: Vec<_> = new_scopes.difference(&old_scopes).cloned().collect();
        let to_remove: Vec<_> = old_scopes.difference(&new_scopes).cloned().collect();
        let cleared_states: Vec<StateId> = to_remove
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|name| !new_scopes.iter().any(|(new_name, _)| new_name == name))
            .collect();

        let mut newly_created_scopes = Vec::new();
        let mut initial_payloads = Vec::new();
        let mut targets_to_unwatch = Vec::new();

        {
            let mut scoped_states = self.scoped_states.write().await;
            for key in to_add {
                let state = scoped_states.entry(key.clone()).or_insert_with(|| {
                    newly_created_scopes.push(key.clone());
                    ScopedState::new(HashSet::new())
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
                    if let Some(removed) = scoped_states.remove(&key) {
                        removed.cancel();
                    }
                    if let Some(state_config) = config.state.get(&key.0)
                        && state_config.should_watch()
                    {
                        // Check if any OTHER active state is using this target before unwatching
                        let target = &key.1;
                        let mut keep = false;
                        for (other_key, _) in scoped_states.iter() {
                            if other_key.1 == *target
                                && let Some(sc) = config.state.get(&other_key.0)
                                && sc.should_watch()
                            {
                                keep = true;
                                break;
                            }
                        }
                        if !keep {
                            targets_to_unwatch.push(key.1);
                        }
                    }
                }
            }
        }

        for target in targets_to_unwatch {
            if self.has_active_watch_scope(&target, config).await {
                continue;
            }
            let watcher_clone = watcher.clone();
            let target_clone = target.clone();
            let _ = tokio::task::spawn_blocking(move || {
                watcher_clone.unwatch(&target_clone);
            })
            .await;
            self.git_latencies.write().await.remove(&target);
        }

        // Clear any states that are no longer in scope and send any cached payloads
        if !cleared_states.is_empty() || !initial_payloads.is_empty() {
            let mut clients = self.clients.write().await;
            if let Some(client) = clients.get_mut(&pid) {
                for name in cleared_states {
                    client.last_state.remove(&name);
                    let msg = format!("update {} \n", name);
                    let _ = client.sender.try_send(msg);
                }
                for (name, payload) in initial_payloads {
                    if client.last_state.get(&name).map(String::as_str) != Some(payload.as_str()) {
                        let msg = format_update_message(&name.0, &payload);
                        client.last_state.insert(name, payload);
                        let _ = client.sender.try_send(msg);
                    }
                }
            }
        }

        // Ensure every watch-enabled scope for this client has an active file monitor,
        // even if the scope already existed when this client entered.
        for key in &new_scopes {
            if let Some(state_config) = config.state.get(&key.0)
                && state_config.should_watch()
                && !watcher.is_watching(&key.1)
            {
                let watcher_clone = watcher.clone();
                let target = key.1.clone();
                let reasons = vec![key.0.0.clone()];
                let _ = tokio::task::spawn_blocking(move || {
                    watcher_clone.watch(&target, &reasons);
                })
                .await;
            }
        }

        // For newly created scopes, trigger initial evaluation immediately
        for key in newly_created_scopes {
            if let Some(state_config) = config.state.get(&key.0) {
                if matches!(state_config.source, StateSource::Command { .. }) {
                    let state_clone = Arc::clone(self);
                    let config_clone = config.clone();
                    tokio::spawn(async move {
                        let start_trigger = Instant::now();
                        evaluate_scoped_state(&key.0, &key.1, &state_clone, &config_clone).await;
                        state_clone.last_trigger_refresh_micros.store(
                            start_trigger.elapsed().as_micros() as u64,
                            Ordering::Relaxed,
                        );
                    });
                } else {
                    let start_trigger = Instant::now();
                    evaluate_scoped_state(&key.0, &key.1, self, config).await;
                    self.last_trigger_refresh_micros.store(
                        start_trigger.elapsed().as_micros() as u64,
                        Ordering::Relaxed,
                    );
                }
            }
        }
    }

    async fn has_active_watch_scope(&self, target: &Path, config: &Config) -> bool {
        let scoped_states = self.scoped_states.read().await;
        for (other_key, _) in scoped_states.iter() {
            if other_key.1 == target
                && let Some(sc) = config.state.get(&other_key.0)
                && sc.should_watch()
            {
                return true;
            }
        }
        false
    }

    pub async fn handle_client_disconnect(
        &self,
        pid: Pid,
        watcher: Arc<SyncWatcher>,
        config: &Config,
    ) {
        self.remove_client(pid).await;
        let mut old_scopes = HashSet::new();
        {
            let scoped_states = self.scoped_states.read().await;
            for (key, state) in scoped_states.iter() {
                if state.subscribers.read().await.contains(&pid) {
                    old_scopes.insert(key.clone());
                }
            }
        }

        let mut targets_to_unwatch = Vec::new();
        {
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
                    if let Some(removed) = scoped_states.remove(&key) {
                        removed.cancel();
                    }
                    if let Some(state_config) = config.state.get(&key.0)
                        && state_config.should_watch()
                    {
                        let target = &key.1;
                        let mut keep = false;
                        for (other_key, _) in scoped_states.iter() {
                            if other_key.1 == *target
                                && let Some(sc) = config.state.get(&other_key.0)
                                && sc.should_watch()
                            {
                                keep = true;
                                break;
                            }
                        }
                        if !keep {
                            targets_to_unwatch.push(key.1);
                        }
                    }
                }
            }
        }

        for target in targets_to_unwatch {
            if self.has_active_watch_scope(&target, config).await {
                continue;
            }
            let watcher_clone = watcher.clone();
            let target_clone = target.clone();
            let _ = tokio::task::spawn_blocking(move || {
                watcher_clone.unwatch(&target_clone);
            })
            .await;
            self.git_latencies.write().await.remove(&target);
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

        let msg = format_update_message(&name.0, payload);
        let mut clients = self.clients.write().await;
        for pid in clients_to_notify {
            if let Some(client) = clients.get_mut(&pid)
                && client.last_state.get(name).map(String::as_str) != Some(payload)
            {
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
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::tempdir;
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
        state
            .scoped_states
            .write()
            .await
            .insert((StateId::git(), path.clone()), ScopedState::new([1].into()));
        state.scoped_states.write().await.insert(
            (StateId::from("other"), path.clone()),
            ScopedState::new([1].into()),
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
    async fn test_update_scoped_state_escapes_newlines() {
        let state = DaemonState::new();
        let (tx, mut rx) = mpsc::channel(10);
        let path = PathBuf::from("/tmp/injection");

        state.add_client(1, path.clone(), tx).await;
        state.scoped_states.write().await.insert(
            (StateId::from("inj"), path.clone()),
            ScopedState::new([1].into()),
        );

        state
            .update_scoped_state(&StateId::from("inj"), &path, "x\nupdate evil forged")
            .await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, "update inj x%0Aupdate evil forged\n");

        state
            .update_scoped_state(&StateId::from("inj"), &path, "a\x1fb%25c")
            .await;
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg, "update inj a%1Fb%2525c\n");
    }

    #[tokio::test]
    async fn test_status_dump_with_latencies() {
        let state = DaemonState::new();
        state
            .git_latencies
            .write()
            .await
            .insert(PathBuf::from("/tmp/myrepo"), 1500);

        let config = Config::default();
        let dump = state.get_status_dump(2, &config).await;
        assert!(dump.contains("Git Monitor Latencies:"));
        assert!(dump.contains("/tmp/myrepo"));
        assert!(dump.contains("1.5 ms"));
    }

    #[tokio::test]
    async fn test_handle_cd_out_of_git_repo_clears_state() {
        let state = DaemonState::new();
        let watcher = SyncWatcher::new();
        let config = Config::default();
        let (tx, mut rx) = mpsc::channel(10);

        let dir = tempdir().unwrap();
        let repo_dir = dir.path().join("repo");
        let non_repo_dir = dir.path().join("outside");
        fs::create_dir_all(&repo_dir).unwrap();
        fs::create_dir_all(&non_repo_dir).unwrap();

        let status = Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(&repo_dir)
            .status()
            .unwrap();
        assert!(status.success());

        state.add_client(1, PathBuf::new(), tx).await;

        // 1. cd into the git repository
        state
            .handle_cd(1, repo_dir.clone(), watcher.clone(), &config)
            .await;
        let msg_in_repo = rx
            .try_recv()
            .expect("Expected git state update on entering repo");
        assert!(msg_in_repo.starts_with("update git "));
        assert!(msg_in_repo.contains(repo_dir.to_str().unwrap()));
        assert!(
            state
                .clients
                .read()
                .await
                .get(&1)
                .unwrap()
                .last_state
                .contains_key("git")
        );

        // 2. cd out of the git repository into a non-git directory
        state
            .handle_cd(1, non_repo_dir.clone(), watcher.clone(), &config)
            .await;
        let msg_out_of_repo = rx
            .try_recv()
            .expect("Expected empty git update on leaving repo");
        assert_eq!(msg_out_of_repo, "update git \n");
        assert!(
            !state
                .clients
                .read()
                .await
                .get(&1)
                .unwrap()
                .last_state
                .contains_key("git")
        );

        // 3. cd back into the git repository and ensure state is sent again
        state
            .handle_cd(1, repo_dir.clone(), watcher.clone(), &config)
            .await;
        let msg_reenter = rx
            .try_recv()
            .expect("Expected git state update on re-entering repo");
        assert_eq!(msg_reenter, msg_in_repo);
    }

    #[tokio::test]
    async fn test_status_dump_with_clients_and_active_scopes() {
        use crate::config::{StateConfig, StateInvalidate};
        use crate::scope::Scope;

        let state = DaemonState::new();
        let (tx, _rx) = mpsc::channel(10);
        let path = PathBuf::from("/tmp/repo");

        state.add_client(101, path.clone(), tx.clone()).await;
        state.add_client(202, path.clone(), tx).await;

        let active_scope = ScopedState::new([101, 202].into());
        *active_scope.last_value.write().await = Some("main".to_string());
        state
            .scoped_states
            .write()
            .await
            .insert((StateId::git(), path.clone()), active_scope);

        let empty_scope = ScopedState::new(HashSet::new());
        state
            .scoped_states
            .write()
            .await
            .insert((StateId::from("orphan"), path.clone()), empty_scope);

        state
            .update_scoped_state(&StateId::git(), &path, "main_updated")
            .await;

        let mut config = Config::default();
        config.state.insert(
            StateId::from("orphan"),
            StateConfig {
                scope: Scope::Cwd,
                source: StateSource::Command {
                    command: "echo orphan".to_string(),
                },
                invalidate: StateInvalidate::default(),
            },
        );
        let dump = state.get_status_dump(1, &config).await;
        assert!(dump.contains("- PID: 101"));
        assert!(dump.contains("Cached States: 1"));
        assert!(dump.contains("- git"));
        assert!(dump.contains("Subscribers: [101, 202]"));
        assert!(dump.contains("Subscribers: none"));
    }

    #[tokio::test]
    async fn test_handle_cd_initial_payloads_and_shared_watch_across_states_and_reload() {
        use crate::config::{StateConfig, StateInvalidate};
        use crate::scope::Scope;
        use tokio::sync::watch;

        let state = DaemonState::new();
        let watcher = SyncWatcher::new();
        let dir = tempdir().unwrap();
        let sub_dir = dir.path().join("sub");
        let outside_dir = dir.path().join("outside");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::create_dir_all(&outside_dir).unwrap();
        fs::write(dir.path().join(".marker_a"), "").unwrap();
        fs::write(dir.path().join(".marker_b"), "val_b").unwrap();

        let mut config = Config::default();
        config.state.remove("git");
        config.state.insert(
            StateId::from("state_a"),
            StateConfig {
                scope: Scope::Nearest(vec![".marker_a".to_string()]),
                source: StateSource::File {
                    file: ".marker_b".to_string(),
                },
                invalidate: StateInvalidate::default(),
            },
        );
        config.state.insert(
            StateId::from("state_b"),
            StateConfig {
                scope: Scope::Topmost(vec![".marker_b".to_string()]),
                source: StateSource::File {
                    file: ".marker_b".to_string(),
                },
                invalidate: StateInvalidate::default(),
            },
        );
        config.inject_implicit_states();
        config.state.remove("git");

        let (_config_tx, config_rx) = watch::channel(config.clone());
        watcher.start(state.clone(), config_rx).unwrap();

        let (tx1, mut rx1) = mpsc::channel(10);
        let (tx2, mut rx2) = mpsc::channel(10);
        state.add_client(1, PathBuf::new(), tx1).await;
        state.add_client(2, PathBuf::new(), tx2).await;

        // Client 1 enters sub_dir -> creates both state_a and state_b on dir.path()
        state
            .handle_cd(1, sub_dir.clone(), watcher.clone(), &config)
            .await;
        let msg1 = rx1.try_recv().unwrap();
        let msg2 = rx1.try_recv().unwrap();
        assert!(msg1.contains("val_b") && msg2.contains("val_b"));
        assert!(watcher.is_watching(dir.path()));

        // Client 2 enters sub_dir -> scopes already exist with cached last_value, so initial_payloads are sent immediately
        state
            .handle_cd(2, sub_dir.clone(), watcher.clone(), &config)
            .await;
        let c2_msg1 = rx2.try_recv().unwrap();
        let c2_msg2 = rx2.try_recv().unwrap();
        assert!(c2_msg1.contains("val_b") && c2_msg2.contains("val_b"));

        // Now add a nested .marker_a in sub_dir so state_a moves to sub_dir while state_b stays on dir.path()
        fs::write(sub_dir.join(".marker_a"), "").unwrap();
        fs::write(sub_dir.join(".marker_b"), "sub_val").unwrap();

        // Client 2 disconnects from sub_dir first so Client 1 is the sole subscriber on dir.path()
        state
            .handle_client_disconnect(2, watcher.clone(), &config)
            .await;
        assert!(watcher.is_watching(dir.path()));

        // Client 1 re-cds into sub_dir: state_a on dir.path() is orphaned and removed,
        // but state_b on dir.path() is still active so dir.path() MUST stay watched!
        state
            .handle_cd(1, sub_dir.clone(), watcher.clone(), &config)
            .await;
        assert!(
            watcher.is_watching(dir.path()),
            "dir.path() must remain watched because state_b still uses it"
        );

        // Now test handle_config_reload adding a new state for connected client 1
        let mut reloaded_config = config.clone();
        reloaded_config.state.insert(
            StateId::from("reloaded_file"),
            StateConfig {
                scope: Scope::Cwd,
                source: StateSource::File {
                    file: ".marker_b".to_string(),
                },
                invalidate: StateInvalidate::default(),
            },
        );
        state
            .handle_config_reload(watcher.clone(), &reloaded_config)
            .await;
        let mut found_reloaded = false;
        while let Ok(msg) = rx1.try_recv() {
            if msg.starts_with("update reloaded_file sub_val") {
                found_reloaded = true;
            }
        }
        assert!(
            found_reloaded,
            "handle_config_reload must evaluate newly added states for active clients"
        );

        // Finally disconnect client 1 and verify watches are cleaned up
        state
            .handle_client_disconnect(1, watcher.clone(), &reloaded_config)
            .await;
        assert_eq!(watcher.get_watch_count(), 0);
    }
}
