use crate::{
    graph::{Graph, Link},
    handlers::environment::{
        BigVirtualWallHandler, DockerHandler, EnvironmentHandler, MininetHandler,
        VirtualWallHandler, VirtualWallLiteHandler,
    },
    metrics_logger::{MetricsLogger, MetricsLoggerError},
    structs::ExperimentFile,
};
use serde_json::Value;
use socketioxide::SocketIo;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

use super::action_executor::ActionExecutor;

const MININET_BASE_LINK_BW_MBIT: f64 = 1_000.0;

fn bandwidth_to_mbit(raw: &str) -> Option<f64> {
    let normalized: String = raw
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let number_end = normalized
        .find(|character: char| !(character.is_ascii_digit() || character == '.'))
        .unwrap_or(normalized.len());
    let (number, unit) = normalized.split_at(number_end);
    let value = number.parse::<f64>().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }

    let multiplier = match unit {
        "bit" | "bps" => 0.000_001,
        "kbit" | "kbps" => 0.001,
        "mbit" | "mbps" => 1.0,
        "gbit" | "gbps" => 1_000.0,
        "tbit" | "tbps" => 1_000_000.0,
        _ => return None,
    };
    Some(value * multiplier)
}

fn mininet_default_link_bw_mbit(experiment: &ExperimentFile) -> f64 {
    experiment
        .actions
        .iter()
        .flatten()
        .filter_map(|action| action.bandwidth.as_deref())
        .filter_map(bandwidth_to_mbit)
        .fold(MININET_BASE_LINK_BW_MBIT, f64::max)
}

pub struct ExperimentHandler {
    handlers: HashMap<String, Box<dyn EnvironmentHandler + Send + Sync>>,
    active_environment: Option<String>,
    current_experiment: Option<ExperimentFile>,
    action_executor: Option<ActionExecutor>,
    graph: Option<Graph>,
    metrics_logger: Option<MetricsLogger>,
}

impl Clone for ExperimentHandler {
    fn clone(&self) -> Self {
        let handlers = self
            .handlers
            .iter()
            .map(|(key, handler)| (key.clone(), dyn_clone::clone_box(&**handler)))
            .collect();
        Self {
            handlers,
            active_environment: self.active_environment.clone(),
            current_experiment: self.current_experiment.clone(),
            action_executor: self.action_executor.clone(),
            graph: self.graph.clone(),
            metrics_logger: self.metrics_logger.clone(),
        }
    }
}

impl ExperimentHandler {
    fn clear_environment_runtime_state(&mut self) {
        self.active_environment = None;
        self.current_experiment = None;
        self.action_executor = None;
        self.graph = None;
    }

    pub fn new() -> Self {
        let mut handlers: HashMap<String, Box<dyn EnvironmentHandler + Send + Sync>> =
            HashMap::new();
        handlers.insert("mininet".to_string(), Box::new(MininetHandler::new()));
        handlers.insert("docker".to_string(), Box::new(DockerHandler));
        handlers.insert(
            "virtualwall".to_string(),
            Box::new(VirtualWallHandler::new()),
        );
        handlers.insert(
            "virtualwalllite".to_string(),
            Box::new(VirtualWallLiteHandler::new()),
        );
        handlers.insert(
            "bigvirtualwall".to_string(),
            Box::new(BigVirtualWallHandler::new()),
        );
        Self {
            handlers,
            active_environment: None,
            current_experiment: None,
            action_executor: None,
            graph: None,
            metrics_logger: None,
        }
    }

    #[allow(dead_code)]
    pub fn get_current_experiment(&self) -> Option<ExperimentFile> {
        self.current_experiment.clone()
    }

    pub fn metrics_logger(&self) -> Result<MetricsLogger, MetricsLoggerError> {
        self.metrics_logger
            .clone()
            .ok_or(MetricsLoggerError::LoggerNotInitialized)
    }

    pub fn route_updates_enabled(&self) -> bool {
        let Some(experiment) = &self.current_experiment else {
            return false;
        };

        let topology_is_geant = experiment
            .environment
            .topology
            .as_deref()
            .map(|v| v.trim().eq_ignore_ascii_case("geant"))
            .unwrap_or(false);

        let weighted_enabled = experiment
            .environment
            .geant_weighted_nexthops
            .unwrap_or(false);

        topology_is_geant && weighted_enabled
    }

