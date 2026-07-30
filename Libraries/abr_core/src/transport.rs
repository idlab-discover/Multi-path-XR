use crate::Observation;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCounterDirection {
    Rx,
    Tx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TransportMetricsSnapshot {
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    pub rtt: Duration,
    pub cwnd_bytes: u64,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub pacing_rate_bps: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportObservationPolicy {
    pub throughput_direction: TransportCounterDirection,
    pub min_sample_bytes: Option<u64>,
    pub include_pacing_rate: bool,
    pub floor_throughput_with_pacing_rate: bool,
}

impl TransportObservationPolicy {
    pub const fn new(throughput_direction: TransportCounterDirection) -> Self {
        Self {
            throughput_direction,
            min_sample_bytes: None,
            include_pacing_rate: true,
            floor_throughput_with_pacing_rate: false,
        }
    }

    pub const fn with_min_sample_bytes(mut self, min_sample_bytes: Option<u64>) -> Self {
        self.min_sample_bytes = min_sample_bytes;
        self
    }

    pub const fn with_pacing_rate_visibility(mut self, include_pacing_rate: bool) -> Self {
        self.include_pacing_rate = include_pacing_rate;
        self
    }

    pub const fn with_throughput_floor_from_pacing_rate(mut self, enabled: bool) -> Self {
        self.floor_throughput_with_pacing_rate = enabled;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TransportObservationReport {
    pub observation: Observation,
    pub raw_throughput_sample_bps: Option<f64>,
    pub effective_throughput_sample_bps: Option<f64>,
    pub sample_suppressed_by_min_bytes: bool,
    pub pacing_rate_floor_applied: bool,
}

#[derive(Debug, Clone)]
pub struct TransportObservationAdapter {
    started_at: Instant,
    last_snapshot: Option<(Instant, TransportMetricsSnapshot)>,
}

impl TransportObservationAdapter {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            last_snapshot: None,
        }
    }

    pub fn reset(&mut self) {
        self.started_at = Instant::now();
        self.last_snapshot = None;
    }

    pub fn observe_snapshot(
        &mut self,
        snapshot: TransportMetricsSnapshot,
        observed_at: Instant,
        throughput_direction: TransportCounterDirection,
    ) -> Observation {
        self.observe_snapshot_with_policy(
            snapshot,
            observed_at,
            TransportObservationPolicy::new(throughput_direction),
        )
        .observation
    }

    pub fn observe_snapshot_with_policy(
        &mut self,
        snapshot: TransportMetricsSnapshot,
        observed_at: Instant,
        policy: TransportObservationPolicy,
    ) -> TransportObservationReport {
        let (
            raw_throughput_sample_bps,
            lost_packets_delta,
            lost_bytes_delta,
            sample_suppressed_by_min_bytes,
        ) = if let Some((last_observed_at, last_snapshot)) = self.last_snapshot {
            let elapsed = observed_at.saturating_duration_since(last_observed_at);
            let elapsed_s = elapsed.as_secs_f64();
            let delta_bytes = transport_bytes(snapshot, policy.throughput_direction)
                .saturating_sub(transport_bytes(last_snapshot, policy.throughput_direction));
            let throughput_sample_bps = if policy
                .min_sample_bytes
                .is_some_and(|min_sample_bytes| delta_bytes < min_sample_bytes)
            {
                None
            } else if elapsed_s > 0.0 {
                Some((delta_bytes as f64 * 8.0) / elapsed_s)
            } else {
                None
            };

            (
                throughput_sample_bps,
                Some(
                    snapshot
                        .lost_packets
                        .saturating_sub(last_snapshot.lost_packets),
                ),
                Some(snapshot.lost_bytes.saturating_sub(last_snapshot.lost_bytes)),
                policy
                    .min_sample_bytes
                    .is_some_and(|min_sample_bytes| delta_bytes < min_sample_bytes),
            )
        } else {
            (None, None, None, false)
        };

        self.last_snapshot = Some((observed_at, snapshot));

        let pacing_rate_bps = snapshot
            .pacing_rate_bps
            .map(|value| value as f64)
            .filter(|value| value.is_finite() && *value >= 0.0);
        let (effective_throughput_sample_bps, pacing_rate_floor_applied) =
            match (raw_throughput_sample_bps, pacing_rate_bps) {
                (Some(throughput_sample_bps), Some(pacing_rate_bps))
                    if policy.floor_throughput_with_pacing_rate =>
                {
                    (
                        Some(throughput_sample_bps.max(pacing_rate_bps)),
                        pacing_rate_bps > throughput_sample_bps,
                    )
                }
                (None, Some(pacing_rate_bps)) if policy.floor_throughput_with_pacing_rate => {
                    (Some(pacing_rate_bps), true)
                }
                _ => (raw_throughput_sample_bps, false),
            };

        TransportObservationReport {
            observation: Observation {
                throughput_sample_bps: effective_throughput_sample_bps,
                decision_time_ms: Some(elapsed_ms_u64(
                    observed_at.saturating_duration_since(self.started_at),
                )),
                estimated_rtt_s: Some(snapshot.rtt.as_secs_f64()),
                pacing_rate_bps: if policy.include_pacing_rate {
                    pacing_rate_bps
                } else {
                    None
                },
                congestion_window_bytes: Some(snapshot.cwnd_bytes),
                lost_packets_delta,
                lost_bytes_delta,
                ..Default::default()
            },
            raw_throughput_sample_bps,
            effective_throughput_sample_bps,
            sample_suppressed_by_min_bytes,
            pacing_rate_floor_applied,
        }
    }
}

fn transport_bytes(
    snapshot: TransportMetricsSnapshot,
    direction: TransportCounterDirection,
) -> u64 {
    match direction {
        TransportCounterDirection::Rx => snapshot.rx_bytes,
        TransportCounterDirection::Tx => snapshot.tx_bytes,
    }
}

fn elapsed_ms_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(
        rx_bytes: u64,
        tx_bytes: u64,
        lost_packets: u64,
        lost_bytes: u64,
        pacing_rate_bps: u64,
    ) -> TransportMetricsSnapshot {
        TransportMetricsSnapshot {
            tx_bytes,
            rx_bytes,
            rtt: Duration::from_millis(8),
            cwnd_bytes: 64_000,
            lost_packets,
            lost_bytes,
            pacing_rate_bps: Some(pacing_rate_bps),
        }
    }

    #[test]
    fn first_transport_sample_has_no_throughput_or_loss_delta() {
        let mut adapter = TransportObservationAdapter::new();

        let observation = adapter.observe_snapshot(
            snapshot(1_000, 10, 4, 100, 900_000),
            Instant::now(),
            TransportCounterDirection::Rx,
        );

        assert_eq!(observation.throughput_sample_bps, None);
        assert_eq!(observation.lost_packets_delta, None);
        assert_eq!(observation.lost_bytes_delta, None);
        assert_eq!(observation.pacing_rate_bps, Some(900_000.0));
        assert_eq!(observation.congestion_window_bytes, Some(64_000));
    }

    #[test]
    fn second_transport_sample_tracks_directional_throughput_and_loss_delta() {
        let mut adapter = TransportObservationAdapter::new();
        let start = Instant::now();
        adapter.observe_snapshot(
            snapshot(1_000, 10, 4, 100, 900_000),
            start,
            TransportCounterDirection::Rx,
        );

        let observation = adapter.observe_snapshot(
            snapshot(2_000, 20, 7, 180, 1_500_000),
            start + Duration::from_secs(1),
            TransportCounterDirection::Rx,
        );

        assert_eq!(observation.throughput_sample_bps, Some(8_000.0));
        assert_eq!(observation.lost_packets_delta, Some(3));
        assert_eq!(observation.lost_bytes_delta, Some(80));
    }

    #[test]
    fn tx_direction_uses_tx_byte_counter() {
        let mut adapter = TransportObservationAdapter::new();
        let start = Instant::now();
        adapter.observe_snapshot(
            snapshot(1_000, 10, 0, 0, 900_000),
            start,
            TransportCounterDirection::Tx,
        );

        let observation = adapter.observe_snapshot(
            snapshot(2_000, 1_010, 0, 0, 1_500_000),
            start + Duration::from_secs(1),
            TransportCounterDirection::Tx,
        );

        assert_eq!(observation.throughput_sample_bps, Some(8_000.0));
    }

    #[test]
    fn transport_policy_can_suppress_small_tx_sample_and_floor_from_pacing() {
        let mut adapter = TransportObservationAdapter::new();
        let start = Instant::now();
        let policy = TransportObservationPolicy::new(TransportCounterDirection::Tx)
            .with_min_sample_bytes(Some(64 * 1024))
            .with_throughput_floor_from_pacing_rate(true);

        adapter.observe_snapshot_with_policy(
            snapshot(0, 1_000_000, 0, 0, 40_000_000),
            start,
            policy,
        );

        let report = adapter.observe_snapshot_with_policy(
            snapshot(0, 1_000_100, 0, 0, 40_000_000),
            start + Duration::from_millis(250),
            policy,
        );

        assert_eq!(report.raw_throughput_sample_bps, None);
        assert_eq!(report.effective_throughput_sample_bps, Some(40_000_000.0));
        assert!(report.sample_suppressed_by_min_bytes);
        assert!(report.pacing_rate_floor_applied);
    }

    #[test]
    fn transport_policy_can_hide_pacing_rate_from_observation() {
        let mut adapter = TransportObservationAdapter::new();

        let report = adapter.observe_snapshot_with_policy(
            snapshot(1_000, 10, 4, 100, 900_000),
            Instant::now(),
            TransportObservationPolicy::new(TransportCounterDirection::Rx)
                .with_pacing_rate_visibility(false),
        );

        assert_eq!(report.observation.pacing_rate_bps, None);
    }
}
