use crate::{
    config::AbrConfig,
    model::{QualityId, RankedQuality},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualitySelectionRole {
    Publisher,
    Subscriber,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QualitySelectionPolicy {
    pub role: QualitySelectionRole,
    pub allow_composite_qualities: bool,
}

impl QualitySelectionPolicy {
    pub const fn publisher() -> Self {
        Self {
            role: QualitySelectionRole::Publisher,
            allow_composite_qualities: false,
        }
    }

    pub const fn subscriber() -> Self {
        Self::subscriber_with_composite_qualities(true)
    }

    pub const fn subscriber_with_composite_qualities(allow_composite_qualities: bool) -> Self {
        Self {
            role: QualitySelectionRole::Subscriber,
            allow_composite_qualities,
        }
    }
}

impl Default for QualitySelectionPolicy {
    fn default() -> Self {
        Self::subscriber()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QualitySelection {
    pub quality_ids: Vec<QualityId>,
    pub total_bitrate_bps: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SelectionObservationContext {
    pub playback_buffer_s: Option<f64>,
    pub decision_time_ms: Option<u64>,
    pub has_fresh_observation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QualitySelectionDiagnostics {
    pub held_by_upswitch_hysteresis: bool,
    pub held_by_downswitch_hysteresis: bool,
    pub blocked_by_hold_down: bool,
    pub blocked_by_buffer_guard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StabilizedQualitySelection {
    pub selection: QualitySelection,
    pub diagnostics: QualitySelectionDiagnostics,
}

/// Final selection gate for the ABR pipeline.
///
/// This stage takes a candidate quality selection and decides whether to apply
/// it now or hold the previous selection because of hysteresis, confirmation,
/// hold-down, or buffer guard rules.
#[derive(Debug, Clone)]
pub struct QualitySelectionStabilizer {
    config: AbrConfig,
    current_selection: QualitySelection,
    pending_selection: Option<QualitySelection>,
    pending_observations: u8,
    last_switch_at_ms: Option<u64>,
}

impl QualitySelectionStabilizer {
    pub fn new(config: AbrConfig) -> Self {
        Self {
            config,
            current_selection: QualitySelection::default(),
            pending_selection: None,
            pending_observations: 0,
            last_switch_at_ms: None,
        }
    }

    pub fn update_config(&mut self, config: AbrConfig) {
        self.config = config;
        self.clear_pending_selection();
    }

    pub fn reset(&mut self) {
        self.current_selection = QualitySelection::default();
        self.clear_pending_selection();
        self.last_switch_at_ms = None;
    }

    pub fn select(
        &mut self,
        quality_bitrates: &BTreeMap<QualityId, u64>,
        allowed_qualities: &[RankedQuality],
        budget_bps: u64,
        fallback_quality_id: Option<QualityId>,
        selection_policy: QualitySelectionPolicy,
        context: SelectionObservationContext,
    ) -> StabilizedQualitySelection {
        let candidate = select_quality_set_from_allowed_qualities(
            quality_bitrates,
            allowed_qualities,
            budget_bps,
            fallback_quality_id,
            selection_policy,
        );
        let selection = self.stabilize_selection(candidate, budget_bps, selection_policy, context);
        self.current_selection = selection.selection.clone();
        selection
    }

    fn stabilize_selection(
        &mut self,
        candidate: QualitySelection,
        budget_bps: u64,
        selection_policy: QualitySelectionPolicy,
        context: SelectionObservationContext,
    ) -> StabilizedQualitySelection {
        let mut diagnostics = QualitySelectionDiagnostics::default();

        if !should_stabilize_selection(selection_policy) {
            return self.accept_selection(candidate, context.decision_time_ms, diagnostics);
        }

        if self.current_selection.quality_ids.is_empty() {
            return self.accept_selection(candidate, context.decision_time_ms, diagnostics);
        }

        if candidate.quality_ids == self.current_selection.quality_ids {
            self.clear_pending_selection();
            return self.hold_current(diagnostics);
        }

        if !context.has_fresh_observation {
            self.clear_pending_selection();
            return self.hold_current(diagnostics);
        }

        let Some(previous_total_bitrate_bps) = self.current_selection.total_bitrate_bps else {
            return self.accept_selection(candidate, context.decision_time_ms, diagnostics);
        };
        let Some(candidate_total_bitrate_bps) = candidate.total_bitrate_bps else {
            return self.accept_selection(candidate, context.decision_time_ms, diagnostics);
        };

        if candidate_total_bitrate_bps > previous_total_bitrate_bps
            && should_apply_upswitch_guards(selection_policy)
        {
            if self.config.enable_buffer_guard {
                if let Some(playback_buffer_s) = context.playback_buffer_s {
                    if playback_buffer_s < self.config.min_buffer_for_upswitch_s {
                        diagnostics.blocked_by_buffer_guard = true;
                        self.clear_pending_selection();
                        return self.hold_current(diagnostics);
                    }
                }
            }

            if self.config.enable_hold_down {
                if let (Some(now_ms), Some(last_switch_at_ms)) =
                    (context.decision_time_ms, self.last_switch_at_ms)
                {
                    if now_ms.saturating_sub(last_switch_at_ms)
                        < self.config.min_upswitch_interval_ms
                    {
                        diagnostics.blocked_by_hold_down = true;
                        self.clear_pending_selection();
                        return self.hold_current(diagnostics);
                    }
                }
            }
        }

        if !self.budget_confirms_switch(
            previous_total_bitrate_bps,
            candidate_total_bitrate_bps,
            budget_bps,
            &mut diagnostics,
        ) {
            self.clear_pending_selection();
            return self.hold_current(diagnostics);
        }

        if !requires_confirmation(selection_policy) {
            return self.accept_selection(candidate, context.decision_time_ms, diagnostics);
        }

        if self
            .pending_selection
            .as_ref()
            .map(|selection| selection.quality_ids.as_slice())
            == Some(candidate.quality_ids.as_slice())
        {
            self.pending_observations = self.pending_observations.saturating_add(1);
        } else {
            self.pending_selection = Some(candidate.clone());
            self.pending_observations = 1;
        }

        let required_observations = self.config.selection_confirmation_samples.max(1);
        if self.pending_observations < required_observations {
            return self.hold_current(diagnostics);
        }

        self.accept_selection(candidate, context.decision_time_ms, diagnostics)
    }

    fn budget_confirms_switch(
        &self,
        previous_total_bitrate_bps: u64,
        candidate_total_bitrate_bps: u64,
        budget_bps: u64,
        diagnostics: &mut QualitySelectionDiagnostics,
    ) -> bool {
        if !self.config.enable_hysteresis {
            return true;
        }

        let budget_bps = budget_bps as f64;
        if candidate_total_bitrate_bps > previous_total_bitrate_bps {
            let confirmed = budget_bps
                >= candidate_total_bitrate_bps as f64
                    * self.config.upswitch_hysteresis_factor.max(1.0);
            if !confirmed {
                diagnostics.held_by_upswitch_hysteresis = true;
            }
            confirmed
        } else if candidate_total_bitrate_bps < previous_total_bitrate_bps {
            let confirmed = budget_bps
                < previous_total_bitrate_bps as f64
                    * self.config.downswitch_hysteresis_factor.clamp(0.0, 1.0);
            if !confirmed {
                diagnostics.held_by_downswitch_hysteresis = true;
            }
            confirmed
        } else {
            let confirmed = budget_bps
                >= candidate_total_bitrate_bps as f64
                    * self.config.upswitch_hysteresis_factor.max(1.0);
            if !confirmed {
                diagnostics.held_by_upswitch_hysteresis = true;
            }
            confirmed
        }
    }

    fn hold_current(&self, diagnostics: QualitySelectionDiagnostics) -> StabilizedQualitySelection {
        StabilizedQualitySelection {
            selection: self.current_selection.clone(),
            diagnostics,
        }
    }

    fn accept_selection(
        &mut self,
        selection: QualitySelection,
        decision_time_ms: Option<u64>,
        diagnostics: QualitySelectionDiagnostics,
    ) -> StabilizedQualitySelection {
        self.clear_pending_selection();
        if selection.quality_ids != self.current_selection.quality_ids {
            if let Some(now_ms) = decision_time_ms {
                self.last_switch_at_ms = Some(now_ms);
            }
        }

        StabilizedQualitySelection {
            selection,
            diagnostics,
        }
    }

    fn clear_pending_selection(&mut self) {
        self.pending_selection = None;
        self.pending_observations = 0;
    }
}

pub fn total_bitrate_for_quality_ids(
    quality_bitrates: &BTreeMap<QualityId, u64>,
    quality_ids: &[QualityId],
) -> Option<u64> {
    quality_ids
        .iter()
        .try_fold(0_u64, |total_bitrate_bps, quality_id| {
            let bitrate_bps = quality_bitrates.get(quality_id).copied()?;
            Some(total_bitrate_bps.saturating_add(bitrate_bps))
        })
}

fn should_stabilize_selection(selection_policy: QualitySelectionPolicy) -> bool {
    matches!(selection_policy.role, QualitySelectionRole::Subscriber)
}

fn requires_confirmation(selection_policy: QualitySelectionPolicy) -> bool {
    matches!(selection_policy.role, QualitySelectionRole::Subscriber)
        && selection_policy.allow_composite_qualities
}

fn should_apply_upswitch_guards(selection_policy: QualitySelectionPolicy) -> bool {
    matches!(selection_policy.role, QualitySelectionRole::Subscriber)
        && !selection_policy.allow_composite_qualities
}

fn select_quality_set_from_allowed_qualities(
    quality_bitrates: &BTreeMap<QualityId, u64>,
    allowed_qualities: &[RankedQuality],
    budget_bps: u64,
    fallback_quality_id: Option<QualityId>,
    selection_policy: QualitySelectionPolicy,
) -> QualitySelection {
    if allowed_qualities.is_empty() {
        return fallback_selection(quality_bitrates, fallback_quality_id);
    }

    match selection_policy.role {
        QualitySelectionRole::Publisher => select_publisher_quality_set_from_allowed_qualities(
            allowed_qualities,
            quality_bitrates,
            budget_bps,
            fallback_quality_id,
        ),
        QualitySelectionRole::Subscriber if selection_policy.allow_composite_qualities => {
            let candidates = allowed_qualities
                .iter()
                .map(|quality| (quality.id, quality.nominal_bitrate_bps))
                .collect::<Vec<_>>();
            select_highest_total_quality_combination(
                &candidates,
                quality_bitrates,
                budget_bps,
                fallback_quality_id,
            )
        }
        QualitySelectionRole::Subscriber => select_best_single_quality_from_allowed_qualities(
            allowed_qualities,
            budget_bps,
            quality_bitrates,
            fallback_quality_id,
        ),
    }
}

pub fn select_quality_set(
    quality_bitrates: &BTreeMap<QualityId, u64>,
    allowed_quality_ids: &BTreeSet<QualityId>,
    budget_bps: u64,
    fallback_quality_id: Option<QualityId>,
    selection_policy: QualitySelectionPolicy,
) -> QualitySelection {
    let candidates = quality_bitrates
        .iter()
        .filter(|(quality_id, _)| allowed_quality_ids.contains(quality_id))
        .map(|(quality_id, bitrate_bps)| (*quality_id, *bitrate_bps))
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return fallback_selection(quality_bitrates, fallback_quality_id);
    }

    match selection_policy.role {
        QualitySelectionRole::Publisher => select_publisher_quality_set(
            &candidates,
            quality_bitrates,
            budget_bps,
            fallback_quality_id,
        ),
        QualitySelectionRole::Subscriber if selection_policy.allow_composite_qualities => {
            select_highest_total_quality_combination(
                &candidates,
                quality_bitrates,
                budget_bps,
                fallback_quality_id,
            )
        }
        QualitySelectionRole::Subscriber => select_best_single_quality(
            &candidates,
            quality_bitrates,
            budget_bps,
            fallback_quality_id,
        ),
    }
}

fn fallback_selection(
    quality_bitrates: &BTreeMap<QualityId, u64>,
    fallback_quality_id: Option<QualityId>,
) -> QualitySelection {
    fallback_quality_id
        .and_then(|quality_id| {
            quality_bitrates
                .get(&quality_id)
                .copied()
                .map(|bitrate_bps| QualitySelection {
                    quality_ids: vec![quality_id],
                    total_bitrate_bps: Some(bitrate_bps),
                })
        })
        .unwrap_or_default()
}

fn select_publisher_quality_set(
    candidates: &[(QualityId, u64)],
    quality_bitrates: &BTreeMap<QualityId, u64>,
    budget_bps: u64,
    fallback_quality_id: Option<QualityId>,
) -> QualitySelection {
    let mut ascending_candidates = candidates.to_vec();
    ascending_candidates
        .sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));

    let mut selected_quality_ids = Vec::new();
    let mut total_bitrate_bps = 0_u64;
    for (quality_id, bitrate_bps) in ascending_candidates {
        if total_bitrate_bps.saturating_add(bitrate_bps) > budget_bps {
            break;
        }

        total_bitrate_bps = total_bitrate_bps.saturating_add(bitrate_bps);
        selected_quality_ids.push(quality_id);
    }

    if selected_quality_ids.is_empty() {
        return fallback_selection(quality_bitrates, fallback_quality_id);
    }

    QualitySelection {
        quality_ids: selected_quality_ids,
        total_bitrate_bps: Some(total_bitrate_bps),
    }
}

fn select_publisher_quality_set_from_allowed_qualities(
    allowed_qualities: &[RankedQuality],
    quality_bitrates: &BTreeMap<QualityId, u64>,
    budget_bps: u64,
    fallback_quality_id: Option<QualityId>,
) -> QualitySelection {
    let mut selected_quality_ids = Vec::with_capacity(allowed_qualities.len());
    let mut total_bitrate_bps = 0_u64;
    for quality in allowed_qualities.iter().rev() {
        if total_bitrate_bps.saturating_add(quality.nominal_bitrate_bps) > budget_bps {
            break;
        }

        total_bitrate_bps = total_bitrate_bps.saturating_add(quality.nominal_bitrate_bps);
        selected_quality_ids.push(quality.id);
    }

    if selected_quality_ids.is_empty() {
        return fallback_selection(quality_bitrates, fallback_quality_id);
    }

    QualitySelection {
        quality_ids: selected_quality_ids,
        total_bitrate_bps: Some(total_bitrate_bps),
    }
}

fn select_best_single_quality(
    candidates: &[(QualityId, u64)],
    quality_bitrates: &BTreeMap<QualityId, u64>,
    budget_bps: u64,
    fallback_quality_id: Option<QualityId>,
) -> QualitySelection {
    let best_candidate = candidates
        .iter()
        .filter(|(_, bitrate_bps)| *bitrate_bps <= budget_bps)
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));

    match best_candidate {
        Some((quality_id, bitrate_bps)) => QualitySelection {
            quality_ids: vec![*quality_id],
            total_bitrate_bps: Some(*bitrate_bps),
        },
        None => fallback_selection(quality_bitrates, fallback_quality_id),
    }
}

fn select_best_single_quality_from_allowed_qualities(
    allowed_qualities: &[RankedQuality],
    budget_bps: u64,
    quality_bitrates: &BTreeMap<QualityId, u64>,
    fallback_quality_id: Option<QualityId>,
) -> QualitySelection {
    allowed_qualities
        .iter()
        .find(|quality| quality.nominal_bitrate_bps <= budget_bps)
        .map(|quality| QualitySelection {
            quality_ids: vec![quality.id],
            total_bitrate_bps: Some(quality.nominal_bitrate_bps),
        })
        .unwrap_or_else(|| fallback_selection(quality_bitrates, fallback_quality_id))
}

fn select_highest_total_quality_combination(
    candidates: &[(QualityId, u64)],
    quality_bitrates: &BTreeMap<QualityId, u64>,
    budget_bps: u64,
    fallback_quality_id: Option<QualityId>,
) -> QualitySelection {
    let mut best_ids = Vec::new();
    let mut best_total_bitrate_bps = 0_u64;

    if candidates.len() <= 20 {
        let subset_count = 1_u64 << candidates.len();
        for mask in 1_u64..subset_count {
            let mut total_bitrate_bps = 0_u64;
            let mut quality_ids = Vec::new();
            let mut over_budget = false;

            for (bit_index, (quality_id, bitrate_bps)) in candidates.iter().enumerate() {
                if (mask & (1_u64 << bit_index)) == 0 {
                    continue;
                }

                total_bitrate_bps = total_bitrate_bps.saturating_add(*bitrate_bps);
                if total_bitrate_bps > budget_bps {
                    over_budget = true;
                    break;
                }

                quality_ids.push(*quality_id);
            }

            if over_budget {
                continue;
            }

            quality_ids.sort_unstable();
            if total_bitrate_bps > best_total_bitrate_bps
                || (total_bitrate_bps == best_total_bitrate_bps
                    && quality_ids.len() > best_ids.len())
                || (total_bitrate_bps == best_total_bitrate_bps
                    && quality_ids.len() == best_ids.len()
                    && quality_ids > best_ids)
            {
                best_total_bitrate_bps = total_bitrate_bps;
                best_ids = quality_ids;
            }
        }
    } else {
        let mut descending_candidates = candidates.to_vec();
        descending_candidates
            .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));
        for (quality_id, bitrate_bps) in descending_candidates {
            if best_total_bitrate_bps.saturating_add(bitrate_bps) > budget_bps {
                continue;
            }
            best_total_bitrate_bps = best_total_bitrate_bps.saturating_add(bitrate_bps);
            best_ids.push(quality_id);
        }
        best_ids.sort_unstable();
    }

    if best_ids.is_empty() {
        return fallback_selection(quality_bitrates, fallback_quality_id);
    }

    QualitySelection {
        quality_ids: best_ids,
        total_bitrate_bps: Some(best_total_bitrate_bps),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AbrConfig, AbrMode};

    fn id(value: u32) -> QualityId {
        QualityId::new(value)
    }

    fn composite_policy() -> QualitySelectionPolicy {
        QualitySelectionPolicy::subscriber_with_composite_qualities(true)
    }

    fn ranked_qualities(entries: &[(u32, u64)]) -> Vec<RankedQuality> {
        entries
            .iter()
            .map(|(quality_id, bitrate_bps)| RankedQuality {
                id: id(*quality_id),
                nominal_bitrate_bps: *bitrate_bps,
                score: *bitrate_bps as f64,
            })
            .collect()
    }

    fn context(has_fresh_observation: bool) -> SelectionObservationContext {
        SelectionObservationContext {
            has_fresh_observation,
            ..Default::default()
        }
    }

    fn timed_context(
        decision_time_ms: u64,
        playback_buffer_s: Option<f64>,
    ) -> SelectionObservationContext {
        SelectionObservationContext {
            playback_buffer_s,
            decision_time_ms: Some(decision_time_ms),
            has_fresh_observation: true,
        }
    }

    #[test]
    fn selected_qualities_fill_budget_with_multiple_base_qualities() {
        let selection = select_quality_set(
            &BTreeMap::from([
                (id(0), 15_000_000),
                (id(1), 25_000_000),
                (id(2), 60_000_000),
            ]),
            &BTreeSet::from([id(0), id(1), id(2)]),
            40_000_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber(),
        );

        assert_eq!(selection.quality_ids, vec![id(0), id(1)]);
        assert_eq!(selection.total_bitrate_bps, Some(40_000_000));
    }

    #[test]
    fn selected_qualities_fall_back_to_lowest_when_budget_is_too_small() {
        let selection = select_quality_set(
            &BTreeMap::from([
                (id(0), 15_000_000),
                (id(1), 25_000_000),
                (id(2), 60_000_000),
            ]),
            &BTreeSet::from([id(0)]),
            10_000_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber(),
        );

        assert_eq!(selection.quality_ids, vec![id(0)]);
        assert_eq!(selection.total_bitrate_bps, Some(15_000_000));
    }

    #[test]
    fn publisher_selection_preserves_lower_qualities_before_highest_quality() {
        let selection = select_quality_set(
            &BTreeMap::from([
                (id(0), 15_000_000),
                (id(1), 25_000_000),
                (id(2), 60_000_000),
            ]),
            &BTreeSet::from([id(0), id(1), id(2)]),
            60_000_000,
            Some(id(0)),
            QualitySelectionPolicy::publisher(),
        );

        assert_eq!(selection.quality_ids, vec![id(0), id(1)]);
        assert_eq!(selection.total_bitrate_bps, Some(40_000_000));
    }

    #[test]
    fn subscriber_without_composite_qualities_picks_best_single_base_quality() {
        let selection = select_quality_set(
            &BTreeMap::from([
                (id(0), 15_000_000),
                (id(1), 25_000_000),
                (id(2), 60_000_000),
            ]),
            &BTreeSet::from([id(0), id(1), id(2)]),
            60_000_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
        );

        assert_eq!(selection.quality_ids, vec![id(2)]);
        assert_eq!(selection.total_bitrate_bps, Some(60_000_000));
    }

    #[test]
    fn total_bitrate_for_quality_ids_sums_selected_bitrates() {
        let total_bitrate_bps = total_bitrate_for_quality_ids(
            &BTreeMap::from([
                (id(0), 15_000_000),
                (id(1), 25_000_000),
                (id(2), 60_000_000),
            ]),
            &[id(0), id(2)],
        );

        assert_eq!(total_bitrate_bps, Some(75_000_000));
    }

    #[test]
    fn composite_selection_controller_requires_confirmation_before_switching() {
        let mut controller = QualitySelectionStabilizer::new(AbrConfig::for_mode(AbrMode::Simple));
        let quality_bitrates = BTreeMap::from([(id(1), 26_000_000), (id(2), 58_000_000)]);
        let allowed_qualities = ranked_qualities(&[(2, 58_000_000), (1, 26_000_000)]);

        let initial = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            30_000_000,
            Some(id(1)),
            composite_policy(),
            context(false),
        );
        assert_eq!(initial.selection.quality_ids, vec![id(1)]);

        let pending = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            84_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        assert_eq!(pending.selection.quality_ids, vec![id(1)]);

        let confirmed = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            84_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        assert_eq!(confirmed.selection.quality_ids, vec![id(1), id(2)]);
        assert_eq!(confirmed.selection.total_bitrate_bps, Some(84_000_000));
    }

    #[test]
    fn composite_selection_controller_uses_configured_upswitch_hysteresis() {
        let mut controller =
            QualitySelectionStabilizer::new(AbrConfig::for_mode(AbrMode::Balanced));
        let quality_bitrates = BTreeMap::from([(id(1), 26_000_000), (id(2), 58_000_000)]);
        let allowed_qualities = ranked_qualities(&[(2, 58_000_000), (1, 26_000_000)]);

        controller.select(
            &quality_bitrates,
            &allowed_qualities,
            30_000_000,
            Some(id(1)),
            composite_policy(),
            context(false),
        );

        let held = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            90_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        assert_eq!(held.selection.quality_ids, vec![id(1)]);
        assert!(held.diagnostics.held_by_upswitch_hysteresis);

        let pending = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            95_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        assert_eq!(pending.selection.quality_ids, vec![id(1)]);

        let confirmed = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            95_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        assert_eq!(confirmed.selection.quality_ids, vec![id(1), id(2)]);
    }

    #[test]
    fn composite_selection_controller_uses_configured_downswitch_hysteresis() {
        let mut controller =
            QualitySelectionStabilizer::new(AbrConfig::for_mode(AbrMode::Balanced));
        let quality_bitrates = BTreeMap::from([(id(1), 26_000_000), (id(2), 58_000_000)]);
        let allowed_qualities = ranked_qualities(&[(2, 58_000_000), (1, 26_000_000)]);

        controller.select(
            &quality_bitrates,
            &allowed_qualities,
            30_000_000,
            Some(id(1)),
            composite_policy(),
            context(false),
        );
        controller.select(
            &quality_bitrates,
            &allowed_qualities,
            100_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        let composite = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            100_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        assert_eq!(composite.selection.quality_ids, vec![id(1), id(2)]);

        let held = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            80_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        assert_eq!(held.selection.quality_ids, vec![id(1), id(2)]);
        assert!(held.diagnostics.held_by_downswitch_hysteresis);

        let pending = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            79_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        assert_eq!(pending.selection.quality_ids, vec![id(1), id(2)]);

        let confirmed = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            79_000_000,
            Some(id(1)),
            composite_policy(),
            context(true),
        );
        assert_eq!(confirmed.selection.quality_ids, vec![id(2)]);
    }

    #[test]
    fn single_quality_subscriber_uses_configured_upswitch_hysteresis() {
        let mut controller =
            QualitySelectionStabilizer::new(AbrConfig::for_mode(AbrMode::Balanced));
        let quality_bitrates = BTreeMap::from([(id(0), 4_000_000), (id(1), 8_000_000)]);
        let allowed_qualities = ranked_qualities(&[(1, 8_000_000), (0, 4_000_000)]);

        let initial = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            5_000_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
            timed_context(0, Some(0.2)),
        );
        assert_eq!(initial.selection.quality_ids, vec![id(0)]);

        let held = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            8_500_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
            timed_context(1000, Some(0.2)),
        );
        assert_eq!(held.selection.quality_ids, vec![id(0)]);
        assert!(held.diagnostics.held_by_upswitch_hysteresis);

        let switched = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            8_900_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
            timed_context(2000, Some(0.2)),
        );
        assert_eq!(switched.selection.quality_ids, vec![id(1)]);
    }

    #[test]
    fn single_quality_subscriber_uses_hold_down_without_confirmation() {
        let mut controller =
            QualitySelectionStabilizer::new(AbrConfig::for_mode(AbrMode::Advanced));
        let quality_bitrates = BTreeMap::from([(id(0), 4_000_000), (id(1), 8_000_000)]);
        let allowed_qualities = ranked_qualities(&[(1, 8_000_000), (0, 4_000_000)]);

        let initial = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            5_000_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
            timed_context(0, Some(0.2)),
        );
        assert_eq!(initial.selection.quality_ids, vec![id(0)]);

        let held = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            12_000_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
            timed_context(100, Some(0.2)),
        );
        assert_eq!(held.selection.quality_ids, vec![id(0)]);
        assert!(held.diagnostics.blocked_by_hold_down);

        let switched = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            12_000_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
            timed_context(1000, Some(0.2)),
        );
        assert_eq!(switched.selection.quality_ids, vec![id(1)]);
    }

    #[test]
    fn single_quality_subscriber_uses_buffer_guard_without_confirmation() {
        let mut config = AbrConfig::for_mode(AbrMode::Balanced);
        config.enable_hysteresis = false;

        let mut controller = QualitySelectionStabilizer::new(config);
        let quality_bitrates = BTreeMap::from([(id(0), 4_000_000), (id(1), 8_000_000)]);
        let allowed_qualities = ranked_qualities(&[(1, 8_000_000), (0, 4_000_000)]);

        let initial = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            5_000_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
            timed_context(0, Some(0.2)),
        );
        assert_eq!(initial.selection.quality_ids, vec![id(0)]);

        let held = controller.select(
            &quality_bitrates,
            &allowed_qualities,
            12_000_000,
            Some(id(0)),
            QualitySelectionPolicy::subscriber_with_composite_qualities(false),
            timed_context(1000, Some(0.01)),
        );
        assert_eq!(held.selection.quality_ids, vec![id(0)]);
        assert!(held.diagnostics.blocked_by_buffer_guard);
    }
}
