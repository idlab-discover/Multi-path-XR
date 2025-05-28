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
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
//#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub name: String,
    pub number_of_nodes: u32,
    pub number_of_paths: u32,
    pub roles: Vec<Role>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
//#[serde(rename_all = "camelCase")]
pub struct Role {
    pub role: String,
    pub target: String,
    pub alias: String,
    pub server_ip: Option<String>,
    pub disable_parser: Option<bool>,
    pub visible: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
//#[serde(rename_all = "camelCase")]
pub struct ExperimentFile {
    pub experiment_name: String,
    pub description: Option<String>,
    pub environment: Environment,
    pub actions: Option<Vec<Action>>,
}