    pub async fn start_environment(
        &mut self,
        env: &str,
        experiment_filename: &str,
        io: Arc<SocketIo>,
        agent_registry: Arc<Mutex<HashMap<String, String>>>,
    ) -> Result<String, String> {
        if !self.handlers.contains_key(env) {
            return Err(format!("Environment '{env}' is not supported"));
        }

        self.clear_environment_runtime_state();
        self.active_environment = Some(env.to_string());

        let path = format!("./dist/experiments/{experiment_filename}");
        let contents =
            std::fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {e}"))?;
        let mut parsed: ExperimentFile =
            serde_yaml::from_str(&contents).map_err(|e| format!("Failed to parse YAML: {e}"))?;

        for role in &mut parsed.environment.roles {
            if role.visible.is_none() {
                role.visible = Some(false);
            }
            if role.disable_parser.is_none() {
                role.disable_parser = Some(false);
            }
            if role.proxy.is_none() {
                role.proxy = Some(false);
            }
        }

        let n_paths = parsed.environment.number_of_paths;
        let n_nodes = parsed.environment.number_of_nodes;
        let topology = parsed.environment.topology.clone();
        let geant_cities = parsed.environment.geant_cities.clone();
        let geant_weighted_nexthops = parsed.environment.geant_weighted_nexthops;
        let geant_hop_weights = parsed.environment.geant_hop_weights.clone();

        let mut options = format!("n_nodes={n_nodes}&n_paths={n_paths}");
        if let Some(topology) = topology {
            let normalized = topology.trim().to_lowercase();
            if !normalized.is_empty() {
                options.push_str(&format!("&topology={normalized}"));
            }
        }

        if let Some(cities) = geant_cities {
            let normalized = cities.trim().replace(' ', "");
            if !normalized.is_empty() {
                options.push_str(&format!("&geant_cities={normalized}"));
            }
        }

        if let Some(weighted) = geant_weighted_nexthops {
            options.push_str(&format!("&geant_weighted_nexthops={weighted}"));
        }

        if let Some(weights) = geant_hop_weights {
            let normalized = weights.trim().replace(' ', "");
            if !normalized.is_empty() {
                options.push_str(&format!("&geant_hop_weights={normalized}"));
            }
        }

        if env == "mininet" {
            let default_link_bw_mbit = mininet_default_link_bw_mbit(&parsed);
            if default_link_bw_mbit > MININET_BASE_LINK_BW_MBIT {
                options.push_str(&format!("&default_link_bw_mbit={default_link_bw_mbit}"));
            }
        }

        self.current_experiment = Some(parsed);

        let result = {
            let handler = self.handlers.get(env).unwrap();
            handler.start(&options).await
        };
        if let Err(err) = &result {
            self.clear_environment_runtime_state();
            return Err(format!("Failed to start environment '{env}': {}", err));
        } else if let Some(experiment) = self.current_experiment.clone() {
            // TODO: allow the logger to be disabled from the yaml
            let logger = MetricsLogger::new(experiment_filename)
                .await
                .map_err(|e| format!("{e:?}"))?;
            logger.clone().start().await.map_err(|e| format!("{e:?}"))?;
            self.metrics_logger = Some(logger);

            self.generate_graph().await?;

            if let Some(executor) = ActionExecutor::new_from_experiment(
                &experiment,
                io.clone(),
                self.graph.clone(),
                agent_registry.clone(),
            ) {
                self.action_executor = Some(executor);
            }
        }
        Ok(format!("Environment '{env}' started successfully"))
    }

    pub async fn start_actions(&self) -> Result<String, String> {
        if let Some(executor) = &self.action_executor {
            executor.start().await;
            Ok("Experiment actions started successfully".to_string())
        } else {
            Ok("No experiment actions are pending for this environment".to_string())
        }
    }

    pub async fn stop_environment(&mut self) -> Result<String, String> {
        // Cancel the measurements logger when stopping the environment
        if let Some(lg) = self.metrics_logger.take() {
            lg.stop().await.ok();
        }

        // Cancel actions before stopping the environment
        if let Some(executor) = self.action_executor.take() {
            executor.stop(); // Send cancellation signal
        }

        // Cancel the environment itself
        if let Some(env) = &self.active_environment {
            let result = {
                let handler = self.handlers.get(env).unwrap();
                handler.stop().await
            };
            if result.is_ok() {
                self.clear_environment_runtime_state();
            }
            result
        } else {
            Err("No active environment to stop".to_string())
        }
    }

