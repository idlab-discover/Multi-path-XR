use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use which::which;

use crate::error::{Result, VirtualWallError};

const DEFAULT_CANDIDATES: &[&str] = &[
    "virtual_wall.private.toml",
    "virtual_wall.toml",
    "virtualwall.toml",
    "config.toml",
];

const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 5 * 60;
const DEFAULT_PING_TIMEOUT_SECS: u64 = 3;

/// How SSH host key checking should behave.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HostKeyChecking {
    /// Use OpenSSH defaults (typically "ask" / strict) and fail on unknown keys.
    Strict,
    /// Automatically accept new host keys, but still error on changed keys.
    #[default]
    AcceptNew,
    /// Disable host key checking (unsafe for untrusted networks).
    Off,
}

/// Optional SSH jump proxy configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct JumpProxy {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
}

fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct VirtualWallConfigFile {
    /// Path to a manifest RSpec XML file.
    pub rspec_path: Option<PathBuf>,
    /// Optional path to a `state.json` file produced by the full manager.
    pub state_json: Option<PathBuf>,

    /// SSH binary to use. If omitted, `ssh` is located in `PATH`.
    pub ssh_binary: Option<PathBuf>,
    /// Default SSH username (used when the rspec/state doesn't provide one).
    pub ssh_username: Option<String>,
    /// Default SSH private key.
    pub ssh_private_key: Option<PathBuf>,

    /// Default SSH private key for the **jump proxy** hop (bastion).
    ///
    /// This is intentionally separate from `ssh_private_key`, because some setups
    /// require a dedicated key for the proxy hop (e.g., JFed key) while the final
    /// nodes authenticate using your normal keys (often via `ssh-agent`).
    pub ssh_proxy_private_key: Option<PathBuf>,

    /// Whether to request SSH agent forwarding (`-A`) when an agent is available.
    pub ssh_forward_agent: Option<bool>,
    /// Whether to request X11 forwarding (`-X`).
    pub ssh_forward_x11: Option<bool>,
    /// SSH keepalive interval (`ServerAliveInterval`).
    pub ssh_server_alive_interval_seconds: Option<u64>,

    /// Location where state files (known_hosts, caches) are stored.
    pub state_dir: Option<PathBuf>,

    /// Prefer connecting through a jump proxy (bastion).
    pub use_jump_proxy: Option<bool>,
    /// Default jump proxy used when the state file does not contain a per-node proxy.
    pub jump_proxy: Option<JumpProxy>,

    /// Path to a dedicated known_hosts file (recommended for ephemeral nodes).
    pub known_hosts: Option<PathBuf>,

    #[serde(default)]
    pub host_key_checking: Option<HostKeyChecking>,

    /// SSH connect timeout.
    pub connect_timeout_seconds: Option<u64>,
    /// Overall remote command timeout.
    pub command_timeout_seconds: Option<u64>,
    /// ICMP ping timeout.
    pub ping_timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct VirtualWallConfig {
    pub rspec_path: PathBuf,
    pub state_json: Option<PathBuf>,

    pub ssh_binary: PathBuf,
    pub ssh_username: Option<String>,
    pub ssh_private_key: Option<PathBuf>,

    pub ssh_proxy_private_key: Option<PathBuf>,
    pub ssh_forward_agent: bool,
    pub ssh_forward_x11: bool,
    pub ssh_server_alive_interval: Duration,

    pub state_dir: PathBuf,
    pub use_jump_proxy: bool,
    pub jump_proxy: Option<JumpProxy>,

    pub known_hosts: PathBuf,
    pub host_key_checking: HostKeyChecking,

    pub connect_timeout: Duration,
    pub command_timeout: Duration,
    pub ping_timeout: Duration,
}

/// Read an environment variable and treat empty/whitespace-only values as missing.
fn env_nonempty(key: &str) -> Option<String> {
    match env::var(key) {
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        }
        Err(_) => None,
    }
}

fn env_path_nonempty(key: &str) -> Option<PathBuf> {
    env_nonempty(key).map(PathBuf::from)
}

fn opt_path_nonempty(p: Option<PathBuf>) -> Option<PathBuf> {
    p.and_then(|p| {
        let s = p.as_os_str().to_string_lossy();
        if s.trim().is_empty() {
            None
        } else {
            Some(p)
        }
    })
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn env_bool(key: &str) -> Option<bool> {
    env_nonempty(key).and_then(|v| parse_bool(&v))
}

fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    p.to_path_buf()
}

