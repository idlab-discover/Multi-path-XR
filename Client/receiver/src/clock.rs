use metrics::{get_metrics, Metrics as AppMetrics};
use prometheus::IntGauge;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Keep offsets usable across the 30 s WebSocket refresh cadence, with room for
// scheduler jitter and network variance before falling back to uncorrected time.
const CLOCK_OFFSET_STALE_AFTER: Duration = Duration::from_secs(45);
const CLOCK_SAMPLE_MAX_RTT_US: u64 = 5_000_000;
const LOW_TRUST_MIN_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClockDomain {
    Dash,
    Flute,
    Moq,
    WebRtc,
    WebSocket,
    Unknown,
}

impl ClockDomain {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dash => "dash",
            Self::Flute => "flute",
            Self::Moq => "moq",
            Self::WebRtc => "webrtc",
            Self::WebSocket => "websocket",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ClockSampleTrust {
    LowOneWay,
    MediumTransportRtt,
    HighRtt,
}

impl ClockSampleTrust {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LowOneWay => "low_one_way",
            Self::MediumTransportRtt => "medium_transport_rtt",
            Self::HighRtt => "high_rtt",
        }
    }

    const fn weight(self) -> f64 {
        match self {
            Self::LowOneWay => 0.02,
            Self::MediumTransportRtt => 0.10,
            Self::HighRtt => 0.25,
        }
    }

    const fn metric_value(self) -> i64 {
        match self {
            Self::LowOneWay => 1,
            Self::MediumTransportRtt => 2,
            Self::HighRtt => 3,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClockSourceKey {
    pub server_instance_id: Option<String>,
    pub publisher_id: Option<String>,
    pub fallback_transport: ClockDomain,
}

impl ClockSourceKey {
    pub fn new(
        server_instance_id: Option<String>,
        publisher_id: Option<String>,
        fallback_transport: ClockDomain,
    ) -> Self {
        Self {
            server_instance_id: normalize_optional_id(server_instance_id),
            publisher_id: normalize_optional_id(publisher_id),
            fallback_transport,
        }
    }

    pub fn for_transport(fallback_transport: ClockDomain) -> Self {
        Self::new(None, None, fallback_transport)
    }

    pub fn with_server_id(
        fallback_transport: ClockDomain,
        server_instance_id: impl Into<String>,
    ) -> Self {
        Self::new(Some(server_instance_id.into()), None, fallback_transport)
    }

    pub fn clock_source_label(&self) -> String {
        match (&self.server_instance_id, &self.publisher_id) {
            (Some(server), Some(publisher)) => format!("{server}/{publisher}"),
            (Some(server), None) => server.clone(),
            (None, Some(publisher)) => {
                format!("{}:{publisher}", self.fallback_transport.as_str())
            }
            (None, None) => format!("transport:{}", self.fallback_transport.as_str()),
        }
    }

    fn state_key(&self) -> Self {
        if self.server_instance_id.is_some() {
            Self {
                server_instance_id: self.server_instance_id.clone(),
                publisher_id: self.publisher_id.clone(),
                fallback_transport: ClockDomain::Unknown,
            }
        } else {
            self.clone()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockOffsetSample {
    pub remote_now_us: u64,
    pub local_send_us: u64,
    pub local_receive_us: u64,
    pub server_wait_us: Option<u64>,
}

impl ClockOffsetSample {
    pub fn offset_us(self) -> Option<i64> {
        let total_rtt_us = self.local_receive_us.checked_sub(self.local_send_us)?;
        let network_rtt_us = total_rtt_us
            .saturating_sub(self.server_wait_us.unwrap_or(0))
            .min(CLOCK_SAMPLE_MAX_RTT_US);
        let remote_adjusted_us = i128::from(self.remote_now_us) + i128::from(network_rtt_us / 2);
        let offset_us = remote_adjusted_us - i128::from(self.local_receive_us);
        Some(offset_us.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimestampCorrection {
    pub send_time_us: u64,
    pub presentation_time_us: u64,
    pub offset_us: Option<i64>,
}

impl TimestampCorrection {
    pub fn applied(self) -> bool {
        self.offset_us.is_some()
    }
}

#[derive(Clone, Copy, Debug)]
struct ClockOffsetState {
    offset_us: f64,
    observed_at: Instant,
    trust: ClockSampleTrust,
    last_low_trust_at: Option<Instant>,
}

#[derive(Clone)]
struct ClockOffsetSampleMetrics {
    offset_us: IntGauge,
    sample_trust: IntGauge,
}

#[derive(Clone)]
struct ClockOffsetCorrectionMetrics {
    offset_age_us: IntGauge,
    correction_applied: IntGauge,
}

impl ClockOffsetSampleMetrics {
    fn new(
        metrics: &AppMetrics,
        transport: ClockDomain,
        clock_source: &str,
        trust: ClockSampleTrust,
    ) -> Self {
        Self {
            offset_us: metrics
                .get_or_create_labelled_gauge(
                    "receiver_clock_offset_us",
                    "Estimated publisher-minus-receiver wall-clock offset in microseconds",
                    &["transport", "clock_source", "trust"],
                    &[transport.as_str(), clock_source, trust.as_str()],
                )
                .expect("receiver_clock_offset_us"),
            sample_trust: metrics
                .get_or_create_labelled_gauge(
                    "receiver_clock_sample_trust",
                    "Trust class for the latest receiver clock-offset sample",
                    &["transport", "clock_source"],
                    &[transport.as_str(), clock_source],
                )
                .expect("receiver_clock_sample_trust"),
        }
    }
}

impl ClockOffsetCorrectionMetrics {
    fn new(metrics: &AppMetrics, transport: ClockDomain, clock_source: &str) -> Self {
        Self {
            offset_age_us: metrics
                .get_or_create_labelled_gauge(
                    "receiver_clock_offset_age_us",
                    "Age of the latest usable receiver clock-offset sample in microseconds",
                    &["transport", "clock_source"],
                    &[transport.as_str(), clock_source],
                )
                .expect("receiver_clock_offset_age_us"),
            correction_applied: metrics
                .get_or_create_labelled_gauge(
                    "receiver_timestamp_correction_applied",
                    "Whether receiver timestamp correction was applied to the latest frame for this source",
                    &["transport", "clock_source"],
                    &[transport.as_str(), clock_source],
                )
                .expect("receiver_timestamp_correction_applied"),
        }
    }
}

type SampleMetricKey = (ClockDomain, String, ClockSampleTrust);
type CorrectionMetricKey = (ClockDomain, String);

pub struct ClockOffsetEstimator {
    states: Mutex<HashMap<ClockSourceKey, ClockOffsetState>>,
    metrics: Option<AppMetrics>,
    sample_metric_handles: Mutex<HashMap<SampleMetricKey, ClockOffsetSampleMetrics>>,
    correction_metric_handles: Mutex<HashMap<CorrectionMetricKey, ClockOffsetCorrectionMetrics>>,
}

impl ClockOffsetEstimator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            states: Mutex::new(HashMap::new()),
            metrics: Some(get_metrics()),
            sample_metric_handles: Mutex::new(HashMap::new()),
            correction_metric_handles: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    fn new_without_metrics() -> Self {
        Self {
            states: Mutex::new(HashMap::new()),
            metrics: None,
            sample_metric_handles: Mutex::new(HashMap::new()),
            correction_metric_handles: Mutex::new(HashMap::new()),
        }
    }

    pub fn observe_sample(
        &self,
        source: ClockSourceKey,
        trust: ClockSampleTrust,
        sample: ClockOffsetSample,
    ) -> Option<i64> {
        let offset_us = sample.offset_us()?;
        self.observe_offset_us(source, trust, offset_us)
    }

    pub fn observe_offset_us(
        &self,
        source: ClockSourceKey,
        trust: ClockSampleTrust,
        offset_us: i64,
    ) -> Option<i64> {
        self.observe_offset_us_at(source, trust, offset_us, Instant::now())
    }

    fn observe_offset_us_at(
        &self,
        source: ClockSourceKey,
        trust: ClockSampleTrust,
        offset_us: i64,
        now: Instant,
    ) -> Option<i64> {
        let state_key = source.state_key();
        let mut states = self.states.lock().unwrap();
        let state = states.entry(state_key).or_insert(ClockOffsetState {
            offset_us: offset_us as f64,
            observed_at: now,
            trust,
            last_low_trust_at: None,
        });

        if trust == ClockSampleTrust::LowOneWay {
            if state
                .last_low_trust_at
                .is_some_and(|last| now.saturating_duration_since(last) < LOW_TRUST_MIN_INTERVAL)
            {
                return None;
            }
            state.last_low_trust_at = Some(now);
        }

        let weight = if trust > state.trust {
            1.0
        } else {
            trust.weight()
        };
        state.offset_us = weight * offset_us as f64 + (1.0 - weight) * state.offset_us;
        state.observed_at = now;
        if trust > state.trust {
            state.trust = trust;
        }
        let smoothed_offset = round_i64_saturating(state.offset_us);
        drop(states);

        if let Some(metrics) = self.sample_metrics_for(&source, trust) {
            metrics.offset_us.set(smoothed_offset);
            metrics.sample_trust.set(trust.metric_value());
        }

        Some(smoothed_offset)
    }

    pub fn correct_frame_timestamps(
        &self,
        source: ClockSourceKey,
        send_time_us: u64,
        presentation_time_us: u64,
    ) -> TimestampCorrection {
        let offset_us = self.usable_offset_us(&source);
        let correction = if let Some(offset_us) = offset_us {
            TimestampCorrection {
                send_time_us: apply_publisher_minus_receiver_offset(send_time_us, offset_us),
                presentation_time_us: apply_publisher_minus_receiver_offset(
                    presentation_time_us,
                    offset_us,
                ),
                offset_us: Some(offset_us),
            }
        } else {
            TimestampCorrection {
                send_time_us,
                presentation_time_us,
                offset_us: None,
            }
        };

        if let Some(metrics) = self.correction_metrics_for(&source) {
            metrics
                .correction_applied
                .set(if correction.applied() { 1 } else { 0 });
        }

        correction
    }

    pub fn usable_offset_us(&self, source: &ClockSourceKey) -> Option<i64> {
        self.usable_offset_us_at(source, Instant::now())
    }

    fn usable_offset_us_at(&self, source: &ClockSourceKey, now: Instant) -> Option<i64> {
        let state = {
            let states = self.states.lock().unwrap();
            states.get(&source.state_key()).copied()
        }?;

        let age = now.saturating_duration_since(state.observed_at);
        if let Some(metrics) = self.correction_metrics_for(source) {
            metrics.offset_age_us.set(duration_us_i64(age));
        }

        if age > CLOCK_OFFSET_STALE_AFTER {
            return None;
        }

        Some(round_i64_saturating(state.offset_us))
    }

    fn sample_metrics_for(
        &self,
        source: &ClockSourceKey,
        trust: ClockSampleTrust,
    ) -> Option<ClockOffsetSampleMetrics> {
        let metrics = self.metrics.as_ref()?;
        let clock_source = source.clock_source_label();
        let key = (source.fallback_transport, clock_source, trust);
        let mut handles = self.sample_metric_handles.lock().unwrap();
        Some(
            handles
                .entry(key.clone())
                .or_insert_with(|| {
                    ClockOffsetSampleMetrics::new(metrics, key.0, key.1.as_str(), key.2)
                })
                .clone(),
        )
    }

    fn correction_metrics_for(
        &self,
        source: &ClockSourceKey,
    ) -> Option<ClockOffsetCorrectionMetrics> {
        let metrics = self.metrics.as_ref()?;
        let clock_source = source.clock_source_label();
        let key = (source.fallback_transport, clock_source);
        let mut handles = self.correction_metric_handles.lock().unwrap();
        Some(
            handles
                .entry(key.clone())
                .or_insert_with(|| {
                    ClockOffsetCorrectionMetrics::new(metrics, key.0, key.1.as_str())
                })
                .clone(),
        )
    }
}

fn normalize_optional_id(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn apply_publisher_minus_receiver_offset(timestamp_us: u64, offset_us: i64) -> u64 {
    let corrected = i128::from(timestamp_us) - i128::from(offset_us);
    corrected.clamp(0, i128::from(u64::MAX)) as u64
}

fn round_i64_saturating(value: f64) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64
}

fn duration_us_i64(duration: Duration) -> i64 {
    duration.as_micros().min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_offset_sample_accounts_for_network_rtt_and_server_wait() {
        let sample = ClockOffsetSample {
            remote_now_us: 1_050_000,
            local_send_us: 999_000,
            local_receive_us: 1_001_000,
            server_wait_us: Some(1_000),
        };

        assert_eq!(sample.offset_us(), Some(49_500));
    }

    #[test]
    fn timestamp_correction_subtracts_positive_offset() {
        let estimator = ClockOffsetEstimator::new_without_metrics();
        let source = ClockSourceKey::for_transport(ClockDomain::Moq);
        estimator.observe_offset_us(source.clone(), ClockSampleTrust::MediumTransportRtt, 50_000);

        let correction = estimator.correct_frame_timestamps(source, 1_000_000, 1_100_000);

        assert_eq!(correction.send_time_us, 950_000);
        assert_eq!(correction.presentation_time_us, 1_050_000);
        assert_eq!(correction.offset_us, Some(50_000));
    }

    #[test]
    fn timestamp_correction_adds_when_offset_is_negative() {
        let estimator = ClockOffsetEstimator::new_without_metrics();
        let source = ClockSourceKey::for_transport(ClockDomain::Flute);
        estimator.observe_offset_us(source.clone(), ClockSampleTrust::LowOneWay, -25_000);

        let correction = estimator.correct_frame_timestamps(source, 1_000_000, 1_100_000);

        assert_eq!(correction.send_time_us, 1_025_000);
        assert_eq!(correction.presentation_time_us, 1_125_000);
        assert_eq!(correction.offset_us, Some(-25_000));
    }

    #[test]
    fn missing_offset_preserves_timestamps() {
        let estimator = ClockOffsetEstimator::new_without_metrics();

        let correction = estimator.correct_frame_timestamps(
            ClockSourceKey::for_transport(ClockDomain::WebSocket),
            1_000_000,
            1_100_000,
        );

        assert_eq!(correction.send_time_us, 1_000_000);
        assert_eq!(correction.presentation_time_us, 1_100_000);
        assert_eq!(correction.offset_us, None);
    }

    #[test]
    fn same_server_id_reuses_offset_across_transports() {
        let estimator = ClockOffsetEstimator::new_without_metrics();
        estimator.observe_offset_us(
            ClockSourceKey::with_server_id(ClockDomain::Moq, "server-a"),
            ClockSampleTrust::MediumTransportRtt,
            40_000,
        );

        let correction = estimator.correct_frame_timestamps(
            ClockSourceKey::with_server_id(ClockDomain::Flute, "server-a"),
            1_000_000,
            1_100_000,
        );

        assert_eq!(correction.offset_us, Some(40_000));
        assert_eq!(correction.send_time_us, 960_000);
    }

    #[test]
    fn different_server_ids_do_not_reuse_offsets() {
        let estimator = ClockOffsetEstimator::new_without_metrics();
        estimator.observe_offset_us(
            ClockSourceKey::with_server_id(ClockDomain::Moq, "server-a"),
            ClockSampleTrust::MediumTransportRtt,
            40_000,
        );

        let correction = estimator.correct_frame_timestamps(
            ClockSourceKey::with_server_id(ClockDomain::Flute, "server-b"),
            1_000_000,
            1_100_000,
        );

        assert_eq!(correction.offset_us, None);
        assert_eq!(correction.send_time_us, 1_000_000);
    }

    #[test]
    fn high_trust_sample_overrides_low_trust_state() {
        let estimator = ClockOffsetEstimator::new_without_metrics();
        let source = ClockSourceKey::with_server_id(ClockDomain::Flute, "server-a");
        estimator.observe_offset_us(source.clone(), ClockSampleTrust::LowOneWay, 100_000);
        estimator.observe_offset_us(source.clone(), ClockSampleTrust::HighRtt, 10_000);

        let correction = estimator.correct_frame_timestamps(source, 1_000_000, 1_100_000);

        assert_eq!(correction.offset_us, Some(10_000));
    }

    #[test]
    fn low_trust_samples_are_rate_limited() {
        let estimator = ClockOffsetEstimator::new_without_metrics();
        let source = ClockSourceKey::with_server_id(ClockDomain::Flute, "server-a");
        let now = Instant::now();

        assert_eq!(
            estimator.observe_offset_us_at(
                source.clone(),
                ClockSampleTrust::LowOneWay,
                100_000,
                now,
            ),
            Some(100_000)
        );
        assert_eq!(
            estimator.observe_offset_us_at(
                source.clone(),
                ClockSampleTrust::LowOneWay,
                200_000,
                now + Duration::from_secs(1),
            ),
            None
        );

        let correction = estimator.correct_frame_timestamps(source, 1_000_000, 1_100_000);
        assert_eq!(correction.offset_us, Some(100_000));
    }

    #[test]
    fn high_trust_offsets_remain_usable_across_refresh_interval() {
        let estimator = ClockOffsetEstimator::new_without_metrics();
        let source = ClockSourceKey::with_server_id(ClockDomain::WebSocket, "server-a");
        let now = Instant::now();

        estimator.observe_offset_us_at(source.clone(), ClockSampleTrust::HighRtt, 12_345, now);

        assert_eq!(
            estimator.usable_offset_us_at(&source, now + Duration::from_secs(30)),
            Some(12_345)
        );
    }

    #[test]
    fn offsets_expire_after_stale_window() {
        let estimator = ClockOffsetEstimator::new_without_metrics();
        let source = ClockSourceKey::with_server_id(ClockDomain::WebSocket, "server-a");
        let now = Instant::now();

        estimator.observe_offset_us_at(source.clone(), ClockSampleTrust::HighRtt, 12_345, now);

        assert_eq!(
            estimator.usable_offset_us_at(
                &source,
                now + CLOCK_OFFSET_STALE_AFTER + Duration::from_secs(1)
            ),
            None
        );
    }
}