    pub async fn cleanup_environment_processes(&self) -> Result<String, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.cleanup_processes().await
        } else {
            Err("No active environment to clean up".to_string())
        }
    }

    pub async fn exec_command(&self, params: HashMap<String, String>) -> Result<String, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.exec(params).await
        } else {
            Err("No active environment to execute command".to_string())
        }
    }

    pub async fn get_nodes(&self) -> Result<Value, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.nodes().await
        } else {
            Err("No active environment to get nodes".to_string())
        }
    }

    pub async fn get_links(&self) -> Result<Value, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.links().await
        } else {
            Err("No active environment to get links".to_string())
        }
    }

    pub async fn get_status(&self) -> Result<Value, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.status().await
        } else {
            Err("No active environment to get status".to_string())
        }
    }

    pub async fn get_visualization(&self) -> Result<Vec<u8>, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.visualize().await
        } else {
            Err("No active environment to visualize".to_string())
        }
    }

    pub async fn start_xterm(&self, params: HashMap<String, String>) -> Result<String, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.start_xterm(params).await
        } else {
            Err("No active environment to start xterm".to_string())
        }
    }

    pub async fn ping_all(&self) -> Result<Value, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.ping_all().await
        } else {
            Err("No active environment to ping".to_string())
        }
    }

    pub async fn open_tunnel(&self, params: HashMap<String, String>) -> Result<Value, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.open_tunnel(params).await
        } else {
            Err("No active environment to open a tunnel".to_string())
        }
    }

    pub async fn close_tunnel(&self, id: &str) -> Result<String, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.close_tunnel(id).await
        } else {
            Err("No active environment to close a tunnel".to_string())
        }
    }

    pub async fn list_tunnels(&self) -> Result<Value, String> {
        if let Some(env) = &self.active_environment {
            let handler = self.handlers.get(env).unwrap();
            handler.list_tunnels().await
        } else {
            Err("No active environment to list tunnels".to_string())
        }
    }

    async fn generate_graph(&mut self) -> Result<(), String> {
        let nodes_val = self
            .get_nodes()
            .await
            .map_err(|e| format!("Failed to get nodes: {e}"))?;
        let links_val = self
            .get_links()
            .await
            .map_err(|e| format!("Failed to get links: {e}"))?;

        let nodes: Vec<serde_json::Value> =
            serde_json::from_value(nodes_val).map_err(|e| format!("Invalid nodes JSON: {e}"))?;
        let links: Vec<serde_json::Value> =
            serde_json::from_value(links_val).map_err(|e| format!("Invalid links JSON: {e}"))?;

        let mut graph = Graph::new();
        for node in nodes {
            // info!("Processing node: {node}");
            let name = node
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let typ = node
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            graph.add_node(name, typ);
        }
        for link in links {
            // info!("Processing link: {link}");
            let link: Link =
                serde_json::from_value(link).map_err(|e| format!("Invalid link format: {e}"))?;
            graph.add_link(link);
        }
        self.graph = Some(graph);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{bandwidth_to_mbit, mininet_default_link_bw_mbit};
    use crate::structs::ExperimentFile;

    fn experiment_from_yaml(actions: &str) -> ExperimentFile {
        serde_yaml::from_str(&format!(
            r#"
experiment_name: bandwidth-test
environment:
  name: mininet
  number_of_nodes: 2
  number_of_paths: 2
  roles: []
actions:
{actions}
"#
        ))
        .expect("test experiment should parse")
    }

    #[test]
    fn parses_supported_tc_bandwidth_units() {
        assert_eq!(bandwidth_to_mbit("5gbit"), Some(5_000.0));
        assert_eq!(bandwidth_to_mbit("1.5 Gbps"), Some(1_500.0));
        assert_eq!(bandwidth_to_mbit("750mbit"), Some(750.0));
        assert_eq!(bandwidth_to_mbit("1000000kbit"), Some(1_000.0));
        assert_eq!(bandwidth_to_mbit("unlimited"), None);
    }

    #[test]
    fn retains_one_gbit_default_when_no_action_exceeds_it() {
        let experiment = experiment_from_yaml(
            r#"  - action: cap
    type: tc
    bandwidth: 200mbit"#,
        );

        assert_eq!(mininet_default_link_bw_mbit(&experiment), 1_000.0);
    }

    #[test]
    fn uses_highest_bandwidth_above_one_gbit() {
        let experiment = experiment_from_yaml(
            r#"  - action: first cap
    type: tc
    bandwidth: 2.5gbit
  - action: second cap
    type: tc
    bandwidth: 5gbit
  - action: lower cap
    type: tc
    bandwidth: 900mbit"#,
        );

        assert_eq!(mininet_default_link_bw_mbit(&experiment), 5_000.0);
    }
}
