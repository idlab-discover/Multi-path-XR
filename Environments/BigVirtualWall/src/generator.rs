use std::fmt::Write;

use crate::overlay::OverlaySpec;
use crate::planner::{PlanResult, VlanLink, VxlanDevice};

pub type HostNodesCollection =
    std::collections::BTreeMap<String, Vec<(String, Vec<(String, Option<String>, Option<u32>)>)>>;

/// Generate shell script per host to set up OVS bridge, VXLAN devices, VLAN subinterfaces and tc impairments.
pub fn generate_host_scripts(
    plan: &PlanResult,
    vlan_bindings: Option<&std::collections::HashMap<u16, String>>,
) -> Vec<(String, String)> {
    plan.host_plans
        .iter()
        .map(|hp| {
            let mut script = String::new();
            writeln!(
                script,
                "#!/usr/bin/env bash\nset -euo pipefail\n\nBR={}\n",
                hp.bridge
            )
            .unwrap();
            writeln!(script, "echo \"[host:{}] setting up bridge $BR\"", hp.host).unwrap();
            writeln!(
                script,
                "ip link add $BR type bridge 2>/dev/null || true\nip link set $BR up"
            )
            .unwrap();

            // VXLAN devices
            for vx in &hp.vxlan_devices {
                render_vxlan(&mut script, vx);
            }

            // VLAN links + impairments
            for vl in &hp.vlan_links {
                render_vlan(&mut script, vl, vlan_bindings);
            }

            (hp.host.clone(), script)
        })
        .collect()
}

