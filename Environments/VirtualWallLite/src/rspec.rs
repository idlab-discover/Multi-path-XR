//! Manifest RSpec parsing.
//!
//! The parser is intentionally conservative: it extracts only what this crate needs
//! (node ids, login targets, interfaces, IPs, and link memberships).

use std::{fs, path::Path};

use quick_xml::{events::Event, Reader};
use serde::{Deserialize, Serialize};

use crate::error::{Result, VirtualWallError};

/// Parsed view of a manifest RSpec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RspecTopology {
    pub nodes: Vec<RspecNode>,
    pub links: Vec<RspecLink>,
}

impl RspecTopology {
    /// Parse an RSpec manifest from disk.
    pub fn parse_file(path: &Path) -> Result<Self> {
        let xml = fs::read_to_string(path)?;
        Self::parse_str(&xml)
    }

    /// Parse an RSpec manifest from an in-memory string.
    pub fn parse_str(xml: &str) -> Result<Self> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        // Keep namespaces; we match by local name.

        let mut buf = Vec::new();

        let mut nodes = Vec::new();
        let mut links = Vec::new();

        let mut cur_node: Option<RspecNode> = None;
        let mut cur_iface: Option<RspecInterface> = None;
        let mut cur_link: Option<RspecLink> = None;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = e.name();
                    let lname = local_name(name.as_ref());
                    match lname {
                        b"node" => {
                            let client_id = attr_str(&e, b"client_id").ok_or_else(|| {
                                VirtualWallError::Rspec("node missing client_id".to_string())
                            })?;
                            cur_node = Some(RspecNode {
                                client_id,
                                login: None,
                                interfaces: Vec::new(),
                            });
                        }
                        b"interface" => {
                            if cur_node.is_some() {
                                if let Some(client_id) = attr_str(&e, b"client_id") {
                                    cur_iface = Some(RspecInterface {
                                        client_id,
                                        component_id: attr_str(&e, b"component_id"),
                                        ips: Vec::new(),
                                    });
                                }
                            }
                        }
                        b"ip" => {
                            if let Some(iface) = cur_iface.as_mut() {
                                if let Some(address) = attr_str(&e, b"address") {
                                    let netmask = attr_str(&e, b"netmask");
                                    let ty = attr_str(&e, b"type");
                                    iface.ips.push(RspecIp {
                                        address,
                                        netmask,
                                        r#type: ty,
                                    });
                                }
                            }
                        }
                        b"login" => {
                            if let Some(node) = cur_node.as_mut() {
                                let hostname = attr_str(&e, b"hostname");
                                let username = attr_str(&e, b"username");
                                let port = attr_str(&e, b"port")
                                    .and_then(|p| p.parse::<u16>().ok())
                                    .unwrap_or(22);
                                if let Some(host) = hostname {
                                    node.login = Some(RspecLogin {
                                        host,
                                        port,
                                        username,
                                    });
                                }
                            }
                        }
                        b"link" => {
                            let client_id = attr_str(&e, b"client_id").ok_or_else(|| {
                                VirtualWallError::Rspec("link missing client_id".to_string())
                            })?;
                            let vlantag = attr_str(&e, b"vlantag");
                            cur_link = Some(RspecLink {
                                client_id,
                                vlantag,
                                interface_refs: Vec::new(),
                            });
                        }
                        b"interface_ref" => {
                            if let Some(link) = cur_link.as_mut() {
                                if let Some(cid) = attr_str(&e, b"client_id") {
                                    link.interface_refs.push(cid);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Empty(e)) => {
                    let name = e.name();
                    let lname = local_name(name.as_ref());
                    // Same logic as Start for leaf tags.
                    match lname {
                        b"ip" => {
                            if let Some(iface) = cur_iface.as_mut() {
                                if let Some(address) = attr_str(&e, b"address") {
                                    let netmask = attr_str(&e, b"netmask");
                                    let ty = attr_str(&e, b"type");
                                    iface.ips.push(RspecIp {
                                        address,
                                        netmask,
                                        r#type: ty,
                                    });
                                }
                            }
                        }
                        b"login" => {
                            if let Some(node) = cur_node.as_mut() {
                                let hostname = attr_str(&e, b"hostname");
                                let username = attr_str(&e, b"username");
                                let port = attr_str(&e, b"port")
                                    .and_then(|p| p.parse::<u16>().ok())
                                    .unwrap_or(22);
                                if let Some(host) = hostname {
                                    node.login = Some(RspecLogin {
                                        host,
                                        port,
                                        username,
                                    });
                                }
                            }
                        }
                        b"interface_ref" => {
                            if let Some(link) = cur_link.as_mut() {
                                if let Some(cid) = attr_str(&e, b"client_id") {
                                    link.interface_refs.push(cid);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(e)) => {
                    let name = e.name();
                    let lname = local_name(name.as_ref());
                    match lname {
                        b"interface" => {
                            if let (Some(node), Some(iface)) = (cur_node.as_mut(), cur_iface.take())
                            {
                                node.interfaces.push(iface);
                            }
                        }
                        b"node" => {
                            if let Some(node) = cur_node.take() {
                                nodes.push(node);
                            }
                        }
                        b"link" => {
                            if let Some(link) = cur_link.take() {
                                links.push(link);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(VirtualWallError::Rspec(format!("XML parse error: {e}")));
                }
                _ => {}
            }

            buf.clear();
        }

        // De-duplicate interface refs while preserving order.
        for l in &mut links {
            // Can't store &str during retain (elements may move); use owned keys.
            let mut seen = std::collections::HashSet::<String>::new();
            l.interface_refs.retain(|s| seen.insert(s.clone()));
        }

        Ok(Self { nodes, links })
    }
}

/// A node in the RSpec (client_id is often `node0`, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RspecNode {
    pub client_id: String,
    pub login: Option<RspecLogin>,
    pub interfaces: Vec<RspecInterface>,
}

/// Login details as declared in the RSpec `<services><login ...>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RspecLogin {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
}

/// An interface on a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RspecInterface {
    pub client_id: String,
    pub component_id: Option<String>,
    pub ips: Vec<RspecIp>,
}

impl RspecInterface {
    pub fn manifest_device_name(&self) -> Option<&str> {
        interface_device_name_from_component_id(self.component_id.as_deref()?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RspecIp {
    pub address: String,
    pub netmask: Option<String>,
    pub r#type: Option<String>,
}

/// A link (LAN or point-to-point) from the RSpec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RspecLink {
    pub client_id: String,
    pub vlantag: Option<String>,
    pub interface_refs: Vec<String>,
}

fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|b| *b == b':') {
        Some(idx) => &name[idx + 1..],
        None => name,
    }
}

fn attr_str(e: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            if let Ok(v) = a.unescape_value() {
                let s = v.into_owned();
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return None;
                }
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn interface_device_name_from_component_id(component_id: &str) -> Option<&str> {
    let trimmed = component_id.trim();
    if trimmed.is_empty() {
        return None;
    }

    let device = trimmed.rsplit(':').next()?.trim();
    if device.is_empty() {
        return None;
    }

    Some(device)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::RspecTopology;

    type InterfaceInventoryEntry = (&'static str, &'static str, &'static [&'static str]);
    type NodeInterfaceInventory = (&'static str, &'static [InterfaceInventoryEntry]);

    const EXPECTED_INTERFACE_INVENTORY: &[NodeInterfaceInventory] = &[
        ("core-a", &[("core-a:if0", "eth3", &["192.168.10.11"]), ("core-a:if1", "eth2", &["192.168.110.11"]), ("core-a:if2", "eth1", &["192.168.20.10"]), ("core-a:if3", "eth0", &["192.168.120.10"])]),
        ("core-b", &[("core-b:if0", "eth1", &["192.168.10.12"]), ("core-b:if1", "eth3", &["192.168.110.12"]), ("core-b:if2", "eth0", &["192.168.21.10"]), ("core-b:if3", "eth2", &["192.168.121.10"])]),
        ("core-c", &[("core-c:if0", "eth6", &["192.168.10.13"]), ("core-c:if1", "eth3", &["192.168.110.13"]), ("core-c:if2", "eth7", &["192.168.22.10"]), ("core-c:if3", "eth2", &["192.168.122.10"])]),
        ("edge-a1-1", &[("edge-a1-1:if0", "eth3", &["192.168.30.11"]), ("edge-a1-1:if1", "eth2", &["192.168.130.11"])]),
        ("edge-a1-2", &[("edge-a1-2:if0", "eth3", &["192.168.30.12"]), ("edge-a1-2:if1", "eth6", &["192.168.130.12"])]),
        ("edge-a2-1", &[("edge-a2-1:if0", "eth3", &["192.168.31.11"]), ("edge-a2-1:if1", "eth2", &["192.168.131.11"])]),
        ("edge-a2-2", &[("edge-a2-2:if0", "eth1", &["192.168.31.12"]), ("edge-a2-2:if1", "eth0", &["192.168.131.12"])]),
        ("edge-b1-1", &[("edge-b1-1:if0", "eth0", &["192.168.32.11"]), ("edge-b1-1:if1", "eth3", &["192.168.132.11"])]),
        ("edge-b1-2", &[("edge-b1-2:if0", "eth1", &["192.168.32.12"]), ("edge-b1-2:if1", "eth3", &["192.168.132.12"])]),
        ("edge-b2-1", &[("edge-b2-1:if0", "eth3", &["192.168.33.11"]), ("edge-b2-1:if1", "eth2", &["192.168.133.11"])]),
        ("edge-b2-2", &[("edge-b2-2:if0", "eth3", &["192.168.33.12"]), ("edge-b2-2:if1", "eth2", &["192.168.133.12"])]),
        ("edge-c2-1", &[("edge-c2-1:if0", "eth6", &["192.168.35.11"]), ("edge-c2-1:if1", "eth3", &["192.168.135.11"])]),
        ("regional-a1", &[("regional-a1:if0", "eth3", &["192.168.20.11"]), ("regional-a1:if1", "eth2", &["192.168.120.11"]), ("regional-a1:if2", "eth1", &["192.168.30.10"]), ("regional-a1:if3", "eth0", &["192.168.130.10"])]),
        ("regional-a2", &[("regional-a2:if0", "eth3", &["192.168.20.12"]), ("regional-a2:if1", "eth2", &["192.168.120.12"]), ("regional-a2:if2", "eth1", &["192.168.31.10"]), ("regional-a2:if3", "eth0", &["192.168.131.10"])]),
        ("regional-b1", &[("regional-b1:if0", "eth0", &["192.168.21.11"]), ("regional-b1:if1", "eth1", &["192.168.121.11"]), ("regional-b1:if2", "eth5", &["192.168.32.10"]), ("regional-b1:if3", "eth2", &["192.168.132.10"])]),
        ("regional-b2", &[("regional-b2:if0", "eth3", &["192.168.21.12"]), ("regional-b2:if1", "eth2", &["192.168.121.12"]), ("regional-b2:if2", "eth1", &["192.168.33.10"]), ("regional-b2:if3", "eth0", &["192.168.133.10"])]),
        ("regional-c1", &[("regional-c1:if0", "eth2", &["192.168.22.11"]), ("regional-c1:if1", "eth1", &["192.168.122.11"])]),
        ("regional-c2", &[("regional-c2:if0", "eth3", &["192.168.22.12"]), ("regional-c2:if1", "eth6", &["192.168.122.12"]), ("regional-c2:if2", "eth7", &["192.168.35.10"]), ("regional-c2:if3", "eth2", &["192.168.135.10"])]),
        ("server", &[("server:if0", "eth1", &["192.168.10.10"]), ("server:if1", "eth3", &["192.168.110.10"])]),
    ];

    fn actual_inventory(topology: &RspecTopology) -> BTreeMap<String, Vec<(String, String, Vec<String>)>> {
        topology
            .nodes
            .iter()
            .map(|node| {
                let mut interfaces = node
                    .interfaces
                    .iter()
                    .map(|interface| {
                        let mut ips = interface
                            .ips
                            .iter()
                            .map(|ip| ip.address.clone())
                            .collect::<Vec<_>>();
                        ips.sort();
                        (
                            interface.client_id.clone(),
                            interface.manifest_device_name().unwrap_or_default().to_string(),
                            ips,
                        )
                    })
                    .collect::<Vec<_>>();
                interfaces.sort();
                (node.client_id.clone(), interfaces)
            })
            .collect()
    }

    fn expected_inventory() -> BTreeMap<String, Vec<(String, String, Vec<String>)>> {
        EXPECTED_INTERFACE_INVENTORY
            .iter()
            .map(|(node_id, interfaces)| {
                let mut rows = interfaces
                    .iter()
                    .map(|(client_id, device, ips)| {
                        (
                            (*client_id).to_string(),
                            (*device).to_string(),
                            ips.iter().map(|ip| (*ip).to_string()).collect::<Vec<_>>(),
                        )
                    })
                    .collect::<Vec<_>>();
                rows.sort();
                ((*node_id).to_string(), rows)
            })
            .collect()
    }

    #[test]
    fn parses_virtualwall_manifest_interface_inventory() {
        let topology = RspecTopology::parse_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/multipathxr-meta.private.rspec"
        )))
        .expect("manifest should parse");

        let actual = actual_inventory(&topology);
        let expected = expected_inventory();

        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 19);
        assert_eq!(actual.values().map(Vec::len).sum::<usize>(), 54);
    }
}
