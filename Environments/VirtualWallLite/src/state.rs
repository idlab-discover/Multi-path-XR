//! Optional state file support.
//!
//! If you have a `state.json` created by the full Virtual Wall manager, this crate can
//! reuse it to recover:
//! - friendly node names (`resources[].name`),
//! - per-node SSH logins (including jump proxy details).

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{config::JumpProxy, error::Result};

/// Top-level state file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct VirtualWallStateLite {
    pub experiment_name: Option<String>,
    pub experiment_id: Option<String>,
    pub resources: Vec<ResourceRecordLite>,
}

/// Resource record (subset).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ResourceRecordLite {
    pub name: String,
    pub addresses: Vec<String>,
    pub status: Option<String>,
    pub expires_at: Option<String>,
    pub metadata: Option<ResourceMetadataLite>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ResourceMetadataLite {
    pub ssh_logins: Vec<SshLoginLite>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SshLoginLite {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: Option<String>,
    pub jump_proxy: Option<JumpProxy>,
}

fn default_ssh_port() -> u16 {
    22
}

impl VirtualWallStateLite {
    /// Load state from a JSON file.
    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read_to_string(path)?;
        let parsed: Self = serde_json::from_str(&data)?;
        Ok(parsed)
    }
}
