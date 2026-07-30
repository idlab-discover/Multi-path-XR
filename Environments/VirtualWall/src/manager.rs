use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::{fs, io::AsyncReadExt, process::Command, time::sleep};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    config::VirtualWallConfig,
    error::{Result, VirtualWallError},
    resource_spec::{ResourceSpec, ResourceSpecFactory},
    slices::{ResourceDetail, ResourceSummary, SlicesClient},
    state::{ResourceRecord, StateStore},
    topology::{self, TopologyState},
    tunnels::{TunnelDirection, TunnelInfo, TunnelRequest},
};

#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub nodes: usize,
    pub paths: Option<usize>,
    pub reuse: bool,
}

impl StartOptions {
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

#[derive(Clone)]
pub struct VirtualWallManager {
    config: VirtualWallConfig,
    state: std::sync::Arc<StateStore>,
    slices: std::sync::Arc<SlicesClient>,
    tunnels: std::sync::Arc<Mutex<HashMap<String, TunnelHandle>>>,
}

impl VirtualWallManager {
    pub fn try_from_path(config_path: Option<&Path>) -> Result<Self> {
        debug!("Loading config from {:?}", config_path);
        let config = VirtualWallConfig::load(config_path)?;
        debug!("Loaded config: {:#?}", config);
        let state_path = config.state_dir.join("state.json");
        let state_store = StateStore::new(state_path)?;
        Ok(Self::new(config, state_store))
    }

    pub fn new(config: VirtualWallConfig, state_store: StateStore) -> Self {
        let mut slices = SlicesClient::new(config.slices_binary.clone());

        // Ensure all `slices bi ...` calls target the intended infrastructure.
        // This is required for the wall2 staging setup (and avoids orchestrator weirdness).
        if let Some(infra) = config
            .site_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            slices.set_bi_infra_id(Some(infra.to_string()));
        }

        // Ensure SLICES core config is applied even when the user does NOT export env vars
        // (e.g., when keeping staging details in a gitignored toml file).
        if let Some(path) = config.custom_config.as_ref() {
            slices.set_env(
                "SLICES_CUSTOM_CONFIG",
                path.as_os_str().to_string_lossy().to_string(),
            );
        }

