use crate::config::{AbrConfig, AbrMode};
use crate::constraints::HardConstraints;
use crate::diagnostics::DecisionDiagnostics;
use crate::estimators::ThroughputEstimator;
use crate::model::{Decision, Observation, QualityLadder, RankedQuality};

#[derive(Debug, Clone)]
pub struct Abr {
    config: AbrConfig,
    constraints: HardConstraints,
    quality_ladder: QualityLadder,
    bandwidth_estimator: ThroughputEstimator,
    bandwidth_floor_bps: Option<f64>,
    allowed_qualities: Vec<RankedQuality>,
    last_observation: Observation,
    last_diagnostics: DecisionDiagnostics,
    ranking_dirty: bool,
}

impl Abr {
    pub fn new(config: AbrConfig, quality_ladder: QualityLadder) -> Self {
        let initial_estimate = config.default_bandwidth_bps;
        let initial_budget =
            initial_estimate * (1.0 - config.bandwidth_overhead_fraction.clamp(0.0, 0.95));
        Self {
            constraints: HardConstraints::default(),
            bandwidth_estimator: ThroughputEstimator::new(
                config.bandwidth_alpha,
                config.default_bandwidth_bps,
            ),
            bandwidth_floor_bps: None,
            config,
            quality_ladder,
            allowed_qualities: Vec::new(),
            last_observation: Observation::default(),
            last_diagnostics: DecisionDiagnostics::empty(
                config.mode,
                initial_estimate,
                initial_budget,
            ),
            ranking_dirty: true,
        }
    }

    pub fn update_config(&mut self, config: AbrConfig) {
        self.bandwidth_estimator.set_alpha(config.bandwidth_alpha);
        self.bandwidth_estimator
            .set_default_bandwidth_bps(config.default_bandwidth_bps);
        self.config = config;
        self.ranking_dirty = true;
    }

    pub fn update_quality_ladder(&mut self, quality_ladder: QualityLadder) {
        self.quality_ladder = quality_ladder;
        self.ranking_dirty = true;
    }

    pub fn update_constraints(&mut self, constraints: HardConstraints) {
        self.constraints = constraints;
        self.ranking_dirty = true;
    }

    pub fn observe(&mut self, observation: Observation) {
        self.observe_with_bandwidth_floor_bps(observation, None);
    }

    pub fn observe_with_bandwidth_floor_bps(
        &mut self,
        observation: Observation,
        bandwidth_floor_bps: Option<f64>,
    ) {
        let bandwidth_floor_bps =
            bandwidth_floor_bps.filter(|value| value.is_finite() && *value > 0.0);
        if self.bandwidth_floor_bps != bandwidth_floor_bps {
            self.bandwidth_floor_bps = bandwidth_floor_bps;
            self.ranking_dirty = true;
        }

        if self.last_observation != observation {
            self.last_observation = observation;
            self.ranking_dirty = true;
        }

        if let Some(sample_bps) = observation.throughput_sample_bps {
            if self
                .bandwidth_estimator
                .observe_sample_bps(sample_bps)
                .is_some()
            {
                self.ranking_dirty = true;
            }
        }
    }

    pub fn get_last_observation(&self) -> &Observation {
        &self.last_observation
    }

    pub fn observe_estimated_bandwidth_bps(&mut self, estimated_bandwidth_bps: f64) {
        if self.bandwidth_floor_bps.take().is_some() {
            self.ranking_dirty = true;
        }

        if self
            .bandwidth_estimator
            .observe_sample_bps(estimated_bandwidth_bps)
            .is_some()
        {
            self.ranking_dirty = true;
        }
    }

    pub fn observe_throughput_bytes(&mut self, bytes: usize, duration_s: f64) {
        self.observe(Observation::from_bytes_and_duration(bytes, duration_s));
    }

    pub fn estimated_bandwidth_bps(&self) -> f64 {
        self.bandwidth_estimator.estimate_bps()
    }

    pub fn allowed_qualities(&mut self) -> &[RankedQuality] {
        self.refresh_ranking();
        &self.allowed_qualities
    }

    pub fn recommend_quality(&mut self) -> Option<RankedQuality> {
        self.refresh_ranking();
        self.allowed_qualities.first().copied()
    }

    pub fn decide(&mut self) -> Decision {
        self.refresh_ranking();
        Decision {
            allowed_qualities: self.allowed_qualities.clone(),
            recommended_quality: self.allowed_qualities.first().copied(),
            estimated_bandwidth_bps: self.last_diagnostics.estimated_bandwidth_bps,
            diagnostics: self.last_diagnostics.clone(),
        }
    }

    pub fn last_diagnostics(&mut self) -> &DecisionDiagnostics {
        self.refresh_ranking();
        &self.last_diagnostics
    }

