use clap::{Parser, ValueEnum};
use std::{path::PathBuf, time::Duration};

use crate::error::AppError;

/// Cache key normalization mode for query parameters.
#[derive(Debug, Copy, Clone, ValueEnum)]
pub enum KeyNormalizationMode {
    /// No normalization; keep the raw query string.
    None,
    /// Drop all query parameters.
    DropAllQuery,
    /// Keep only parameters listed in `key_query_whitelist`.
    Whitelist,
    /// Drop parameters listed in `key_query_blacklist`.
    Blacklist,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, ValueEnum)]
pub enum LogLevel {
    Trace = 0, // Designates very fine-grained informational events, extremely verbose.
    Debug = 1, // Designates fine-grained informational events.
    Info = 2,  // Designates informational messages.
    Warn = 3,  // Designates hazardous situations.
    Error = 4, // Designates very serious errors.
}

/// Runtime configuration for the proxy/cache.
#[derive(Debug, Clone, Parser)]
pub struct Config {
    /// Log level (default: info).
    #[arg(long, env = "DCP_LOG_LEVEL", value_enum, default_value_t = LogLevel::Info)]
    pub log_level: LogLevel,
    /// Address to bind to, e.g. 0.0.0.0:8080
    #[arg(long, env = "DCP_LISTEN_ADDR", default_value = "0.0.0.0:8080")]
    pub listen_addr: String,

    /// Origin base URL, e.g. https://origin.example.com/
    #[arg(long, env = "DCP_ORIGIN_BASE_URL")]
    pub origin_base_url: String,

    /// Root directory for disk cache.
    #[arg(long, env = "DCP_CACHE_DIR", default_value = "./cache")]
    pub cache_dir: PathBuf,

    /// Memory cache TTL in seconds (0 disables memory caching).
    #[arg(long, env = "DCP_MEMORY_TTL_SECS", default_value_t = 5)]
    pub memory_ttl_secs: u64,

    /// Disk cache TTL in seconds (0 disables disk caching).
    #[arg(long, env = "DCP_DISK_TTL_SECS", default_value_t = 60)]
    pub disk_ttl_secs: u64,

    /// Total memory cache capacity in bytes.
    #[arg(long, env = "DCP_MEMORY_MAX_BYTES", default_value_t = 64 * 1024 * 1024)]
    pub memory_max_bytes: u64,

    /// Maximum single object size to store (disk and/or memory).
    #[arg(long, env = "DCP_MAX_OBJECT_BYTES", default_value_t = 256 * 1024 * 1024)]
    pub max_object_bytes: u64,

    /// Maximum per-object size to buffer into memory (even if disk-cached).
    #[arg(long, env = "DCP_MEMORY_OBJECT_MAX_BYTES", default_value_t = 4 * 1024 * 1024)]
    pub memory_object_max_bytes: u64,

    /// Disk sweeper interval in seconds.
    #[arg(long, env = "DCP_SWEEP_INTERVAL_SECS", default_value_t = 30)]
    pub sweep_interval_secs: u64,

    /// Optional hard cap for disk usage (0 = unlimited). Sweeper will evict oldest.
    #[arg(long, env = "DCP_MAX_DISK_BYTES", default_value_t = 0)]
    pub max_disk_bytes: u64,

    /// Allow caching Range requests (default: off).
    #[arg(long, env = "DCP_CACHE_RANGE_REQUESTS", default_value_t = false)]
    pub cache_range_requests: bool,

    /// Cache only DASH-like extensions by default.
    #[arg(long, env = "DCP_CACHE_MODE", value_enum, default_value_t = CacheMode::Dash)]
    pub cache_mode: CacheMode,

    /// Origin connect/read timeout in milliseconds.
    #[arg(long, env = "DCP_ORIGIN_TIMEOUT_MS", default_value_t = 10_000)]
    pub origin_timeout_ms: u64,

    /// Optional TLS cert (PEM) for inbound HTTPS.
    #[arg(long, env = "DCP_TLS_CERT_PEM")]
    pub tls_cert_pem: Option<PathBuf>,

