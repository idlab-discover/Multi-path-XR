// In ExperimentHandler (experiment.rs)
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
//#[serde(rename_all = "camelCase")]
pub struct Action {
    pub action: String,
    #[serde(rename = "type")]
    pub action_type: String,
    pub target: Option<String>,
    pub execution_delay: Option<u64>,
    pub connected_node: Option<String>,
    pub bandwidth: Option<String>,
    pub packet_loss: Option<String>,
    pub network_delay: Option<String>,
    pub htb_explicit_limits: Option<bool>,
    pub url: Option<String>,
    pub command: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
//#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub name: String,
    pub number_of_nodes: u32,
    pub number_of_paths: u32,
    pub topology: Option<String>,
    pub geant_cities: Option<String>,
    pub geant_weighted_nexthops: Option<bool>,
    pub geant_hop_weights: Option<String>,
    pub roles: Vec<Role>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
//#[serde(rename_all = "camelCase")]
pub struct Role {
    pub role: String,
    pub target: String,
    pub alias: String,
    pub count: Option<u32>,
    pub server_ip: Option<String>, // Legacy, use http_ip instead
    pub http_ip: Option<String>,
    pub websocket_ip: Option<String>,
    pub proxy: Option<bool>,
    pub disable_parser: Option<bool>,
    pub visible: Option<bool>,
    pub moq_enable: Option<bool>,
    pub moq_relay: Option<bool>,
    pub moq_namespace: Option<String>,
    pub moq_client: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
//#[serde(rename_all = "camelCase")]
pub struct ExperimentFile {
    pub experiment_name: String,
    pub description: Option<String>,
    pub environment: Environment,
    pub actions: Option<Vec<Action>>,
}
