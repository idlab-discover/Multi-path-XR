use crate::{
    graph::Graph,
    router::{update_network_conditions_on_agent, NetworkConditionData},
    structs::{Action, ExperimentFile},
};
use serde_json::{json, Value};
use socketioxide::{extract::SocketRef, SendError, SocketError, SocketIo};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::{
    sync::{watch, Mutex},
    time::{sleep, sleep_until, Duration, Instant},
};
use tracing::{info, warn};

const AGENT_CONNECT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const AGENT_CONNECT_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone)]
pub struct ActionExecutor {
    actions: Arc<Vec<Action>>,
    started: Arc<AtomicBool>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    io: Arc<SocketIo>,
    graph: Option<Graph>,
    agent_registry: Arc<Mutex<HashMap<String, String>>>,
}

impl ActionExecutor {
    pub fn new_from_experiment(
        exp: &ExperimentFile,
        io: Arc<SocketIo>,
        graph: Option<Graph>,
        agent_registry: Arc<Mutex<HashMap<String, String>>>,
    ) -> Option<Self> {
        // Create a map of the role targets, where the key is the target and the value is also the target.
        // Additionally, push all the aliases as key to the map with the target as value.
        // This is done to allow the user to use either the target or the alias in the experiment file.
        let mut role_map = std::collections::HashMap::new();
        for role in &exp.environment.roles {
            role_map.insert(role.target.clone(), role.target.clone());
            role_map.insert(role.alias.clone(), role.target.clone());
        }

        // Replace the target and connected_node in the actions with the value from the role_map
        let mut actions = exp.actions.clone().unwrap_or_default();
        for action in &mut actions {
            if let Some(target) = &action.target {
                if let Some(new_target) = role_map.get(target) {
                    action.target = Some(new_target.clone());
                }
            }
            if let Some(connected_node) = &action.connected_node {
                if let Some(new_connected_node) = role_map.get(connected_node) {
                    action.connected_node = Some(new_connected_node.clone());
                }
            }
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Some(Self {
            actions: Arc::new(actions),
            started: Arc::new(AtomicBool::new(false)),
            shutdown_tx,
            shutdown_rx,
            io,
            graph,
            agent_registry,
        })
    }

    async fn get_agent_socket(&self, node_id: &str) -> Option<SocketRef> {
        let socket_id = {
            let registry = self.agent_registry.lock().await;
            registry.get(node_id).cloned()
        }?;

        self.io
            .sockets()
            .into_iter()
            .find(|socket| socket.id.to_string() == socket_id && socket.connected())
    }

    async fn wait_for_agent_socket(
        &self,
        node_id: &str,
        max_wait: Duration,
    ) -> Option<SocketRef> {
        if let Some(socket) = self.get_agent_socket(node_id).await {
            return Some(socket);
        }

        let deadline = Instant::now() + max_wait;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }

            let remaining = deadline.saturating_duration_since(now);
            sleep(std::cmp::min(AGENT_CONNECT_POLL_INTERVAL, remaining)).await;

            if let Some(socket) = self.get_agent_socket(node_id).await {
                return Some(socket);
            }
        }
    }

    async fn execute_curl_via_agent(
        &self,
        target: &str,
        url: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(socket) = self
            .wait_for_agent_socket(target, AGENT_CONNECT_WAIT_TIMEOUT)
            .await
        else {
            warn!(
                "Skipping agent curl for target '{target}': agent is not connected after waiting {}s: {url}",
                AGENT_CONNECT_WAIT_TIMEOUT.as_secs()
            );
            return Ok(());
        };

        let payload = json!({ "url": url, "method": "GET" });
        match socket.emit_with_ack::<Value, Value>("curl", &payload) {
            Ok(ack_stream) => match ack_stream.await {
                Ok(ack_payload) => {
                    info!(
                        "Agent curl ack from {} for {}: {}",
                        target, url, ack_payload
                    );
                }
                Err(err) => {
                    warn!(
                        "Agent curl ack failed for target '{}' and url '{}': {:?}",
                        target, url, err
                    );
                }
            },
            Err(SendError::Socket(socket_error)) => match socket_error {
                SocketError::Closed => {
                    warn!(
                        "Unable to send curl request to target '{}': socket is closed",
                        target
                    );
                }
                other => {
                    warn!(
                        "Unable to send curl request to target '{}': {:?}",
                        target, other
                    );
                }
            },
            Err(SendError::Serialize(err)) => {
                warn!(
                    "Failed to serialize curl payload for target '{}': {:?}",
                    target, err
                );
            }
        }

        Ok(())
    }

    async fn execute_curl_locally(&self, url: &str) -> Result<(), Box<dyn std::error::Error>> {
        //info!("Executing local curl to {}", url);
        match reqwest::get(url).await {
            Ok(resp) => {
                let status = resp.status();
                match resp.text().await {
                    Ok(body) => {
                        info!("CURL {} -> {}: {}", url, status, body);
                    }
                    Err(e) => {
                        warn!("CURL {} -> {} (failed to read body): {}", url, status, e);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to GET {}: {}", url, e);
            }
        };

        Ok(())
    }

    pub async fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            info!("Action executor start requested, but actions are already running");
            return;
        }

        let start_time = Instant::now();
        for action in self.actions.iter() {
            let mut shutdown_rx = self.shutdown_rx.clone();
            let delay_ms = action.execution_delay.unwrap_or(0);
            let scheduled_at = start_time + Duration::from_millis(delay_ms);
            let action_clone = action.clone();
            let executor_clone = self.clone();

            // Schedule task
            tokio::spawn(async move {
                info!(
                    "Scheduled action '{}' (type: {}) to execute in {}ms",
                    action_clone.action, action_clone.action_type, delay_ms
                );
                tokio::select! {
                    _ = sleep_until(scheduled_at) => {
                        if *shutdown_rx.borrow() {
                            info!("Cancelled execution of action: {}", action_clone.action);
                            return;
                        }
                        let action_name = action_clone.action.clone();
                        if let Err(err) = executor_clone.execute(action_clone, start_time.elapsed()).await {
                            warn!("Action '{}' failed: {}", action_name, err);
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        info!("Cancelled pending action due to shutdown signal: {}", action_clone.action);
                    }
                }
            });
        }
    }

    /// Can be called to stop all pending actions.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    async fn execute(
        &self,
        action: Action,
        elapsed: Duration,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now_ms = elapsed.as_millis();
        info!(
            "Executing action '{}' (type: {}) after {}ms",
            action.action, action.action_type, now_ms
        );

        match action.action_type.as_str() {
            "tc" => {
                let target = action.target.clone().unwrap_or_default();
                warn!(
                    "Apply TC to {} connected to {:?}",
                    target.clone(),
                    action.connected_node
                );

                let (interface, interface_ip) = {
                    if let Some(connected_node) = action.connected_node {
                        if let Some(graph) = &self.graph {
                            if let Some((_path, segments)) = graph.shortest_path(&target, &connected_node) {
                                if let Some(first_segment) = segments.first() {
                                    if first_segment.from == target {
                                        (
                                            Some(first_segment.from_interface.clone()),
                                            Some(first_segment.from_ip.clone()),
                                        )
                                    } else if first_segment.to == target {
                                        (
                                            Some(first_segment.to_interface.clone()),
                                            Some(first_segment.to_ip.clone()),
                                        )
                                    } else {
                                        (None, None)
                                    }
                                } else {
                                    (None, None)
                                }
                            } else {
                                (None, None)
                            }
                        } else {
                            (None, None)
                        }
                    } else {
                        (None, None)
                    }
                };

                let interface_ip = interface_ip.and_then(|value| {
                    let trimmed = value.trim();
                    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("N/A") {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                });

                let settings = NetworkConditionData {
                    node_id: target,
                    bandwidth: action.bandwidth.unwrap_or("200mbit".to_string()),
                    latency: action.network_delay.unwrap_or("0ms".to_string()),
                    loss: action.packet_loss.unwrap_or("0%".to_string()),
                    htb_explicit_limits: Some(action.htb_explicit_limits.unwrap_or(false)),
                    interface,
                    interface_ip,
                };

                let target_node = settings.node_id.clone();
                let interface_name = settings.interface.clone().unwrap_or_default();
                let interface_ip = settings.interface_ip.clone().unwrap_or_default();
                let bandwidth = settings.bandwidth.clone();
                let latency = settings.latency.clone();
                let loss = settings.loss.clone();
                let (status, body) =
                    update_network_conditions_on_agent(axum::Json(settings), self.io.clone()).await;
                if !status.is_success() {
                    let detail = serde_json::to_string(&body.0)
                        .unwrap_or_else(|err| format!("{{\"serialize_error\":\"{err}\"}}"));
                    return Err(std::io::Error::other(format!(
                        "Failed to send TC request to '{}' via interface hint '{}' (ip '{}'): status={} response={detail}",
                        target_node,
                        interface_name,
                        interface_ip,
                        status
                    ))
                    .into());
                }
                info!(
                    "Sent TC request to '{}' via interface hint '{}' (ip '{}'): bw={} latency={} loss={}",
                    target_node,
                    interface_name,
                    interface_ip,
                    bandwidth,
                    latency,
                    loss
                );
            }
            "curl" => {
                let url = action.url.clone().unwrap_or_default();
                info!("Send GET request to {}", url);

                if let Some(target) = action
                    .target
                    .clone()
                    .filter(|target| !target.trim().is_empty())
                {
                    self.execute_curl_via_agent(&target, &url).await?;
                } else {
                    self.execute_curl_locally(&url).await?;
                }
            }
            "agent_command" => {
                if let (Some(target), Some(command)) =
                    (action.target.clone(), action.command.clone())
                {
                    let room = format!("agent_{target}");
                    match self.io.to(room).emit("start_process", &command).await {
                        Ok(_) => info!("Sent agent command '{}' to {}", command, target),
                        Err(err) => warn!(
                            "Failed to send agent command '{}' to {}: {err:?}",
                            command, target
                        ),
                    }
                } else {
                    warn!("agent_command requires 'target' and 'command' fields");
                }
            }
            "agent_stop" => {
                if let Some(target) = action.target.clone() {
                    let room = format!("agent_{target}");
                    match self.io.to(room).emit("stop_process", &json!({})).await {
                        Ok(_) => info!("Sent stop_process to {}", target),
                        Err(err) => warn!("Failed to send stop_process to {}: {err:?}", target),
                    }
                } else {
                    warn!("agent_stop requires 'target'");
                }
            }
            "exit" => {
                info!("TODO: Exiting experiment automatically");
            }
            "ignore" => {
                info!("Ignoring action: {}", action.action);
            }
            other => {
                warn!("Unknown action type '{}'", other);
            }
        }

        Ok(())
    }
}
