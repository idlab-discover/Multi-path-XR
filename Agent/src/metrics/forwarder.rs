use crate::metrics::parser::PrometheusSample;
use crate::metrics::store::{MetricsStore, MetricsStoreSnapshot, TargetMetricsSnapshot};
use rust_socketio::client::Client;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, error};

/// Runtime configuration for reporting local metrics snapshots back to the controller.
#[derive(Debug, Clone)]
pub struct MetricsForwarderConfig {
    /// Event emitted to the controller.
    pub event_name: String,
    /// Poll interval used while waiting for a new scrape round to complete.
    pub poll_interval: Duration,
    /// Maximum interval between two forwarded snapshots, even when the
    /// underlying metrics content did not change.
    pub force_emit_interval: Duration,
}

impl MetricsForwarderConfig {
    /// Builds the forwarder configuration from environment variables.
    pub fn from_env() -> Self {
        Self {
            event_name: std::env::var("PC_AGENT_METRICS_EVENT_NAME")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "agent_metrics_snapshot".to_string()),
            poll_interval: Duration::from_millis(
                read_env_u64("PC_AGENT_METRICS_FORWARDER_POLL_INTERVAL_MS").unwrap_or(200),
            ),
            force_emit_interval: Duration::from_millis(
                read_env_u64("PC_AGENT_METRICS_FORCE_EMIT_INTERVAL_MS").unwrap_or(10_000),
            ),
        }
    }

    /// Prevent polling slower than the scrape cadence, which would otherwise
    /// skip changed snapshots when the scanner is configured for faster sampling.
    pub fn clamp_to_scan_interval(&mut self, scan_interval: Duration) {
        if self.poll_interval > scan_interval {
            self.poll_interval = scan_interval;
        }
        if self.force_emit_interval < self.poll_interval {
            self.force_emit_interval = self.poll_interval;
        }
    }
}

fn read_env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse::<u64>().ok()
}

/// Starts the background forwarder thread.
pub fn start_metrics_forwarder(
    config: MetricsForwarderConfig,
    store: Arc<MetricsStore>,
    socket: Arc<Mutex<Client>>,
    shutdown: Arc<AtomicBool>,
    node_id: String,
) -> Result<JoinHandle<()>, std::io::Error> {
    thread::Builder::new()
        .name("metrics_forwarder_thread".to_string())
        .spawn(move || run_metrics_forwarder(config, store, socket, shutdown, node_id))
}

fn run_metrics_forwarder(
    config: MetricsForwarderConfig,
    store: Arc<MetricsStore>,
    socket: Arc<Mutex<Client>>,
    shutdown: Arc<AtomicBool>,
    node_id: String,
) {
    sleep_interruptibly(Duration::from_millis(3000), &shutdown);

    let mut last_forwarded_round;
    let mut last_forwarded_at: Option<Instant> = None;
    let mut last_forwarded_content: Option<ComparableSnapshot> = None;

    while !shutdown.load(Ordering::Acquire) {
        let snapshot = store.snapshot();
        let comparable_snapshot = ComparableSnapshot::from_store_snapshot(&snapshot);

        let content_changed = last_forwarded_content
            .as_ref()
            .map(|previous| previous != &comparable_snapshot)
            .unwrap_or(true);

        let should_force_emit = last_forwarded_at
            .map(|at| at.elapsed() >= config.force_emit_interval)
            .unwrap_or(true);

        if content_changed || should_force_emit {
            let payload = ForwardMetricsSnapshot::from_store_snapshot(&node_id, snapshot);
            match serde_json::to_value(&payload) {
                Ok(value) => match socket.lock() {
                    Ok(client) => {
                        if let Err(error) = client.emit(config.event_name.as_str(), value) {
                            error!(
                                "Failed to emit '{}' metrics snapshot to controller: {}",
                                config.event_name, error
                            );
                        } else {
                            let reason = if content_changed {
                                "content_changed"
                            } else {
                                "forced_periodic_emit"
                            };
                            last_forwarded_round = payload.scan_rounds_completed;
                            last_forwarded_at = Some(Instant::now());
                            last_forwarded_content = Some(comparable_snapshot);
                            debug!(
                                "Forwarded metrics snapshot round {} to controller ({})",
                                last_forwarded_round, reason
                            );
                        }
                    }
                    Err(error) => {
                        error!(
                            "Failed to acquire websocket client lock for metrics forwarder: {}",
                            error
                        );
                    }
                },
                Err(error) => {
                    error!(
                        "Failed to serialize metrics snapshot for controller forwarding: {}",
                        error
                    );
                }
            }
        }

        sleep_interruptibly(config.poll_interval, &shutdown);
    }
}

