use abr_core::{
    AbrConfig, AbrFactory, AbrMode, Observation, QualityDescriptor, QualityId, QualityLadder,
};

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
fn simple_mode_recommends_highest_quality_under_budget() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe_estimated_bandwidth_bps(10_000_000.0);

    let recommended = abr.recommend_quality().unwrap();
    let allowed = abr.allowed_qualities();

    assert_eq!(recommended.id.as_index(), 1);
    assert_eq!(allowed.len(), 2);
    assert_eq!(allowed[0].id.as_index(), 1);
    assert_eq!(allowed[1].id.as_index(), 0);
}

#[test]
fn simple_mode_falls_back_to_lowest_quality_when_over_budget() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe_estimated_bandwidth_bps(2_000_000.0);

    let recommended = abr.recommend_quality().unwrap();

    assert_eq!(recommended.id.as_index(), 0);
}

#[test]
fn config_update_preserves_estimated_bandwidth() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe_estimated_bandwidth_bps(10_000_000.0);
    let original_estimate = abr.estimated_bandwidth_bps();

    let mut config = AbrConfig::for_mode(AbrMode::Simple);
    config.bandwidth_overhead_fraction = 0.20;
    abr.update_config(config);

    assert_eq!(abr.estimated_bandwidth_bps(), original_estimate);
    assert_eq!(abr.recommend_quality().unwrap().id.as_index(), 1);
}

#[test]
fn ladder_update_preserves_estimated_bandwidth() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000]))
        .build();

    abr.observe_estimated_bandwidth_bps(10_000_000.0);
    abr.update_quality_ladder(ladder(&[4_000_000, 8_000_000, 9_000_000]));

    assert_eq!(abr.estimated_bandwidth_bps(), 10_000_000.0);
    assert_eq!(abr.recommend_quality().unwrap().id.as_index(), 2);
}

#[test]
fn simple_mode_ignores_latency_risk_signals() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe(Observation {
        throughput_sample_bps: Some(10_000_000.0),
        time_to_first_byte_s: Some(0.400),
        estimated_rtt_s: Some(0.200),
        completion_time_s: Some(0.900),
        segment_duration_s: Some(1.0),
        ..Default::default()
    });

    let decision = abr.decide();

    assert_eq!(decision.estimated_bandwidth_bps, 10_000_000.0);
    assert_eq!(decision.diagnostics.latency_risk_fraction, 0.0);
    assert_eq!(
        decision.diagnostics.bandwidth_budget_bps,
        decision.diagnostics.risk_adjusted_bandwidth_budget_bps,
    );
    assert_eq!(decision.recommended_quality.unwrap().id.as_index(), 1);
}

#[test]
fn optional_bandwidth_floor_rescues_decision_without_mutating_estimator_state() {
    let mut abr = AbrFactory::new_default(AbrMode::Simple)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe(Observation {
        throughput_sample_bps: Some(2_000_000.0),
        ..Default::default()
    });
    assert_eq!(abr.estimated_bandwidth_bps(), 2_000_000.0);
    assert_eq!(abr.recommend_quality().unwrap().id.as_index(), 0);

    abr.observe_with_bandwidth_floor_bps(
        Observation {
            throughput_sample_bps: Some(2_000_000.0),
            ..Default::default()
        },
        Some(10_000_000.0),
    );

    let decision = abr.decide();
    assert_eq!(decision.estimated_bandwidth_bps, 10_000_000.0);
    assert_eq!(decision.recommended_quality.unwrap().id.as_index(), 1);
    assert_eq!(abr.estimated_bandwidth_bps(), 2_000_000.0);

    abr.observe(Observation {
        throughput_sample_bps: Some(2_000_000.0),
        ..Default::default()
    });

    let decision = abr.decide();
    assert_eq!(decision.estimated_bandwidth_bps, 2_000_000.0);
    assert_eq!(decision.recommended_quality.unwrap().id.as_index(), 0);
}
