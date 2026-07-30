use super::EnvironmentHandler;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;
use std::{collections::HashMap, future::Future, pin::Pin, process::Stdio, sync::Arc};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::{
    fs,
    sync::{oneshot, Mutex},
    task,
};
use tracing::{error, info, warn};
use uuid::Uuid;
use virtual_wall::{TunnelDirection, TunnelEndpoint, TunnelInfo, TunnelRequest};

#[derive(Clone)]
pub struct MininetHandler {
    process: Arc<Mutex<Option<Child>>>,
    client: Client,
    base_url: String,
    tunnels: Arc<Mutex<HashMap<String, MininetTunnel>>>,
}

impl MininetHandler {
    fn leaked_host_process_cleanup_script() -> String {
        const LEAKED_BINARIES: &[&str] = &[
            "metrics",
            "pc-agent",
            "pc-receiver",
            "cdn_proxy",
            "pc-server",
        ];

        LEAKED_BINARIES
            .iter()
            .flat_map(|binary| {
                [
                    format!(
                        "pkill -f -- '[t]arget/x86_64-unknown-linux-gnu/release/{binary}' || true"
                    ),
                    format!("pkill -f -- '[t]arget/debug/{binary}' || true"),
                    format!("pkill -x -- '{binary}' || true"),
                ]
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    async fn cleanup_leaked_host_processes(&self) -> Result<String, String> {
        let cleanup_script = Self::leaked_host_process_cleanup_script();
        let output = Command::new("sh")
            .arg("-lc")
            .arg(&cleanup_script)
            .output()
            .await
            .map_err(|e| format!("Failed to run Mininet host cleanup: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!(
                "Mininet host cleanup failed with status {}{}",
                output.status,
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !stdout.is_empty() {
            info!("Mininet host cleanup stdout: {stdout}");
        }
        if !stderr.is_empty() {
            warn!("Mininet host cleanup stderr: {stderr}");
        }

        Ok("Mininet host cleanup completed".to_string())
    }

    pub fn new() -> Self {
        MininetHandler {
            process: Arc::new(Mutex::new(None)),
            client: Client::new(),
            base_url: "http://127.0.0.1:5000".to_string(), // Adjust if your Mininet server runs on a different address
            tunnels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn is_server_running(&self) -> bool {
        let process_guard = self.process.lock().await;
        process_guard.is_some()
    }

    async fn ensure_server_running(&self) -> Result<(), String> {
        let mut process_guard = self.process.lock().await;
        if process_guard.is_some() {
            return Ok(()); // Server is already running
        }

        info!("Starting Mininet server process");
        // Start the Mininet server process
        let mut command = Command::new("../run.sh");
        command
            .arg("--mininet") // Path to your Mininet server script
            .arg("--no-clear")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        match command.spawn() {
            Ok(mut child) => {
                // Set up logging for stdout
                if let Some(stdout) = child.stdout.take() {
                    let mut reader = BufReader::new(stdout).lines();
                    let log_prefix = format!("\x1b[38;5;36m[{}]\x1b[0m", "Mininet").to_string();
                    task::spawn(async move {
                        while let Ok(Some(line)) = reader.next_line().await {
                            info!("{} {}", log_prefix, line);
                            // We should yield here, as this while loop is not very important
                            // and we want to allow other tasks to run.
                            tokio::task::yield_now().await;
                        }
                    });
                }

                // Set up logging for stderr
                if let Some(stderr) = child.stderr.take() {
                    let mut reader = BufReader::new(stderr).lines();
                    let log_prefix = format!("\x1b[38;5;36m[{}]\x1b[0m", "Mininet").to_string();
                    task::spawn(async move {
                        while let Ok(Some(line)) = reader.next_line().await {
                            error!("{} {}", log_prefix, line);
                            // We should yield here, as this while loop is not very important
                            // and we want to allow other tasks to run.
                            tokio::task::yield_now().await;
                        }
                    });
                }

                *process_guard = Some(child);
                // Wait a bit for the server to start
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                Ok(())
            }
            Err(e) => {
                error!("Failed to start Mininet server: {:?}", e);
                Err(format!("Failed to start Mininet server: {e:?}"))
            }
        }
    }

    async fn stop_server(&self) -> Result<(), String> {
        let mut process_guard = self.process.lock().await;
        if let Some(mut child) = process_guard.take() {
            info!("Stopping Mininet server process");
            match child.kill().await {
                Ok(_) => {
                    let _ = child.wait().await;
                    Ok(())
                }
                Err(e) => {
                    error!("Failed to stop Mininet server: {:?}", e);
                    Err(format!("Failed to stop Mininet server: {e:?}"))
                }
            }
        } else {
            Ok(())
        }
    }

    async fn is_network_running_via_status(&self) -> bool {
        let url = format!("{}/status", self.base_url);
        let response = self.client.get(&url).send().await;

        let Ok(resp) = response else {
            return false;
        };
        if !resp.status().is_success() {
            return false;
        }

        let Ok(json) = resp.json::<Value>().await else {
            return false;
        };

        matches!(
            json.get("status").and_then(|s| s.as_str()),
            Some("running") | Some("success")
        )
    }

    fn build_tunnel_request(params: HashMap<String, String>) -> Result<TunnelRequest, String> {
        let direction = match params
            .get("direction")
            .map(|s| s.as_str())
            .unwrap_or("local")
        {
            "local" | "local-forward" => TunnelDirection::Local,
            "remote" | "remote-forward" => TunnelDirection::Remote,
            other => return Err(format!("Invalid `direction`: {other}")),
        };
        let node = params
            .get("node")
            .cloned()
            .unwrap_or_else(|| "local".to_string());
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

    async fn spawn_local_forwarder(&self, req: TunnelRequest) -> Result<TunnelInfo, String> {
        let listen_addr = format!("{}:{}", req.listen.host, req.listen.port);
        let target = req.target.clone();
        let direction = req.direction.clone();
        let id = Uuid::new_v4().to_string();

        // Bind listener first to fail fast.
        let listener = TcpListener::bind(&listen_addr)
            .await
            .map_err(|e| format!("Failed to bind {listen_addr}: {e}"))?;

        // Allow either direction but note this is a host-side forwarder only.
        let (stop_tx, mut stop_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((mut inbound, _peer)) => {
                                let target = target.clone();
                                tokio::spawn(async move {
                                    match TcpStream::connect((target.host.as_str(), target.port)).await {
                                        Ok(mut outbound) => {
                                            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                                        }
                                        Err(err) => {
                                            error!("Tunnel forward connect failed: {err}");
                                        }
                                    }
                                });
                            }
                            Err(err) => {
                                error!("Tunnel accept error: {err}");
                                break;
                            }
                        }
                    }
                }
            }
        });

        let info = TunnelInfo {
            id: id.clone(),
            node: req.node,
            direction,
            listen: req.listen,
            target: req.target,
            username: req.username,
            pid: None,
        };

        let mut guard = self.tunnels.lock().await;
        guard.insert(
            id.clone(),
            MininetTunnel {
                info: info.clone(),
                runtime: TunnelRuntime::Local {
                    stop: stop_tx,
                    handle,
                },
            },
        );

        Ok(info)
    }

    async fn spawn_remote_forwarder(&self, req: TunnelRequest) -> Result<TunnelInfo, String> {
        let id = Uuid::new_v4().to_string();
        let pidfile = format!("/tmp/tunnel-{id}.pid");
        let socat_cmd = format!(
            "bash -c 'nohup socat TCP-LISTEN:{},fork,reuseaddr,bind={} TCP:{}:{} >/tmp/tunnel-{}.log 2>&1 < /dev/null & echo $! | tee {}'",
            req.listen.port, req.listen.host, req.target.host, req.target.port, id, pidfile
        );
        info!("Spawning remote tunnel on node {}: {}", req.node, socat_cmd);
        let output = self.exec_on_node(&req.node, &socat_cmd, false).await?;
        let pid_line = output
            .lines()
            .rev()
            .find(|l| l.trim().chars().all(|c| c.is_ascii_digit()))
            .unwrap_or("")
            .trim()
            .to_string();
        let pid_num = pid_line.parse::<u32>().ok();
        if let Some(pid) = pid_num {
            info!(
                "Tunnel {} on node {} running with pid {}",
                id, req.node, pid
            );
        } else {
            warn!(
                "Tunnel {} on node {} started but no PID captured (output: {})",
                id, req.node, output
            );
        }

        let info = TunnelInfo {
            id: id.clone(),
            node: req.node.clone(),
            direction: req.direction.clone(),
            listen: req.listen.clone(),
            target: req.target.clone(),
            username: req.username.clone(),
            pid: pid_num,
        };

        let exec_handle = self.clone();
        #[allow(clippy::type_complexity)]
        let exec_fn: ExecutorFnType = Arc::new(move |node, command| {
            let exec_handle = exec_handle.clone();
            Box::pin(async move { exec_handle.exec_on_node(&node, &command, true).await })
        });

        let mut guard = self.tunnels.lock().await;
        guard.insert(
            id.clone(),
            MininetTunnel {
                info: info.clone(),
                runtime: TunnelRuntime::Remote {
                    node: req.node.clone(),
                    pid: if pid_line.is_empty() {
                        None
                    } else {
                        Some(pid_line)
                    },
                    pidfile,
                    exec: exec_fn,
                },
            },
        );

        Ok(info)
    }

    async fn exec_on_node(
        &self,
        node: &str,
        command: &str,
        background: bool,
    ) -> Result<String, String> {
        if !self.is_server_running().await {
            return Err("Mininet server is not running".to_string());
        }
        let url = format!("{}/exec", self.base_url);
        let params = [
            ("node", node.to_string()),
            ("command", command.to_string()),
            (
                "background",
                if background {
                    "true".to_string()
                } else {
                    "false".to_string()
                },
            ),
        ];
        let response = self.client.get(&url).query(&params).send().await;
        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    resp.text()
                        .await
                        .map_err(|e| format!("Failed to read exec output: {e}"))
                } else {
                    let err = resp
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    Err(err)
                }
            }
            Err(e) => Err(format!("Failed to execute command: {e}")),
        }
    }
}

