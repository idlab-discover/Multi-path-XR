use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::overlay::{
    default_bridge, default_vxlan_dev, Endpoint, Impairment, OverlaySpec, OverlayTunnel,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostPlan {
    pub host: String,
    pub bridge: String,
    pub vxlan_devices: Vec<VxlanDevice>,
    pub vlan_links: Vec<VlanLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VxlanDevice {
    pub name: String,
    pub vni: u32,
    pub local_underlay: String,
    pub remote_underlay: String,
    pub peer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VlanLink {
    pub vlan_id: u16,
    pub vxlan_dev: String,
    pub endpoints: [Endpoint; 2],
    pub impairment: Option<Impairment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanResult {
    pub host_plans: Vec<HostPlan>,
}

/// Planner: assigns VLANs to cross-host links and groups them by host.
pub fn plan_overlay(spec: &OverlaySpec) -> PlanResult {
    plan_overlay_with_underlay(spec, &HashMap::new())
}

pub fn plan_overlay_with_underlay(
    spec: &OverlaySpec,
    underlay_map: &HashMap<String, String>,
) -> PlanResult {
    let hosts: HashMap<_, _> = spec.hosts.iter().map(|h| (&h.name, h)).collect();
    let nodes: HashMap<_, _> = spec.nodes.iter().map(|n| (&n.name, n)).collect();

    // Map host pairs to tunnels (assume user provided tunnels define underlay IPs).
    let mut tunnel_map: HashMap<(String, String), &OverlayTunnel> = HashMap::new();
    for t in &spec.tunnels {
        tunnel_map.insert((t.local_host.clone(), t.remote_host.clone()), t);
    }

    let mut next_vlan = spec.vlan_start;
    let mut host_plans: BTreeMap<String, HostPlan> = BTreeMap::new();

    // Ensure each host has a HostPlan even if there are no links.
    for h in hosts.values() {
        host_plans
            .entry(h.name.clone())
            .or_insert_with(|| HostPlan {
                host: h.name.clone(),
                bridge: h.bridge.clone(),
                vxlan_devices: Vec::new(),
                vlan_links: Vec::new(),
            });
    }

    for link in &spec.links {
        let src_host = nodes
            .get(&link.src.node)
            .map(|n| {
                if n.host == "auto" {
                    n.name.clone()
                } else {
                    n.host.clone()
                }
            })
            .unwrap_or_default();
        let dst_host = nodes
            .get(&link.dst.node)
            .map(|n| {
                if n.host == "auto" {
                    n.name.clone()
                } else {
                    n.host.clone()
                }
            })
            .unwrap_or_default();

        let vlan_id = link.vlan_id.unwrap_or_else(|| {
            let id = next_vlan;
            next_vlan += 1;
            id
        });

        if src_host == dst_host {
            // Intra-host: no VXLAN/VLAN needed.
            continue;
        }

        // Find tunnel for this host pair; default VNI/device if absent.
        let tunnel = tunnel_map
            .get(&(src_host.clone(), dst_host.clone()))
            .or_else(|| tunnel_map.get(&(dst_host.clone(), src_host.clone())));

        let (vxlan_dev, vni, local_underlay, remote_underlay) = if let Some(tun) = tunnel {
            (
                tun.dev.clone(),
                tun.vni,
                tun.local_underlay.clone(),
                tun.remote_underlay.clone(),
            )
        } else {
            (
                default_vxlan_dev(),
                2000,
                resolve_underlay(&src_host, &hosts, underlay_map),
                resolve_underlay(&dst_host, &hosts, underlay_map),
            )
        };

        for (host, peer, local_underlay, remote_underlay) in [
            (
                src_host.clone(),
                dst_host.clone(),
                local_underlay.clone(),
                remote_underlay.clone(),
            ),
            (
                dst_host.clone(),
                src_host.clone(),
                remote_underlay,
                local_underlay,
            ),
        ] {
            let hp = host_plans.entry(host.clone()).or_insert_with(|| HostPlan {
                host: host.clone(),
                bridge: hosts
                    .get(&host)
                    .map(|h| h.bridge.clone())
                    .unwrap_or_else(default_bridge),
                vxlan_devices: Vec::new(),
                vlan_links: Vec::new(),
            });

            if !hp.vxlan_devices.iter().any(|d| d.name == vxlan_dev) {
                hp.vxlan_devices.push(VxlanDevice {
                    name: vxlan_dev.clone(),
                    vni,
                    local_underlay: local_underlay.clone(),
                    remote_underlay: remote_underlay.clone(),
                    peer,
                });
            }

            hp.vlan_links.push(VlanLink {
                vlan_id,
                vxlan_dev: vxlan_dev.clone(),
                endpoints: [link.src.clone(), link.dst.clone()],
                impairment: link.impairment.clone(),
            });
        }
    }

    debug!("Planned host overlays: {:?}", host_plans.keys());
    PlanResult {
        host_plans: host_plans.into_values().collect(),
    }
}

fn resolve_underlay(
    host: &str,
    hosts: &HashMap<&String, &crate::overlay::OverlayHost>,
    map: &HashMap<String, String>,
) -> String {
    if let Some(h) = hosts.get(&host.to_string()) {
        if let Some(ul) = &h.underlay {
            return ul.clone();
        }
    }
    map.get(host).cloned().unwrap_or_default()
}
