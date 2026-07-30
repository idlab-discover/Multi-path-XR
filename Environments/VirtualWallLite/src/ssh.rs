//! SSH helpers.
//!
//! We intentionally shell out to OpenSSH (`ssh`) instead of embedding an SSH library.
//! This keeps the dependency surface minimal and reuses battle-tested bastion/
//! ProxyJump behavior.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    time::Duration,
};

use serde_json::json;
use tokio::process::{Child, Command};
use tokio::time;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    config::{HostKeyChecking, JumpProxy},
    error::{Result, VirtualWallError},
    tunnels::{TunnelDirection, TunnelEndpoint},
};

const DEFAULT_CONTROL_PERSIST_SECS: u64 = 15 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ControlMasterPurpose {
    Command,
    Tunnel,
}

#[derive(Debug)]
pub enum SpawnedTunnel {
    Child(Child),
    ControlMaster { control_path: PathBuf },
}

impl ControlMasterPurpose {
    fn label(self) -> &'static str {
        match self {
            Self::Command => "c",
            Self::Tunnel => "t",
        }
    }
}

/// SSH login target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub jump_proxy: Option<JumpProxy>,
}

impl SshTarget {
    /// Format `[user@]host` for OpenSSH.
    pub fn destination(&self, username_override: Option<&str>) -> String {
        let user = username_override
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| self.username.clone().filter(|s| !s.trim().is_empty()));

        match user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        }
    }
}

/// Options for creating SSH processes.
#[derive(Debug, Clone)]
pub struct SshOptions {
    pub ssh_binary: PathBuf,
    pub default_username: Option<String>,
    pub default_private_key: Option<PathBuf>,
    /// Default identity to use for the **jump proxy** hop.
    pub default_proxy_private_key: Option<PathBuf>,
    /// Whether to request agent forwarding (`-A`) when an agent socket exists.
    pub forward_agent: bool,
    /// Whether to request X11 forwarding (`-X`).
    pub forward_x11: bool,
    /// SSH keepalive interval (`ServerAliveInterval`).
    pub server_alive_interval: Duration,
    pub use_jump_proxy: bool,
    pub default_jump_proxy: Option<JumpProxy>,
    pub known_hosts: PathBuf,
    pub host_key_checking: HostKeyChecking,
    pub connect_timeout: Duration,
    pub control_dir: PathBuf,
    pub control_persist: Duration,
}

impl SshOptions {
    fn accessible_file(p: &Path) -> bool {
        match std::fs::metadata(p) {
            Ok(m) => m.is_file(),
            Err(_) => false,
        }
    }

    fn control_persist_seconds(&self) -> u64 {
        self.control_persist
            .as_secs()
            .max(DEFAULT_CONTROL_PERSIST_SECS)
    }

