use std::collections::HashMap;

use crate::overlay::OverlaySpec;
use virtual_wall::Result;

/// Basic safety checks to avoid touching management NICs or missing underlay config.
pub fn validate_safety(spec: &OverlaySpec, underlay_map: &HashMap<String, String>) -> Result<()> {
    let mut required_hosts = std::collections::BTreeSet::new();

    // Hosts explicitly listed in the spec
    for host in &spec.hosts {
        required_hosts.insert(host.name.clone());
    }
    // Hosts implied by nodes (auto maps node->host name)
    for node in &spec.nodes {
        let host = if node.host == "auto" {
            node.name.clone()
        } else {
            node.host.clone()
        };
        required_hosts.insert(host);
    }
    // Hosts implied by tunnels
    for tun in &spec.tunnels {
        required_hosts.insert(tun.local_host.clone());
        required_hosts.insert(tun.remote_host.clone());
    }

    for host_name in required_hosts {
        let resolved = spec
            .hosts
            .iter()
            .find(|h| h.name == host_name)
            .and_then(|h| h.underlay.clone())
            .or_else(|| underlay_map.get(&host_name).cloned())
            .unwrap_or_default();
        if resolved.is_empty() {
            return Err(virtual_wall::VirtualWallError::Configuration(format!(
                "Host {host_name} missing underlay interface/ip for VXLAN binding"
            )));
        }

        // Guard against using the management subnet as underlay unless explicitly allowed.
        let allow_mgmt = std::env::var("BIGVW_ALLOW_MGMT_UNDERLAY")
            .ok()
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);
        if !allow_mgmt && is_mgmt_like(&resolved) {
            return Err(virtual_wall::VirtualWallError::Configuration(format!(
                "Host {host_name} underlay {resolved} looks like a management address; refuse to bind VXLAN there (set BIGVW_ALLOW_MGMT_UNDERLAY=1 to override)"
            )));
        }
    }

    // VLAN space sanity: keep within 1-4094 (IEEE 802.1Q)
    let vlan_count = spec.links.len() as u16;
    if vlan_count > 0 {
        let last = spec.vlan_start.saturating_add(vlan_count);
        if last > 4094 {
            return Err(virtual_wall::VirtualWallError::Configuration(format!(
                "VLAN range exceeded (start={} + {} links > 4094)",
                spec.vlan_start, vlan_count
            )));
        }
        if let Some(pool) = &spec.vlan_pool {
            if let Some(max) = pool.count {
                if vlan_count > max {
                    return Err(virtual_wall::VirtualWallError::Configuration(format!(
                        "VLAN pool too small: {} links exceed pool count {}",
                        vlan_count, max
                    )));
                }
            }
        }
    }
    Ok(())
}

fn is_mgmt_like(ip: &str) -> bool {
    if let Ok(addr) = ip.parse::<std::net::Ipv4Addr>() {
        let octets = addr.octets();
        // Empirical mgmt range seen on Virtual Wall (10.2.64.x); also guard broader 10.2.0.0/16.
        return octets[0] == 10 && octets[1] == 2;
    }
    false
}
