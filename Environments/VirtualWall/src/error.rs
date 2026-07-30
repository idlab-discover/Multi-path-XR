use std::{path::PathBuf, time::Duration};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum VirtualWallError {
    #[error("configuration error: {0}")]
    Configuration(String),

    #[error("state error: {0}")]
    State(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("slices CLI binary not found (expected at {0:?})")]
    CliNotFound(PathBuf),

    #[error("slices CLI command `{command}` failed with status {status}: {stderr}")]
    CliFailure {
        command: String,
        status: i32,
        stderr: String,
    },

    #[error("invalid CLI output for `{command}`: {message}")]
    CliOutput { command: String, message: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),

    #[error(transparent)]
    SerdeYaml(#[from] serde_yaml::Error),

    #[error(transparent)]
    TomlDe(#[from] toml::de::Error),

    #[error(transparent)]
    ChronoParse(#[from] chrono::ParseError),

    #[error("timeout after {timeout:?}: {operation}")]
    Timeout {
        operation: String,
        timeout: Duration,
    },
}

pub type Result<T> = std::result::Result<T, VirtualWallError>;
