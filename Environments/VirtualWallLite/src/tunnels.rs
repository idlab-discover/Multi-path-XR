use serde::{Deserialize, Serialize};

/// Direction of the SSH tunnel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TunnelDirection {
    /// Create a local forward (`-L`): listen on the controller and forward to the node.
    Local,
    /// Create a remote forward (`-R`): listen on the node and forward back to the controller.
    Remote,
}

/// One endpoint of a tunnel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelEndpoint {
    pub host: String,
    pub port: u16,
}

/// A request to open a tunnel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelRequest {
    pub node: String,
    pub direction: TunnelDirection,
    pub listen: TunnelEndpoint,
    pub target: TunnelEndpoint,
    #[serde(default)]
    pub username: Option<String>,
}

/// Information about an active tunnel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TunnelInfo {
    pub id: String,
    pub node: String,
    pub direction: TunnelDirection,
    pub listen: TunnelEndpoint,
    pub target: TunnelEndpoint,
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}