    fn effective_username(&self, target: &SshTarget, username_override: Option<&str>) -> Option<String> {
        username_override
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                target
                    .username
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
            })
            .or_else(|| {
                self.default_username
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
            })
    }

    fn effective_jump_proxy(&self, target: &SshTarget) -> Option<JumpProxy> {
        target
            .jump_proxy
            .clone()
            .or_else(|| self.default_jump_proxy.clone())
            .filter(|proxy| !proxy.host.trim().is_empty())
    }

    fn control_path_for(
        &self,
        target: &SshTarget,
        username_override: Option<&str>,
        key_override: Option<&Path>,
        purpose: ControlMasterPurpose,
    ) -> PathBuf {
        let effective_user = self.effective_username(target, username_override);
        let effective_key = key_override
            .map(|p| p.to_path_buf())
            .or_else(|| self.default_private_key.clone());
        let proxy = self.effective_jump_proxy(target);

        let mut hasher = DefaultHasher::new();
        target.host.hash(&mut hasher);
        target.port.hash(&mut hasher);
        effective_user.hash(&mut hasher);
        effective_key
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .hash(&mut hasher);
        self.use_jump_proxy.hash(&mut hasher);
        proxy.as_ref().map(|p| p.host.clone()).hash(&mut hasher);
        proxy.as_ref().map(|p| p.port).hash(&mut hasher);
        proxy
            .as_ref()
            .and_then(|p| p.username.clone())
            .hash(&mut hasher);
        purpose.hash(&mut hasher);

        let digest = hasher.finish();
        self.control_dir
            .join(format!("{}-{digest:016x}", purpose.label()))
    }

    fn append_control_client_args(&self, args: &mut Vec<String>, control_path: &Path) {
        args.push("-oControlMaster=no".to_string());
        args.push(format!(
            "-oControlPath={}",
            control_path.to_string_lossy()
        ));
    }

    fn tunnel_flag_and_spec(
        direction: &TunnelDirection,
        listen: &TunnelEndpoint,
        forward_to: &TunnelEndpoint,
    ) -> (&'static str, String) {
        let flag = match direction {
            TunnelDirection::Local => "-L",
            TunnelDirection::Remote => "-R",
        };
        let spec = format!(
            "{}:{}:{}:{}",
            listen.host.trim(),
            listen.port,
            forward_to.host.trim(),
            forward_to.port
        );
        (flag, spec)
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_control_tunnel_forward(
        &self,
        target: &SshTarget,
        direction: &TunnelDirection,
        listen: &TunnelEndpoint,
        forward_to: &TunnelEndpoint,
        username_override: Option<&str>,
        key_override: Option<&Path>,
        control_path: &Path,
        operation: &str,
    ) -> Result<()> {
        let dest = target.destination(username_override.or(self.default_username.as_deref()));
        let mut args = self.base_args_for_tunnel(target, username_override, key_override);
        self.append_control_client_args(&mut args, control_path);
        args.push("-O".to_string());
        args.push(operation.to_string());

        let (flag, spec) = Self::tunnel_flag_and_spec(direction, listen, forward_to);
        args.push(flag.to_string());
        args.push(spec);
        args.push(dest);

        info!(
            "ssh tunnel control ({operation}): {} {:?}",
            self.ssh_binary.display(),
            args
        );

        let mut command = Command::new(&self.ssh_binary);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let output = match time::timeout(self.connect_timeout + Duration::from_secs(2), command.output()).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(VirtualWallError::Timeout {
                    operation: format!("ssh tunnel control {operation} for {}", target.host),
                    timeout: self.connect_timeout + Duration::from_secs(2),
                });
            }
        };

        if output.status.success() {
            return Ok(());
        }

        let status = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(VirtualWallError::SshFailure { status, stderr })
    }

    async fn control_master_is_alive(
        &self,
        target: &SshTarget,
        username_override: Option<&str>,
        key_override: Option<&Path>,
        control_path: &Path,
    ) -> bool {
        if !control_path.exists() {
            return false;
        }

        let dest = target.destination(username_override.or(self.default_username.as_deref()));
        let mut args = self.base_args(target, username_override, key_override);
        self.append_control_client_args(&mut args, control_path);
        args.push("-O".to_string());
        args.push("check".to_string());
        args.push(dest);

        let mut command = Command::new(&self.ssh_binary);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        match time::timeout(self.connect_timeout + Duration::from_secs(1), command.status()).await {
            Ok(Ok(status)) => status.success(),
            Ok(Err(e)) => {
                warn!(
                    "SSH control-master health check failed for {}: {e}",
                    target.host
                );
                false
            }
            Err(_) => false,
        }
    }

    async fn spawn_control_master(
        &self,
        target: &SshTarget,
        username_override: Option<&str>,
        key_override: Option<&Path>,
        control_path: &Path,
        purpose: ControlMasterPurpose,
    ) -> Result<()> {
        let dest = target.destination(username_override.or(self.default_username.as_deref()));
        let mut args = self.base_args(target, username_override, key_override);
        args.push("-oControlMaster=yes".to_string());
        args.push(format!(
            "-oControlPersist={}",
            self.control_persist_seconds()
        ));
        args.push(format!(
            "-oControlPath={}",
            control_path.to_string_lossy()
        ));
        args.push("-N".to_string());
        args.push("-T".to_string());
        args.push("-f".to_string());
        args.push(dest);

        info!(
            "ssh control master ({}) : {} {:?}",
            purpose.label(),
            self.ssh_binary.display(),
            args
        );

        let mut command = Command::new(&self.ssh_binary);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let output = match time::timeout(self.connect_timeout + Duration::from_secs(2), command.output()).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(VirtualWallError::Timeout {
                    operation: format!("ssh control master spawn for {}", target.host),
                    timeout: self.connect_timeout + Duration::from_secs(2),
                });
            }
        };

        if output.status.success() {
            return Ok(());
        }

        let status = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(VirtualWallError::SshFailure { status, stderr })
    }

    async fn ensure_control_master(
        &self,
        target: &SshTarget,
        username_override: Option<&str>,
        key_override: Option<&Path>,
        purpose: ControlMasterPurpose,
    ) -> Option<PathBuf> {
        if let Err(e) = std::fs::create_dir_all(&self.control_dir) {
            warn!(
                "Failed to create SSH control directory '{}': {e}",
                self.control_dir.display()
            );
            return None;
        }

        let control_path =
            self.control_path_for(target, username_override, key_override, purpose);
        if self
            .control_master_is_alive(target, username_override, key_override, &control_path)
            .await
        {
            return Some(control_path);
        }

        if control_path.exists() {
            let _ = std::fs::remove_file(&control_path);
        }

        if let Err(e) = self
            .spawn_control_master(
                target,
                username_override,
                key_override,
                &control_path,
                purpose,
            )
            .await
        {
            warn!(
                "Falling back to a standalone SSH connection for {} after {} control-master setup failed: {e}",
                target.host,
                purpose.label()
            );
            let _ = std::fs::remove_file(&control_path);
            return None;
        }

        if self
            .control_master_is_alive(target, username_override, key_override, &control_path)
            .await
        {
            Some(control_path)
        } else {
            warn!(
                "SSH {} control master for {} did not become ready; falling back to a standalone SSH connection",
                purpose.label(),
                target.host
            );
            let _ = std::fs::remove_file(&control_path);
            None
        }
    }

    fn shutdown_control_master_blocking(
        &self,
        target: &SshTarget,
        username_override: Option<&str>,
        key_override: Option<&Path>,
        purpose: ControlMasterPurpose,
    ) {
        let control_path =
            self.control_path_for(target, username_override, key_override, purpose);
        if !control_path.exists() {
            return;
        }

        let dest = target.destination(username_override.or(self.default_username.as_deref()));
        let mut args = self.base_args(target, username_override, key_override);
        self.append_control_client_args(&mut args, &control_path);
        args.push("-O".to_string());
        args.push("exit".to_string());
        args.push(dest);

        match StdCommand::new(&self.ssh_binary)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(_) | Err(_) => {
                let _ = std::fs::remove_file(&control_path);
            }
        }
    }

    pub fn shutdown_cached_sessions_for_target(
        &self,
        target: &SshTarget,
        username_override: Option<&str>,
        key_override: Option<&Path>,
    ) {
        self.shutdown_control_master_blocking(
            target,
            username_override,
            key_override,
            ControlMasterPurpose::Command,
        );
        self.shutdown_control_master_blocking(
            target,
            username_override,
            key_override,
            ControlMasterPurpose::Tunnel,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn cancel_tunnel_forward(
        &self,
        target: &SshTarget,
        direction: &TunnelDirection,
        listen: &TunnelEndpoint,
        forward_to: &TunnelEndpoint,
        username_override: Option<&str>,
        key_override: Option<&Path>,
        control_path: &Path,
    ) -> Result<()> {
        self.request_control_tunnel_forward(
            target,
            direction,
            listen,
            forward_to,
            username_override,
            key_override,
            control_path,
            "cancel",
        )
        .await
    }

    /// Build the base OpenSSH argument list for a target.
    pub fn base_args(
        &self,
        target: &SshTarget,
        username_override: Option<&str>,
        key_override: Option<&Path>,
    ) -> Vec<String> {
        let mut args = Vec::with_capacity(24);

        if self.forward_agent && std::env::var_os("SSH_AUTH_SOCK").is_some() {
            args.push("-A".to_string());
        }
        if self.forward_x11 {
            args.push("-X".to_string());
        }

        args.push(format!(
            "-oConnectTimeout={}",
            self.connect_timeout.as_secs().max(1)
        ));

        match self.host_key_checking {
            HostKeyChecking::Strict => {
                // Rely on OpenSSH defaults.

                // Isolate host keys into a dedicated file.
                args.push(format!(
                    "-oUserKnownHostsFile={}",
                    self.known_hosts.to_string_lossy()
                ));
            }
            HostKeyChecking::AcceptNew => {
                // Isolate host keys into a dedicated file.
                args.push(format!(
                    "-oUserKnownHostsFile={}",
                    self.known_hosts.to_string_lossy()
                ));

                args.push("-oStrictHostKeyChecking=accept-new".to_string());
            }
            HostKeyChecking::Off => {
                args.push("-oStrictHostKeyChecking=no".to_string());
                args.push("-oUserKnownHostsFile=/dev/null".to_string());
            }
        }

        // Keep tunnels alive; harmless for exec as well.
        args.push(format!(
            "-oServerAliveInterval={}",
            self.server_alive_interval.as_secs().max(1)
        ));
        args.push("-o".to_string());
        args.push("ServerAliveCountMax=3".to_string());

        // Port.
        // Port (e.g. -oPort=22).
        args.push(format!("-oPort={}", target.port));

        // Identity for the **node hop** only.
        let key_candidate = key_override
            .map(|p| p.to_path_buf())
            .or_else(|| self.default_private_key.clone());

        let key = match key_candidate {
            Some(p) if Self::accessible_file(&p) => Some(p),
            Some(p) => {
                warn!(
                    "SSH node identity file '{}' not accessible; falling back to ssh-agent / default identities",
                    p.to_string_lossy()
                );
                None
            }
            None => None,
        };

        if let Some(key) = key {
            args.push("-i".to_string());
            args.push(key.to_string_lossy().to_string());
        } else {
            // Explicitly allow agent/default keys even if user ssh config sets IdentitiesOnly=yes.
            args.push("-oIdentitiesOnly=no".to_string());
        }

        // Proxy via ProxyCommand (matches: ssh ... -oProxyCommand="ssh ... bastion -W %h:%p")
        if self.use_jump_proxy {
            let proxy = target
                .jump_proxy
                .clone()
                .or_else(|| self.default_jump_proxy.clone());

            if let Some(proxy) = proxy {
                if !proxy.host.trim().is_empty() {
                    // Build: ssh [-i key] -p <port> <user@bastion> -W %h:%p
                    // Use the proxy username exactly as configured (no heuristics).
                    let proxy_user = proxy
                        .username
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty());

                    let default_user = username_override.or(self.default_username.as_deref());

                    let proxy_dest = match proxy_user {
                        Some(u) => format!("{u}@{}", proxy.host.trim()),
                        None => format!(
                            "{}@{}",
                            default_user.unwrap_or("default"),
                            proxy.host.trim()
                        ),
                    };

                    let mut pc_parts: Vec<String> = Vec::with_capacity(16);

                    // Use the same ssh binary (absolute path is fine).
                    pc_parts.push(sh_quote(&self.ssh_binary.to_string_lossy()));

                    pc_parts.push(format!(
                        "-oConnectTimeout={}",
                        self.connect_timeout.as_secs().max(1)
                    ));

                    // Host key behavior for the proxy hop.
                    match self.host_key_checking {
                        HostKeyChecking::Strict => {}
                        HostKeyChecking::AcceptNew => {
                            pc_parts.push("-oStrictHostKeyChecking=accept-new".to_string());
                            pc_parts.push(format!(
                                "-oUserKnownHostsFile={}",
                                sh_quote(&self.known_hosts.to_string_lossy())
                            ));
                        }
                        HostKeyChecking::Off => {
                            pc_parts.push("-oStrictHostKeyChecking=no".to_string());
                            pc_parts.push("-oUserKnownHostsFile=/dev/null".to_string());
                        }
                    }

                    // Identity for proxy hop: use the dedicated proxy key.
                    if let Some(key) = self.default_proxy_private_key.clone() {
                        if Self::accessible_file(&key) {
                            pc_parts.push("-i".to_string());
                            pc_parts.push(sh_quote(&key.to_string_lossy()));
                            pc_parts.push("-oIdentitiesOnly=yes".to_string());
                        } else {
                            warn!(
                                "SSH proxy identity file '{}' not accessible; attempting proxy auth via agent/default identities",
                                key.to_string_lossy()
                            );
                        }
                    }

                    // Port (use the same style as your working command).
                    pc_parts.push(format!("-oPort={}", proxy.port));

                    pc_parts.push(sh_quote(&proxy_dest));
                    pc_parts.push("-W".to_string());
                    pc_parts.push("%h:%p".to_string());

                    let proxy_command = pc_parts.join(" ");

                    args.push(format!("-oProxyCommand={proxy_command}"));
                }
            }
        }

        // Username overrides are expressed via destination `user@host`.
        let _ = username_override;
        args
    }

    pub fn base_args_for_tunnel(
        &self,
        target: &SshTarget,
        username_override: Option<&str>,
        key_override: Option<&Path>,
    ) -> Vec<String> {
        let mut args = self.base_args(target, username_override, key_override);

        // Remove BatchMode for tunnels if present (best-effort).
        // This matches typical interactive ssh behavior more closely.
        let mut i = 0;
        while i + 1 < args.len() {
            if args[i] == "-o" && args[i + 1].starts_with("BatchMode=") {
                args.drain(i..=i + 1);
                continue;
            }
            i += 1;
        }

        args
    }

    /// Execute a remote command and capture stdout.
    pub async fn exec(
        &self,
        target: &SshTarget,
        command: &str,
        username_override: Option<&str>,
        key_override: Option<&Path>,
        timeout: Option<Duration>,
        background: bool,
    ) -> Result<String> {
        let cmd = Self::sanitize_command(command)?;

        if background {
            return self
                .exec_background(target, command, username_override, key_override, timeout)
                .await;
        }

        let dest = target.destination(username_override.or(self.default_username.as_deref()));
        let mut args = self.base_args(target, username_override, key_override);
        if let Some(control_path) = self
            .ensure_control_master(
                target,
                username_override,
                key_override,
                ControlMasterPurpose::Command,
            )
            .await
        {
            self.append_control_client_args(&mut args, &control_path);
        }
        args.push(dest);
        args.push(cmd.to_string());

        info!("ssh exec: {} {:?}", self.ssh_binary.display(), args);

        let mut c = Command::new(&self.ssh_binary);
        c.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Ensure the SSH process doesn't outlive this future on early returns.
        // (tokio supports this on stable; harmless if already exited.)
        c.kill_on_drop(true);

        let mut child = c.spawn()?;

        // Important: avoid `wait_with_output()` so we can still kill on timeout.
        // Also: read stdout/stderr concurrently to avoid pipe buffer deadlocks.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            if let Some(mut out) = stdout {
                out.read_to_end(&mut buf).await?;
            }
            Ok::<Vec<u8>, std::io::Error>(buf)
        });

        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            if let Some(mut err) = stderr {
                err.read_to_end(&mut buf).await?;
            }
            Ok::<Vec<u8>, std::io::Error>(buf)
        });

        let status = match timeout {
            Some(t) => match time::timeout(t, child.wait()).await {
                Ok(res) => res?,
                Err(_) => {
                    // Best-effort kill + reap; then also join output tasks.
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    return Err(VirtualWallError::Timeout {
                        operation: "ssh exec".to_string(),
                        timeout: t,
                    });
                }
            },
            None => child.wait().await?,
        };

        let out_buf = stdout_task
            .await
            .map_err(|e| VirtualWallError::State(format!("stdout task join failed: {e}")))??;
        let err_buf = stderr_task
            .await
            .map_err(|e| VirtualWallError::State(format!("stderr task join failed: {e}")))??;

        if !status.success() {
            let status = status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&out_buf).trim().to_string();
            let stderr = String::from_utf8_lossy(&err_buf).trim().to_string();
            info!(
                "ssh exec failed (code={status}). stdout: {}, stderr: {}",
                if stdout.is_empty() {
                    "<empty>"
                } else {
                    &stdout
                },
                if stderr.is_empty() {
                    "<empty>"
                } else {
                    &stderr
                }
            );
            return Err(VirtualWallError::SshFailure { status, stderr });
        }

        Ok(String::from_utf8_lossy(&out_buf).trim().to_string())
    }

    fn sanitize_command(command: &str) -> Result<String> {
        let cmd = command.trim().to_string();

        if cmd.is_empty() {
            return Err(VirtualWallError::State("Empty command".to_string()));
        }
        if cmd.contains('\0') {
            return Err(VirtualWallError::State(
                "Command contains NUL byte".to_string(),
            ));
        }

        Ok(cmd)
    }

    fn escape_single_quotes_for_sh(s: &str) -> String {
        // For: sh -lc '<HERE>'
        // The POSIX pattern to embed single quotes inside single-quoted strings.
        s.replace('\'', r#"'"'"'"#)
    }

    fn build_background_remote_script(sanitized_cmd: &str, tag: &str) -> (String, String, String) {
        // Remote log paths (deterministic + collision-resistant enough for our use).
        let stdout_path = format!("/tmp/vw_exec_{tag}.out");
        let stderr_path = format!("/tmp/vw_exec_{tag}.err");

        // NOTE:
        // - We use `nohup` + redirect + `&` to detach.
        // - `echo $!` returns the background pid (best-effort).
        // - `exec </dev/null` prevents ssh from keeping stdin open.
        // - We wrap in `sh -lc` to ensure shell job control + $! semantics.
        //
        // We do NOT try to be clever about quoting the user's command: we run it
        // exactly as provided (after minimal sanitation), under the remote shell.
        let script = format!(
            "exec </dev/null; nohup {cmd} >{out} 2>{err} & echo $!",
            cmd = sanitized_cmd,
            out = stdout_path,
            err = stderr_path
        );

        (script, stdout_path, stderr_path)
    }

    async fn exec_background(
        &self,
        target: &SshTarget,
        command: &str,
        username_override: Option<&str>,
        key_override: Option<&Path>,
        timeout: Option<Duration>,
    ) -> Result<String> {
        // Keep this cheap; also makes log files easy to correlate.
        let tag = Uuid::new_v4().to_string();

        let (remote_script, stdout_path, stderr_path) =
            Self::build_background_remote_script(command, &tag);

        // We explicitly run: sh -lc '<script>'
        // so that `nohup ... & echo $!` behaves consistently.
        let quoted_script = Self::escape_single_quotes_for_sh(&remote_script);
        let remote_cmd = format!("sh -lc '{quoted_script}'");

        let dest = target.destination(username_override.or(self.default_username.as_deref()));
        let mut args = self.base_args(target, username_override, key_override);
        if let Some(control_path) = self
            .ensure_control_master(
                target,
                username_override,
                key_override,
                ControlMasterPurpose::Command,
            )
            .await
        {
            self.append_control_client_args(&mut args, &control_path);
        }
        args.push(dest);
        args.push(remote_cmd);

        info!(
            "ssh exec (background): {} {:?}",
            self.ssh_binary.display(),
            args
        );

        let mut c = Command::new(&self.ssh_binary);
        c.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        c.kill_on_drop(true);

        let mut child = c.spawn()?;

        // Read stdout/stderr concurrently to avoid pipe deadlocks.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let stdout_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            if let Some(mut out) = stdout {
                out.read_to_end(&mut buf).await?;
            }
            Ok::<Vec<u8>, std::io::Error>(buf)
        });

        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            if let Some(mut err) = stderr {
                err.read_to_end(&mut buf).await?;
            }
            Ok::<Vec<u8>, std::io::Error>(buf)
        });

        let status = match timeout {
            Some(t) => match time::timeout(t, child.wait()).await {
                Ok(res) => res?,
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let _ = stdout_task.await;
                    let _ = stderr_task.await;
                    return Err(VirtualWallError::Timeout {
                        operation: "ssh exec background".to_string(),
                        timeout: t,
                    });
                }
            },
            None => child.wait().await?,
        };

        let out_buf = stdout_task
            .await
            .map_err(|e| VirtualWallError::State(format!("stdout task join failed: {e}")))??;
        let err_buf = stderr_task
            .await
            .map_err(|e| VirtualWallError::State(format!("stderr task join failed: {e}")))??;

        if !status.success() {
            let status = status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&err_buf).trim().to_string();
            return Err(VirtualWallError::SshFailure { status, stderr });
        }

        let stdout_s = String::from_utf8_lossy(&out_buf).trim().to_string();
        let pid = stdout_s.parse::<u32>().ok();

        let msg = if let Some(pid) = pid {
            format!(
                "Command is running in background on node {} with PID {pid}",
                target.host
            )
        } else {
            format!(
                "Command is running in background on node {} (pid unknown)",
                target.host
            )
        };
        Ok(json!({
            "message": msg,
            "pid": pid,
            "stdout": stdout_path,
            "stderr": stderr_path,
        })
        .to_string())
    }

    /// Spawn a long-running SSH tunnel process (`ssh -N ...`).
    pub async fn spawn_tunnel(
        &self,
        target: &SshTarget,
        direction: TunnelDirection,
        listen: &TunnelEndpoint,
        forward_to: &TunnelEndpoint,
        username_override: Option<&str>,
        key_override: Option<&Path>,
    ) -> Result<SpawnedTunnel> {
        if listen.port == 0 || forward_to.port == 0 {
            return Err(VirtualWallError::State(
                "Tunnel ports must be non-zero".to_string(),
            ));
        }

        let listen_host = listen.host.trim();
        let forward_host = forward_to.host.trim();
        if listen_host.is_empty() || forward_host.is_empty() {
            return Err(VirtualWallError::State(
                "Tunnel host must be non-empty".to_string(),
            ));
        }

        let dest = target.destination(username_override.or(self.default_username.as_deref()));
        let mut args = self.base_args_for_tunnel(target, username_override, key_override);
        if let Some(control_path) = self
            .ensure_control_master(
                target,
                username_override,
                key_override,
                ControlMasterPurpose::Tunnel,
            )
            .await
        {
            match self
                .request_control_tunnel_forward(
                    target,
                    &direction,
                    listen,
                    forward_to,
                    username_override,
                    key_override,
                    &control_path,
                    "forward",
                )
                .await
            {
                Ok(()) => {
                    return Ok(SpawnedTunnel::ControlMaster { control_path });
                }
                Err(err) => {
                    warn!(
                        "Falling back to a standalone SSH tunnel for {} after tunnel control-forward setup failed: {err}",
                        target.host
                    );
                }
            }
        }

        args.push("-N".to_string());
        args.push("-T".to_string()); // no TTY (more deterministic)
        args.push("-oExitOnForwardFailure=yes".to_string());
        args.push("-oLogLevel=ERROR".to_string()); // keep stderr useful but not noisy

        let (flag, spec) = Self::tunnel_flag_and_spec(&direction, listen, forward_to);
        args.push(flag.to_string());
        args.push(spec);

        args.push(dest);

        info!("ssh tunnel: {} {:?}", self.ssh_binary.display(), args);

        let mut c = Command::new(&self.ssh_binary);
        c.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        // Ensure we don't leak a tunnel process if the manager drops unexpectedly.
        c.kill_on_drop(true);

        let child = c.spawn().map_err(|e| {
            VirtualWallError::TunnelSpawn(format!("failed to spawn tunnel ({flag}): {e}"))
        })?;
        Ok(SpawnedTunnel::Child(child))
    }
}

