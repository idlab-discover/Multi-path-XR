use std::{path::PathBuf, time::Duration};

use thiserror::Error;

/// Error type for the Virtual Wall manager.
#[derive(Error, Debug)]
pub enum VirtualWallError {
    /// Invalid or missing configuration.
    #[error("configuration error: {0}")]
    Configuration(String),

    /// The provided manifest/state does not match expectations.
    #[error("state error: {0}")]
    State(String),

    /// An operation is not supported in this minimal manager.
    #[error("unsupported operation: {0}")]
    Unsupported(String),

    /// Required executable could not be found.
    #[error("binary not found: {name} (searched: {path:?})")]
    BinaryNotFound {
        name: &'static str,
        path: Option<PathBuf>,
    },

    /// The SSH process exited with a non-zero status.
    #[error("ssh command failed with status {status}: {stderr}")]
    SshFailure { status: i32, stderr: String },

    /// The SSH tunnel process failed to spawn.
    #[error("failed to spawn ssh tunnel: {0}")]
    TunnelSpawn(String),

    /// A timeout elapsed.
    #[error("timeout after {timeout:?}: {operation}")]
    Timeout {
        operation: String,
        timeout: Duration,
    },

    /// IO failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// JSON parse/serialize failure.
    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),

    /// YAML parse failure.
    #[error(transparent)]
    SerdeYaml(#[from] serde_yaml::Error),

    /// TOML parse failure.
    #[error(transparent)]
    TomlDe(#[from] toml::de::Error),

    /// XML parse failure.
    #[error("rspec parse error: {0}")]
    Rspec(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, VirtualWallError>;