    /// Optional TLS private key (PEM) for inbound HTTPS.
    #[arg(long, env = "DCP_TLS_KEY_PEM")]
    pub tls_key_pem: Option<PathBuf>,

    /// MPD memory cache TTL in seconds (commonly very small, e.g. 1).
    #[arg(long, env = "DCP_MPD_MEMORY_TTL_SECS", default_value_t = 1)]
    pub mpd_memory_ttl_secs: u64,

    /// MPD disk cache TTL in seconds (0 disables disk caching for MPDs).
    #[arg(long, env = "DCP_MPD_DISK_TTL_SECS", default_value_t = 1)]
    pub mpd_disk_ttl_secs: u64,

    /// Memory budget reserved for MPDs (bytes). Remaining budget is used for segments.
    #[arg(long, env = "DCP_MPD_MEMORY_MAX_BYTES", default_value_t = 8 * 1024 * 1024)]
    pub mpd_memory_max_bytes: u64,

    /// Cache key query normalization mode.
    #[arg(long, env = "DCP_KEY_NORMALIZATION_MODE", value_enum, default_value_t = KeyNormalizationMode::None)]
    pub key_normalization_mode: KeyNormalizationMode,

    /// Comma-separated whitelist of query parameter names (used when mode=whitelist).
    #[arg(
        long,
        env = "DCP_KEY_QUERY_WHITELIST",
        value_delimiter = ',',
        default_value = ""
    )]
    pub key_query_whitelist: Vec<String>,

    /// Comma-separated blacklist of query parameter names (used when mode=blacklist).
    #[arg(
        long,
        env = "DCP_KEY_QUERY_BLACKLIST",
        value_delimiter = ',',
        default_value = "cachebust,cb,_,t,ts,timestamp,rand,random,nocache"
    )]
    pub key_query_blacklist: Vec<String>,
}

/// Cache selection policy for request paths.
#[derive(Debug, Copy, Clone, ValueEnum)]
pub enum CacheMode {
    /// Cache typical DASH files (.mpd, .m4s, .mp4, .m4a, .cmfv, .cmfa).
    Dash,
    /// Cache everything (still only GET/HEAD).
    All,
}

impl Config {
    /// Parses config from CLI args + environment variables.
    pub fn from_env() -> Result<Self, AppError> {
        Ok(Self::parse())
    }

    /// Validates configuration invariants.
    pub fn validate(&self) -> Result<(), AppError> {
        if self.origin_base_url.trim().is_empty() {
            return Err(AppError::Config("origin_base_url is required".to_string()));
        }
        if self.max_object_bytes == 0 {
            return Err(AppError::Config("max_object_bytes must be > 0".to_string()));
        }
        if self.memory_object_max_bytes > self.max_object_bytes {
            return Err(AppError::Config(
                "memory_object_max_bytes must be <= max_object_bytes".to_string(),
            ));
        }

        if self.mpd_memory_max_bytes > self.memory_max_bytes {
            return Err(AppError::Config(
                "mpd_memory_max_bytes must be <= memory_max_bytes".to_string(),
            ));
        }
        Ok(())
    }

    /// Memory TTL as Duration.
    pub fn memory_ttl(&self) -> Duration {
        Duration::from_secs(self.memory_ttl_secs)
    }

    /// Disk TTL as Duration.
    #[allow(dead_code)]
    pub fn disk_ttl(&self) -> Duration {
        Duration::from_secs(self.disk_ttl_secs)
    }

    /// Origin timeout as Duration.
    pub fn origin_timeout(&self) -> Duration {
        Duration::from_millis(self.origin_timeout_ms)
    }

    /// Returns MPD memory TTL.
    pub fn mpd_memory_ttl(&self) -> Duration {
        Duration::from_secs(self.mpd_memory_ttl_secs)
    }

    /// Returns MPD disk TTL.
    #[allow(dead_code)]
    pub fn mpd_disk_ttl(&self) -> Duration {
        Duration::from_secs(self.mpd_disk_ttl_secs)
    }
}