fn resolve_relative(base: &Path, p: &Path) -> PathBuf {
    let p = expand_tilde(p);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

impl VirtualWallConfig {
    /// Load config from an explicit path, env var, or default candidates.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut file_config: Option<VirtualWallConfigFile> = None;
        let candidates = Self::candidate_paths(path);
        debug!("Config search order: {:?}", candidates);

        for candidate in &candidates {
            match Self::load_file(candidate) {
                Ok(parsed) => {
                    info!("Loaded Virtual Wall config from {}", candidate.display());
                    file_config = Some(parsed);
                    break;
                }
                Err(e) => {
                    warn!("Skipping config {}: {e}", candidate.display());
                }
            }
        }

        if let Some(cfg) = file_config {
            return Self::resolve(cfg);
        }

        Err(VirtualWallError::Configuration(
            "No valid config file found in candidates or VIRTUAL_WALL_CONFIG".to_string(),
        ))
    }

    fn candidate_paths(path: Option<&Path>) -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        if let Some(path) = path {
            candidates.push(path.to_path_buf());
            if path.is_relative() {
                if let Ok(current) = env::current_dir() {
                    candidates.push(current.join(path));
                }
            }
        } else if let Ok(env_path) = env::var("VIRTUAL_WALL_CONFIG") {
            let p = PathBuf::from(env_path);
            candidates.push(p.clone());
            if p.is_relative() {
                if let Ok(current) = env::current_dir() {
                    candidates.push(current.join(p));
                }
            }
        }

        if let Ok(current) = env::current_dir() {
            for filename in DEFAULT_CANDIDATES {
                candidates.push(current.join(filename));
            }
        }

        candidates
    }

    fn load_file(path: &Path) -> Result<VirtualWallConfigFile> {
        if !path.exists() {
            return Err(VirtualWallError::Configuration(format!(
                "Config file not found: {}",
                path.display()
            )));
        }
        let contents = fs::read_to_string(path)?;
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("toml");
        let parsed: VirtualWallConfigFile = match ext {
            "yaml" | "yml" => serde_yaml::from_str(&contents)?,
            "json" => serde_json::from_str(&contents)?,
            "toml" => toml::from_str(&contents)?,
            _ => toml::from_str(&contents)?,
        };
        Ok(parsed)
    }

    fn resolve(mut cfg: VirtualWallConfigFile) -> Result<Self> {
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // RSpec path is mandatory.
        let rspec_path = env_path_nonempty("VIRTUAL_WALL_RSPEC")
            .or_else(|| opt_path_nonempty(cfg.rspec_path.take()))
            .map(|p| resolve_relative(&cwd, &p))
            .ok_or_else(|| {
                VirtualWallError::Configuration(
                    "Missing `rspec_path` (set in config or VIRTUAL_WALL_RSPEC)".to_string(),
                )
            })?;
        if !rspec_path.exists() {
            return Err(VirtualWallError::Configuration(format!(
                "RSpec file does not exist: {}",
                rspec_path.display()
            )));
        }

        let state_dir = env_path_nonempty("VIRTUAL_WALL_STATE_DIR")
            .or_else(|| opt_path_nonempty(cfg.state_dir.take()).map(|p| resolve_relative(&cwd, &p)))
            .or_else(|| dirs::config_dir().map(|p| p.join("virtual-wall")))
            .unwrap_or_else(|| PathBuf::from("./.virtual-wall"));

        let state_json = env_path_nonempty("VIRTUAL_WALL_STATE_JSON")
            .or_else(|| {
                opt_path_nonempty(cfg.state_json.take()).map(|p| resolve_relative(&cwd, &p))
            })
            .or_else(|| {
                // Default to `state_dir/state.json` if present.
                let candidate = state_dir.join("state.json");
                candidate.exists().then_some(candidate)
            });

        let ssh_binary = env_path_nonempty("VIRTUAL_WALL_SSH_BIN")
            .or_else(|| {
                opt_path_nonempty(cfg.ssh_binary.take()).map(|p| resolve_relative(&cwd, &p))
            })
            .or_else(|| which("ssh").ok())
            .ok_or(VirtualWallError::BinaryNotFound {
                name: "ssh",
                path: None,
            })?;

        let ssh_username =
            env_nonempty("VIRTUAL_WALL_SSH_USERNAME").or_else(|| cfg.ssh_username.take());

        let ssh_private_key = env_path_nonempty("VIRTUAL_WALL_SSH_KEY").or_else(|| {
            opt_path_nonempty(cfg.ssh_private_key.take()).map(|p| resolve_relative(&cwd, &p))
        });

        let ssh_proxy_private_key = env_path_nonempty("VIRTUAL_WALL_SSH_PROXY_KEY").or_else(|| {
            opt_path_nonempty(cfg.ssh_proxy_private_key.take()).map(|p| resolve_relative(&cwd, &p))
        });

        let ssh_forward_agent = env_bool("VIRTUAL_WALL_SSH_FORWARD_AGENT")
            .or(cfg.ssh_forward_agent)
            .unwrap_or(true);

        let ssh_forward_x11 = env_bool("VIRTUAL_WALL_SSH_FORWARD_X11")
            .or(cfg.ssh_forward_x11)
            .unwrap_or(false);

        let ssh_server_alive_interval = Duration::from_secs(
            cfg.ssh_server_alive_interval_seconds
                .or_else(|| {
                    env_nonempty("VIRTUAL_WALL_SSH_SERVER_ALIVE_INTERVAL")
                        .and_then(|v| v.parse().ok())
                })
                .unwrap_or(120)
                .max(1),
        );

        let use_jump_proxy = env_nonempty("VIRTUAL_WALL_USE_JUMP_PROXY")
            .and_then(|v| parse_bool(&v))
            .or(cfg.use_jump_proxy)
            .unwrap_or(true);

        let jump_proxy = {
            let env_host = env_nonempty("VIRTUAL_WALL_PROXY_HOST");
            let env_port =
                env_nonempty("VIRTUAL_WALL_PROXY_PORT").and_then(|p| p.parse::<u16>().ok());
            let env_user = env_nonempty("VIRTUAL_WALL_PROXY_USER");

            if let Some(host) = env_host {
                Some(JumpProxy {
                    host,
                    port: env_port.unwrap_or(22),
                    username: env_user,
                })
            } else {
                cfg.jump_proxy
                    .take()
                    .map(|mut jp| {
                        // Treat empty host as missing.
                        if jp.host.trim().is_empty() {
                            jp.host = "".to_string();
                        }
                        jp
                    })
                    .filter(|jp| !jp.host.trim().is_empty())
            }
        };

        let known_hosts = env_path_nonempty("VIRTUAL_WALL_KNOWN_HOSTS")
            .or_else(|| {
                opt_path_nonempty(cfg.known_hosts.take()).map(|p| resolve_relative(&cwd, &p))
            })
            .unwrap_or_else(|| state_dir.join("known_hosts"));

        let host_key_checking = cfg.host_key_checking.take().unwrap_or_default();

        let connect_timeout = Duration::from_secs(
            cfg.connect_timeout_seconds
                .or_else(|| {
                    env_nonempty("VIRTUAL_WALL_CONNECT_TIMEOUT").and_then(|v| v.parse().ok())
                })
                .unwrap_or(DEFAULT_CONNECT_TIMEOUT_SECS)
                .max(1),
        );

        let command_timeout = Duration::from_secs(
            cfg.command_timeout_seconds
                .or_else(|| {
                    env_nonempty("VIRTUAL_WALL_COMMAND_TIMEOUT").and_then(|v| v.parse().ok())
                })
                .unwrap_or(DEFAULT_COMMAND_TIMEOUT_SECS)
                .max(1),
        );

        let ping_timeout = Duration::from_secs(
            cfg.ping_timeout_seconds
                .or_else(|| env_nonempty("VIRTUAL_WALL_PING_TIMEOUT").and_then(|v| v.parse().ok()))
                .unwrap_or(DEFAULT_PING_TIMEOUT_SECS)
                .max(1),
        );

        Ok(Self {
            rspec_path,
            state_json,
            ssh_binary,
            ssh_username,
            ssh_private_key,
            ssh_proxy_private_key,
            ssh_forward_agent,
            ssh_forward_x11,
            ssh_server_alive_interval,
            state_dir,
            use_jump_proxy,
            jump_proxy,
            known_hosts,
            host_key_checking,
            connect_timeout,
            command_timeout,
            ping_timeout,
        })
    }
}
