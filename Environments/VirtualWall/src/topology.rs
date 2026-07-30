use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{Result, VirtualWallError},
    resource_spec::{
        LinkDefinition, LinkImpairment, NetworkInterface, ResourceDefinition, ResourceSpec,
    },
};

/// Topology specification to generate a resource request with selective fabric attachments.
///
/// This is the "single source of truth" for:
/// - which nodes exist,
/// - which fabrics (VLAN-backed L2 segments) exist,
/// - how nodes attach to those fabrics,
/// - optional impairment and deterministic addressing.
///
/// Notes:
/// - The Virtual Wall control interface is managed by the testbed and **must not** be modeled here.
/// - `max_experiment_nics_per_node` applies to **experiment** NICs only (excluding control).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopologySpec {
    /// Optional default site identifier for all nodes.
    #[serde(default)]
    pub site_id: Option<String>,
    /// Optional default disk image for all nodes.
    #[serde(default)]
    pub image: Option<String>,
    /// Optional default flavor for all nodes.
    #[serde(default)]
    pub flavor: Option<String>,
    /// Optional cloud-init/user-data file path for all nodes.
    #[serde(default)]
    pub cloud_init: Option<String>,
    /// Optional prefix applied to node names when generating a resource spec.
    #[serde(default)]
    pub resource_prefix: Option<String>,
    /// Port-id naming scheme, e.g. `if0`, `if1`, ...
    #[serde(default)]
    pub port_id_scheme: PortIdScheme,
    /// Max experiment NICs per node (excluding the control interface).
    ///
    /// Virtual Wall nodes have an upper bound on usable NICs; keeping this low avoids provisioning errors.
    #[serde(default = "default_max_experiment_nics_per_node")]
    pub max_experiment_nics_per_node: usize,
    /// Fabric/VLAN definitions.
    pub fabrics: Vec<FabricSpec>,
    /// Role definitions that can be referenced by nodes.
    #[serde(default)]
    pub roles: Vec<RoleSpec>,
    /// Concrete nodes.
    pub nodes: Vec<NodeSpec>,
    /// Optional logical hierarchy metadata (not required for provisioning).
    #[serde(default)]
    pub hierarchy: Option<HierarchySpec>,
}

/// Port-id naming scheme for `NetworkInterface.port_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortIdScheme {
    /// Prefix for the port id (e.g., `if`).
    #[serde(default = "default_port_prefix")]
    pub prefix: String,
    /// Starting index (e.g., 0 -> `if0`).
    #[serde(default)]
    pub start_index: u16,
}

impl Default for PortIdScheme {
    fn default() -> Self {
        Self {
            prefix: default_port_prefix(),
            start_index: 0,
        }
    }
}

fn default_port_prefix() -> String {
    "if".to_string()
}

fn default_max_experiment_nics_per_node() -> usize {
    4
}

/// A named fabric (VLAN/L2 segment).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricSpec {
    /// Logical name used by nodes to reference this fabric.
    pub name: String,
    /// `network_id` used by SLICES.
    #[serde(default)]
    pub network_id: Option<String>,
    /// Link type (defaults to `"lan"`).
    #[serde(default = "default_link_type")]
    pub r#type: String,
    /// Optional shared LAN name (GRE/shared-lan use cases).
    #[serde(default)]
    pub share_lan_name: Option<String>,
    /// Optional deterministic IPv4 subnet for this fabric.
    ///
    /// Example: `"13.0.1.0/24"`.
    #[serde(default)]
    pub ipv4_subnet: Option<String>,
    /// First host address offset inside `ipv4_subnet` to allocate to nodes.
    ///
    /// Default is `10` to leave room for infra/reserved addresses.
    #[serde(default = "default_host_start")]
    pub host_start: u32,
    /// Optional impairment configuration.
    ///
    /// For LAN-wide symmetric impairment, omit `source` and `destination`.
    #[serde(default)]
    pub impairment: Vec<LinkImpairment>,
    /// Optional stable ordering hint for port assignment.
    #[serde(default)]
    pub order: Option<u32>,
}

fn default_link_type() -> String {
    "lan".to_string()
}

fn default_host_start() -> u32 {
    10
}

/// A role definition (reusable attachment bundle).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSpec {
    pub name: String,
    /// Fabrics this role attaches to.
    #[serde(default)]
    pub fabrics: Vec<String>,
}