fn sleep_interruptibly(duration: Duration, shutdown: &AtomicBool) {
    const STEP: Duration = Duration::from_millis(50);

    let started = Instant::now();
    while started.elapsed() < duration {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        let remaining = duration.saturating_sub(started.elapsed());
        thread::sleep(remaining.min(STEP));
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ComparableSnapshot {
    targets: Vec<ComparableTargetMetricsSnapshot>,
}

impl ComparableSnapshot {
    fn from_store_snapshot(snapshot: &MetricsStoreSnapshot) -> Self {
        let mut targets = snapshot
            .targets
            .values()
            .map(ComparableTargetMetricsSnapshot::from_target_snapshot)
            .collect::<Vec<_>>();
        targets.sort_by_key(|target| target.port);
        Self { targets }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ComparableTargetMetricsSnapshot {
    port: u16,
    source_ip: String,
    agent_node_id: String,
    source_instance: String,
    scrape_ok: bool,
    error: Option<String>,
    raw_body: String,
    samples: Vec<PrometheusSample>,
    malformed_lines: usize,
}

impl ComparableTargetMetricsSnapshot {
    fn from_target_snapshot(snapshot: &TargetMetricsSnapshot) -> Self {
        Self {
            port: snapshot.port,
            source_ip: snapshot.source_ip.clone(),
            agent_node_id: snapshot.agent_node_id.clone(),
            source_instance: snapshot.source_instance.clone(),
            scrape_ok: snapshot.scrape_ok,
            error: snapshot.error.clone(),
            raw_body: snapshot.raw_body.clone(),
            samples: snapshot.samples.clone(),
            malformed_lines: snapshot.malformed_lines,
        }
    }
}

#[derive(Debug, Serialize)]
struct ForwardMetricsSnapshot {
    node_id: String,
    last_scan_completed_at_ms: Option<u64>,
    last_scan_duration_ms: Option<u64>,
    scan_rounds_completed: u64,
    targets: Vec<ForwardTargetMetricsSnapshot>,
}

impl ForwardMetricsSnapshot {
    fn from_store_snapshot(node_id: &str, snapshot: MetricsStoreSnapshot) -> Self {
        let mut targets = snapshot
            .targets
            .into_values()
            .map(ForwardTargetMetricsSnapshot::from_target_snapshot)
            .collect::<Vec<_>>();
        targets.sort_by_key(|target| target.port);

        Self {
            node_id: node_id.to_string(),
            last_scan_completed_at_ms: snapshot
                .last_scan_completed_at
                .and_then(system_time_to_unix_ms),
            last_scan_duration_ms: snapshot
                .last_scan_duration
                .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
            scan_rounds_completed: snapshot.scan_rounds_completed,
            targets,
        }
    }
}

#[derive(Debug, Serialize)]
struct ForwardTargetMetricsSnapshot {
    port: u16,
    source_ip: String,
    agent_node_id: String,
    source_instance: String,
    scraped_at_ms: u64,
    scrape_duration_ms: u64,
    scrape_ok: bool,
    error: Option<String>,
    malformed_lines: usize,
    sample_count: usize,
    samples: Vec<PrometheusSample>,
}

impl ForwardTargetMetricsSnapshot {
    fn from_target_snapshot(snapshot: TargetMetricsSnapshot) -> Self {
        Self {
            port: snapshot.port,
            source_ip: snapshot.source_ip,
            agent_node_id: snapshot.agent_node_id,
            source_instance: snapshot.source_instance,
            scraped_at_ms: system_time_to_unix_ms(snapshot.scraped_at).unwrap_or(0),
            scrape_duration_ms: u64::try_from(snapshot.scrape_duration.as_millis())
                .unwrap_or(u64::MAX),
            scrape_ok: snapshot.scrape_ok,
            error: snapshot.error,
            malformed_lines: snapshot.malformed_lines,
            sample_count: snapshot.samples.len(),
            samples: snapshot.samples,
        }
    }
}

fn system_time_to_unix_ms(value: SystemTime) -> Option<u64> {
    let duration = value.duration_since(UNIX_EPOCH).ok()?;
    Some(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
}
