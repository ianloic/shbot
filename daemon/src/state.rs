use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

pub type Pid = u32;

#[derive(Debug)]
pub struct ClientInfo {
    pub cwd: PathBuf,
    pub sender: mpsc::Sender<String>,
    pub last_state: HashMap<String, String>,
}

pub struct AppState {
    pub clients: RwLock<HashMap<Pid, ClientInfo>>,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            clients: RwLock::new(HashMap::new()),
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
    }

    pub async fn remove_client(&self, pid: Pid) {
        let mut clients = self.clients.write().await;
        clients.remove(&pid);
    }

    pub async fn update_cwd(&self, pid: Pid, cwd: PathBuf) {
        let mut clients = self.clients.write().await;
        if let Some(client) = clients.get_mut(&pid) {
            client.cwd = cwd;
        }
    }

    pub async fn get_cwd(&self, pid: Pid) -> Option<PathBuf> {
        self.clients.read().await.get(&pid).map(|c| c.cwd.clone())
    }

    pub async fn is_cwd_active(&self, cwd: &Path) -> bool {
        self.clients.read().await.values().any(|c| c.cwd == cwd)
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
    }

    pub async fn get_unique_cwds(&self) -> HashSet<PathBuf> {
        let clients = self.clients.read().await;
        clients
            .values()
            .map(|c| c.cwd.clone())
            .filter(|p| p.components().count() > 0)
            .collect()
    }

    pub async fn get_client_count(&self) -> usize {
        let clients = self.clients.read().await;
        clients.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_cwd_tracking() {
        let state = AppState::new();
        let (tx, _rx) = mpsc::channel(1);

        let path1 = PathBuf::from("/tmp/foo");
        let path2 = PathBuf::from("/tmp/bar");

        // Add two clients
        state.add_client(100, path1.clone(), tx.clone()).await;
        state.add_client(200, path2.clone(), tx.clone()).await;

        assert_eq!(state.get_cwd(100).await, Some(path1.clone()));
        assert_eq!(state.get_cwd(200).await, Some(path2.clone()));
        assert_eq!(state.get_cwd(999).await, None);

        assert!(state.is_cwd_active(&path1).await);
        assert!(state.is_cwd_active(&path2).await);
        assert!(!state.is_cwd_active(&PathBuf::from("/tmp/baz")).await);

        // Client 1 moves to path 2
        state.update_cwd(100, path2.clone()).await;
        assert_eq!(state.get_cwd(100).await, Some(path2.clone()));

        assert!(!state.is_cwd_active(&path1).await); // Path 1 is no longer active
        assert!(state.is_cwd_active(&path2).await); // Path 2 has both clients

        // Client 1 disconnects
        state.remove_client(100).await;
        assert_eq!(state.get_cwd(100).await, None); // Should be gone

        assert!(state.is_cwd_active(&path2).await); // Client 2 still here

        // Client 2 disconnects
        state.remove_client(200).await;
        assert!(!state.is_cwd_active(&path2).await); // Nobody here
    }
}
