use crate::metrics::parser::PrometheusSample;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

/// Latest per-target metrics snapshot kept in memory for future forwarding.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct TargetMetricsSnapshot {
    /// Local TCP port from which `/metrics` was scraped.
    pub port: u16,
    // Source IP address observed during the scrape.
    pub source_ip: String,
    // Agent node ID observed during the scrape.
    pub agent_node_id: String,
    // Source instance string synthesized from `source_ip` and `agent_node_id`.
    pub source_instance: String,
    /// Wall-clock timestamp of the most recent scrape attempt.
    pub scraped_at: SystemTime,
    /// Duration of the scrape attempt.
    pub scrape_duration: Duration,
    /// True when the HTTP request succeeded and the body was parsed.
    pub scrape_ok: bool,
    /// Human-readable error string when scraping failed.
    pub error: Option<String>,
    /// Raw Prometheus exposition text body.
    pub raw_body: String,
    /// Parsed metric samples extracted from `raw_body`.
    pub samples: Vec<PrometheusSample>,
    /// Number of malformed metric lines ignored by the parser.
    pub malformed_lines: usize,
}

/// Immutable snapshot of the full metrics store.
///
/// This is intended for future forwarding logic so it can work with a stable,
/// read-only copy without holding internal locks.
#[derive(Debug, Clone)]
pub struct MetricsStoreSnapshot {
    /// Per-port latest scrape results.
    pub targets: BTreeMap<u16, TargetMetricsSnapshot>,
    /// Timestamp of the last completed scan round, if any.
    pub last_scan_completed_at: Option<SystemTime>,
    /// Duration of the last completed scan round, if any.
    pub last_scan_duration: Option<Duration>,
    /// Number of scan rounds completed since startup.
    pub scan_rounds_completed: u64,
}

/// Thread-safe in-memory store for all locally scraped Prometheus metrics.
#[derive(Debug, Default)]
pub struct MetricsStore {
    inner: RwLock<MetricsStoreInner>,
}

#[derive(Debug, Default)]
struct MetricsStoreInner {
    targets: BTreeMap<u16, TargetMetricsSnapshot>,
    last_scan_completed_at: Option<SystemTime>,
    last_scan_duration: Option<Duration>,
    scan_rounds_completed: u64,
}

impl MetricsStore {
    /// Creates a new empty metrics store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces the latest snapshot for a target port.
    pub fn upsert_target(&self, snapshot: TargetMetricsSnapshot) {
        let Ok(mut guard) = self.inner.write() else {
            return;
        };
        guard.targets.insert(snapshot.port, snapshot);
    }

    /// Removes targets that were not observed during the latest scan round.
    pub fn retain_only_ports(&self, retained_ports: &BTreeSet<u16>) {
        let Ok(mut guard) = self.inner.write() else {
            return;
        };
        guard
            .targets
            .retain(|port, _| retained_ports.contains(port));
    }

    /// Updates scan-round bookkeeping metadata.
    pub fn complete_scan_round(&self, finished_at: SystemTime, duration: Duration) {
        let Ok(mut guard) = self.inner.write() else {
            return;
        };
        guard.last_scan_completed_at = Some(finished_at);
        guard.last_scan_duration = Some(duration);
        guard.scan_rounds_completed = guard.scan_rounds_completed.saturating_add(1);
    }

    /// Returns a stable clone of the current in-memory store state.
    pub fn snapshot(&self) -> MetricsStoreSnapshot {
        let Ok(guard) = self.inner.read() else {
            return MetricsStoreSnapshot {
                targets: BTreeMap::new(),
                last_scan_completed_at: None,
                last_scan_duration: None,
                scan_rounds_completed: 0,
            };
        };

        MetricsStoreSnapshot {
            targets: guard.targets.clone(),
            last_scan_completed_at: guard.last_scan_completed_at,
            last_scan_duration: guard.last_scan_duration,
            scan_rounds_completed: guard.scan_rounds_completed,
        }
    }

    /// Returns the number of tracked targets currently present in the store.
    pub fn target_count(&self) -> usize {
        let Ok(guard) = self.inner.read() else {
            return 0;
        };
        guard.targets.len()
    }
}
