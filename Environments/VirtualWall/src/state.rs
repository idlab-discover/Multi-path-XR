use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::debug;

use crate::error::Result;
use crate::topology::TopologyState;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceRecord {
    pub name: String,
    pub resource_id: Option<String>,
    pub site_id: Option<String>,
    pub addresses: Vec<String>,
    #[serde(default)]
    pub hostnames: Vec<String>,
    pub status: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualWallState {
    pub last_project: Option<String>,
    pub experiment_name: Option<String>,
    pub experiment_id: Option<String>,
    pub last_known_site: Option<String>,
    #[serde(default)]
    pub resources: Vec<ResourceRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology: Option<TopologyState>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Default for VirtualWallState {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            last_project: None,
            experiment_name: None,
            experiment_id: None,
            last_known_site: None,
            resources: Vec::new(),
            topology: None,
            created_at: now,
            updated_at: now,
        }
    }
}

pub struct StateStore {
    path: PathBuf,
    state: RwLock<VirtualWallState>,
}

impl StateStore {
    pub fn new(path: PathBuf) -> Result<Self> {
        let state = if path.exists() {
            let data = fs::read_to_string(&path)?;
            serde_json::from_str(&data)?
        } else {
            VirtualWallState::default()
        };

        Ok(Self {
            path,
            state: RwLock::new(state),
        })
    }

    pub async fn get(&self) -> VirtualWallState {
        self.state.read().await.clone()
    }

    pub async fn update<F>(&self, update_fn: F) -> Result<VirtualWallState>
    where
        F: FnOnce(&mut VirtualWallState),
    {
        let mut guard = self.state.write().await;
        update_fn(&mut guard);
        guard.updated_at = Utc::now();
        let serialized = serde_json::to_string_pretty(&*guard)?;
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(&self.path, serialized)?;
        debug!("Persisted Virtual Wall state to {}", self.path.display());
        Ok(guard.clone())
    }

    pub async fn replace(&self, new_state: VirtualWallState) -> Result<VirtualWallState> {
        let mut guard = self.state.write().await;
        *guard = new_state;
        guard.updated_at = Utc::now();
        let serialized = serde_json::to_string_pretty(&*guard)?;
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(&self.path, serialized)?;
        debug!("Persisted Virtual Wall state to {}", self.path.display());
        Ok(guard.clone())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