    fn refresh_ranking(&mut self) {
        if !self.ranking_dirty {
            return;
        }

        self.allowed_qualities.clear();
        let estimated_bandwidth_bps = self
            .bandwidth_floor_bps
            .map(|bandwidth_floor_bps| {
                self.bandwidth_estimator
                    .estimate_bps()
                    .max(bandwidth_floor_bps)
            })
            .unwrap_or_else(|| self.bandwidth_estimator.estimate_bps());
        let raw_budget_bps = estimated_bandwidth_bps
            * (1.0 - self.config.bandwidth_overhead_fraction.clamp(0.0, 0.95));
        let latency_risk_fraction = self.latency_risk_fraction();
        let mut budget_bps = raw_budget_bps * (1.0 - latency_risk_fraction);
        if !matches!(self.config.mode, AbrMode::Simple) {
            if let Some(pacing_rate_bps) = self
                .last_observation
                .pacing_rate_bps
                .filter(|value| value.is_finite() && *value > 0.0)
            {
                budget_bps = budget_bps.min(pacing_rate_bps);
            }
        }
        let mut filtered_by_constraints = 0;
        let mut filtered_by_bandwidth = 0;
        let mut fallback_to_lowest = false;

        let mut constraint_allowed = Vec::new();

        for quality in self.quality_ladder.as_slice() {
            if !quality.enabled {
                filtered_by_constraints += 1;
                continue;
            }

            if !self.constraints.allows(quality) {
                filtered_by_constraints += 1;
                continue;
            }

            let ranked = RankedQuality {
                id: quality.id,
                nominal_bitrate_bps: quality.nominal_bitrate_bps,
                score: quality.nominal_bitrate_bps as f64,
            };
            constraint_allowed.push(ranked);

            if quality.nominal_bitrate_bps as f64 <= budget_bps {
                self.allowed_qualities.push(ranked);
            } else {
                filtered_by_bandwidth += 1;
            }
        }

        constraint_allowed.sort_by(|left, right| {
            right
                .nominal_bitrate_bps
                .cmp(&left.nominal_bitrate_bps)
                .then_with(|| left.id.as_u32().cmp(&right.id.as_u32()))
        });

        self.allowed_qualities.sort_by(|left, right| {
            right
                .nominal_bitrate_bps
                .cmp(&left.nominal_bitrate_bps)
                .then_with(|| left.id.as_u32().cmp(&right.id.as_u32()))
        });

        if self.allowed_qualities.is_empty() {
            if let Some(lowest_quality) = constraint_allowed
                .iter()
                .min_by_key(|quality| quality.nominal_bitrate_bps)
            {
                fallback_to_lowest = true;
                self.allowed_qualities.push(*lowest_quality);
            }
        }

        self.last_diagnostics = DecisionDiagnostics {
            mode: self.config.mode,
            estimated_bandwidth_bps,
            bandwidth_budget_bps: raw_budget_bps,
            risk_adjusted_bandwidth_budget_bps: budget_bps,
            latency_risk_fraction,
            filtered_by_constraints,
            filtered_by_bandwidth,
            fallback_to_lowest,
            held_by_upswitch_hysteresis: false,
            held_by_downswitch_hysteresis: false,
            blocked_by_hold_down: false,
            blocked_by_buffer_guard: false,
        };

        self.ranking_dirty = false;
    }

    fn latency_risk_fraction(&self) -> f64 {
        let max_risk = match self.config.mode {
            AbrMode::Simple => return 0.0,
            AbrMode::Balanced => 0.20,
            AbrMode::Advanced => 0.35,
        };

        let mut risk = 0.0;

        if let Some(estimated_rtt_s) = self.last_observation.estimated_rtt_s {
            risk += ((estimated_rtt_s - 0.040) / 0.160).clamp(0.0, 1.0) * 0.08;
        }

        if let Some(time_to_first_byte_s) = self.last_observation.time_to_first_byte_s {
            risk += ((time_to_first_byte_s - 0.100) / 0.400).clamp(0.0, 1.0) * 0.10;
        }

        if let (Some(completion_time_s), Some(segment_duration_s)) = (
            self.last_observation.completion_time_s,
            self.last_observation.segment_duration_s,
        ) {
            let completion_ratio = completion_time_s / segment_duration_s.max(0.001);
            risk += ((completion_ratio - 0.35) / 0.65).clamp(0.0, 1.0) * 0.17;
        } else if let Some(completion_time_s) = self.last_observation.completion_time_s {
            risk += ((completion_time_s - 0.250) / 0.500).clamp(0.0, 1.0) * 0.12;
        }

        if let Some(lost_packets_delta) = self.last_observation.lost_packets_delta {
            risk += ((lost_packets_delta as f64) / 8.0).clamp(0.0, 1.0) * 0.05;
        }

        if let Some(lost_bytes_delta) = self.last_observation.lost_bytes_delta {
            risk += ((lost_bytes_delta as f64) / 64_000.0).clamp(0.0, 1.0) * 0.08;
        }

        risk.clamp(0.0, max_risk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AbrConfig, AbrMode, QualityDescriptor, QualityId};

    fn ladder() -> QualityLadder {
        QualityLadder::new(vec![
            QualityDescriptor::new(QualityId::new(1), 26_000_000),
            QualityDescriptor::new(QualityId::new(2), 58_000_000),
        ])
    }

    #[test]
    fn zero_pacing_rate_is_treated_as_unknown() {
        let mut config = AbrConfig::for_mode(AbrMode::Advanced);
        config.bandwidth_alpha = 1.0;

        let mut abr = Abr::new(config, ladder());
        abr.observe(Observation {
            throughput_sample_bps: Some(90_000_000.0),
            pacing_rate_bps: Some(0.0),
            ..Default::default()
        });

        let decision = abr.decide();
        assert_eq!(
            decision
                .recommended_quality
                .map(|quality| quality.id.as_u32()),
            Some(2)
        );
    }
}