/// Generate per-host runtime scripts to create lightweight virtual nodes (netns) and attach veths to the host bridge.
/// IP addressing/MTU is applied if provided on node interfaces, otherwise defaults are auto-assigned per VLAN link.
pub fn generate_host_runtime_scripts(
    plan: &PlanResult,
    spec: &OverlaySpec,
) -> Vec<(String, String)> {
    let default_mtu = 1450;
    let subnet_overrides = spec
        .vlan_pool
        .as_ref()
        .map(|p| p.subnets.clone())
        .unwrap_or_default();

    // Build address/mtu suggestions per (node,intf) from VLAN links.
    let mut addr_map: std::collections::HashMap<(String, String), (String, u32)> =
        std::collections::HashMap::new();
    // Dedup links across host plans
    let mut seen_links = std::collections::BTreeSet::new();
    for hp in &plan.host_plans {
        for vl in &hp.vlan_links {
            let key = (
                vl.vlan_id,
                vl.endpoints[0].node.clone(),
                vl.endpoints[0].intf.clone(),
                vl.endpoints[1].node.clone(),
                vl.endpoints[1].intf.clone(),
            );
            if !seen_links.insert(key) {
                continue;
            }

            // Derive a subnet per VLAN: either override from spec or default to 192.168.(50+vlan%200).X/30
            let (a1, a2, gw) = if let Some(subnet) = subnet_overrides.get(&vl.vlan_id) {
                // crude: assume /24 and allocate x.1/x.2, gw=x.254
                if let Some(prefix) = subnet.strip_suffix(".0/24") {
                    (
                        format!("{prefix}.1/24"),
                        format!("{prefix}.2/24"),
                        Some(format!("{prefix}.254")),
                    )
                } else {
                    let idx = 50 + (vl.vlan_id % 200);
                    (
                        format!("192.168.{}.1/30", idx),
                        format!("192.168.{}.2/30", idx),
                        Some(format!("192.168.{}.1", idx)),
                    )
                }
            } else {
                let idx = 50 + (vl.vlan_id % 200);
                (
                    format!("192.168.{}.1/30", idx),
                    format!("192.168.{}.2/30", idx),
                    Some(format!("192.168.{}.1", idx)),
                )
            };
            let mtu = default_mtu;
            let ep1 = (&vl.endpoints[0].node, &vl.endpoints[0].intf);
            let ep2 = (&vl.endpoints[1].node, &vl.endpoints[1].intf);
            addr_map
                .entry((ep1.0.clone(), ep1.1.clone()))
                .or_insert((a1, mtu));
            addr_map
                .entry((ep2.0.clone(), ep2.1.clone()))
                .or_insert((a2, mtu));
            // store gw as well
            if let Some(gw_ip) = gw {
                addr_map
                    .entry((ep1.0.clone(), ep1.1.clone()))
                    .and_modify(|a| a.0.push_str(&format!("|gw:{}", gw_ip)));
                addr_map
                    .entry((ep2.0.clone(), ep2.1.clone()))
                    .and_modify(|a| a.0.push_str(&format!("|gw:{}", gw_ip)));
            }
        }
    }

    // Build host -> list of (node, interfaces) from spec.
    let mut host_nodes: HostNodesCollection = std::collections::BTreeMap::new();
    // Map (node,intf) -> (vlan_id, bridge)
    let mut vlan_map: std::collections::HashMap<(String, String), (u16, String)> =
        std::collections::HashMap::new();
    for hp in &plan.host_plans {
        for vl in &hp.vlan_links {
            let br = format!("br-vw-{}", vl.vlan_id);
            vlan_map.insert(
                (vl.endpoints[0].node.clone(), vl.endpoints[0].intf.clone()),
                (vl.vlan_id, br.clone()),
            );
            vlan_map.insert(
                (vl.endpoints[1].node.clone(), vl.endpoints[1].intf.clone()),
                (vl.vlan_id, br.clone()),
            );
        }
    }
    for node in &spec.nodes {
        let host = if node.host == "auto" {
            node.name.clone()
        } else {
            node.host.clone()
        };

        // Start with explicit interfaces (name, address, mtu)
        let mut intfs: Vec<(String, Option<String>, Option<u32>)> = node
            .interfaces
            .iter()
            .map(|i| (i.name.clone(), None, i.mtu))
            .collect();
        // Add link endpoints (no IP/MTU info)
        for link in &spec.links {
            if link.src.node == node.name {
                let addr = addr_map
                    .get(&(link.src.node.clone(), link.src.intf.clone()))
                    .cloned();
                intfs.push((
                    link.src.intf.clone(),
                    addr.as_ref().map(|a| a.0.clone()),
                    addr.as_ref().map(|a| a.1),
                ));
            }
            if link.dst.node == node.name {
                let addr = addr_map
                    .get(&(link.dst.node.clone(), link.dst.intf.clone()))
                    .cloned();
                intfs.push((
                    link.dst.intf.clone(),
                    addr.as_ref().map(|a| a.0.clone()),
                    addr.as_ref().map(|a| a.1),
                ));
            }
        }
        intfs.sort_by_key(|(n, _, _)| n.clone());
        intfs.dedup_by_key(|(n, _, _)| n.clone());

        host_nodes
            .entry(host)
            .or_default()
            .push((node.name.clone(), intfs));
    }

    plan.host_plans
        .iter()
        .map(|hp| {
            let mut script = String::new();
            writeln!(
                script,
                "#!/usr/bin/env bash\nset -euo pipefail\n\nBR={}\n",
                hp.bridge
            )
            .unwrap();
            writeln!(
                script,
                "echo \"[host:{}] creating virtual namespaces and veths\"\n",
                hp.host
            )
            .unwrap();
            writeln!(script, "ip link set $BR up || true").unwrap();

            if let Some(nodes) = host_nodes.get(&hp.host) {
                for (node, intfs) in nodes {
                    let ns = format!("ns-{node}");
                    writeln!(
                        script,
                        "ip netns add {ns} 2>/dev/null || true\nip netns exec {ns} ip link set lo up",
                    )
                    .unwrap();
                    for intf in intfs {
                        let host_if = format!("veth-{node}-{}", intf.0);
                        let ns_if = format!("{node}-{}", intf.0);
                        writeln!(
                            script,
                            "ip link del {host_if} 2>/dev/null || true\nip link add {host_if} type veth peer name {ns_if}\nip link set {ns_if} netns {ns}\nip netns exec {ns} ip link set {ns_if} up\nip link set {host_if} up",
                        )
                        .unwrap();
                        if let Some((_, br)) =
                            vlan_map.get(&(node.clone(), intf.0.clone()))
                        {
                            writeln!(
                                script,
                                "ip link set {host_if} master {br}",
                                host_if = host_if,
                                br = br
                            )
                            .unwrap();
                        } else {
                            writeln!(
                                script,
                                "ip link set {host_if} master $BR",
                                host_if = host_if
                            )
                            .unwrap();
                        }
                        if let Some(mtu) = intf.2 {
                            writeln!(
                                script,
                                "ip netns exec {ns} ip link set {ns_if} mtu {mtu}",
                                ns = ns,
                                ns_if = ns_if,
                                mtu = mtu
                            )
                            .unwrap();
                        }
                        if let Some(addr) = &intf.1 {
                            // addr may contain "|gw:<ip>" suffix
                            let parts: Vec<&str> = addr.split("|gw:").collect();
                            let ip_part = parts[0];
                            writeln!(
                                script,
                                "ip netns exec {ns} ip addr add {addr} dev {ns_if}",
                                ns = ns,
                                ns_if = ns_if,
                                addr = ip_part
                            )
                            .unwrap();
                            if let Some(gw) = parts.get(1) {
                                writeln!(
                                    script,
                                    "ip netns exec {ns} ip route add default via {gw}",
                                    ns = ns,
                                    gw = gw
                                )
                                .unwrap();
                            }
                        }
                    }
                }
            }

            (hp.host.clone(), script)
        })
        .collect()
}

