use std::{collections::HashMap, path::{Path, PathBuf}, time::Duration};

use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::time;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    config::VirtualWallConfig,
    error::{Result, VirtualWallError},
    rspec::{RspecLink, RspecTopology},
    ssh::{SpawnedTunnel, SshOptions, SshTarget},
    state::VirtualWallStateLite,
    tunnels::{TunnelInfo, TunnelRequest},
};

/// Options that were used by the original manager.
///
/// In this minimal crate they are accepted for API compatibility, but provisioning is unsupported.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// Number of nodes requested (kept for compatibility only).
    pub nodes: usize,
    /// Optional number of paths requested (kept for compatibility only).
    pub paths: Option<usize>,
    /// Whether to attempt reuse (kept for compatibility only).
    pub reuse: bool,
}

impl StartOptions {
    /// Parse `application/x-www-form-urlencoded` query options.
    pub fn from_query(query: &str) -> Self {
        let mut options = StartOptions {
            nodes: 1,
            paths: None,
            reuse: true,
        };

        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                match key {
                    "n_nodes" | "nodes" => {
                        if let Ok(val) = value.parse::<usize>() {
                            options.nodes = val.max(1);
                        }
                    }
                    "n_paths" | "paths" => {
                        if let Ok(val) = value.parse::<usize>() {
                            options.paths = Some(val);
                        }
                    }
                    "reuse" => {
                        options.reuse = value != "0" && value != "false";
                    }
                    _ => {}
                }
            }
        }
        options
    }
}

/// Summary returned by `start_from_options` in the original crate.
#[derive(Debug, Clone, Serialize)]
pub struct StartSummary {
    pub experiment_name: String,
    pub experiment_id: Option<String>,
    pub resources: Vec<String>,
}

#[derive(Debug)]
enum TunnelRuntime {
    Child(Child),
    ControlMaster {
        target: SshTarget,
        username: Option<String>,
        key_override: Option<PathBuf>,
        control_path: PathBuf,
    },
}

#[derive(Debug)]
struct TunnelHandle {
    info: TunnelInfo,
    runtime: TunnelRuntime,
}

impl TunnelHandle {
    fn info_with_pid(&mut self) -> TunnelInfo {
        let mut info = self.info.clone();
        info.pid = match &mut self.runtime {
            TunnelRuntime::Child(child) => child.id(),
            TunnelRuntime::ControlMaster { .. } => None,
        };
        info
    }
}

fn tunnel_binding_matches(
    info: &TunnelInfo,
    node_id: &str,
    username: Option<&str>,
    request: &TunnelRequest,
) -> bool {
    info.node == node_id
        && info.direction == request.direction
        && info.listen == request.listen
        && info.username.as_deref() == username
}

fn tunnel_matches_request(
    info: &TunnelInfo,
    node_id: &str,
    username: Option<&str>,
    request: &TunnelRequest,
) -> bool {
    tunnel_binding_matches(info, node_id, username, request) && info.target == request.target
}

fn tunnel_conflicts_with_request(
    info: &TunnelInfo,
    node_id: &str,
    username: Option<&str>,
    request: &TunnelRequest,
) -> bool {
    tunnel_binding_matches(info, node_id, username, request) && info.target != request.target
}

fn reusable_and_conflicting_tunnels_from_guard(
    guard: &mut HashMap<String, TunnelHandle>,
    node_id: &str,
    username: Option<&str>,
    request: &TunnelRequest,
) -> (Option<TunnelInfo>, Vec<TunnelHandle>) {
    let mut remove_ids = Vec::new();
    let mut reusable = None;

    for (id, handle) in guard.iter_mut() {
        let exact_match = tunnel_matches_request(&handle.info, node_id, username, request);
        let conflict = tunnel_conflicts_with_request(&handle.info, node_id, username, request);
        if !exact_match && !conflict {
            continue;
        }

        let is_alive = match &mut handle.runtime {
            TunnelRuntime::Child(child) => matches!(child.try_wait(), Ok(None)),
            TunnelRuntime::ControlMaster { control_path, .. } => control_path.exists(),
        };

        if is_alive {
            if exact_match && reusable.is_none() {
                reusable = Some(handle.info_with_pid());
            } else {
                remove_ids.push(id.clone());
            }
        } else {
            remove_ids.push(id.clone());
        }
    }

    let mut removed_handles = Vec::new();
    for id in remove_ids {
        if let Some(handle) = guard.remove(&id) {
            removed_handles.push(handle);
        }
    }

    (reusable, removed_handles)
}