// Change signature so we can optionally fall back to the node username for ProxyJump.
#[allow(dead_code)]
fn format_proxy_jump(proxy: &JumpProxy, fallback_username: Option<&str>) -> String {
    let host_port = if proxy.port == 22 {
        proxy.host.clone()
    } else {
        format!("{}:{}", proxy.host, proxy.port)
    };

    let proxy_user = proxy
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // Heuristic: if the proxy username is missing/empty OR is the placeholder "proxy",
    // fall back to the effective ssh username (often what Virtual Wall expects).
    let effective_user = match proxy_user {
        Some(u) if u != "proxy" => Some(u),
        _ => fallback_username.map(str::trim).filter(|s| !s.is_empty()),
    };

    match effective_user {
        Some(u) => format!("{u}@{host_port}"),
        None => host_port,
    }
}

fn sh_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes().all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/' | b':' | b'@')) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_options() -> SshOptions {
        SshOptions {
            ssh_binary: PathBuf::from("/usr/bin/ssh"),
            default_username: Some("alice".to_string()),
            default_private_key: Some(PathBuf::from("/tmp/node.key")),
            default_proxy_private_key: Some(PathBuf::from("/tmp/proxy.key")),
            forward_agent: false,
            forward_x11: false,
            server_alive_interval: Duration::from_secs(30),
            use_jump_proxy: true,
            default_jump_proxy: Some(JumpProxy {
                host: "bastion.example".to_string(),
                port: 22,
                username: Some("proxyuser".to_string()),
            }),
            known_hosts: PathBuf::from("/tmp/known_hosts"),
            host_key_checking: HostKeyChecking::AcceptNew,
            connect_timeout: Duration::from_secs(5),
            control_dir: PathBuf::from("/tmp/ssh-control"),
            control_persist: Duration::from_secs(900),
        }
    }

    fn sample_target() -> SshTarget {
        SshTarget {
            host: "node-1.example".to_string(),
            port: 22,
            username: Some("remote".to_string()),
            jump_proxy: None,
        }
    }

    #[test]
    fn control_path_is_stable_for_same_target() {
        let options = sample_options();
        let target = sample_target();

        let first =
            options.control_path_for(&target, None, None, ControlMasterPurpose::Command);
        let second =
            options.control_path_for(&target, None, None, ControlMasterPurpose::Command);

        assert_eq!(first, second);
    }

    #[test]
    fn control_path_changes_when_effective_user_changes() {
        let options = sample_options();
        let target = sample_target();

        let base =
            options.control_path_for(&target, None, None, ControlMasterPurpose::Command);
        let overridden = options.control_path_for(
            &target,
            Some("other-user"),
            None,
            ControlMasterPurpose::Command,
        );

        assert_ne!(base, overridden);
    }

    #[test]
    fn control_path_changes_between_command_and_tunnel_roles() {
        let options = sample_options();
        let target = sample_target();

        let command_path =
            options.control_path_for(&target, None, None, ControlMasterPurpose::Command);
        let tunnel_path =
            options.control_path_for(&target, None, None, ControlMasterPurpose::Tunnel);

        assert_ne!(command_path, tunnel_path);
    }
}
