use std::collections::BTreeSet;
use std::time::Duration;

/// Runtime configuration for the local Prometheus metrics scanner.
#[derive(Debug, Clone)]
pub struct MetricsScannerConfig {
    /// Inclusive start of the local TCP port range to probe.
    pub port_start: u16,
    /// Inclusive end of the local TCP port range to probe.
    pub port_end: u16,
    /// Ports that must never be scanned, such as the controller WebSocket port.
    pub excluded_ports: BTreeSet<u16>,
    /// Whether to prefilter candidate ports using `ss -ltn` before scraping `/metrics`.
    pub prefer_ss_listening_port_discovery: bool,
    /// Optional override for the `ss` binary path.
    pub ss_command: String,
    /// Cadence for refreshing the candidate port set and retrying unknown/backoff ports.
    pub discovery_interval: Duration,
    /// Target cadence for successful exporters.
    pub scan_interval: Duration,
    /// Short timeout for probing unknown or currently backed-off localhost ports.
    pub discovery_connect_timeout: Duration,
    /// Timeout for connecting to a known/open exporter.
    pub scrape_connect_timeout: Duration,
    /// HTTP read timeout for known/open exporters.
    pub read_timeout: Duration,
    /// HTTP write timeout for known/open exporters.
    pub write_timeout: Duration,
    /// Maximum accepted `/metrics` body size in bytes.
    pub max_response_bytes: usize,
    /// Number of concurrent scrapes for known active exporters.
    pub scrape_concurrency: usize,
    /// Number of concurrent discovery probes for unknown/backoff ports.
    pub discovery_concurrency: usize,
    /// Maximum backoff applied to repeatedly failing ports.
    pub max_probe_backoff: Duration,
    /// Whether to keep the raw exposition body in memory.
    pub keep_raw_body: bool,
}

impl MetricsScannerConfig {
    /// Builds the scanner configuration from environment variables, falling back
    /// to safe defaults when values are absent or invalid.
    pub fn from_env(excluded_ports: BTreeSet<u16>) -> Self {
        let port_start = read_env_u16("PC_AGENT_METRICS_SCAN_START_PORT").unwrap_or(3000);
        let port_end = read_env_u16("PC_AGENT_METRICS_SCAN_END_PORT").unwrap_or(6000);

        let (port_start, port_end) = if port_start <= port_end {
            (port_start, port_end)
        } else {
            (port_end, port_start)
        };

        Self {
            port_start,
            port_end,
            excluded_ports,
            prefer_ss_listening_port_discovery: read_env_bool(
                "PC_AGENT_METRICS_PREFER_SS_DISCOVERY",
            )
            .unwrap_or(true),
            ss_command: std::env::var("PC_AGENT_METRICS_SS_COMMAND")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "ss".to_string()),
            discovery_interval: Duration::from_millis(
                read_env_u64("PC_AGENT_METRICS_DISCOVERY_INTERVAL_MS")
                    .unwrap_or(1_000)
                    .max(1),
            ),
            scan_interval: Duration::from_millis(
                read_env_u64("PC_AGENT_METRICS_SCAN_INTERVAL_MS")
                    .unwrap_or(1_000)
                    .max(1),
            ),
            discovery_connect_timeout: Duration::from_millis(
                read_env_u64("PC_AGENT_METRICS_DISCOVERY_CONNECT_TIMEOUT_MS").unwrap_or(3),
            ),
            scrape_connect_timeout: Duration::from_millis(
                read_env_u64("PC_AGENT_METRICS_SCRAPE_CONNECT_TIMEOUT_MS").unwrap_or(20),
            ),
            read_timeout: Duration::from_millis(
                read_env_u64("PC_AGENT_METRICS_READ_TIMEOUT_MS").unwrap_or(200),
            ),
            write_timeout: Duration::from_millis(
                read_env_u64("PC_AGENT_METRICS_WRITE_TIMEOUT_MS").unwrap_or(100),
            ),
            max_response_bytes: read_env_usize("PC_AGENT_METRICS_MAX_RESPONSE_BYTES")
                .unwrap_or(2 * 1024 * 1024),
            scrape_concurrency: read_env_usize("PC_AGENT_METRICS_SCRAPE_CONCURRENCY")
                .unwrap_or(32)
                .max(1),
            discovery_concurrency: read_env_usize("PC_AGENT_METRICS_DISCOVERY_CONCURRENCY")
                .unwrap_or(64)
                .max(1),
            max_probe_backoff: Duration::from_secs(
                read_env_u64("PC_AGENT_METRICS_MAX_PROBE_BACKOFF_SECS").unwrap_or(10),
            ),
            keep_raw_body: read_env_bool("PC_AGENT_METRICS_KEEP_RAW_BODY").unwrap_or(false),
        }
    }

    /// Returns true when the given port should be skipped entirely.
    pub fn is_excluded(&self, port: u16) -> bool {
        self.excluded_ports.contains(&port)
    }
}

fn read_env_u16(key: &str) -> Option<u16> {
    std::env::var(key).ok()?.trim().parse::<u16>().ok()
}

fn read_env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse::<u64>().ok()
}

fn read_env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.trim().parse::<usize>().ok()
}

fn read_env_bool(key: &str) -> Option<bool> {
    match std::env::var(key)
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