async fn close_tunnel_handles(ssh: &SshOptions, handles: Vec<TunnelHandle>) {
    for handle in handles {
        match handle.runtime {
            TunnelRuntime::Child(mut child) => {
                if child.id().is_some() {
                    let _ = child.kill().await;
                }
                let _ = child.wait().await;
            }
            TunnelRuntime::ControlMaster {
                target,
                username,
                key_override,
                control_path,
            } => {
                let _ = ssh
                    .cancel_tunnel_forward(
                        &target,
                        &handle.info.direction,
                        &handle.info.listen,
                        &handle.info.target,
                        username.as_deref(),
                        key_override.as_deref(),
                        &control_path,
                    )
                    .await;
            }
        }
    }
}

/// Node resolved from state/rspec.
#[derive(Debug, Clone)]
struct Node {
    /// Canonical id exposed to the outside (friendly name if state.json is present).
    id: String,
    /// Original rspec node client_id (e.g. `node0`).
    rspec_client_id: Option<String>,
    /// SSH target.
    ssh: SshTarget,
    /// Cached interface/ip data from rspec.
    interfaces: Vec<Value>,
}

/// Manager for an existing Virtual Wall experiment.
#[derive(Clone)]
pub struct VirtualWallManager {
    config: VirtualWallConfig,
    ssh: SshOptions,
    nodes: HashMap<String, Node>,
    /// rspec_client_id -> canonical id
    rspec_map: HashMap<String, String>,
    /// Cached links from the rspec.
    links: Vec<RspecLink>,
    /// Optional state file (for experiment metadata).
    state: Option<VirtualWallStateLite>,
    tunnels: std::sync::Arc<Mutex<HashMap<String, TunnelHandle>>>,
}

impl VirtualWallManager {
    /// Load configuration and parse the provided RSpec/state.
    pub fn try_from_path(config_path: Option<&Path>) -> Result<Self> {
        let config = VirtualWallConfig::load(config_path)?;
        Self::from_config(config)
    }

