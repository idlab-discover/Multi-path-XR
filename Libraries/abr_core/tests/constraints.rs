use abr_core::{AbrFactory, AbrMode, HardConstraints, QualityDescriptor, QualityId, QualityLadder};

fn ladder(bitrates: &[u64]) -> QualityLadder {
    QualityLadder::new(
        bitrates
            .iter()
            .enumerate()
            .map(|(index, bitrate)| QualityDescriptor::new(QualityId::from_index(index), *bitrate))
            .collect(),
    )
}

#[test]
fn hard_constraints_cap_allowed_qualities() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe_estimated_bandwidth_bps(20_000_000.0);
    abr.update_constraints(HardConstraints {
        max_quality: Some(QualityId::from_index(1)),
        ..HardConstraints::default()
    });

    let allowed = abr.allowed_qualities();

    assert_eq!(allowed.len(), 2);
    assert_eq!(allowed[0].id.as_index(), 1);
    assert_eq!(allowed[1].id.as_index(), 0);
}

#[test]
fn hard_constraints_can_disable_specific_quality() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe_estimated_bandwidth_bps(20_000_000.0);
    abr.update_constraints(HardConstraints {
        disabled_qualities: vec![QualityId::from_index(2)],
        ..HardConstraints::default()
    });

    assert_eq!(abr.recommend_quality().unwrap().id.as_index(), 1);
}

#[test]
fn decision_reports_constraint_and_bandwidth_diagnostics() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe_estimated_bandwidth_bps(10_000_000.0);
    abr.update_constraints(HardConstraints {
        disabled_qualities: vec![QualityId::from_index(0)],
        ..HardConstraints::default()
    });

    let decision = abr.decide();

    assert_eq!(decision.recommended_quality.unwrap().id.as_index(), 1);
    assert_eq!(decision.diagnostics.filtered_by_constraints, 1);
    assert_eq!(decision.diagnostics.filtered_by_bandwidth, 1);
    assert!(!decision.diagnostics.fallback_to_lowest);
    assert!(!decision.diagnostics.held_by_upswitch_hysteresis);
    assert!(!decision.diagnostics.held_by_downswitch_hysteresis);
}

#[test]
fn fallback_respects_constraints_when_everything_is_over_budget() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe_estimated_bandwidth_bps(1_000_000.0);
    abr.update_constraints(HardConstraints {
        min_quality: Some(QualityId::from_index(1)),
        ..HardConstraints::default()
    });

    let decision = abr.decide();

    assert_eq!(decision.recommended_quality.unwrap().id.as_index(), 1);
    assert!(decision.diagnostics.fallback_to_lowest);
}
