use crate::{graph::Graph, router::{update_network_conditions_on_agent, NetworkConditionData}, structs::{Action, ExperimentFile}};
use std::sync::Arc;
use socketioxide::SocketIo;
use tokio::{sync::watch, time::{sleep_until, Duration, Instant}};
use tracing::{info, warn};

#[derive(Clone)]
pub struct ActionExecutor {
    actions: Arc<Vec<Action>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    io: Arc<SocketIo>,
    graph: Option<Graph>
}

impl ActionExecutor {
    pub fn new_from_experiment(exp: &ExperimentFile, io: Arc<SocketIo>, graph: Option<Graph>) -> Option<Self> {
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
            shutdown_tx,
            shutdown_rx,
            io,
            graph
        })
    }

    pub async fn start(&self) {
        let start_time = Instant::now();
        for action in self.actions.iter() {
            let mut shutdown_rx = self.shutdown_rx.clone();
            let delay_ms = action.execution_delay.unwrap_or(0);
            let scheduled_at = start_time + Duration::from_millis(delay_ms);
            let action_clone = action.clone();
            let executor_clone = self.clone();

            // Schedule task
            tokio::spawn(async move {
                tokio::select! {
                    _ = sleep_until(scheduled_at) => {
                        if *shutdown_rx.borrow() {
                            info!("Cancelled execution of action: {}", action_clone.action);
                            return;
                        }
                        executor_clone.execute(action_clone, start_time.elapsed()).await;
                    }
                    _ = shutdown_rx.changed() => {
                        info!("Cancelled pending action due to shutdown signal.");
                    }
                }
            });
        }
    }

    /// Can be called to stop all pending actions.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    async fn execute(&self, action: Action, elapsed: Duration) {
        let now_ms = elapsed.as_millis();
        info!(
            "Executing action '{}' (type: {}) after {}ms",
            action.action, action.action_type, now_ms
        );

        match action.action_type.as_str() {
            "tc" => {
                let target = action.target.clone().unwrap_or_default();
                warn!("Apply TC to {} connected to {:?}", target.clone(), action.connected_node);

                let interface = {
                    if let Some(connected_node) = action.connected_node {
                        if let Some(graph) = &self.graph {
                            let hops = graph.interface_hops_from(&target);
                            if let Some(hops) = hops.get(&connected_node) {
                                // info!("Hops from {} to {}: {:?}", target, connected_node, hops);
                                if let Some((_, out_iface)) = hops.first() {
                                    //info!("Using interface: {:?}", out_iface);
                                    out_iface.clone()
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };

                let settings = NetworkConditionData {
                    node_id: target,
                    bandwidth: action.bandwidth.unwrap_or("200mbit".to_string()),
                    latency: action.network_delay.unwrap_or("0ms".to_string()),
                    loss: action.packet_loss.unwrap_or("0%".to_string()),
                    interface
                };

                let _ = update_network_conditions_on_agent(
                    axum::Json(settings),
                    self.io.clone(),
                ).await;
            }
            "curl" => {
                warn!("Fire CURL to {:?}", action.url);
                // Just call the URL as a GET request
                let url = action.url.clone().unwrap_or_default();
                let _ = reqwest::get(&url).await;
            }
            "exit" => {
                info!("TODO: Exiting experiment automatically");
            }
            other => {
                warn!("Unknown action type '{}'", other);
            }
        }
    }
}