    /// Create a manager from a fully resolved config.
    pub fn from_config(config: VirtualWallConfig) -> Result<Self> {
        let rspec = RspecTopology::parse_file(&config.rspec_path)?;

        let state = match config.state_json.as_ref() {
            Some(path) if path.exists() => {
                let st = VirtualWallStateLite::load(path)?;
                Some(st)
            }
            _ => None,
        };

        // Ensure state dir exists so known_hosts can be created.
        if let Some(parent) = config.known_hosts.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let ssh = SshOptions {
            ssh_binary: config.ssh_binary.clone(),
            default_username: config.ssh_username.clone(),
            default_private_key: config.ssh_private_key.clone(),
            default_proxy_private_key: config.ssh_proxy_private_key.clone(),
            forward_agent: config.ssh_forward_agent,
            forward_x11: config.ssh_forward_x11,
            server_alive_interval: config.ssh_server_alive_interval,
            use_jump_proxy: config.use_jump_proxy,
            default_jump_proxy: config.jump_proxy.clone(),
            known_hosts: config.known_hosts.clone(),
            host_key_checking: config.host_key_checking,
            connect_timeout: config.connect_timeout,
            control_dir: config.state_dir.join("c"),
            control_persist: Duration::from_secs(15 * 60),
        };

        let (nodes, rspec_map) = build_nodes(&config, &rspec, state.as_ref())?;
        let links = rspec.links;

        Ok(Self {
            config,
            ssh,
            nodes,
            rspec_map,
            links,
            state,
            tunnels: std::sync::Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// In the minimal manager this is a **no-op** that returns a summary of the parsed inventory.
    ///
    /// This keeps API-compatibility with callers that always invoke `start()`.
    pub async fn start_from_options(&self, _options: StartOptions) -> Result<StartSummary> {
        let experiment_name = self
            .state
            .as_ref()
            .and_then(|s| s.experiment_name.clone())
            .unwrap_or_else(|| "existing".to_string());

        let experiment_id = self.state.as_ref().and_then(|s| s.experiment_id.clone());
        let resources = self.nodes.keys().cloned().collect::<Vec<_>>();

        Ok(StartSummary {
            experiment_name,
            experiment_id,
            resources,
        })
    }

    /// Stop the environment while preserving reusable local SSH state.
    pub async fn stop(&self) -> Result<()> {
        let _ = self.list_tunnels().await;
        Ok(())
    }

    /// Execute a command on a node via SSH.
    pub async fn exec(
        &self,
        node: &str,
        command: &str,
        username: Option<&str>,
        key_path: Option<&Path>,
        timeout: Option<Duration>,
        background: bool,
    ) -> Result<String> {
        let n = self.resolve_node(node)?;
        let t = timeout.or(Some(self.config.command_timeout));
        self.ssh
            .exec(&n.ssh, command, username, key_path, t, background)
            .await
    }

    /// Returns node inventory as JSON.
    pub async fn nodes(&self) -> Result<Value> {
        let list: Vec<Value> = self
            .nodes
            .values()
            .map(|n| {
                json!({
                    "name": n.id,
                    "type": "VirtualWall",
                    "rspec_client_id": n.rspec_client_id,
                    "ssh": {
                        "host": n.ssh.host,
                        "port": n.ssh.port,
                        "username": n.ssh.username,
                        "jump_proxy": n.ssh.jump_proxy,
                    },
                    "interfaces": n.interfaces,
                })
            })
            .collect();
        Ok(Value::Array(list))
    }

    /// Returns link inventory as JSON (controller-compatible).
    ///
    /// Controller expects each element to deserialize into `graph::Link`.
    pub async fn links(&self) -> Result<Value> {
        let mut out: Vec<Value> = Vec::new();

        for l in &self.links {
            // Resolve endpoints for this LAN/link.
            // Each interface_ref is typically "nodeX:ifY".
            let mut endpoints: Vec<(String, String, String)> =
                Vec::with_capacity(l.interface_refs.len());

            for iface_ref in &l.interface_refs {
                let node_cid = iface_ref.split(':').next().unwrap_or("").trim();
                if node_cid.is_empty() {
                    continue;
                }

                let canon = match self.rspec_map.get(node_cid) {
                    Some(c) => c.as_str(),
                    None => node_cid,
                };

                let Some(node) = self.nodes.get(canon) else {
                    continue;
                };

                let ip = first_ip_for_iface(node, iface_ref).to_string();
                endpoints.push((canon.to_string(), iface_ref.clone(), ip));
            }

            // Need at least 2 endpoints to form edges.
            if endpoints.len() < 2 {
                continue;
            }

            // RSpec links can be multi-access (LAN with N members).
            // Controller Link is point-to-point, so we expand a LAN into a star.
            let (root_node, root_intf, root_ip) = endpoints[0].clone();

            for (node2, intf2, ip2) in endpoints.iter().skip(1).cloned() {
                let status = if root_ip != "N/A" && ip2 != "N/A" {
                    "up"
                } else {
                    "unknown"
                };

                out.push(json!({
                    // ✅ exact fields expected by `graph::Link`
                    "intf1": root_intf,
                    "intf2": intf2,
                    "ip1": root_ip,
                    "ip2": ip2,
                    "node1": root_node,
                    "node2": node2,
                    "status": status,

                    // optional extras (serde will ignore these in `Link`)
                    "link_id": l.client_id,
                    "vlantag": l.vlantag,
                }));
            }
        }

        Ok(Value::Array(out))
    }

    /// Returns best-effort status information.
    pub async fn status(&self) -> Result<Value> {
        let tunnels = self.list_tunnels().await.unwrap_or_default();
        Ok(json!({
            "rspec_path": self.config.rspec_path,
            "state_json": self.config.state_json,
            "experiment": {
                "name": self.state.as_ref().and_then(|s| s.experiment_name.clone()),
                "id": self.state.as_ref().and_then(|s| s.experiment_id.clone()),
            },
            "nodes": self.nodes().await.unwrap_or_default(),
            "links": self.links().await.unwrap_or_default(),
            "tunnels": tunnels,
        }))
    }

    /// Build a simple graph (nodes + switch-per-link) for visualization.
    pub async fn visualize(&self) -> Result<Value> {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for n in self.nodes.values() {
            nodes.push(json!({
                "id": n.id,
                "type": "node",
                "color": "#2563eb"
            }));
        }

        // Represent each LAN as a switch node to keep the edge count linear.
        for l in &self.links {
            nodes.push(json!({
                "id": format!("sw:{}", l.client_id),
                "type": "switch",
                "color": "#6b7280",
            }));

            for iface in &l.interface_refs {
                let node_cid = iface.split(':').next().unwrap_or("");
                let Some(canon) = self.rspec_map.get(node_cid) else {
                    continue;
                };
                edges.push(json!({
                    "src": canon,
                    "dst": format!("sw:{}", l.client_id),
                    "color": "#94a3b8",
                }));
            }
        }

        Ok(json!({"nodes": nodes, "edges": edges}))
    }

    /// Best-effort spawn of a local terminal emulator connected to the node.
    ///
    /// If no terminal emulator is available, this returns the SSH command line.
    pub async fn start_terminal(&self, node: &str) -> Result<String> {
        let n = self.resolve_node(node)?;

        let args = self
            .ssh
            .base_args(&n.ssh, None, None)
            .into_iter()
            .map(shell_quote)
            .collect::<Vec<_>>();
        let dest = shell_quote(n.ssh.destination(self.config.ssh_username.as_deref()));

        let ssh_cmd = format!(
            "{} {} {}",
            shell_quote(self.config.ssh_binary.to_string_lossy()),
            args.join(" "),
            dest
        );

        // Best-effort spawn of a terminal; if unavailable, return the command string.
        if std::process::Command::new("x-terminal-emulator")
            .arg("-e")
            .arg("bash")
            .arg("-lc")
            .arg(&ssh_cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return Ok(format!("Launched terminal with: {ssh_cmd}"));
        }

        if std::process::Command::new("gnome-terminal")
            .arg("--")
            .arg("bash")
            .arg("-lc")
            .arg(&ssh_cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return Ok(format!("Launched terminal with: {ssh_cmd}"));
        }

        Ok(ssh_cmd)
    }

    /// Ping all nodes from the controller (best-effort reachability).
    pub async fn ping_all(&self) -> Result<Value> {
        // Limit concurrency to avoid hammering DNS/proxy.
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let mut tasks = Vec::new();

        for n in self.nodes.values().cloned() {
            let sem = semaphore.clone();
            let timeout = self.config.ping_timeout;
            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.ok();
                let host = n.ssh.host;
                let ok = ping_once(&host, timeout).await;
                (n.id, ok)
            }));
        }

        let mut out = serde_json::Map::new();
        for t in tasks {
            if let Ok((id, ok)) = t.await {
                out.insert(id, json!({"reachable": ok}));
            }
        }

        Ok(Value::Object(out))
    }

