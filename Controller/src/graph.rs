use std::collections::{HashMap, VecDeque};

use serde::Deserialize;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Node {
    pub name: String,
    pub node_type: String,
    pub interfaces: HashMap<String, Vec<String>>,
    pub neighbors: Vec<(String, usize)>, // (neighbor_name, link_index)
}

impl Node {
    pub fn new(name: &str, node_type: &str) -> Self {
        Self {
            name: name.to_string(),
            node_type: node_type.to_string(),
            interfaces: HashMap::new(),
            neighbors: Vec::new(),
        }
    }

    pub fn add_interface(&mut self, iface: &str, ip: &str) {
        if ip.eq_ignore_ascii_case("N/A") {
            return;
        }
        self.interfaces.entry(iface.to_string()).or_default().push(ip.to_string());
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Link {
    pub intf1: String,
    pub intf2: String,
    pub ip1: String,
    pub ip2: String,
    pub node1: String,
    pub node2: String,
    pub status: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Segment {
    pub from: String,
    pub to: String,
    pub from_interface: String,
    pub to_interface: String,
    pub from_ip: String,
    pub to_ip: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct Graph {
    pub nodes: HashMap<String, Node>,
    pub links: Vec<Link>,
}

impl Graph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            links: Vec::new(),
        }
    }

    pub fn add_node(&mut self, name: &str, node_type: &str) {
        self.nodes.entry(name.to_string()).or_insert_with(|| Node::new(name, node_type));
    }

    pub fn add_link(&mut self, link: Link) {
        let index = self.links.len();
        self.links.push(link.clone());

        self.nodes.entry(link.node1.clone())
            .or_insert_with(|| Node::new(&link.node1, "Unknown"))
            .add_interface(&link.intf1, &link.ip1);

        self.nodes.entry(link.node2.clone())
            .or_insert_with(|| Node::new(&link.node2, "Unknown"))
            .add_interface(&link.intf2, &link.ip2);

        self.nodes.get_mut(&link.node1).unwrap().neighbors.push((link.node2.clone(), index));
        self.nodes.get_mut(&link.node2).unwrap().neighbors.push((link.node1.clone(), index));
    }

    pub fn shortest_path(&self, start: &str, end: &str) -> Option<(Vec<String>, Vec<Segment>)> {
        let mut queue = VecDeque::new();
        let mut visited = HashMap::new();

        queue.push_back(start.to_string());
        visited.insert(start.to_string(), None);

        while let Some(current) = queue.pop_front() {
            if current == end {
                break;
            }

            for (neighbor_name, link_index) in &self.nodes[&current].neighbors {
                if !visited.contains_key(neighbor_name) {
                    visited.insert(neighbor_name.clone(), Some((current.clone(), *link_index)));
                    queue.push_back(neighbor_name.clone());
                }
            }
        }

        if !visited.contains_key(end) {
            return None;
        }

        let mut node_path = vec![end.to_string()];
        let mut segments = vec![];

        let mut current = end.to_string();
        while let Some((prev, link_idx)) = visited[&current].clone() {
            node_path.push(prev.clone());

            let link = &self.links[link_idx];
            let (segment, next) = if link.node1 == prev && link.node2 == current {
                (Segment {
                    from: prev.clone(),
                    to: current.clone(),
                    from_interface: link.intf1.clone(),
                    to_interface: link.intf2.clone(),
                    from_ip: link.ip1.clone(),
                    to_ip: link.ip2.clone(),
                    status: link.status.clone(),
                }, prev)
            } else {
                (Segment {
                    from: prev.clone(),
                    to: current.clone(),
                    from_interface: link.intf2.clone(),
                    to_interface: link.intf1.clone(),
                    from_ip: link.ip2.clone(),
                    to_ip: link.ip1.clone(),
                    status: link.status.clone(),
                }, prev)
            };
            segments.push(segment);
            current = next;
        }

        node_path.reverse();
        segments.reverse();
        Some((node_path, segments))
    }

    
    #[allow(dead_code)]
    pub fn ip_mapping_from(&self, start: &str) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for node_name in self.nodes.keys() {
            if node_name == start {
                continue;
            }
            if let Some((_path, segments)) = self.shortest_path(start, node_name) {
                if let Some(last) = segments.last() {
                    if !last.to_ip.eq_ignore_ascii_case("N/A") {
                        map.insert(node_name.clone(), last.to_ip.clone());
                    }
                }
            }
        }
        map
    }

    /// For each destination, returns a vector of (in_interface, out_interface) per hop.
    /// - in_interface is None for the source node.
    /// - out_interface is None for the destination node.
    #[allow(clippy::type_complexity)]
    pub fn interface_hops_from(&self, start: &str) -> HashMap<String, Vec<(Option<String>, Option<String>)>> {
        let mut map = HashMap::new();

        for node_name in self.nodes.keys() {
            if node_name == start {
                continue;
            }

            if let Some((path, segments)) = self.shortest_path(start, node_name) {
                let mut hop_interfaces = Vec::new();

                for i in 0..path.len() {
                    let in_iface = if i == 0 {
                        None
                    } else {
                        let seg = &segments[i - 1];
                        if seg.to == path[i] {
                            Some(seg.to_interface.clone())
                        } else if seg.from == path[i] {
                            Some(seg.from_interface.clone())
                        } else {
                            None
                        }
                    };

                    let out_iface = if i == path.len() - 1 {
                        None
                    } else {
                        let seg = &segments[i];
                        if seg.from == path[i] {
                            Some(seg.from_interface.clone())
                        } else if seg.to == path[i] {
                            Some(seg.to_interface.clone())
                        } else {
                            None
                        }
                    };

                    hop_interfaces.push((in_iface, out_iface));
                }

                map.insert(node_name.clone(), hop_interfaces);
            }
        }

        map
    }
}
