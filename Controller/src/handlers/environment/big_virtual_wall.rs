use super::EnvironmentHandler;
use async_trait::async_trait;
use big_virtual_wall::{
    apply_overlay, clean_overlay, discover_hosts, validate_safety, OverlaySpec,
};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
use tracing::info;
use virtual_wall::{
    StartOptions, TunnelDirection, TunnelEndpoint, TunnelRequest, VirtualWallManager,
};

#[derive(Clone)]
pub struct BigVirtualWallHandler {
    manager: Arc<Mutex<Option<Arc<VirtualWallManager>>>>,
    overlay_state: Arc<Mutex<Option<OverlaySession>>>,
}

#[derive(Clone)]
struct OverlaySession {
    spec_path: PathBuf,
    experiment: Option<String>,
    plan: big_virtual_wall::PlanResult,
    node_to_host: HashMap<String, String>,
}

impl BigVirtualWallHandler {
    pub fn new() -> Self {
        Self {
            manager: Arc::new(Mutex::new(None)),
            overlay_state: Arc::new(Mutex::new(None)),
        }
    }

    async fn ensure_manager(&self) -> Result<Arc<VirtualWallManager>, String> {
        {
            let guard = self.manager.lock().await;
            if let Some(manager) = guard.as_ref() {
                return Ok(manager.clone());
            }
        }

        let mut guard = self.manager.lock().await;
        if let Some(manager) = guard.as_ref() {
            return Ok(manager.clone());
        }
        match VirtualWallManager::try_from_path(None) {
            Ok(manager) => {
                let manager = Arc::new(manager);
                *guard = Some(manager.clone());
                Ok(manager)
            }
            Err(err) => Err(format!("Failed to initialize Virtual Wall manager: {err}")),
        }
    }

    fn parse_bool(val: Option<&String>) -> bool {
        val.map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false)
    }

    async fn clean_with_cached_spec(
        &self,
        manager: &Arc<VirtualWallManager>,
    ) -> Result<(), String> {
        let session = { self.overlay_state.lock().await.clone() };
        if let Some(sess) = session {
            let spec = OverlaySpec::load(&sess.spec_path).map_err(|e| e.to_string())?;
            let inv = discover_hosts(manager)
                .await
                .map_err(|e| format!("Failed to discover hosts: {e}"))?;
            let mut map = HashMap::new();
            for h in inv {
                map.insert(h.name, h.underlay);
            }
            let _ = validate_safety(&spec, &map);
            let _ = clean_overlay(manager, &spec, Some(map), false).await;
        }
        Ok(())
    }

    fn build_tunnel_request(params: HashMap<String, String>) -> Result<TunnelRequest, String> {
        let direction = match params
            .get("direction")
            .map(|s| s.as_str())
            .unwrap_or("remote")
        {
            "local" | "local-forward" => TunnelDirection::Local,
            "remote" | "remote-forward" => TunnelDirection::Remote,
            other => return Err(format!("Invalid `direction`: {other}")),
        };

        let node = params
            .get("node")
            .cloned()
            .ok_or_else(|| "Missing `node` parameter".to_string())?;
        let listen_host = params
            .get("listen_host")
            .cloned()
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let listen_port = params
            .get("listen_port")
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or_else(|| "Missing or invalid `listen_port` parameter".to_string())?;
        let target_host = params
            .get("target_host")
            .cloned()
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let target_port = params
            .get("target_port")
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or_else(|| "Missing or invalid `target_port` parameter".to_string())?;

        Ok(TunnelRequest {
            node,
            direction,
            listen: TunnelEndpoint {
                host: listen_host,
                port: listen_port,
            },
            target: TunnelEndpoint {
                host: target_host,
                port: target_port,
            },
            username: params.get("username").cloned(),
        })
    }
}

