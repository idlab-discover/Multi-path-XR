use serde::{Deserialize, Serialize};
use tracing::debug;
use virtual_wall::{Result, VirtualWallManager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInventory {
    pub name: String,
    pub underlay: String,
    pub bridge: String,
}

/// Build inventory from the current experiment using the VirtualWall manager.
pub async fn discover_hosts(manager: &VirtualWallManager) -> Result<Vec<HostInventory>> {
    // Ensure state is refreshed from the live experiment
    let state = manager.status().await?;
    let mut hosts = Vec::new();
    if let Some(resources) = state.get("resources").and_then(|r| r.as_array()) {
        for res in resources {
            let name = res.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            // We rely on the "hostnames" list if present; fallback to first address.
            let underlay = res
                .get("hostnames")
                .and_then(|h| h.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
                .or_else(|| {
                    res.get("addresses")
                        .and_then(|a| a.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|v| v.as_str())
                })
                .unwrap_or_default()
                .to_string();

            let bridge = "br-vw".to_string();

            if !name.is_empty() && !underlay.is_empty() {
                hosts.push(HostInventory {
                    name: name.to_string(),
                    underlay,
                    bridge,
                });
            }
        }
    }
    debug!(
        "Discovered hosts: {:?}",
        hosts.iter().map(|h| &h.name).collect::<Vec<_>>()
    );
    Ok(hosts)
}
