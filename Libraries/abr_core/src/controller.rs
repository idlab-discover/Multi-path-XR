use crate::{
    config::AbrConfig,
    constraints::HardConstraints,
    engine::Abr,
    model::{AbrSelectionDecision, Observation, QualityDescriptor, QualityId, QualityLadder},
    selection::{QualitySelectionPolicy, QualitySelectionStabilizer, SelectionObservationContext},
};
use std::collections::BTreeMap;

/// Public end-to-end ABR pipeline.
///
/// This type composes the evaluator stage that turns observations into ranked
/// candidates and the final selection stabilizer that decides when switches are
/// actually applied.
#[derive(Debug, Clone)]
pub struct AbrController {
    config: AbrConfig,
    constraints: HardConstraints,
    selection_policy: QualitySelectionPolicy,
    quality_bitrates: BTreeMap<QualityId, u64>,
    evaluator: Abr,
    selection_stabilizer: QualitySelectionStabilizer,
    pending_observations: u8,
    last_selection_decision: Option<AbrSelectionDecision>,
}

impl AbrController {
    pub fn new(config: AbrConfig, selection_policy: QualitySelectionPolicy) -> Self {
        Self {
            config,
            constraints: HardConstraints::default(),
            selection_policy,
            quality_bitrates: BTreeMap::new(),
            evaluator: Abr::new(config, QualityLadder::default()),
            selection_stabilizer: QualitySelectionStabilizer::new(config),
            pending_observations: 0,
            last_selection_decision: None,
        }
    }

    pub fn reset(&mut self) {
        self.selection_stabilizer.reset();
        self.evaluator = Abr::new(self.config, build_quality_ladder(&self.quality_bitrates));
        self.evaluator.update_constraints(self.constraints.clone());
        self.pending_observations = 0;
        self.last_selection_decision = None;
    }

    pub fn update_config(&mut self, config: AbrConfig) {
        self.config = config;
        self.selection_stabilizer.update_config(config);
        self.evaluator.update_config(config);
        self.last_selection_decision = None;
    }

    pub fn update_constraints(&mut self, constraints: HardConstraints) {
        self.constraints = constraints.clone();
        self.evaluator.update_constraints(constraints);
        self.last_selection_decision = None;
    }

    pub fn update_quality_ladder(&mut self, quality_bitrates: &BTreeMap<QualityId, u64>) {
        self.quality_bitrates = quality_bitrates.clone();
        self.evaluator
            .update_quality_ladder(build_quality_ladder(&self.quality_bitrates));
        self.last_selection_decision = None;
    }

    pub fn update_quality_bitrate(&mut self, quality_id: QualityId, bitrate_bps: u64) {
        self.quality_bitrates.insert(quality_id, bitrate_bps);
        self.evaluator
            .update_quality_ladder(build_quality_ladder(&self.quality_bitrates));
    }

    pub fn observe(&mut self, observation: Observation) {
        self.evaluator.observe(observation);
        self.pending_observations = self.pending_observations.saturating_add(1);
    }

    pub fn observe_with_bandwidth_floor_bps(
        &mut self,
        observation: Observation,
        bandwidth_floor_bps: Option<f64>,
    ) {
        self.evaluator
            .observe_with_bandwidth_floor_bps(observation, bandwidth_floor_bps);
        self.pending_observations = self.pending_observations.saturating_add(1);
    }

    pub fn observe_estimated_bandwidth_bps(&mut self, estimated_bandwidth_bps: f64) {
        self.evaluator
            .observe_estimated_bandwidth_bps(estimated_bandwidth_bps);
        self.pending_observations = self.pending_observations.saturating_add(1);
    }

    pub fn known_quality_ids(&self) -> Vec<QualityId> {
        self.quality_bitrates.keys().copied().collect()
    }

    pub fn decide_pending(&mut self) -> AbrSelectionDecision {
        if self.pending_observations == 0 {
            if let Some(last_selection_decision) = &self.last_selection_decision {
                return last_selection_decision.clone();
            }

            return self.decide(false);
        }

        self.pending_observations = self.pending_observations.saturating_sub(1);
        self.decide(true)
    }

