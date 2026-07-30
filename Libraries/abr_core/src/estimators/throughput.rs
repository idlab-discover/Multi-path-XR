use std::time::Duration;

use crate::estimators::Ewma;

#[derive(Debug, Clone)]
pub struct ThroughputEstimator {
    ewma_bps: Ewma,
    default_bandwidth_bps: f64,
}

impl ThroughputEstimator {
    pub fn new(alpha: f64, default_bandwidth_bps: f64) -> Self {
        Self {
            ewma_bps: Ewma::new(alpha),
            default_bandwidth_bps,
        }
    }

    pub fn set_alpha(&mut self, alpha: f64) {
        self.ewma_bps.set_alpha(alpha);
    }

    pub fn set_default_bandwidth_bps(&mut self, default_bandwidth_bps: f64) {
        self.default_bandwidth_bps = default_bandwidth_bps;
    }

    pub fn reset(&mut self) {
        self.ewma_bps.reset();
    }

    pub fn observe_sample_bps(&mut self, sample_bps: f64) -> Option<u64> {
        if sample_bps < 0.0 || !sample_bps.is_finite() {
            return None;
        }

        self.ewma_bps
            .update(sample_bps)
            .map(|value| value.round().max(0.0) as u64)
    }

    pub fn observe_bytes(&mut self, bytes: u64, duration: Duration) -> Option<u64> {
        let duration_s = duration.as_secs_f64();
        if duration_s <= 0.0 {
            return None;
        }

        let sample_bps = (bytes as f64 * 8.0) / duration_s;
        self.observe_sample_bps(sample_bps)
    }

    pub fn estimate_bps(&self) -> f64 {
        self.ewma_bps.value_or(self.default_bandwidth_bps)
    }
}
