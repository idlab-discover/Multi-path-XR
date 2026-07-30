use std::{collections::HashMap, fs, path::Path};

use serde::{Deserialize, Serialize};
use virtual_wall::{Result, VirtualWallError};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OverlaySpec {
    #[serde(default)]
    pub hosts: Vec<OverlayHost>,
    #[serde(default)]
    pub nodes: Vec<OverlayNode>,
    #[serde(default)]
    pub links: Vec<OverlayLink>,
    #[serde(default)]
    pub tunnels: Vec<OverlayTunnel>,
    #[serde(default)]
    pub host_pool: Option<HostPool>,
    #[serde(default)]
    pub virtual_pool: Option<VirtualPool>,
    #[serde(default)]
    pub vlan_pool: Option<VlanPool>,
    /// Optional mapping of VLAN ID -> host parent interface (for binding VLANs to specific NICs).
    #[serde(default)]
    pub vlan_bindings: Option<std::collections::HashMap<u16, String>>,
    /// Optional start VLAN id when auto-assigning tags for cross-host links.
    #[serde(default = "default_vlan_start")]
    pub vlan_start: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OverlayHost {
    pub name: String,
    /// Management address used for slices ssh. Optional when inventory is supplied externally.
    #[serde(default)]
    pub mgmt_addr: Option<String>,
    /// Underlay interface or IP used for VXLAN binding (must not be the mgmt bridge).
    #[serde(default)]
    pub underlay: Option<String>,
    /// Bridge to attach Mininet/root switch.
    #[serde(default = "default_bridge")]
    pub bridge: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OverlayNode {
    pub name: String,
    pub host: String,
    #[serde(default)]
    pub interfaces: Vec<NodeInterface>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NodeInterface {
    pub name: String,
    #[serde(default)]
    pub mtu: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OverlayLink {
    pub src: Endpoint,
    pub dst: Endpoint,
    #[serde(default)]
    pub impairment: Option<Impairment>,
    /// Optional VLAN for this link (applied on the VXLAN path); auto-assigned when absent.
    #[serde(default)]
    pub vlan_id: Option<u16>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Endpoint {
    pub node: String,
    pub intf: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Impairment {
    #[serde(default)]
    pub latency_ms: Option<u32>,
    #[serde(default)]
    pub loss_pct: Option<f32>,
    #[serde(default)]
    pub rate_mbps: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct HostPool {
    /// Number of bare-metal hosts to materialize (vw-node-1..N).
    pub count: Option<usize>,
    /// Name prefix for generated hosts.
    #[serde(default = "default_host_prefix")]
    pub prefix: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct VirtualPool {
    /// Number of virtual nodes per host.
    pub per_host: Option<usize>,
    /// Prefix for generated virtual nodes.
    #[serde(default = "default_virtual_prefix")]
    pub prefix: String,
    /// Starting index for per-host numbering.
    #[serde(default = "default_virtual_start")]
    pub start_index: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct VlanPool {
    /// VLAN start for auto assignment.
    #[serde(default = "default_vlan_start")]
    pub start: u16,
    /// Optional maximum number of VLANs to allocate.
    pub count: Option<u16>,
    /// Optional per-VLAN subnet (e.g., "192.168.50.0/24"); if provided for a VLAN id, overrides defaults.
    #[serde(default)]
    pub subnets: std::collections::HashMap<u16, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OverlayTunnel {
    /// Host on which this tunnel resides.
    pub local_host: String,
    /// Remote host.
    pub remote_host: String,
    /// Underlay local IP to bind the VXLAN device.
    pub local_underlay: String,
    /// Underlay remote IP.
    pub remote_underlay: String,
    /// VXLAN VNI.
    pub vni: u32,
    /// VXLAN device name (per host).
    #[serde(default = "default_vxlan_dev")]
    pub dev: String,
}

#[derive(Debug, Clone)]
pub struct OverlayValidation {
    pub missing_hosts: Vec<String>,
    pub missing_nodes: Vec<String>,
}

impl OverlaySpec {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)?;
        let spec: OverlaySpec = match path.extension().and_then(|p| p.to_str()) {
            Some("yaml") | Some("yml") => serde_yaml::from_str(&contents)?,
            Some("json") => serde_json::from_str(&contents)?,
            _ => toml::from_str(&contents)?,
        };
        let mut spec = spec;
        spec.expand_dynamic()?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<()> {
        let host_set: HashMap<_, _> = self.hosts.iter().map(|h| (&h.name, h)).collect();
        let node_set: HashMap<_, _> = self.nodes.iter().map(|n| (&n.name, n)).collect();
        let mut missing_hosts = Vec::new();
        let mut missing_nodes = Vec::new();

        for node in &self.nodes {
            // Host validation: skip when using auto placement or when hosts are discovered.
            if !self.hosts.is_empty() && node.host != "auto" && !host_set.contains_key(&node.host) {
                missing_hosts.push(node.host.clone());
            }
        }
        for link in &self.links {
            for ep in [&link.src, &link.dst] {
                if !node_set.contains_key(&ep.node) {
                    missing_nodes.push(ep.node.clone());
                }
            }
        }

        if !missing_hosts.is_empty() || !missing_nodes.is_empty() {
            return Err(VirtualWallError::Configuration(format!(
                "Overlay validation failed: missing_hosts={:?} missing_nodes={:?}",
                missing_hosts, missing_nodes
            )));
        }
        Ok(())
    }

    /// Expand pools/counts into concrete hosts/nodes and configure VLAN start/count.
    fn expand_dynamic(&mut self) -> Result<()> {
        // Hosts from pool if none provided.
        if self.hosts.is_empty() {
            if let Some(pool) = &self.host_pool {
                if let Some(count) = pool.count {
                    for i in 1..=count {
                        let name = format!("{}{}", pool.prefix, i);
                        self.hosts.push(OverlayHost {
                            name,
                            mgmt_addr: None,
                            underlay: None,
                            bridge: default_bridge(),
                        });
                    }
                }
            }
        }

        // Virtual nodes per host if none provided.
        if self.nodes.is_empty() {
            if let Some(vp) = &self.virtual_pool {
                if let Some(per_host) = vp.per_host {
                    for host in &self.hosts {
                        for idx in 0..per_host {
                            let name =
                                format!("{}-{}-{}", vp.prefix, host.name, vp.start_index + idx);
                            self.nodes.push(OverlayNode {
                                name,
                                host: host.name.clone(),
                                interfaces: Vec::new(),
                            });
                        }
                    }
                }
            }
        }

        // VLAN pool override
        if let Some(vp) = &self.vlan_pool {
            self.vlan_start = vp.start;
            if let Some(max) = vp.count {
                let links = self.links.len() as u16;
                if links > max {
                    return Err(VirtualWallError::Configuration(format!(
                        "VLAN pool too small: {} links requested but pool count is {}",
                        links, max
                    )));
                }
            }
        }

        Ok(())
    }
}

pub fn default_bridge() -> String {
    "br-vw".to_string()
}

pub fn default_vxlan_dev() -> String {
    "vxlan-vw".to_string()
}

fn default_vlan_start() -> u16 {
    200
}

fn default_host_prefix() -> String {
    "vw-node-".to_string()
}

fn default_virtual_prefix() -> String {
    "vn".to_string()
}

fn default_virtual_start() -> usize {
    1
}