/// Generate shell script per host to tear down VXLAN/VLAN artifacts created by the overlay plan.
pub fn generate_host_cleanup_scripts(plan: &PlanResult) -> Vec<(String, String)> {
    plan.host_plans
        .iter()
        .map(|hp| {
            let mut script = String::new();
            writeln!(script, "#!/usr/bin/env bash\nset -euo pipefail\n").unwrap();
            writeln!(
                script,
                "echo \"[host:{}] cleaning VLANs and VXLAN devices\"",
                hp.host
            )
            .unwrap();

            // Remove VLAN subinterfaces and tc
            for vl in &hp.vlan_links {
                let sub = format!("{}.{:}", vl.vxlan_dev, vl.vlan_id);
                let br = format!("br-vw-{}", vl.vlan_id);
                writeln!(
                    script,
                    "tc qdisc del dev {sub} root 2>/dev/null || true\nip link del {sub} 2>/dev/null || true\nip link set {br} down 2>/dev/null || true\nip link del {br} 2>/dev/null || true"
                )
                .unwrap();
                // Remove any attached veths on this vlan subinterface
                writeln!(
                    script,
                    "for v in $(ip -o link show | awk -F': ' '/veth-{}/{}/ {{print $2}}' | tr -d ':'); do ip link del $v 2>/dev/null || true; done",
                    vl.vlan_id, vl.vxlan_dev
                )
                .unwrap();
            }

            // Remove VXLAN devices
            for vx in &hp.vxlan_devices {
                writeln!(
                    script,
                    "ip link set {dev} down 2>/dev/null || true\nip link del {dev} 2>/dev/null || true",
                    dev = vx.name
                )
                .unwrap();
            }

            (hp.host.clone(), script)
        })
        .collect()
}

fn render_vxlan(out: &mut String, vx: &VxlanDevice) {
    writeln!(
        out,
        "\n# VXLAN to {} on dev {}\nif ip link show {} >/dev/null 2>&1; then\n  ip link set {} down || true\n  ip link del {} || true\nfi",
        vx.peer, vx.name, vx.name, vx.name, vx.name
    )
    .unwrap();
    writeln!(
        out,
        "ip link add {name} type vxlan id {vni} local {local} remote {remote} dstport 4789 || true",
        name = vx.name,
        vni = vx.vni,
        local = vx.local_underlay,
        remote = vx.remote_underlay
    )
    .unwrap();
    writeln!(out, "ip link set {0} master $BR", vx.name).unwrap();
    writeln!(out, "ip link set {0} up", vx.name).unwrap();
}

fn render_vlan(
    out: &mut String,
    vl: &VlanLink,
    bindings: Option<&std::collections::HashMap<u16, String>>,
) {
    let parent = bindings
        .and_then(|m| m.get(&vl.vlan_id))
        .cloned()
        .unwrap_or_else(|| vl.vxlan_dev.clone());
    let sub = format!("{}.{:}", parent, vl.vlan_id);
    let br = format!("br-vw-{}", vl.vlan_id);
    writeln!(
        out,
        "\n# VLAN {} on {}\nif ip link show {sub} >/dev/null 2>&1; then\n  ip link set {sub} down || true\n  ip link del {sub} || true\nfi",
        vl.vlan_id, parent
    )
    .unwrap();
    writeln!(
        out,
        "ip link add link {dev} name {sub} type vlan id {vid}",
        dev = parent,
        vid = vl.vlan_id
    )
    .unwrap();
    writeln!(
        out,
        "ip link add name {br} type bridge 2>/dev/null || true\nip link set {br} up\nip link set {sub} master {br}\nip link set {sub} up",
        br = br
    )
    .unwrap();
    // Apply impairment via tc if provided
    if let Some(imp) = &vl.impairment {
        writeln!(out, "tc qdisc del dev {sub} root 2>/dev/null || true").unwrap();
        writeln!(out, "tc qdisc add dev {sub} root handle 1: htb default 1").unwrap();
        if let Some(rate) = imp.rate_mbps {
            writeln!(
                out,
                "tc class add dev {sub} parent 1: classid 1:1 htb rate {rate}mbit ceil {rate}mbit",
            )
            .unwrap();
        }
        if imp.latency_ms.is_some() || imp.loss_pct.is_some() {
            let mut netem = format!("tc qdisc add dev {sub} parent 1:1 handle 10: netem",);
            if let Some(lat) = imp.latency_ms {
                write!(netem, " delay {lat}ms").unwrap();
            }
            if let Some(loss) = imp.loss_pct {
                write!(netem, " loss {loss}%").unwrap();
            }
            writeln!(out, "{}", netem).unwrap();
        }
    }
}