struct MininetTunnel {
    info: TunnelInfo,
    runtime: TunnelRuntime,
}

impl MininetTunnel {
    async fn stop(self) {
        match self.runtime {
            TunnelRuntime::Local { stop, handle } => {
                let _ = stop.send(());
                if !handle.is_finished() {
                    handle.abort();
                }
                let _ = handle.await;
            }
            TunnelRuntime::Remote {
                node,
                pid,
                pidfile,
                exec,
            } => {
                let mut pid_val = pid;
                if pid_val.is_none() {
                    if let Ok(contents) = fs::read_to_string(&pidfile).await {
                        pid_val = contents.trim().parse::<u32>().ok().map(|p| p.to_string());
                    }
                }

                if let Some(pid) = pid_val {
                    let _ = exec(node.clone(), format!("kill -TERM {pid}")).await;
                    let _ = exec(node.clone(), format!("kill -KILL {pid}")).await;
                }
                let _ = fs::remove_file(pidfile).await;
            }
        }
    }
}

pub type ExecutorFnType = Arc<
    dyn Fn(String, String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync
        + 'static,
>;

enum TunnelRuntime {
    Local {
        stop: oneshot::Sender<()>,
        handle: tokio::task::JoinHandle<()>,
    },
    Remote {
        node: String,
        pid: Option<String>,
        pidfile: String,
        exec: ExecutorFnType,
    },
}

impl Drop for MininetHandler {
    fn drop(&mut self) {
        let process_clone = self.process.clone();
        let tunnels = self.tunnels.clone();
        tokio::spawn(async move {
            let mut process_guard = process_clone.lock().await;
            if let Some(mut child) = process_guard.take() {
                info!("Dropping MininetHandler and stopping server");
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            // Stop any running tunnels.
            let handles: Vec<_> = {
                let mut guard = tunnels.lock().await;
                guard.drain().map(|(_, t)| t).collect()
            };
            for tunnel in handles {
                tunnel.stop().await;
            }
        });
    }
}

#[async_trait]
impl EnvironmentHandler for MininetHandler {
    async fn start(&self, options: &str) -> Result<String, String> {
        self.ensure_server_running().await?;

        let url = format!("{}/start", self.base_url);
        // Parse the options string into query parameters
        let options_params: Vec<(&str, &str)> = options
            .split('&')
            .filter_map(|s| {
                let mut iter = s.splitn(2, '=');
                if let (Some(k), Some(v)) = (iter.next(), iter.next()) {
                    Some((k, v))
                } else {
                    None
                }
            })
            .collect();
        let response = self.client.get(&url).query(&options_params).send().await;
        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    let json: Value = resp.json().await.unwrap_or_default();
                    info!("Mininet started successfully: {}", json);
                    Ok(json["message"]
                        .as_str()
                        .unwrap_or("Mininet started")
                        .to_string())
                } else {
                    let err = resp
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    if self.is_network_running_via_status().await {
                        warn!(
                            "Mininet /start returned non-success ('{}') but /status reports running; continuing",
                            err
                        );
                        Ok(
                            "Mininet started (recovered after transient /start response error)"
                                .to_string(),
                        )
                    } else {
                        Err(err)
                    }
                }
            }
            Err(e) => {
                if self.is_network_running_via_status().await {
                    warn!(
                        "Mininet /start request failed ('{}') but /status reports running; continuing",
                        e
                    );
                    Ok(
                        "Mininet started (recovered after transient /start connection drop)"
                            .to_string(),
                    )
                } else {
                    Err(format!("Failed to start Mininet: {e}"))
                }
            }
        }
    }

    async fn stop(&self) -> Result<String, String> {
        let url = format!("{}/stop", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    self.stop_server().await?;
                    let json: Value = resp.json().await.unwrap_or_default();
                    Ok(json["message"]
                        .as_str()
                        .unwrap_or("Mininet stopped")
                        .to_string())
                } else {
                    let err = resp
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    Err(err)
                }
            }
            Err(e) => Err(format!("Failed to stop Mininet: {e}")),
        }
    }

    async fn cleanup_processes(&self) -> Result<String, String> {
        self.cleanup_leaked_host_processes().await
    }

    async fn exec(&self, params: HashMap<String, String>) -> Result<String, String> {
        if !self.is_server_running().await {
            return Err("Mininet server is not running".to_string());
        }

        let url = format!("{}/exec", self.base_url);
        let response = self.client.get(&url).query(&params).send().await;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    Ok(text)
                } else {
                    let err = resp
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    Err(err)
                }
            }
            Err(e) => Err(format!("Failed to execute command: {e}")),
        }
    }

    async fn nodes(&self) -> Result<Value, String> {
        if !self.is_server_running().await {
            return Err("Mininet server is not running".to_string());
        }

        let url = format!("{}/nodes", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => resp.json().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("Failed to get nodes: {e}")),
        }
    }

    async fn links(&self) -> Result<Value, String> {
        if !self.is_server_running().await {
            return Err("Mininet server is not running".to_string());
        }

        let url = format!("{}/links", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => resp.json().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("Failed to get links: {e}")),
        }
    }

    async fn status(&self) -> Result<Value, String> {
        if !self.is_server_running().await {
            return Err("Mininet server is not running".to_string());
        }

        let url = format!("{}/status", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => resp.json().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("Failed to get status: {e}")),
        }
    }

    async fn visualize(&self) -> Result<Vec<u8>, String> {
        if !self.is_server_running().await {
            return Err("Mininet server is not running".to_string());
        }

        let url = format!("{}/visualize", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
                    Ok(bytes.to_vec())
                } else {
                    let err = resp
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error".to_string());
                    Err(err)
                }
            }
            Err(e) => Err(format!("Failed to get visualization: {e}")),
        }
    }

    async fn start_xterm(&self, params: HashMap<String, String>) -> Result<String, String> {
        if !self.is_server_running().await {
            return Err("Mininet server is not running".to_string());
        }

        let url = format!("{}/start_xterm", self.base_url);
        let response = self.client.get(&url).query(&params).send().await;

        match response {
            Ok(resp) => {
                if resp.status().is_success() {
                    let json: Value = resp.json().await.unwrap_or_default();
                    Ok(json["message"]
                        .as_str()
                        .unwrap_or("Xterm started")
                        .to_string())
                } else {
                    let json: Value = resp.json().await.unwrap_or_default();
                    Err(json["error"]
                        .as_str()
                        .unwrap_or("Unknown error")
                        .to_string())
                }
            }
            Err(e) => Err(format!("Failed to start xterm: {e}")),
        }
    }

    async fn ping_all(&self) -> Result<Value, String> {
        if !self.is_server_running().await {
            return Err("Mininet server is not running".to_string());
        }

        let url = format!("{}/ping_all", self.base_url);
        let response = self.client.get(&url).send().await;

        match response {
            Ok(resp) => resp.json().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("Failed to ping all: {e}")),
        }
    }

    async fn open_tunnel(&self, params: HashMap<String, String>) -> Result<Value, String> {
        let req = Self::build_tunnel_request(params)?;
        info!(
            "Opening Mininet tunnel: node={} direction={:?} listen={}:{} target={}:{}",
            req.node,
            req.direction,
            req.listen.host,
            req.listen.port,
            req.target.host,
            req.target.port
        );

        let info = match req.direction {
            TunnelDirection::Local => self.spawn_local_forwarder(req).await,
            TunnelDirection::Remote => self.spawn_remote_forwarder(req).await,
        }
        .map_err(|e| format!("Failed to open tunnel: {e}"))?;

        info!(
            "Tunnel opened: id={} node={} pid={:?}",
            info.id, info.node, info.pid
        );
        serde_json::to_value(info).map_err(|e| format!("Failed to serialize tunnel: {e}"))
    }

    async fn close_tunnel(&self, id: &str) -> Result<String, String> {
        let removed = {
            let mut guard = self.tunnels.lock().await;
            guard.remove(id)
        };
        if let Some(tunnel) = removed {
            tunnel.stop().await;
            Ok(format!("Closed tunnel {id}"))
        } else {
            Err(format!("Tunnel {id} not found"))
        }
    }

    async fn list_tunnels(&self) -> Result<Value, String> {
        let mut guard = self.tunnels.lock().await;
        guard.retain(|_, t| match &t.runtime {
            TunnelRuntime::Local { handle, .. } => !handle.is_finished(),
            TunnelRuntime::Remote { .. } => true,
        });
        let tunnels: Vec<TunnelInfo> = guard.values().map(|t| t.info.clone()).collect();
        serde_json::to_value(tunnels).map_err(|e| format!("Failed to serialize tunnels: {e}"))
    }
}
