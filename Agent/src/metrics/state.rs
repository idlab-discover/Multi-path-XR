use std::time::{Duration, Instant, SystemTime};

/// Health state of a scanned localhost port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortHealth {
    /// The port has not yet been classified.
    Unknown,
    /// The port recently exposed valid metrics and should be scraped at the fast cadence.
    Active,
    /// The port recently failed and should be retried with backoff.
    Backoff,
}

/// Mutable scheduling state for a scanned localhost port.
#[derive(Debug, Clone)]
pub struct PortScanState {
    /// The local TCP port.
    pub port: u16,
    /// Current state of the port.
    pub health: PortHealth,
    /// Consecutive failed scrape attempts.
    pub consecutive_failures: u32,
    /// Consecutive successful scrape attempts.
    pub consecutive_successes: u32,
    /// Next time the port should be probed again.
    pub next_probe_at: Instant,
    /// Last successful scrape timestamp.
    pub last_success_at: Option<SystemTime>,
    /// Last error observed for this port.
    pub last_error: Option<String>,
}

impl PortScanState {
    /// Creates a new port state scheduled to probe immediately.
    pub fn new(port: u16, now: Instant) -> Self {
        Self {
            port,
            health: PortHealth::Unknown,
            consecutive_failures: 0,
            consecutive_successes: 0,
            next_probe_at: now,
            last_success_at: None,
            last_error: None,
        }
    }

    /// Marks the port as successfully scraped and schedules the next fast refresh.
    pub fn mark_success(&mut self, now: Instant, wall_clock_now: SystemTime, interval: Duration) {
        self.health = PortHealth::Active;
        self.consecutive_successes = self.consecutive_successes.saturating_add(1);
        self.consecutive_failures = 0;
        self.last_success_at = Some(wall_clock_now);
        self.last_error = None;
        self.next_probe_at = now + interval;
    }

    /// Marks the port as failed and schedules the next retry using exponential backoff.
    pub fn mark_failure(&mut self, now: Instant, error: String, max_backoff: Duration) {
        self.health = PortHealth::Backoff;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.consecutive_successes = 0;
        self.last_error = Some(error);

        let shift = self.consecutive_failures.saturating_sub(1).min(8);
        let secs = 1u64 << shift;
        let backoff = Duration::from_secs(secs).min(max_backoff);
        self.next_probe_at = now + backoff;
    }

    /// Returns true when the port is due for another probe.
    pub fn due(&self, now: Instant) -> bool {
        self.next_probe_at <= now
    }
}