    /// Establish or reuse an SSH tunnel to a node.
    pub async fn open_tunnel(&self, req: TunnelRequest) -> Result<TunnelInfo> {
        let node = self.resolve_node(&req.node)?;
        let node_id = node.id.clone();
        let node_ssh = node.ssh.clone();

        let username = req.username.clone().or(self.config.ssh_username.clone());
        let (existing, conflicting_handles) = {
            let mut guard = self.tunnels.lock().await;
            reusable_and_conflicting_tunnels_from_guard(
                &mut guard,
                &node_id,
                username.as_deref(),
                &req,
            )
        };
        close_tunnel_handles(&self.ssh, conflicting_handles).await;

        if let Some(existing) = existing {
            return Ok(existing);
        }

        let id = Uuid::new_v4().to_string();

        let spawned_tunnel = self.ssh.spawn_tunnel(
            &node_ssh,
            req.direction.clone(),
            &req.listen,
            &req.target,
            username.as_deref(),
            self.config.ssh_private_key.as_deref(),
        ).await?;

        let mut pending_handle = match spawned_tunnel {
            SpawnedTunnel::Child(mut child) => {
                // Fail fast if ssh exits immediately (forward failure, bind error, auth, etc.).
                time::sleep(Duration::from_millis(200)).await;

                match child.try_wait() {
                    Ok(Some(status)) => {
                        let stdout = read_child_stdout_best_effort(&mut child).await;
                        let stderr = read_child_stderr_best_effort(&mut child).await;
                        let code = status.code().unwrap_or(-1);
                        info!(
                            "ssh tunnel process exited immediately (code={code}). stdout: {}, stderr: {}",
                            if stdout.is_empty() { "<empty>" } else { &stdout },
                            if stderr.is_empty() { "<empty>" } else { &stderr }
                        );
                        return Err(VirtualWallError::TunnelSpawn(format!(
                            "ssh tunnel exited immediately (code={code}): {stderr}"
                        )));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let stdout = read_child_stdout_best_effort(&mut child).await;
                        let stderr = read_child_stderr_best_effort(&mut child).await;
                        error!(
                            "Failed to poll ssh tunnel process right after spawn: {e}. stdout: {}, stderr: {}",
                            if stdout.is_empty() { "<empty>" } else { &stdout },
                            if stderr.is_empty() { "<empty>" } else { &stderr }
                        );
                        return Err(VirtualWallError::TunnelSpawn(format!(
                            "failed to poll ssh tunnel right after spawn: {e}. stderr: {stderr}"
                        )));
                    }
                }

                Some(TunnelHandle {
                    info: TunnelInfo {
                        id: id.clone(),
                        node: node_id.clone(),
                        direction: req.direction.clone(),
                        listen: req.listen.clone(),
                        target: req.target.clone(),
                        username: username.clone(),
                        pid: child.id(),
                    },
                    runtime: TunnelRuntime::Child(child),
                })
            }
            SpawnedTunnel::ControlMaster { control_path } => Some(TunnelHandle {
                info: TunnelInfo {
                    id: id.clone(),
                    node: node_id.clone(),
                    direction: req.direction.clone(),
                    listen: req.listen.clone(),
                    target: req.target.clone(),
                    username: username.clone(),
                    pid: None,
                },
                runtime: TunnelRuntime::ControlMaster {
                    target: node_ssh.clone(),
                    username: username.clone(),
                    key_override: self.config.ssh_private_key.clone(),
                    control_path,
                },
            }),
        };

        let info = pending_handle
            .as_mut()
            .expect("pending tunnel handle must exist after spawn")
            .info_with_pid();

        let (reusable_after_spawn, conflicting_after_spawn) = {
            let mut guard = self.tunnels.lock().await;
            let (existing, conflicting_handles) = reusable_and_conflicting_tunnels_from_guard(
                &mut guard,
                &node_id,
                username.as_deref(),
                &req,
            );
            if let Some(existing) = existing {
                (Some(existing), conflicting_handles)
            } else {
                guard.insert(
                    id.clone(),
                    pending_handle
                        .take()
                        .expect("pending tunnel handle must exist before insertion"),
                );
                (None, conflicting_handles)
            }
        };

        close_tunnel_handles(&self.ssh, conflicting_after_spawn).await;

        if let Some(existing) = reusable_after_spawn {
            let pending_handle = pending_handle
                .take()
                .expect("duplicate tunnel path should retain the new tunnel handle");
            close_tunnel_handles(&self.ssh, vec![pending_handle]).await;
            return Ok(existing);
        }

        Ok(info)
    }

