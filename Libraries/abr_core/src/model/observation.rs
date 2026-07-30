#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Observation {
    pub throughput_sample_bps: Option<f64>,
    pub playback_buffer_s: Option<f64>,
    pub decision_time_ms: Option<u64>,
    pub time_to_first_byte_s: Option<f64>,
    pub estimated_rtt_s: Option<f64>,
    pub completion_time_s: Option<f64>,
    pub segment_duration_s: Option<f64>,
    pub pacing_rate_bps: Option<f64>,
    pub congestion_window_bytes: Option<u64>,
    pub lost_packets_delta: Option<u64>,
    pub lost_bytes_delta: Option<u64>,
}

impl Observation {
    pub fn from_bytes_and_duration(bytes: usize, duration_s: f64) -> Self {
        if duration_s <= 0.0 {
            return Self::default();
        }

        Self {
            throughput_sample_bps: Some((bytes as f64 * 8.0) / duration_s),
            playback_buffer_s: None,
            decision_time_ms: None,
            time_to_first_byte_s: None,
            estimated_rtt_s: None,
            completion_time_s: None,
            segment_duration_s: None,
            pacing_rate_bps: None,
            congestion_window_bytes: None,
            lost_packets_delta: None,
            lost_bytes_delta: None,
        }
    }
}