    pub fn decide(&mut self, has_fresh_observation: bool) -> AbrSelectionDecision {
        let decision = self.evaluator.decide();
        let observation = self.evaluator.get_last_observation();
        let selection = self.selection_stabilizer.select(
            &self.quality_bitrates,
            &decision.allowed_qualities,
            clamp_budget_bps(decision.diagnostics.risk_adjusted_bandwidth_budget_bps),
            decision.recommended_quality.map(|quality| quality.id),
            self.selection_policy,
            SelectionObservationContext {
                playback_buffer_s: observation.playback_buffer_s,
                decision_time_ms: observation.decision_time_ms,
                has_fresh_observation,
            },
        );

        let mut diagnostics = decision.diagnostics;
        diagnostics.held_by_upswitch_hysteresis = selection.diagnostics.held_by_upswitch_hysteresis;
        diagnostics.held_by_downswitch_hysteresis =
            selection.diagnostics.held_by_downswitch_hysteresis;
        diagnostics.blocked_by_hold_down = selection.diagnostics.blocked_by_hold_down;
        diagnostics.blocked_by_buffer_guard = selection.diagnostics.blocked_by_buffer_guard;

        let selection_decision = AbrSelectionDecision {
            allowed_qualities: decision.allowed_qualities,
            recommended_quality: decision.recommended_quality,
            estimated_bandwidth_bps: decision.estimated_bandwidth_bps,
            selected_quality_ids: selection.selection.quality_ids,
            selected_total_bitrate_bps: selection.selection.total_bitrate_bps,
            diagnostics,
        };

        self.last_selection_decision = Some(selection_decision.clone());
        selection_decision
    }
}

fn clamp_budget_bps(budget_bps: f64) -> u64 {
    budget_bps.clamp(0.0, u64::MAX as f64).floor() as u64
}

fn build_quality_ladder(quality_bitrates: &BTreeMap<QualityId, u64>) -> QualityLadder {
    QualityLadder::new(
        quality_bitrates
            .iter()
            .map(|(quality_id, bitrate_bps)| QualityDescriptor::new(*quality_id, *bitrate_bps))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AbrMode, QualitySelectionPolicy};

    fn id(value: u32) -> QualityId {
        QualityId::new(value)
    }

    #[test]
    fn controller_returns_final_selected_composite_quality_set() {
        let mut config = AbrConfig::for_mode(AbrMode::Simple);
        config.bandwidth_alpha = 1.0;

        let mut controller = AbrController::new(config, QualitySelectionPolicy::subscriber());
        controller
            .update_quality_ladder(&BTreeMap::from([(id(1), 26_000_000), (id(2), 58_000_000)]));

        controller.observe_estimated_bandwidth_bps(30_000_000.0);
        let initial = controller.decide(false);
        assert_eq!(initial.selected_quality_ids, vec![id(1)]);

        controller.observe_estimated_bandwidth_bps(90_000_000.0);
        let pending = controller.decide(true);
        assert_eq!(pending.selected_quality_ids, vec![id(1)]);

        controller.observe_estimated_bandwidth_bps(90_000_000.0);
        let confirmed = controller.decide(true);
        assert_eq!(confirmed.selected_quality_ids, vec![id(1), id(2)]);
        assert_eq!(confirmed.selected_total_bitrate_bps, Some(84_000_000));
    }

    #[test]
    fn single_quality_subscriber_switches_without_composite_confirmation() {
        let mut config = AbrConfig::for_mode(AbrMode::Simple);
        config.bandwidth_alpha = 1.0;

        let mut controller = AbrController::new(
            config,
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
        );
        controller
            .update_quality_ladder(&BTreeMap::from([(id(1), 26_000_000), (id(2), 58_000_000)]));

        controller.observe(Observation {
            throughput_sample_bps: Some(30_000_000.0),
            playback_buffer_s: Some(0.2),
            decision_time_ms: Some(0),
            ..Default::default()
        });
        let initial = controller.decide(true);
        assert_eq!(initial.selected_quality_ids, vec![id(1)]);

        controller.observe(Observation {
            throughput_sample_bps: Some(90_000_000.0),
            playback_buffer_s: Some(0.2),
            decision_time_ms: Some(1000),
            ..Default::default()
        });
        let switched = controller.decide(true);
        assert_eq!(switched.selected_quality_ids, vec![id(2)]);
    }

    #[test]
    fn decide_pending_consumes_each_observation_once() {
        let mut config = AbrConfig::for_mode(AbrMode::Simple);
        config.bandwidth_alpha = 1.0;

        let mut controller = AbrController::new(config, QualitySelectionPolicy::subscriber());
        controller
            .update_quality_ladder(&BTreeMap::from([(id(1), 26_000_000), (id(2), 58_000_000)]));

        controller.observe_estimated_bandwidth_bps(30_000_000.0);
        let initial = controller.decide_pending();
        assert_eq!(initial.selected_quality_ids, vec![id(1)]);

        controller.observe_estimated_bandwidth_bps(90_000_000.0);
        let pending = controller.decide_pending();
        assert_eq!(pending.selected_quality_ids, vec![id(1)]);

        let still_pending = controller.decide_pending();
        assert_eq!(still_pending.selected_quality_ids, vec![id(1)]);

        controller.observe_estimated_bandwidth_bps(90_000_000.0);
        let confirmed = controller.decide_pending();
        assert_eq!(confirmed.selected_quality_ids, vec![id(1), id(2)]);
    }
}