    /// List tunnels and evict dead processes.
    pub async fn list_tunnels(&self) -> Result<Vec<TunnelInfo>> {
        // First pass: detect expired tunnels without awaiting.
        let mut guard = self.tunnels.lock().await;

        let mut expired_ids: Vec<(String, Option<i32>, String)> = Vec::new(); // (id, exit_code, poll_err)
        let mut out = Vec::new();

        for (id, handle) in guard.iter_mut() {
            match &mut handle.runtime {
                TunnelRuntime::Child(child) => match child.try_wait() {
                    Ok(Some(status)) => {
                        expired_ids.push((id.clone(), status.code(), String::new()));
                    }
                    Ok(None) => {
                        out.push(handle.info_with_pid());
                    }
                    Err(e) => {
                        expired_ids.push((id.clone(), None, e.to_string()));
                    }
                },
                TunnelRuntime::ControlMaster { control_path, .. } => {
                    if control_path.exists() {
                        out.push(handle.info_with_pid());
                    } else {
                        expired_ids.push((
                            id.clone(),
                            None,
                            "tunnel control master socket is no longer present".to_string(),
                        ));
                    }
                }
            }
        }

        // Remove expired handles and process them outside the lock.
        let mut expired_handles = Vec::with_capacity(expired_ids.len());
        for (id, code, poll_err) in expired_ids {
            if let Some(handle) = guard.remove(&id) {
                expired_handles.push((id, code, poll_err, handle));
            }
        }
        drop(guard);

        // Now we can await reading stderr without blocking other tunnel operations.
        for (id, code, poll_err, mut handle) in expired_handles {
            match &mut handle.runtime {
                TunnelRuntime::Child(child) => {
                    let stdout = read_child_stdout_best_effort(child).await;
                    let stderr = read_child_stderr_best_effort(child).await;
                    if poll_err.is_empty() {
                        warn!(
                            "Tunnel {id} exited (code={:?}). stdout: {}, stderr: {}",
                            code,
                            if stdout.is_empty() { "<empty>" } else { &stdout },
                            if stderr.is_empty() { "<empty>" } else { &stderr }
                        );
                    } else {
                        warn!(
                            "Failed to poll tunnel {id}: {poll_err}. stdout: {}, stderr: {}",
                            if stdout.is_empty() { "<empty>" } else { &stdout },
                            if stderr.is_empty() { "<empty>" } else { &stderr }
                        );
                    }

                    let _ = child.wait().await;
                }
                TunnelRuntime::ControlMaster { .. } => {
                    warn!("Tunnel {id} is no longer available: {poll_err}");
                }
            }
        }

        Ok(out)
    }