/// A concrete node in the topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSpec {
    pub name: String,
    pub role: String,
    /// Override fabrics for this node (otherwise uses the role's fabrics).
    #[serde(default)]
    pub fabrics: Vec<String>,
    /// Optional explicit per-fabric interface addresses (CIDRs).
    ///
    /// Key is fabric name; value is a list of CIDR strings, e.g. `"13.0.1.2/24"`.
    #[serde(default)]
    pub addresses: BTreeMap<String, Vec<String>>,
    /// Optional node-specific overrides.
    #[serde(default)]
    pub site_id: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub flavor: Option<String>,
    #[serde(default)]
    pub cloud_init: Option<String>,
}

/// Optional logical hierarchy, persisted for orchestration/debugging.
/// Provisioning does not require this field.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HierarchySpec {
    /// Mapping parent -> list of children.
    #[serde(default)]
    pub parents: BTreeMap<String, Vec<String>>,
}

/// Generated resource spec and a deterministic mapping that can be persisted.
#[derive(Debug, Clone)]
pub struct GeneratedTopology {
    pub spec: ResourceSpec,
    pub state: TopologyState,
}

/// Persisted, deterministic mapping of nodes/fabrics/interfaces.
/// Stored in `VirtualWallState` to support repeatable orchestration on a recovered infrastructure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TopologyState {
    pub fabrics: Vec<FabricState>,
    pub nodes: Vec<NodeState>,
    #[serde(default)]
    pub hierarchy: Option<HierarchySpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FabricState {
    pub name: String,
    pub network_id: String,
    pub r#type: String,
    #[serde(default)]
    pub share_lan_name: Option<String>,
    #[serde(default)]
    pub ipv4_subnet: Option<String>,
    #[serde(default)]
    pub impairment: Vec<LinkImpairment>,
    #[serde(default)]
    pub order: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeState {
    pub name: String,
    pub role: String,
    /// Attachments sorted by fabric order.
    pub attachments: Vec<AttachmentState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentState {
    pub fabric: String,
    pub network_id: String,
    pub port_id: String,
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// Generate a `ResourceSpec` from a `TopologySpec` with deterministic wiring.
///
/// - Fabric order is stable: `order` then `name`.
/// - Node allocation order is stable: node `name` ascending.
/// - IP allocation order for a fabric is stable: participating node `name` ascending.
pub fn generate(spec: &TopologySpec) -> Result<GeneratedTopology> {
    validate_topology(spec)?;

    let mut fabrics: Vec<FabricSpec> = spec.fabrics.clone();
    fabrics.sort_by(|a, b| {
        let ao = a.order.unwrap_or(u32::MAX);
        let bo = b.order.unwrap_or(u32::MAX);
        ao.cmp(&bo).then_with(|| a.name.cmp(&b.name))
    });

    let roles: BTreeMap<String, RoleSpec> = spec
        .roles
        .iter()
        .cloned()
        .map(|r| (r.name.clone(), r))
        .collect();

    let fabric_by_name: BTreeMap<String, FabricSpec> = fabrics
        .iter()
        .cloned()
        .map(|f| (f.name.clone(), f))
        .collect();

    // Pre-compute per-fabric deterministic IPv4 allocators.
    let mut allocators: BTreeMap<String, Option<Ipv4Allocator>> = BTreeMap::new();
    for f in &fabrics {
        allocators.insert(
            f.name.clone(),
            match &f.ipv4_subnet {
                Some(cidr) => Some(Ipv4Allocator::new(cidr, f.host_start)?),
                None => None,
            },
        );
    }

    // Determine which nodes participate in which fabric for allocation.
    let mut nodes_sorted = spec.nodes.clone();
    nodes_sorted.sort_by(|a, b| a.name.cmp(&b.name));

    let mut participants: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in &nodes_sorted {
        let fabrics_for_node = effective_fabrics(node, &roles)?;
        for fab in fabrics_for_node {
            participants.entry(fab).or_default().push(node.name.clone());
        }
    }
    for (_fab, nodes) in participants.iter_mut() {
        nodes.sort();
        nodes.dedup();
    }

    let mut resources = Vec::with_capacity(nodes_sorted.len());
    let mut node_states = Vec::with_capacity(nodes_sorted.len());

    for node in nodes_sorted {
        let eff_fabrics = effective_fabrics(&node, &roles)?;
        let mut eff_fabrics_sorted: Vec<String> = eff_fabrics.into_iter().collect();
        eff_fabrics_sorted.sort_by(|a, b| {
            let ao = fabric_by_name
                .get(a)
                .and_then(|f| f.order)
                .unwrap_or(u32::MAX);
            let bo = fabric_by_name
                .get(b)
                .and_then(|f| f.order)
                .unwrap_or(u32::MAX);
            ao.cmp(&bo).then_with(|| a.cmp(b))
        });

        if eff_fabrics_sorted.len() > spec.max_experiment_nics_per_node {
            return Err(VirtualWallError::Configuration(format!(
                "Node '{}' attaches to {} experiment fabrics, exceeding max_experiment_nics_per_node={}",
                node.name,
                eff_fabrics_sorted.len(),
                spec.max_experiment_nics_per_node
            )));
        }

        let mut ifaces = Vec::with_capacity(eff_fabrics_sorted.len());
        let mut attachments = Vec::with_capacity(eff_fabrics_sorted.len());

        for (idx, fab_name) in eff_fabrics_sorted.iter().enumerate() {
            let fabric = fabric_by_name.get(fab_name).ok_or_else(|| {
                VirtualWallError::Configuration(format!(
                    "Unknown fabric '{fab_name}' referenced by node '{}'",
                    node.name
                ))
            })?;
            let port_id = format!(
                "{}{}",
                spec.port_id_scheme.prefix,
                spec.port_id_scheme.start_index as usize + idx
            );
            let network_id = fabric
                .network_id
                .clone()
                .unwrap_or_else(|| fabric.name.clone());

            let mut addresses: Vec<String> =
                node.addresses.get(fab_name).cloned().unwrap_or_default();

            if addresses.is_empty() {
                if let Some(alloc) = allocators.get_mut(fab_name).and_then(|o| o.as_mut()) {
                    let nodes_for_fabric = participants.get(fab_name).cloned().unwrap_or_default();
                    let ordinal = nodes_for_fabric
                        .iter()
                        .position(|n| n == &node.name)
                        .ok_or_else(|| VirtualWallError::Internal(format!(
                            "Internal allocation error: node '{}' not registered as participant for fabric '{}'",
                            node.name, fab_name
                        )))? as u32;

                    if fabric.ipv4_subnet.is_some() {
                        let addr = alloc.allocate(ordinal)?;
                        addresses.push(format!("{}/{}", addr, alloc.prefix));
                    }
                }
            }

            ifaces.push(NetworkInterface {
                port_id: Some(port_id.clone()),
                network_id: network_id.clone(),
                addresses: addresses.clone(),
            });

            attachments.push(AttachmentState {
                fabric: fab_name.clone(),
                network_id,
                port_id,
                addresses,
            });
        }

        resources.push(ResourceDefinition {
            friendly_name: format!(
                "{}{}",
                spec.resource_prefix.clone().unwrap_or_default(),
                node.name
            ),
            site_id: node.site_id.clone().or_else(|| spec.site_id.clone()),
            disk_image: node.image.clone().or_else(|| spec.image.clone()),
            flavor: node.flavor.clone().or_else(|| spec.flavor.clone()),
            userdata_file: node.cloud_init.clone().or_else(|| spec.cloud_init.clone()),
            network_interfaces: ifaces,
        });

        // Deterministic validation: fail early if required fields are missing.
        let last = resources.last().expect("just pushed");
        if last.site_id.as_deref().unwrap_or("").trim().is_empty() {
            return Err(VirtualWallError::Configuration(format!(
                "TopologySpec generated resource '{}' without infra_id/site_id; set `site_id` on topology or node",
                last.friendly_name
            )));
        }
        if last.disk_image.as_deref().unwrap_or("").trim().is_empty() {
            return Err(VirtualWallError::Configuration(format!(
                "TopologySpec generated resource '{}' without disk_image; set `image` on topology or node",
                last.friendly_name
            )));
        }
        if last.flavor.as_deref().unwrap_or("").trim().is_empty() {
            return Err(VirtualWallError::Configuration(format!(
                "TopologySpec generated resource '{}' without flavor; set `flavor` on topology or node",
                last.friendly_name
            )));
        }

        node_states.push(NodeState {
            name: node.name.clone(),
            role: node.role.clone(),
            attachments,
        });
    }

    let links = fabrics
        .clone()
        .into_iter()
        .map(|f| LinkDefinition {
            friendly_name: f.name.clone(),
            network_id: f.network_id.clone().unwrap_or_else(|| f.name.clone()),
            r#type: Some(f.r#type.clone()),
            share_lan_name: f.share_lan_name.clone(),
            impairment: f.impairment.clone(),
        })
        .collect::<Vec<_>>();

    let state = TopologyState {
        fabrics: fabrics
            .iter()
            .map(|f| {
                let name = f.name.clone();
                let network_id = f.network_id.clone().unwrap_or_else(|| name.clone());
                FabricState {
                    name,
                    network_id,
                    r#type: f.r#type.clone(),
                    share_lan_name: f.share_lan_name.clone(),
                    ipv4_subnet: f.ipv4_subnet.clone(),
                    impairment: f.impairment.clone(),
                    order: f.order,
                }
            })
            .collect(),
        nodes: node_states,
        hierarchy: spec.hierarchy.clone(),
    };

    Ok(GeneratedTopology {
        spec: ResourceSpec { resources, links },
        state,
    })
}

fn effective_fabrics(
    node: &NodeSpec,
    roles: &BTreeMap<String, RoleSpec>,
) -> Result<BTreeSet<String>> {
    let mut fabrics = BTreeSet::new();

    for f in &node.fabrics {
        fabrics.insert(f.clone());
    }

    if fabrics.is_empty() {
        let role = roles.get(&node.role).ok_or_else(|| {
            VirtualWallError::Configuration(format!(
                "Node '{}' references unknown role '{}', and provides no explicit fabrics",
                node.name, node.role
            ))
        })?;
        for f in &role.fabrics {
            fabrics.insert(f.clone());
        }
    }

    Ok(fabrics)
}

fn validate_topology(spec: &TopologySpec) -> Result<()> {
    if spec.fabrics.is_empty() {
        return Err(VirtualWallError::Configuration(
            "TopologySpec requires at least one fabric".to_string(),
        ));
    }
    if spec.nodes.is_empty() {
        return Err(VirtualWallError::Configuration(
            "TopologySpec requires at least one node".to_string(),
        ));
    }

    let mut fabric_names = BTreeSet::new();
    for f in &spec.fabrics {
        if !fabric_names.insert(f.name.clone()) {
            return Err(VirtualWallError::Configuration(format!(
                "Duplicate fabric name '{}'",
                f.name
            )));
        }
        if let Some(cidr) = &f.ipv4_subnet {
            Ipv4Allocator::validate_cidr(cidr)?;
        }
    }

    let mut node_names = BTreeSet::new();
    for n in &spec.nodes {
        if !node_names.insert(n.name.clone()) {
            return Err(VirtualWallError::Configuration(format!(
                "Duplicate node name '{}'",
                n.name
            )));
        }
    }

    // Validate role uniqueness.
    let mut role_names = BTreeSet::new();
    for r in &spec.roles {
        if !role_names.insert(r.name.clone()) {
            return Err(VirtualWallError::Configuration(format!(
                "Duplicate role name '{}'",
                r.name
            )));
        }
    }

    // Validate node fabric references.
    let roles: BTreeMap<String, RoleSpec> = spec
        .roles
        .iter()
        .cloned()
        .map(|r| (r.name.clone(), r))
        .collect();

    for n in &spec.nodes {
        let eff = effective_fabrics(n, &roles)?;
        for f in &eff {
            if !fabric_names.contains(f) {
                return Err(VirtualWallError::Configuration(format!(
                    "Node '{}' references unknown fabric '{}'",
                    n.name, f
                )));
            }
        }
    }

    Ok(())
}

/// Deterministic IPv4 allocator for a subnet.
#[derive(Debug, Clone)]
struct Ipv4Allocator {
    net: u32,
    prefix: u8,
    host_start: u32,
    host_max: u32,
}

impl Ipv4Allocator {
    fn new(cidr: &str, host_start: u32) -> Result<Self> {
        let (net, prefix) = Self::parse_cidr(cidr)?;
        if prefix > 30 {
            return Err(VirtualWallError::Configuration(format!(
                "IPv4 subnet '{cidr}' is too small for host allocation (prefix {prefix}); use <= /30"
            )));
        }
        let host_bits = 32u8 - prefix;
        let host_max = (1u64 << host_bits) as u32;
        if host_start >= host_max.saturating_sub(1) {
            return Err(VirtualWallError::Configuration(format!(
                "IPv4 subnet '{cidr}' host_start={host_start} leaves no usable addresses"
            )));
        }
        Ok(Self {
            net,
            prefix,
            host_start,
            host_max,
        })
    }

    fn validate_cidr(cidr: &str) -> Result<()> {
        let _ = Self::parse_cidr(cidr)?;
        Ok(())
    }

    fn allocate(&self, ordinal: u32) -> Result<Ipv4Addr> {
        let offset = self.host_start.saturating_add(ordinal);
        if offset >= self.host_max.saturating_sub(1) {
            return Err(VirtualWallError::Configuration(format!(
                "IPv4 subnet allocation exhausted (prefix /{}); host_start={}, ordinal={}",
                self.prefix, self.host_start, ordinal
            )));
        }
        let ip_u32 = self.net + offset;
        Ok(Ipv4Addr::from(ip_u32))
    }

    fn parse_cidr(cidr: &str) -> Result<(u32, u8)> {
        let (ip_str, prefix_str) = cidr
            .split_once('/')
            .ok_or_else(|| VirtualWallError::Configuration(format!("Invalid CIDR '{cidr}'")))?;
        let prefix: u8 = prefix_str.parse().map_err(|_| {
            VirtualWallError::Configuration(format!("Invalid CIDR prefix in '{cidr}'"))
        })?;
        if prefix > 32 {
            return Err(VirtualWallError::Configuration(format!(
                "Invalid CIDR prefix {prefix} in '{cidr}'"
            )));
        }
        let ip: Ipv4Addr = ip_str.parse().map_err(|_| {
            VirtualWallError::Configuration(format!("Invalid IPv4 address in '{cidr}'"))
        })?;
        let ip_u32 = u32::from(ip);
        let mask = if prefix == 0 {
            0
        } else {
            (!0u32) << (32 - prefix)
        };
        let net = ip_u32 & mask;
        Ok((net, prefix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_deterministic_ports_and_ips() {
        let topo = TopologySpec {
            site_id: None,
            image: None,
            flavor: None,
            cloud_init: None,
            resource_prefix: Some("vw-".to_string()),
            port_id_scheme: PortIdScheme {
                prefix: "if".to_string(),
                start_index: 0,
            },
            max_experiment_nics_per_node: 4,
            fabrics: vec![
                FabricSpec {
                    name: "A".to_string(),
                    network_id: None,
                    r#type: "lan".to_string(),
                    share_lan_name: None,
                    ipv4_subnet: Some("10.1.0.0/24".to_string()),
                    host_start: 10,
                    impairment: vec![],
                    order: Some(1),
                },
                FabricSpec {
                    name: "B".to_string(),
                    network_id: None,
                    r#type: "lan".to_string(),
                    share_lan_name: None,
                    ipv4_subnet: Some("10.2.0.0/24".to_string()),
                    host_start: 10,
                    impairment: vec![],
                    order: Some(2),
                },
            ],
            roles: vec![RoleSpec {
                name: "server".to_string(),
                fabrics: vec!["A".to_string(), "B".to_string()],
            }],
            nodes: vec![NodeSpec {
                name: "server1".to_string(),
                role: "server".to_string(),
                fabrics: vec![],
                addresses: BTreeMap::new(),
                site_id: None,
                image: None,
                flavor: None,
                cloud_init: None,
            }],
            hierarchy: None,
        };

        let generated = generate(&topo).expect("generate");
        assert_eq!(generated.spec.resources.len(), 1);
        let ifaces = &generated.spec.resources[0].network_interfaces;
        assert_eq!(ifaces[0].port_id.as_deref(), Some("if0"));
        assert_eq!(ifaces[1].port_id.as_deref(), Some("if1"));
        assert_eq!(ifaces[0].addresses[0], "10.1.0.10/24");
        assert_eq!(ifaces[1].addresses[0], "10.2.0.10/24");
    }
}
