use crate::error::{Result, VirtualWallError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSpec {
    pub resources: Vec<ResourceDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ResourceDefinition {
    pub friendly_name: String,
    #[serde(rename = "infra_id", skip_serializing_if = "Option::is_none")]
    pub site_id: Option<String>,
    #[serde(rename = "disk_image", skip_serializing_if = "Option::is_none")]
    pub disk_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
    #[serde(rename = "userdata_file", skip_serializing_if = "Option::is_none")]
    pub userdata_file: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_interfaces: Vec<NetworkInterface>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInterface {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port_id: Option<String>,
    pub network_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkDefinition {
    pub friendly_name: String,
    pub network_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_lan_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub impairment: Vec<LinkImpairment>,
}

/// Optional per-link impairments (capacity, loss, latency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkImpairment {
    /// Optional endpoint identifier, e.g. `"node1:if0"`.
    ///
    /// If omitted together with `destination`, the impairment is applied symmetrically to the whole LAN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Optional endpoint identifier, e.g. `"node2:if0"`.
    ///
    /// If omitted together with `source`, the impairment is applied symmetrically to the whole LAN.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity_kbit_per_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packet_loss: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

pub struct ResourceSpecFactory;

impl ResourceSpecFactory {
    pub fn baremetal_cluster(
        hosts: usize,
        vlans: usize,
        site_id: Option<&str>,
        image: Option<&str>,
        flavor: Option<&str>,
        cloud_init: Option<&str>,
        prefix: Option<&str>,
    ) -> Result<ResourceSpec> {
        let mut resources = Vec::new();
        let base_prefix = prefix.map(|p| p.to_string()).unwrap_or_default();

        // Error when the amount of vlans is higher than 50
        if vlans > 50 {
            return Err(VirtualWallError::Configuration(format!(
                "Maximum VLANs is 50 (site may support less); requested {vlans}"
            )));
        }

        // Build host resources.
        for h_idx in 0..hosts {
            let friendly_name = format!("{base_prefix}node{}", h_idx + 1);
            let mut interfaces = Vec::new();
            for r_idx in 0..vlans.max(1) {
                let network_id = format!("lan-{}", r_idx + 1);
                // Start host addresses at .10+index to leave room for infra.
                let host_ip = format!("192.168.{}.{}/24", 10 + r_idx, 10 + h_idx as u32);
                interfaces.push(NetworkInterface {
                    port_id: Some(format!("eth{}", r_idx + 1)),
                    network_id,
                    addresses: vec![host_ip],
                });
            }

            resources.push(ResourceDefinition {
                friendly_name,
                site_id: site_id.map(|s| s.to_string()),
                disk_image: image.map(|i| i.to_string()),
                flavor: flavor.map(|f| f.to_string()),
                userdata_file: cloud_init.map(|p| p.to_string()),
                network_interfaces: interfaces,
            });
        }

        // Links (LANs) to make topology explicit.
        let mut links = Vec::new();
        for r_idx in 0..vlans.max(1) {
            links.push(LinkDefinition {
                friendly_name: format!("lan-{}", r_idx + 1),
                network_id: format!("lan-{}", r_idx + 1),
                r#type: Some("lan".to_string()),
                share_lan_name: None,
                impairment: Vec::new(),
            });
        }

        Ok(ResourceSpec { resources, links })
    }
}
