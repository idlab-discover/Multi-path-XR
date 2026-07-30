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
const DEFAULT_READY_TIMEOUT_SECS: u64 = 20 * 60;
const DEFAULT_READY_POLL_MS: u64 = 2_000;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct VirtualWallConfigFile {
    pub slices_binary: Option<PathBuf>,
    pub project: Option<String>,
    pub experiment: Option<String>,
    pub experiment_duration: Option<String>,
    pub site_id: Option<String>,
    pub image: Option<String>,
    pub flavor: Option<String>,
    /// Optional SLICES core config file (passed via SLICES_CUSTOM_CONFIG).
    pub custom_config: Option<PathBuf>,
    /// Optional path to a SLICES BI custom config JSON (git-ignored).
    pub bi_custom_config: Option<PathBuf>,
    pub ssh_username: Option<String>,
    pub ssh_private_key: Option<PathBuf>,
    pub resource_prefix: Option<String>,
    pub cloud_init_template: Option<PathBuf>,
    pub resource_spec_template: Option<PathBuf>,
    pub topology_spec_template: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub use_jump_proxy: Option<bool>,
    pub ready_timeout_seconds: Option<u64>,
    pub ready_poll_interval_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct VirtualWallConfig {
    pub slices_binary: PathBuf,
    pub project: Option<String>,
    pub experiment: Option<String>,
    pub experiment_duration: Option<String>,
    pub site_id: Option<String>,
    pub image: Option<String>,
    pub flavor: Option<String>,
    /// Optional SLICES core config file (passed via SLICES_CUSTOM_CONFIG).
    pub custom_config: Option<PathBuf>,
    /// Optional path to a SLICES BI custom config JSON (git-ignored).
    pub bi_custom_config: Option<PathBuf>,
    pub ssh_username: Option<String>,
    pub ssh_private_key: Option<PathBuf>,
    pub resource_prefix: Option<String>,
    pub cloud_init_template: Option<PathBuf>,
    pub resource_spec_template: Option<PathBuf>,
    pub topology_spec_template: Option<PathBuf>,
    pub state_dir: PathBuf,
    pub use_jump_proxy: bool,
    pub ready_timeout: Duration,
    pub ready_poll_interval: Duration,
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

/// Read an environment variable as a PathBuf and treat empty/whitespace-only values as missing.
fn env_path_nonempty(key: &str) -> Option<PathBuf> {
    env_nonempty(key).map(PathBuf::from)
}

/// Like `Option<PathBuf>::and_then`, but also treats `Some("")` as `None` after stringification.
fn opt_path_nonempty(p: Option<PathBuf>) -> Option<PathBuf> {
    p.and_then(|p| {
        // `PathBuf` can be empty; treat it as None to avoid confusing downstream behavior.
        let s = p.as_os_str().to_string_lossy();
        if s.trim().is_empty() {
            None
        } else {
            Some(p)
        }
    })
}

impl VirtualWallConfig {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut file_config = VirtualWallConfigFile::default();
        let candidates = Self::candidate_paths(path);
        debug!("Config search order: {:?}", candidates);

        for candidate in &candidates {
            match Self::load_file(candidate) {
                Ok(parsed) => {
                    info!("Loaded Virtual Wall config from {}", candidate.display());
                    file_config = parsed;
                    break;
                }
                Err(e) => {
                    warn!("Skipping config {}: {e}", candidate.display());
                }
            }
        }

        let resolved = Self::resolve(file_config)?;
        Ok(resolved)
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
                candidates.push(current.join("Environments/VirtualWall").join(filename));
                candidates.push(current.join(filename));
            }
        }

        if let Ok(manifest_dir) = env::var("VIRTUAL_WALL_MANIFEST_DIR") {
            for filename in DEFAULT_CANDIDATES {
                candidates.push(PathBuf::from(&manifest_dir).join(filename));
            }
        }

        candidates
    }

    fn env_u64(key: &str) -> Option<u64> {
        let raw = env::var(key).ok()?;
        raw.trim().parse::<u64>().ok()
    }

    fn clamp_poll_ms(ms: u64) -> u64 {
        // Avoid a zero/insane poll interval that could hot-loop.
        ms.max(200)
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

    fn resolve(file: VirtualWallConfigFile) -> Result<Self> {
        let slices_binary = {
            debug!("Resolving slices binary");
            if let Some(path) = file.slices_binary.filter(|p| p.exists()) {
                debug!("Using slices from config path: {}", path.display());
                path
            } else if let Ok(env_path) = env::var("SLICES_CLI_BIN") {
                let env_path = PathBuf::from(env_path);
                if env_path.exists() {
                    debug!("Using slices from SLICES_CLI_BIN: {}", env_path.display());
                    env_path
                } else {
                    debug!(
                        "SLICES_CLI_BIN={} not found, falling back to which/.venv",
                        env_path.display()
                    );
                    which("slices")
                        .ok()
                        .or_else(|| {
                            env::current_dir()
                                .ok()
                                .map(|c| c.join(".venv").join("bin").join("slices"))
                                .filter(|p| p.exists())
                        })
                        .ok_or_else(|| VirtualWallError::CliNotFound(env_path.clone()))?
                }
            } else if let Ok(found) = which("slices") {
                debug!("Using slices from PATH: {}", found.display());
                found
            } else {
                let candidate = env::current_dir()
                    .map(|c| c.join(".venv").join("bin").join("slices"))
                    .unwrap_or_else(|_| PathBuf::from(".venv/bin/slices"));
                if candidate.exists() {
                    debug!("Using slices from .venv: {}", candidate.display());
                    candidate
                } else {
                    debug!("No slices binary found, failing");
                    return Err(VirtualWallError::CliNotFound(candidate));
                }
            }
        };

        let project = env::var("SLICES_PROJECT").ok().or(file.project);
        let experiment = env::var("SLICES_EXPERIMENT").ok().or(file.experiment);
        let experiment_duration = env::var("SLICES_EXPERIMENT_DURATION")
            .ok()
            .or(file.experiment_duration);
        // SLICES moved from "site_id" naming to "infra_id". We treat this as the BI infra selector.
        // Priority:
        // 1) SLICES_BI_INFRA_ID
        // 2) SLICES_BI_SITE_ID (legacy)
        // 3) config file `site_id`
        let site_id = env::var("SLICES_BI_INFRA_ID")
            .ok()
            .or_else(|| env::var("SLICES_BI_SITE_ID").ok())
            .or(file.site_id);

        // Optional: allow a git-ignored custom config to be injected without committing secrets.
        // Priority:
        // 1) SLICES_CUSTOM_CONFIG env var
        // 2) config file `custom_config`
        let custom_config = env::var("SLICES_CUSTOM_CONFIG")
            .ok()
            .map(PathBuf::from)
            .or(file.custom_config);

        // Optional: allow a git-ignored custom BI config to be injected without committing secrets.
        // Priority:
        // 1) SLICES_BI_CUSTOM_CONFIG env var
        // 2) config file `bi_custom_config`
        let bi_custom_config = opt_path_nonempty(
            env_path_nonempty("SLICES_BI_CUSTOM_CONFIG").or(file.bi_custom_config),
        );
        let image = env::var("VIRTUAL_WALL_IMAGE").ok().or(file.image);
        let flavor = env::var("VIRTUAL_WALL_FLAVOR").ok().or(file.flavor);
        let ssh_username = env::var("VIRTUAL_WALL_SSH_USERNAME")
            .ok()
            .or(file.ssh_username);
        let ssh_private_key =
            opt_path_nonempty(env_path_nonempty("VIRTUAL_WALL_SSH_KEY").or(file.ssh_private_key));

        let resource_prefix = env::var("VIRTUAL_WALL_RESOURCE_PREFIX")
            .ok()
            .or(file.resource_prefix);

        let cloud_init_template = opt_path_nonempty(
            env_path_nonempty("VIRTUAL_WALL_CLOUD_INIT_TEMPLATE").or(file.cloud_init_template),
        );
        let resource_spec_template = opt_path_nonempty(
            env_path_nonempty("VIRTUAL_WALL_RESOURCE_SPEC").or(file.resource_spec_template),
        );
        let topology_spec_template = opt_path_nonempty(
            env_path_nonempty("VIRTUAL_WALL_TOPOLOGY_SPEC").or(file.topology_spec_template),
        );

        let use_jump_proxy = env::var("VIRTUAL_WALL_USE_JUMP_PROXY")
            .ok()
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .or(file.use_jump_proxy)
            .unwrap_or(true);

        let state_dir =
            opt_path_nonempty(env_path_nonempty("VIRTUAL_WALL_STATE_DIR").or(file.state_dir))
                .unwrap_or_else(Self::default_state_dir);

        if !state_dir.exists() {
            fs::create_dir_all(&state_dir)?;
        }

        let ready_timeout_seconds = Self::env_u64("VIRTUAL_WALL_READY_TIMEOUT_SECONDS")
            .or(file.ready_timeout_seconds)
            .unwrap_or(DEFAULT_READY_TIMEOUT_SECS);

        let ready_poll_interval_ms = Self::env_u64("VIRTUAL_WALL_READY_POLL_INTERVAL_MS")
            .or(file.ready_poll_interval_ms)
            .map(Self::clamp_poll_ms)
            .unwrap_or(DEFAULT_READY_POLL_MS);

        let ready_timeout = Duration::from_secs(ready_timeout_seconds);
        let ready_poll_interval = Duration::from_millis(ready_poll_interval_ms);

        Ok(Self {
            slices_binary,
            project,
            experiment,
            experiment_duration,
            site_id,
            image,
            flavor,
            custom_config,
            bi_custom_config,
            ssh_username,
            ssh_private_key,
            resource_prefix,
            cloud_init_template,
            resource_spec_template,
            topology_spec_template,
            state_dir,
            use_jump_proxy,
            ready_timeout,
            ready_poll_interval,
        })
    }

    fn default_state_dir() -> PathBuf {
        if let Some(config_dir) = dirs::config_dir() {
            return config_dir.join("multipath-xr").join("virtual-wall");
        }
        env::current_dir()
            .map(|dir| dir.join("virtual-wall-state"))
            .unwrap_or_else(|_| PathBuf::from("./virtual-wall-state"))
    }
}
