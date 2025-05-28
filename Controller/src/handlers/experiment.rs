use crate::{graph::{Graph, Link}, handlers::environment::{DockerHandler, EnvironmentHandler, MininetHandler, VirtualWallHandler}, metrics_logger::MetricsLogger, structs::ExperimentFile};
use std::{collections::HashMap, sync::Arc};
use serde_json::Value;
use socketioxide::SocketIo;

use super::action_executor::ActionExecutor;

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
    pub fn new() -> Self {
        let mut handlers: HashMap<String, Box<dyn EnvironmentHandler + Send + Sync>> = HashMap::new();
        handlers.insert("mininet".to_string(), Box::new(MininetHandler::new()));
        handlers.insert("docker".to_string(), Box::new(DockerHandler));
        handlers.insert("virtualwall".to_string(), Box::new(VirtualWallHandler));
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

    pub async fn start_environment(&mut self, env: &str, experiment_filename: &str, io: Arc<SocketIo>) -> Result<String, String> {
        let handler = self.handlers.get(env);
        if handler.is_none() {
            return Err(format!("Environment '{}' is not supported", env));
        }
        let handler = handler.unwrap();
        self.active_environment = Some(env.to_string());

        let path = format!("./dist/experiments/{}", experiment_filename);
        let contents = std::fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {e}"))?;
        let mut parsed: ExperimentFile = serde_yaml::from_str(&contents)
                    .map_err(|e| format!("Failed to parse YAML: {e}"))?;

        for role in &mut parsed.environment.roles {
            if role.visible.is_none() {
                role.visible = Some(false);
            }
            if role.disable_parser.is_none() {
                role.disable_parser = Some(false);
            }
        }

        let n_paths = parsed.environment.number_of_paths;
        let n_nodes = parsed.environment.number_of_nodes;
    
        let options = format!("n_nodes={}&n_paths={}", n_nodes, n_paths);

        self.current_experiment = Some(parsed);

        let result = handler.start(&options).await;
        if result.is_ok() {
            if let Some(experiment) = self.current_experiment.clone() {
                let logger = MetricsLogger::new(experiment_filename).await.map_err(|e| format!("{e:?}"))?;
                logger.clone().start().await.map_err(|e| format!("{e:?}"))?;
                self.metrics_logger = Some(logger);
                self.generate_graph().await?;
                if let Some(executor) = ActionExecutor::new_from_experiment(&experiment, io.clone(), self.graph.clone()) {
                    executor.clone().start().await;
                    self.action_executor = Some(executor); // <- Store the executor
                }
            }
            Ok(format!("Environment '{}' started successfully", env))
        } else {
            Err(format!("Failed to start environment '{}': {}", env, result.unwrap_err()))
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
            let handler = self.handlers.get(env).unwrap();
            handler.stop().await
        } else {
            Err("No active environment to stop".to_string())
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

    async fn generate_graph(&mut self) -> Result<(), String> {
        let nodes_val = self.get_nodes().await.map_err(|e| format!("Failed to get nodes: {e}"))?;
        let links_val = self.get_links().await.map_err(|e| format!("Failed to get links: {e}"))?;

        let nodes: Vec<serde_json::Value> = serde_json::from_value(nodes_val).map_err(|e| format!("Invalid nodes JSON: {e}"))?;
        let links: Vec<serde_json::Value> = serde_json::from_value(links_val).map_err(|e| format!("Invalid links JSON: {e}"))?;

        let mut graph = Graph::new();
        for node in nodes {
            let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
            let typ = node.get("type").and_then(|v| v.as_str()).unwrap_or("Unknown");
            graph.add_node(name, typ);
        }
        for link in links {
            let link: Link = serde_json::from_value(link).map_err(|e| format!("Invalid link format: {e}"))?;
            graph.add_link(link);
        }
        self.graph = Some(graph);
        Ok(())
    }
}