#[async_trait]
impl EnvironmentHandler for BigVirtualWallHandler {
    async fn start(&self, options: &str) -> Result<String, String> {
        // options: spec=<path>&experiment=...&nodes=...&paths=...&reuse=0/1&dry_run=1
        let params: HashMap<String, String> = url::form_urlencoded::parse(options.as_bytes())
            .into_owned()
            .collect();
        let spec_path = params
            .get("spec")
            .map(PathBuf::from)
            .ok_or_else(|| "Missing `spec` parameter".to_string())?;
        let experiment = params.get("experiment").cloned();
        let dry_run = Self::parse_bool(params.get("dry_run"));

        if let Some(exp) = &experiment {
            std::env::set_var("SLICES_EXPERIMENT", exp);
        }

        // Load spec and derive required host count.
        let spec = OverlaySpec::load(&spec_path).map_err(|e| e.to_string())?;
        let required_hosts = spec.hosts.len();
        if required_hosts == 0 {
            return Err("Spec did not produce any hosts (host_pool?)".to_string());
        }

        // Base provisioning
        let manager = self.ensure_manager().await?;
        let start_opts = StartOptions {
            nodes: params
                .get("nodes")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(required_hosts),
            paths: params
                .get("paths")
                .and_then(|v| v.parse::<usize>().ok())
                .or(Some(2)),
            reuse: !params
                .get("reuse")
                .map(|v| v == "0" || v.to_lowercase() == "false")
                .unwrap_or(false),
        };

        let summary = manager
            .start_from_options(start_opts)
            .await
            .map_err(|e| format!("Failed to start base Virtual Wall: {e}"))?;
        info!(
            "Base Virtual Wall started: experiment {} with {} resources",
            summary.experiment_name,
            summary.resources.len()
        );

        // Discover underlay from running experiment.
        let inv = discover_hosts(&manager)
            .await
            .map_err(|e| format!("Failed to discover hosts: {e}"))?;
        let mut underlay_map = HashMap::new();
        for h in inv {
            underlay_map.insert(h.name, h.underlay);
        }
        validate_safety(&spec, &underlay_map).map_err(|e| e.to_string())?;

        // Keep an overlay snapshot for status reporting.
        let overlay_plan = big_virtual_wall::plan_overlay_with_underlay(&spec, &underlay_map);
        let mut node_to_host = HashMap::new();
        for n in &spec.nodes {
            let host = if n.host == "auto" {
                n.name.clone()
            } else {
                n.host.clone()
            };
            node_to_host.insert(n.name.clone(), host);
        }

        apply_overlay(&manager, &spec, Some(underlay_map), dry_run)
            .await
            .map_err(|e| e.to_string())?;

        // Cache spec/experiment for later clean/stop.
        {
            let mut guard = self.overlay_state.lock().await;
            *guard = Some(OverlaySession {
                spec_path: spec_path.clone(),
                experiment: experiment.clone(),
                plan: overlay_plan.clone(),
                node_to_host,
            });
        }

        Ok(if dry_run {
            format!(
                "Base started ({} nodes), overlay dry-run completed on experiment {}",
                summary.resources.len(),
                summary.experiment_name
            )
        } else {
            format!(
                "Base started ({} nodes), overlay applied on experiment {}",
                summary.resources.len(),
                summary.experiment_name
            )
        })
    }

    async fn stop(&self) -> Result<String, String> {
        // Clean overlay if we can infer a spec path from env; otherwise just stop base.
        let manager = self.ensure_manager().await?;
        let _ = self.clean_with_cached_spec(&manager).await;
        match manager.stop().await {
            Ok(_) => Ok("Big Virtual Wall stopped (overlay cleaned best-effort)".to_string()),
            Err(err) => Err(format!("Failed to stop Virtual Wall: {err}")),
        }
    }