    /// Close and remove a tunnel by ID.
    pub async fn close_tunnel(&self, id: &str) -> Result<()> {
        let handle = {
            let mut guard = self.tunnels.lock().await;
            guard.remove(id)
        };

        let Some(handle) = handle else {
            return Err(VirtualWallError::State(format!("Tunnel {id} not found")));
        };

        close_tunnel_handles(&self.ssh, vec![handle]).await;
        Ok(())
    }

    fn resolve_node(&self, node: &str) -> Result<&Node> {
        let key = node.trim();
        if key.is_empty() {
            return Err(VirtualWallError::State("Missing node".to_string()));
        }

        if let Some(n) = self.nodes.get(key) {
            return Ok(n);
        }

        // Allow resolving by rspec client_id.
        if let Some(canon) = self.rspec_map.get(key) {
            if let Some(n) = self.nodes.get(canon) {
                return Ok(n);
            }
        }

        let available = self.nodes.keys().cloned().collect::<Vec<_>>().join(", ");
        Err(VirtualWallError::State(format!(
            "Unknown node '{key}'. Available: {available}"
        )))
    }
}

impl Drop for VirtualWallManager {
    fn drop(&mut self) {
        for node in self.nodes.values() {
            self.ssh.shutdown_cached_sessions_for_target(
                &node.ssh,
                None,
                self.config.ssh_private_key.as_deref(),
            );
        }
    }
}

async fn read_child_stderr_best_effort(child: &mut tokio::process::Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return String::new();
    };

    let mut buf = Vec::new();
    if stderr.read_to_end(&mut buf).await.is_ok() {
        let s = String::from_utf8_lossy(&buf).trim().to_string();
        return s;
    }

    String::new()
}

async fn read_child_stdout_best_effort(child: &mut tokio::process::Child) -> String {
    let Some(mut stdout) = child.stdout.take() else {
        return String::new();
    };

    let mut buf = Vec::new();
    if stdout.read_to_end(&mut buf).await.is_ok() {
        let s = String::from_utf8_lossy(&buf).trim().to_string();
        return s;
    }

    String::new()
}

