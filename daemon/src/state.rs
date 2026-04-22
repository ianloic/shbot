use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

pub type Pid = u32;

#[derive(Debug)]
#[allow(dead_code)]
pub struct ClientInfo {
    pub pid: Pid,
    pub cwd: PathBuf,
    pub sender: mpsc::Sender<String>,
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
        clients.insert(pid, ClientInfo { pid, cwd, sender });
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

    #[allow(dead_code)]
    pub async fn broadcast(&self, state_id: &str, payload: &str) {
        let msg = format!("update {} {}\n", state_id, payload);
        let clients = self.clients.read().await;
        for client in clients.values() {
            let _ = client.sender.try_send(msg.clone());
        }
    }
    
    pub async fn broadcast_all(&self, state_id: &str, payload: &str) {
        let clients = self.clients.read().await;
        let msg = format!("update {} {}\n", state_id, payload);
        for client in clients.values() {
            let _ = client.sender.try_send(msg.clone());
        }
    }
    pub async fn send_to(&self, pid: Pid, state_id: &str, payload: &str) {
        let msg = format!("update {} {}\n", state_id, payload);
        let clients = self.clients.read().await;
        if let Some(client) = clients.get(&pid) {
            let _ = client.sender.try_send(msg);
        }
    }

    pub async fn broadcast_to_cwd(&self, cwd: &Path, state_id: &str, payload: &str) {
        let msg = format!("update {} {}\n", state_id, payload);
        let clients = self.clients.read().await;
        for client in clients.values() {
            if client.cwd == cwd {
                let _ = client.sender.try_send(msg.clone());
            }
        }
    }

    pub async fn get_unique_cwds(&self) -> HashSet<PathBuf> {
        let clients = self.clients.read().await;
        clients.values().map(|c| c.cwd.clone()).filter(|p| p.components().count() > 0).collect()
    }
    
    pub async fn get_client_count(&self) -> usize {
        let clients = self.clients.read().await;
        clients.len()
    }
}