    async fn exec(&self, params: HashMap<String, String>) -> Result<String, String> {
        let manager = self.ensure_manager().await?;
        let node_param = params
            .get("node")
            .cloned()
            .ok_or_else(|| "Missing `node` parameter".to_string())?;
        let command = params
            .get("command")
            .cloned()
            .ok_or_else(|| "Missing `command` parameter".to_string())?;
        let username = params.get("username").map(|s| s.as_str());

        let key_path = params.get("identity_file").map(PathBuf::from);
        // If the node matches a virtual node in the cached overlay, run inside its netns on the mapped host.
        if let Some(sess) = self.overlay_state.lock().await.clone() {
            if let Some(host) = sess.node_to_host.get(&node_param) {
                let netns_cmd = format!("ip netns exec ns-{} {}", node_param, command);
                return manager
                    .exec(host, &netns_cmd, username, key_path.as_deref(), None)
                    .await
                    .map_err(|e| e.to_string());
            }
        }

        manager
            .exec(&node_param, &command, username, key_path.as_deref(), None)
            .await
            .map_err(|e| e.to_string())
    }

    async fn nodes(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        manager.nodes().await.map_err(|e| e.to_string())
    }

    async fn links(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        manager.links().await.map_err(|e| e.to_string())
    }

    async fn status(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        let mut status = manager.status().await.map_err(|e| e.to_string())?;
        if let Some(sess) = self.overlay_state.lock().await.clone() {
            // Build overlay link/iface info with auto IPs matching generator heuristic.
            let mut links = Vec::new();
            for vl in sess
                .plan
                .host_plans
                .iter()
                .flat_map(|hp| hp.vlan_links.iter())
            {
                let subnet_idx = 50 + (vl.vlan_id % 200) as u32;
                let ip1 = format!("192.168.{}.1/30", subnet_idx);
                let ip2 = format!("192.168.{}.2/30", subnet_idx);
                links.push(json!({
                    "vlan": vl.vlan_id,
                    "vxlan_dev": vl.vxlan_dev,
                    "endpoints": [
                        {
                            "node": vl.endpoints[0].node,
                            "intf": vl.endpoints[0].intf,
                            "ip": ip1,
                        },
                        {
                            "node": vl.endpoints[1].node,
                            "intf": vl.endpoints[1].intf,
                            "ip": ip2,
                        }
                    ]
                }));
            }
            status["overlay"] = json!({
                "spec": sess.spec_path,
                "experiment": sess.experiment,
                "hosts": sess.plan.host_plans.iter().map(|hp| {
                    json!({
                        "host": hp.host,
                        "bridge": hp.bridge,
                        "vxlan": hp.vxlan_devices,
                        "vlan_links": hp.vlan_links,
                    })
                }).collect::<Vec<_>>(),
                "links": links
            });
        }
        Ok(status)
    }

    async fn visualize(&self) -> Result<Vec<u8>, String> {
        Err("BigVirtualWall visualize not implemented".to_string())
    }

    async fn start_xterm(&self, params: HashMap<String, String>) -> Result<String, String> {
        let manager = self.ensure_manager().await?;
        let node = params
            .get("node")
            .cloned()
            .ok_or_else(|| "Missing `node` parameter".to_string())?;
        manager
            .start_terminal(&node)
            .await
            .map_err(|e| format!("Failed to open shell: {e}"))
    }

    async fn ping_all(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        manager.ping_all().await.map_err(|e| e.to_string())
    }

    async fn open_tunnel(&self, params: HashMap<String, String>) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        let request = Self::build_tunnel_request(params)?;
        let tunnel = manager
            .open_tunnel(request)
            .await
            .map_err(|e| e.to_string())?;
        serde_json::to_value(tunnel).map_err(|e| format!("Failed to serialize tunnel: {e}"))
    }

    async fn close_tunnel(&self, id: &str) -> Result<String, String> {
        let manager = self.ensure_manager().await?;
        manager
            .close_tunnel(id)
            .await
            .map_err(|e| format!("Failed to close tunnel {id}: {e}"))?;
        Ok(format!("Closed tunnel {id}"))
    }

    async fn list_tunnels(&self) -> Result<Value, String> {
        let manager = self.ensure_manager().await?;
        let tunnels = manager.list_tunnels().await.map_err(|e| e.to_string())?;
        serde_json::to_value(tunnels).map_err(|e| format!("Failed to serialize tunnels: {e}"))
    }
}