fn build_nodes(
    config: &VirtualWallConfig,
    rspec: &RspecTopology,
    state: Option<&VirtualWallStateLite>,
) -> Result<(HashMap<String, Node>, HashMap<String, String>)> {
    // Build lookup: rspec login host -> rspec node.
    let mut host_to_rspec: HashMap<String, &crate::rspec::RspecNode> = HashMap::new();
    for n in &rspec.nodes {
        if let Some(login) = &n.login {
            host_to_rspec.insert(login.host.to_ascii_lowercase(), n);
        }
    }

    let mut nodes: HashMap<String, Node> = HashMap::new();
    let mut rspec_map: HashMap<String, String> = HashMap::new();

    if let Some(state) = state {
        for r in &state.resources {
            let Some(meta) = r.metadata.as_ref() else {
                continue;
            };
            let Some(login) = meta.ssh_logins.first() else {
                continue;
            };

            let canon_id = r.name.trim().to_string();
            if canon_id.is_empty() {
                continue;
            }

            let host_lc = login.host.to_ascii_lowercase();
            let rspec_node = host_to_rspec.get(&host_lc).copied();

            let rspec_client_id = rspec_node.map(|n| n.client_id.clone());
            if let Some(cid) = rspec_client_id.as_ref() {
                rspec_map.insert(cid.clone(), canon_id.clone());
            }

            let interfaces = rspec_node
                .map(|n| interfaces_to_json(&n.interfaces))
                .unwrap_or_default();

            nodes.insert(
                canon_id.clone(),
                Node {
                    id: canon_id,
                    rspec_client_id,
                    ssh: SshTarget {
                        host: login.host.clone(),
                        port: login.port,
                        username: login.username.clone().or(config.ssh_username.clone()),
                        jump_proxy: login.jump_proxy.clone().or(config.jump_proxy.clone()),
                    },
                    interfaces,
                },
            );
        }

        if !nodes.is_empty() {
            return Ok((nodes, rspec_map));
        }

        warn!(
            "state.json was present but contained no usable ssh_logins; falling back to rspec logins"
        );
    }

    // Fallback: rspec-only.
    for n in &rspec.nodes {
        let Some(login) = &n.login else {
            continue;
        };

        let canon_id = n.client_id.clone();
        rspec_map.insert(n.client_id.clone(), canon_id.clone());

        nodes.insert(
            canon_id.clone(),
            Node {
                id: canon_id.clone(),
                rspec_client_id: Some(n.client_id.clone()),
                ssh: SshTarget {
                    host: login.host.clone(),
                    port: login.port,
                    username: login.username.clone().or(config.ssh_username.clone()),
                    jump_proxy: config.jump_proxy.clone(),
                },
                interfaces: interfaces_to_json(&n.interfaces),
            },
        );
    }

    if nodes.is_empty() {
        return Err(VirtualWallError::State(
            "No nodes with login info found in state.json or rspec".to_string(),
        ));
    }

    Ok((nodes, rspec_map))
}

fn interfaces_to_json(ifaces: &[crate::rspec::RspecInterface]) -> Vec<Value> {
    ifaces
        .iter()
        .map(|i| {
            json!({
                "id": i.client_id,
                "ips": i.ips,
            })
        })
        .collect()
}

async fn ping_once(host: &str, timeout: Duration) -> bool {
    let host = host.trim();
    if host.is_empty() {
        return false;
    }

    let mut cmd = tokio::process::Command::new("ping");

    // Linux: -c 1 (one packet), -W seconds (timeout per ping)
    cmd.arg("-c")
        .arg("1")
        .arg("-W")
        .arg(timeout.as_secs().max(1).to_string())
        .arg(host)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return false,
    };

    match time::timeout(timeout + Duration::from_secs(1), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = child.kill().await;
            false
        }
    }
}

fn shell_quote(s: impl AsRef<str>) -> String {
    // Minimal, deterministic POSIX-ish quoting for display purposes.
    let s = s.as_ref();
    if s.is_empty() {
        return "''".to_string();
    }

    if s.bytes().all(|b| {
        matches!(
            b,
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/' | b':' | b'@'
        )
    }) {
        return s.to_string();
    }

    // The standard POSIX trick: close quotes, insert a single quote, reopen.
    // foo'bar -> 'foo'"'"'bar'
    let escaped = s.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn first_ip_for_iface<'a>(node: &'a Node, iface_id: &str) -> &'a str {
    for iface in &node.interfaces {
        let id = iface.get("id").and_then(|v| v.as_str());
        if id != Some(iface_id) {
            continue;
        }

        if let Some(ip) = iface
            .get("ips")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.get("address"))
            .and_then(|v| v.as_str())
        {
            return ip;
        }

        break;
    }

    "N/A"
}