        // Ensure BI custom config is applied even when the user does NOT export env vars
        // (e.g., when keeping staging details in a gitignored toml file).
        if let Some(path) = config.bi_custom_config.as_ref() {
            slices.set_env(
                "SLICES_BI_CUSTOM_CONFIG",
                path.as_os_str().to_string_lossy().to_string(),
            );
        }
        Self {
            config,
            state: std::sync::Arc::new(state_store),
            slices: std::sync::Arc::new(slices),
            tunnels: std::sync::Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn resolve_experiment_name(&self) -> String {
        env::var("SLICES_EXPERIMENT")
            .ok()
            .or_else(|| self.config.experiment.clone())
            .unwrap_or_else(|| "virtual-wall".into())
    }

    pub async fn start_from_options(&self, options: StartOptions) -> Result<StartSummary> {
        let experiment_name = self.resolve_experiment_name();

        let experiment = self
            .slices
            .ensure_experiment(
                self.config.project.as_deref(),
                &experiment_name,
                self.config.experiment_duration.as_deref(),
            )
            .await?;

        if options.reuse {
            if let Some(existing) = self.try_reuse_existing(&experiment_name).await? {
                info!("Nothing to start, the experiment already exists.");
                return Ok(existing);
            }
        }

        let (spec, topology_state) = self.build_spec(&options).await?;
        // Persist topology mapping early so it survives partial failures.
        if topology_state.is_some() {
            let experiment_id = experiment.id.clone();
            let experiment_name_state = experiment.friendly_name.clone();
            let topology_state_cloned = topology_state.clone();
            self.state
                .update(|s| {
                    s.experiment_name = Some(experiment_name_state);
                    s.experiment_id = Some(experiment_id);
                    s.topology = topology_state_cloned;
                })
                .await?;
        }
        let spec_path = self.persist_spec(&spec).await?;

        let spec_output = self
            .slices
            .bi_create_from_file(&spec_path, true, &experiment.friendly_name)
            .await?;
        let spec_out = spec_output.stdout;
        let spec_err = spec_output.stderr;
        debug!("bi_create_from_file output: {spec_out:#}");
        debug!("bi create_from_file err output:{spec_err:#}");

        // Wait for resources to appear
        self.wait_until_resources_ready(&experiment_name, spec.resources.len())
            .await?;

        // Refresh resources
        self.refresh_state(&experiment_name, experiment.id.clone())
            .await?;

        let state = self.state.get().await;
        Ok(StartSummary {
            experiment_name: experiment.friendly_name,
            experiment_id: experiment.id,
            resources: state.resources.clone(),
        })
    }

    /// Close tunnels and refresh cached state **without** releasing SLICES resources.
    ///
    /// Useful when you want to detach locally but keep the allocation alive.
    pub async fn disconnect(&self) -> Result<()> {
        // Soft stop: keep allocations; just refresh and acknowledge.
        let state = self.state.get().await;
        let experiment_name = state
            .experiment_name
            .clone()
            .or(self.config.experiment.clone())
            .unwrap_or_else(|| "broadcastfxr-virtualwall".to_string());
        // Best-effort: the experiment may have expired already.
        let _ = self.refresh_state_by_name(&experiment_name).await;
        self.cleanup_tunnels().await;
        Ok(())
    }

    /// Polls SLICES until at least `expected_nodes` resources are present and have an IP/login,
    /// or until `config.ready_timeout` elapses.
    async fn wait_until_resources_ready(
        &self,
        experiment_name: &str,
        expected_nodes: usize,
    ) -> Result<()> {
        let start = Instant::now();
        let deadline = start + self.config.ready_timeout;

        loop {
            if Instant::now() >= deadline {
                return Err(VirtualWallError::Timeout {
                    operation: format!(
                        "waiting for {expected_nodes} resources in experiment `{experiment_name}` to become ready"
                    ),
                    timeout: self.config.ready_timeout,
                });
            }

            let resources = match self.slices.experiment_list_resources(experiment_name).await {
                Ok(r) => r,
                Err(e) => {
                    warn!("Waiting for resources: list-resources failed: {e}");
                    sleep(self.config.ready_poll_interval).await;
                    continue;
                }
            };

            let have = resources.len();
            if have < expected_nodes {
                debug!(
                    "Waiting for resources: have {have}/{expected_nodes} ({} elapsed)",
                    (Instant::now() - start).as_secs()
                );
                sleep(self.config.ready_poll_interval).await;
                continue;
            }

            // Heuristic readiness: resource detail should expose at least an IP or ssh login.
            let mut ready = 0usize;
            for r in &resources {
                let Some(id) =
                    r.id.clone()
                        .or_else(|| r.friendly_name.clone())
                        .or_else(|| r.name.clone())
                else {
                    continue;
                };
                match self
                    .slices
                    .bi_show_with_experiment(&id, experiment_name)
                    .await
                {
                    Ok(d) => {
                        let has_ip = d.private_ipv4.is_some()
                            || d.public_ipv4.is_some()
                            || d.private_ipv6.is_some()
                            || d.public_ipv6.is_some()
                            // Some SLICES outputs only surface addresses via network interfaces.
                            || d.network_interfaces
                                .iter()
                                .any(|iface| !iface.addresses.is_empty());

                        let has_login = !d.ssh_logins.is_empty();

                        if has_ip || has_login {
                            ready += 1;
                        }
                    }
                    Err(e) => {
                        debug!("Waiting for resources: show({id}) failed: {e}");
                    }
                }
            }

            if ready >= expected_nodes {
                info!(
                    "Resources appear ready: {ready}/{expected_nodes} ({} elapsed)",
                    (Instant::now() - start).as_secs()
                );
                return Ok(());
            }

            debug!(
                "Waiting for readiness: {ready}/{expected_nodes} ({} elapsed)",
                (Instant::now() - start).as_secs()
            );
            sleep(self.config.ready_poll_interval).await;
        }
    }

    pub async fn nodes(&self) -> Result<Value> {
        self.refresh_from_cache().await.ok();
        let state = self.state.get().await;
        let nodes: Vec<Value> = state
            .resources
            .iter()
            .map(|resource| {
                json!({
                    "name": resource.name,
                    "status": resource.status,
                    "addresses": resource.addresses,
                    "hostnames": resource.hostnames,
                    "site": resource.site_id,
                    "expires_at": resource.expires_at,
                    "healthy": resource.status.as_deref().map(|s| s.eq_ignore_ascii_case("ready")).unwrap_or(false),
                })
            })
            .collect();
        Ok(json!({ "nodes": nodes }))
    }

    pub async fn links(&self) -> Result<Value> {
        self.refresh_from_cache().await.ok();
        let state = self.state.get().await;
        let mut links = Vec::new();
        // Infer LANs from metadata (network_interfaces entries).
        let mut seen = std::collections::BTreeSet::new();
        for res in &state.resources {
            if let Some(intfs) = res
                .metadata
                .get("network_interfaces")
                .and_then(|v| v.as_array())
            {
                for intf in intfs {
                    if let Some(lan) = intf.get("network_id").and_then(|v| v.as_str()) {
                        seen.insert(lan.to_string());
                    }
                }
            }
        }
        for lan_id in seen {
            let mut members = Vec::new();
            for res in &state.resources {
                let Some(intfs) = res
                    .metadata
                    .get("network_interfaces")
                    .and_then(|v| v.as_array())
                else {
                    continue;
                };
                for intf in intfs {
                    if intf
                        .get("network_id")
                        .and_then(|v| v.as_str())
                        .map(|id| id == lan_id)
                        .unwrap_or(false)
                    {
                        members.push(json!({
                            "node": res.name,
                            "intf": intf.get("port_id").and_then(|v| v.as_str()),
                            "addresses": intf.get("addresses").cloned().unwrap_or(Value::Null),
                        }));
                    }
                }
            }
            links.push(json!({"id": lan_id, "members": members}));
        }
        Ok(json!({ "links": links }))
    }

    pub async fn status(&self) -> Result<Value> {
        self.refresh_from_cache().await.ok();
        let state = self.state.get().await;
        Ok(json!({
            "experiment": {
                "name": state.experiment_name,
                "id": state.experiment_id,
            },
            "resources": state.resources.iter().map(|r| {
                json!({
                    "name": r.name,
                    "status": r.status,
                    "site": r.site_id,
                    "addresses": r.addresses,
                    "hostnames": r.hostnames,
                    "expires_at": r.expires_at,
                    "healthy": r.status.as_deref().map(|s| s.eq_ignore_ascii_case("ready")).unwrap_or(false),
                })
            }).collect::<Vec<Value>>()
        }))
    }

    /// Build a simple graph representation with nodes and virtual switches (one per LAN).
    pub async fn visualize(&self) -> Result<Value> {
        self.refresh_from_cache().await.ok();
        let state = self.state.get().await;
        let mut graph_nodes = Vec::new();
        let mut edges = Vec::new();

        // Add resource nodes
        for res in &state.resources {
            graph_nodes.push(json!({
                "id": res.name,
                "type": "node",
                "status": res.status,
                "addresses": res.addresses,
                "hostnames": res.hostnames,
                "color": "#2563eb", // default node color
            }));
        }

        // Collect LAN memberships
        let mut lan_members: std::collections::BTreeMap<String, Vec<Value>> =
            std::collections::BTreeMap::new();
        for res in &state.resources {
            if let Some(intfs) = res
                .metadata
                .get("network_interfaces")
                .and_then(|v| v.as_array())
            {
                for intf in intfs {
                    if let Some(lan) = intf.get("network_id").and_then(|v| v.as_str()) {
                        lan_members.entry(lan.to_string()).or_default().push(json!({
                            "node": res.name,
                            "intf": intf.get("port_id").and_then(|v| v.as_str()),
                            "addresses": intf.get("addresses").cloned().unwrap_or(Value::Null),
                        }));
                    }
                }
            }
        }

        // Add virtual switches and edges
        for (lan, members) in lan_members {
            let sw_id = format!("lan-{lan}");
            graph_nodes.push(json!({
                "id": sw_id,
                "type": "switch",
                "lan": lan,
                "color": "#6b7280", // switch/lan ghost node
            }));
            for m in members {
                if let Some(node) = m.get("node").and_then(|v| v.as_str()) {
                    edges.push(json!({
                        "src": sw_id,
                        "dst": node,
                        "intf": m.get("intf"),
                        "addresses": m.get("addresses"),
                        "color": "#94a3b8",
                    }));
                }
            }
        }

        Ok(json!({ "nodes": graph_nodes, "edges": edges }))
    }

    pub async fn extend_experiment(&self, duration: &str) -> Result<Value> {
        let state = self.state.get().await;
        let experiment_name = state
            .experiment_name
            .clone()
            .or(self.config.experiment.clone())
            .ok_or_else(|| VirtualWallError::State("No experiment associated with state".into()))?;
        let summary = self
            .slices
            .experiment_extend(&experiment_name, duration)
            .await?;
        Ok(json!({
            "experiment": summary.friendly_name,
            "expires_at": summary.expires_at,
        }))
    }

    pub async fn exec(
        &self,
        node: &str,
        command: &str,
        username: Option<&str>,
        _key_path: Option<&Path>,
        timeout: Option<Duration>,
    ) -> Result<String> {
        let state = self.state.get().await;
        let experiment = state
            .experiment_name
            .clone()
            .or(self.config.experiment.clone())
            .ok_or_else(|| VirtualWallError::State("No experiment in state".into()))?;

        // Prefer slices CLI to handle bastion/proxy logic automatically.
        let cmd_parts = shell_words::split(command).map_err(|e| {
            VirtualWallError::State(format!("Failed to parse command '{command}': {e}"))
        })?;
        if cmd_parts.is_empty() {
            return Err(VirtualWallError::State("Empty command".into()));
        }

        let mut args = vec![
            "bi".to_string(),
            "ssh".to_string(),
            node.to_string(),
            "--experiment".to_string(),
            experiment.clone(),
        ];

        if !self.config.use_jump_proxy {
            args.push("--proxy".to_string());
            args.push("off".to_string());
        }
        args.push("--".to_string());
        if let Some(user) = username {
            args.push("ssh".to_string());
            args.push("-l".to_string());
            args.push(user.to_string());
        }
        args.extend(cmd_parts);

        let output = if let Some(timeout) = timeout {
            tokio::time::timeout(timeout, self.slices.run_raw(args))
                .await
                .map_err(|_| {
                    VirtualWallError::State(format!("Command timed out after {timeout:?}"))
                })??
        } else {
            self.slices.run_raw(args).await?
        };

        Ok(output.stdout.trim().to_string())
    }

    /// Establish an SSH tunnel to a node (local forward `-L` or remote forward `-R`).
    pub async fn open_tunnel(&self, req: TunnelRequest) -> Result<TunnelInfo> {
        let state = self.state.get().await;
        let experiment = state
            .experiment_name
            .clone()
            .or(self.config.experiment.clone())
            .ok_or_else(|| VirtualWallError::State("No experiment in state".into()))?;

        let username = req.username.clone().or(self.config.ssh_username.clone());
        let direction = req.direction.clone();
        let listen = req.listen.clone();
        let target = req.target.clone();
        let node = req.node.clone();
        let id = Uuid::new_v4().to_string();

        let mut args = vec![
            "bi".to_string(),
            "ssh".to_string(),
            node.clone(),
            "--experiment".to_string(),
            experiment.clone(),
        ];
        if !self.config.use_jump_proxy {
            args.push("--proxy".to_string());
            args.push("off".to_string());
        }
        args.push("--".to_string());

        let mut ssh_args = vec![
            "-N".to_string(),
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
            "-o".to_string(),
            "ServerAliveInterval=15".to_string(),
            "-o".to_string(),
            "ServerAliveCountMax=3".to_string(),
        ];
        if let Some(user) = username.clone() {
            ssh_args.push("-l".to_string());
            ssh_args.push(user);
        }
        let flag = match direction {
            TunnelDirection::Local => "-L",
            TunnelDirection::Remote => "-R",
        };
        ssh_args.push(format!(
            "{flag}{}:{}:{}:{}",
            listen.host, listen.port, target.host, target.port
        ));

        args.extend(ssh_args);
        debug!("Opening tunnel {id} via slices: {:?}", args);

        let mut command = self.slices.prepare_command(&args);
        command
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        let child = command.spawn().map_err(|e| {
            VirtualWallError::State(format!("Failed to spawn tunnel {flag} on {node}: {e}"))
        })?;

        let info = TunnelInfo {
            id: id.clone(),
            node,
            direction,
            listen,
            target,
            username,
            pid: child.id(),
        };

        {
            let mut guard = self.tunnels.lock().await;
            guard.insert(
                id.clone(),
                TunnelHandle {
                    info: info.clone(),
                    child,
                },
            );
        }

        Ok(info)
    }

    /// List active tunnels and drop any that have already exited.
    pub async fn list_tunnels(&self) -> Result<Vec<TunnelInfo>> {
        let mut guard = self.tunnels.lock().await;
        let mut expired = Vec::new();
        let mut tunnels = Vec::new();

        for (id, handle) in guard.iter_mut() {
            match handle.child.try_wait() {
                Ok(Some(status)) => {
                    warn!("Tunnel {id} exited with status {status}");
                    expired.push(id.clone());
                }
                Ok(None) => {
                    let mut info = handle.info.clone();
                    info.pid = handle.child.id();
                    tunnels.push(info);
                }
                Err(err) => {
                    warn!("Failed to poll tunnel {id}: {err}");
                    expired.push(id.clone());
                }
            }
        }

        for id in expired {
            guard.remove(&id);
        }

        Ok(tunnels)
    }

    /// Close and remove a tunnel by ID.
    pub async fn close_tunnel(&self, id: &str) -> Result<()> {
        let handle = {
            let mut guard = self.tunnels.lock().await;
            guard.remove(id)
        };

        let Some(mut handle) = handle else {
            return Err(VirtualWallError::State(format!("Tunnel {id} not found")));
        };

        if handle.child.id().is_some() {
            let _ = handle.child.kill().await;
        }
        let _ = handle.child.wait().await;
        Ok(())
    }

    async fn cleanup_tunnels(&self) {
        let handles: Vec<_> = {
            let mut guard = self.tunnels.lock().await;
            guard.drain().map(|(_, h)| h).collect()
        };

        for mut handle in handles {
            if handle.child.id().is_some() {
                let _ = handle.child.kill().await;
            }
            let _ = handle.child.wait().await;
        }
    }

    pub async fn ping_all(&self) -> Result<Value> {
        let state = self.state.get().await;
        if state.resources.len() < 2 {
            return Err(VirtualWallError::State(
                "At least two nodes are required to run ping_all".into(),
            ));
        }
        let src = &state.resources[0];
        let targets: Vec<String> = state.resources[1..]
            .iter()
            .filter_map(|r| r.addresses.first().cloned())
            .collect();
        let username = self.config.ssh_username.clone();
        let key = self.config.ssh_private_key.clone();

        let mut results = Vec::new();
        for target in targets {
            match self
                .exec(
                    &src.name,
                    &format!("ping -c 4 {target}"),
                    username.as_deref(),
                    key.as_deref(),
                    Some(Duration::from_secs(20)),
                )
                .await
            {
                Ok(output) => results.push(json!({ "target": target, "output": output })),
                Err(err) => results.push(json!({
                    "target": target,
                    "error": err.to_string()
                })),
            }
        }

        Ok(json!({ "results": results }))
    }

    async fn build_spec(
        &self,
        options: &StartOptions,
    ) -> Result<(ResourceSpec, Option<TopologyState>)> {
        if let Some(template) = &self.config.resource_spec_template {
            let mut file = fs::File::open(template).await?;
            let mut contents = Vec::new();
            file.read_to_end(&mut contents).await?;
            let spec: ResourceSpec = serde_json::from_slice(&contents)
                .or_else(|_| serde_yaml::from_slice(&contents))
                .map_err(VirtualWallError::from)?;
            return Ok((spec, None));
        }

        if let Some(topology_path) = &self.config.topology_spec_template {
            let mut file = fs::File::open(topology_path).await?;
            let mut contents = Vec::new();
            file.read_to_end(&mut contents).await?;
            let topo_spec: topology::TopologySpec = serde_json::from_slice(&contents)
                .or_else(|_| serde_yaml::from_slice(&contents))
                .map_err(VirtualWallError::from)?;

            // Merge defaults from vw config for deterministic behavior.
            let mut topo_spec = topo_spec;
            if topo_spec.site_id.is_none() {
                topo_spec.site_id = self.config.site_id.clone();
            }
            if topo_spec.image.is_none() {
                topo_spec.image = self.config.image.clone();
            }
            if topo_spec.flavor.is_none() {
                topo_spec.flavor = self.config.flavor.clone();
            }

            let gen = topology::generate(&topo_spec)?;
            return Ok((gen.spec, Some(gen.state)));
        }

        let cloud_init = self
            .config
            .cloud_init_template
            .as_ref()
            .map(|p| p.display().to_string());

        let spec = ResourceSpecFactory::baremetal_cluster(
            options.nodes,
            options.paths.unwrap_or(1),
            self.config.site_id.as_deref(),
            self.config.image.as_deref(),
            self.config.flavor.as_deref(),
            cloud_init.as_deref(),
            self.config.resource_prefix.as_deref(),
        )?;
        Ok((spec, None))
    }

    /// Rebuild state from live SLICES discovery (never trusts cache over SLICES).
    pub async fn recover(&self) -> Result<Value> {
        let experiment_name = self.resolve_experiment_name();
        self.refresh_state_by_name(&experiment_name).await?;
        self.status().await
    }

    /// Extend experiment lifetime and all BI resources in it.
    pub async fn extend_all(&self, duration: &str) -> Result<Value> {
        if duration.trim().is_empty() {
            return Err(VirtualWallError::Configuration(
                "duration must be non-empty".into(),
            ));
        }

        let experiment_name = self.resolve_experiment_name();
        let _ = self
            .slices
            .experiment_extend(&experiment_name, duration)
            .await?;

        let resources = self
            .slices
            .experiment_list_resources(&experiment_name)
            .await?;
        let mut results = Vec::new();
        for r in resources {
            let id = r.id.clone().or(r.friendly_name.clone()).or(r.name.clone());
            if let Some(id) = id {
                match self.slices.bi_extend(&id, duration).await {
                    Ok(()) => results.push(json!({"resource": id, "ok": true})),
                    Err(e) => {
                        results.push(json!({"resource": id, "ok": false, "error": e.to_string()}))
                    }
                }
            }
        }

        let _ = self.refresh_state_by_name(&experiment_name).await.ok();
        Ok(json!({"experiment": experiment_name, "duration": duration, "results": results}))
    }

    /// Reset selected resources (or all if `names_or_ids` is empty).
    pub async fn reset(&self, names_or_ids: &[String]) -> Result<Value> {
        let experiment_name = self.resolve_experiment_name();
        let targets: Vec<String> = if names_or_ids.is_empty() {
            self.slices
                .experiment_list_resources(&experiment_name)
                .await?
                .into_iter()
                .filter_map(|r| r.id.or(r.friendly_name).or(r.name))
                .collect()
        } else {
            names_or_ids.to_vec()
        };

        let mut results = Vec::new();
        for t in targets {
            match self.slices.bi_reset(&t).await {
                Ok(()) => results.push(json!({"resource": t, "ok": true})),
                Err(e) => results.push(json!({"resource": t, "ok": false, "error": e.to_string()})),
            }
        }

        let _ = self.refresh_state_by_name(&experiment_name).await.ok();
        Ok(json!({"experiment": experiment_name, "results": results}))
    }

    /// Destroy all resources in the current experiment (best-effort), clear local state, close tunnels.
    pub async fn down_all(&self) -> Result<()> {
        let experiment_name = self.resolve_experiment_name();

        // Best-effort: if experiment doesn't exist anymore, still clear local state/tunnels.
        let resources = self
            .slices
            .experiment_list_resources(&experiment_name)
            .await
            .unwrap_or_default();

        let ids: Vec<String> = resources
            .into_iter()
            .filter_map(|r| r.id.or(r.friendly_name).or(r.name))
            .collect();

        if !ids.is_empty() {
            // Chunk to avoid command-line length limits.
            const CHUNK: usize = 20;
            for chunk in ids.chunks(CHUNK) {
                let chunk_vec: Vec<String> = chunk.to_vec();
                let _ = self.slices.bi_destroy(&experiment_name, &chunk_vec).await;
            }
        }

        self.cleanup_tunnels().await;
        self.state
            .replace(crate::state::VirtualWallState::default())
            .await?;
        Ok(())
    }

    /// Copy files via `slices bi scp`.
    pub async fn scp(&self, src: &str, dst: &str) -> Result<()> {
        self.slices.bi_scp(src, dst).await
    }

    /// Stop environment: destroy resources, clear local state, close tunnels.
    pub async fn stop(&self) -> Result<()> {
        self.down_all().await
    }

    async fn persist_spec(&self, spec: &ResourceSpec) -> Result<PathBuf> {
        let specs_dir = self.config.state_dir.join("specs");
        fs::create_dir_all(&specs_dir).await?;
        let filename = format!("spec-{}.json", Uuid::new_v4());
        let path = specs_dir.join(filename);
        let data = serde_json::to_vec_pretty(spec)?;
        fs::write(&path, data).await?;
        Ok(path)
    }

    async fn update_state_after_start_with_details(
        &self,
        experiment_name: &str,
        experiment_id: String,
        summaries: &[ResourceSummary],
        details: &[ResourceDetail],
    ) -> Result<()> {
        let mut records = Vec::new();
        let previous = self.state.get().await;
        for summary in summaries {
            let name = summary
                .friendly_name
                .clone()
                .or(summary.name.clone())
                .unwrap_or_else(|| "unnamed-resource".into());

            let mut addresses = Vec::new();
            if let Some(ipv4) = summary.ipv4.clone().or(summary.private_ipv4.clone()) {
                addresses.push(ipv4);
            }
            if let Some(ipv6) = summary.ipv6.clone().or(summary.private_ipv6.clone()) {
                addresses.push(ipv6);
            }

            // Try enrich from detailed show
            let mut hostnames = Vec::new();
            if addresses.is_empty() {
                if let Some(detail) = details.iter().find(|d| d.id == summary.id) {
                    if let Some(ip) = detail.private_ipv4.clone().or(detail.public_ipv4.clone()) {
                        addresses.push(ip);
                    }
                    if let Some(ip) = detail.private_ipv6.clone().or(detail.public_ipv6.clone()) {
                        addresses.push(ip);
                    }
                    for login in &detail.ssh_logins {
                        hostnames.push(login.host.clone());
                    }
                }
            }

            if addresses.is_empty() {
                if let Some(prev) = previous.resources.iter().find(|r| r.name == name) {
                    addresses = prev.addresses.clone();
                    hostnames = prev.hostnames.clone();
                }
            }

            let metadata = if let Some(detail) = details.iter().find(|d| d.id == summary.id) {
                serde_json::to_value(detail).unwrap_or(Value::Null)
            } else {
                serde_json::to_value(summary).unwrap_or(Value::Null)
            };
            tracing::debug!(
                "Updating record {} addresses={:?} status={:?}",
                name,
                addresses,
                summary.status
            );

            records.push(ResourceRecord {
                name,
                resource_id: summary.id.clone(),
                site_id: summary.site_id.clone(),
                addresses,
                hostnames,
                status: summary.status.clone(),
                expires_at: summary.expires_at,
                metadata,
            });
        }

        self.state
            .update(|state| {
                state.experiment_name = Some(experiment_name.to_string());
                state.experiment_id = Some(experiment_id.clone());
                state.last_project = self.config.project.clone();
                state.last_known_site = self.config.site_id.clone();
                state.resources = records;
            })
            .await?;
        Ok(())
    }

    async fn refresh_state_by_name(&self, experiment_name: &str) -> Result<()> {
        let cached_id = self.state.get().await.experiment_id.clone();

        let experiment_id = if let Some(id) = cached_id.filter(|s| !s.trim().is_empty()) {
            id
        } else {
            let Some(summary) = self.slices.try_experiment_show(experiment_name).await? else {
                return Err(VirtualWallError::State(format!(
                    "Experiment `{experiment_name}` not found"
                )));
            };
            summary.id
        };

        self.refresh_state(experiment_name, experiment_id).await
    }

    async fn refresh_state(&self, experiment_name: &str, experiment_id: String) -> Result<()> {
        if let Some(project) = &self.config.project {
            let _ = self.slices.ensure_project(project).await;
        }
        info!(
            "Refreshing state for experiment={} id={}",
            experiment_name, experiment_id
        );

        let summaries = self
            .slices
            .experiment_list_resources(experiment_name)
            .await?;
        tracing::debug!(
            "try_reuse_existing found {} resources in experiment {}",
            summaries.len(),
            experiment_name
        );

        // enrich with details for addresses/interfaces
        let mut details = Vec::new();
        for summary in &summaries {
            if let Some(id) = summary.id.clone() {
                if let Ok(detail) = self
                    .slices
                    .bi_show_with_experiment(&id, experiment_name)
                    .await
                {
                    details.push(detail);
                }
            }
        }

        tracing::debug!(
            "Refreshing state for experiment={} ({} resources discovered)",
            experiment_name,
            summaries.len()
        );

        self.update_state_after_start_with_details(
            experiment_name,
            experiment_id,
            &summaries,
            &details,
        )
        .await
    }

    async fn refresh_from_cache(&self) -> Result<()> {
        let state = self.state.get().await;
        if let Some(experiment_name) = state.experiment_name.clone() {
            let experiment_id = state.experiment_id.clone().unwrap_or_default();
            tracing::debug!(
                "refresh_from_cache using state experiment={} id={}",
                experiment_name,
                experiment_id
            );
            return self.refresh_state(&experiment_name, experiment_id).await;
        }

        if let Some(cfg_exp) = self.config.experiment.clone() {
            let experiment_id = state.experiment_id.clone().unwrap_or_default();
            tracing::debug!(
                "refresh_from_cache using config experiment={} id={}",
                cfg_exp,
                experiment_id
            );
            return self.refresh_state(&cfg_exp, experiment_id).await;
        }

        Ok(())
    }

    async fn try_reuse_existing(&self, experiment_name: &str) -> Result<Option<StartSummary>> {
        let summaries = self
            .slices
            .experiment_list_resources(experiment_name)
            .await?;
        if summaries.is_empty() {
            debug!(
                "try_reuse_existing found no resources in experiment {}",
                experiment_name
            );
            return Ok(None);
        }
        debug!(
            "try_reuse_existing found {} resources in experiment {}",
            summaries.len(),
            experiment_name
        );
        let state_before = self.state.get().await;
        let experiment_id = summaries
            .first()
            .and_then(|s| s.extra.get("experiment_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| state_before.experiment_id.clone())
            .unwrap_or_default();
        self.refresh_state(experiment_name, experiment_id.clone())
            .await?;
        let state = self.state.get().await;
        Ok(Some(StartSummary {
            experiment_name: experiment_name.to_string(),
            experiment_id,
            resources: state.resources.clone(),
        }))
    }

    pub async fn start_terminal(&self, node: &str) -> Result<String> {
        let state = self.state.get().await;
        let experiment = state
            .experiment_name
            .clone()
            .or(self.config.experiment.clone())
            .ok_or_else(|| VirtualWallError::State("No experiment in state".into()))?;
        let slices_cmd = format!("slices bi ssh {node} --experiment {experiment}");

        // Best-effort spawn of a terminal; if unavailable, return the command string.
        if let Ok(child) = Command::new("x-terminal-emulator")
            .arg("-e")
            .arg("bash")
            .arg("-lc")
            .arg(&slices_cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            child.id();
            return Ok(format!("Launched terminal with: {slices_cmd}"));
        }

        if let Ok(child) = Command::new("gnome-terminal")
            .arg("--")
            .arg("bash")
            .arg("-lc")
            .arg(&slices_cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            child.id();
            return Ok(format!("Launched terminal with: {slices_cmd}"));
        }

        Ok(slices_cmd)
    }
}

struct TunnelHandle {
    info: TunnelInfo,
    child: tokio::process::Child,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartSummary {
    pub experiment_name: String,
    pub experiment_id: String,
    pub resources: Vec<ResourceRecord>,
}

#[cfg(test)]
mod tests {
    use super::StartOptions;

    #[test]
    fn parses_query_with_defaults() {
        let options = StartOptions::from_query("");
        assert_eq!(options.nodes, 1);
        assert_eq!(options.paths, None);
    }

    #[test]
    fn parses_query_values() {
        let options = StartOptions::from_query("n_nodes=4&paths=2");
        assert_eq!(options.nodes, 4);
        assert_eq!(options.paths, Some(2));
    }

    #[test]
    fn ignores_invalid_values() {
        let options = StartOptions::from_query("n_nodes=abc&n_paths=x");
        assert_eq!(options.nodes, 1);
        assert_eq!(options.paths, None);
    }
}
