use abr_core::{
    AbrConfig, AbrController, AbrFactory, AbrMode, Observation, QualityDescriptor, QualityId,
    QualityLadder, QualitySelectionPolicy,
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

fn balanced_config() -> AbrConfig {
    let mut config = AbrConfig::for_mode(AbrMode::Balanced);
    config.bandwidth_alpha = 1.0;
    config
}

fn single_quality_controller(config: AbrConfig) -> AbrController {
    let mut controller = AbrController::new(
        config,
        QualitySelectionPolicy::subscriber_with_composite_qualities(false),
    );
    controller.update_quality_ladder(&std::collections::BTreeMap::from([
        (QualityId::from_index(0), 4_000_000),
        (QualityId::from_index(1), 8_000_000),
        (QualityId::from_index(2), 12_000_000),
    ]));
    controller
}

#[test]
fn hysteresis_keeps_previous_quality_until_upswitch_margin_is_met() {
    let mut controller = single_quality_controller(balanced_config());

    controller.observe(Observation {
        throughput_sample_bps: Some(5_000_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(0),
        ..Default::default()
    });
    assert_eq!(
        controller.decide_pending().selected_quality_ids,
        vec![QualityId::from_index(0)]
    );

    controller.observe(Observation {
        throughput_sample_bps: Some(9_000_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(1000),
        ..Default::default()
    });

    let decision = controller.decide_pending();
    assert_eq!(
        decision.selected_quality_ids,
        vec![QualityId::from_index(0)]
    );
    assert_eq!(decision.allowed_qualities[0].id.as_index(), 1);
    assert_eq!(decision.allowed_qualities[1].id.as_index(), 0);
    assert!(decision.diagnostics.held_by_upswitch_hysteresis);
}

#[test]
fn hold_down_blocks_fast_upswitches() {
    let mut config = AbrConfig::for_mode(AbrMode::Advanced);
    config.bandwidth_alpha = 1.0;
    config.enable_hysteresis = false;

    let mut controller = single_quality_controller(config);

    controller.observe(Observation {
        throughput_sample_bps: Some(5_000_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(0),
        ..Default::default()
    });
    assert_eq!(
        controller.decide_pending().selected_quality_ids,
        vec![QualityId::from_index(0)]
    );

    controller.observe(Observation {
        throughput_sample_bps: Some(12_000_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(100),
        ..Default::default()
    });
    let held = controller.decide_pending();
    assert_eq!(held.selected_quality_ids, vec![QualityId::from_index(0)]);
    assert!(held.diagnostics.blocked_by_hold_down);

    controller.observe(Observation {
        throughput_sample_bps: Some(12_000_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(1000),
        ..Default::default()
    });
    assert_eq!(
        controller.decide_pending().selected_quality_ids,
        vec![QualityId::from_index(1)]
    );
}

#[test]
fn low_buffer_blocks_upswitch_even_when_bandwidth_allows_it() {
    let mut config = balanced_config();
    config.enable_hysteresis = false;

    let mut controller = single_quality_controller(config);

    controller.observe(Observation {
        throughput_sample_bps: Some(5_000_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(0),
        ..Default::default()
    });
    assert_eq!(
        controller.decide_pending().selected_quality_ids,
        vec![QualityId::from_index(0)]
    );

    controller.observe(Observation {
        throughput_sample_bps: Some(12_000_000.0),
        playback_buffer_s: Some(0.01),
        decision_time_ms: Some(1000),
        ..Default::default()
    });

    let decision = controller.decide_pending();
    assert_eq!(
        decision.selected_quality_ids,
        vec![QualityId::from_index(0)]
    );
    assert_eq!(decision.allowed_qualities[0].id.as_index(), 1);
    assert_eq!(decision.allowed_qualities[1].id.as_index(), 0);
    assert!(decision.diagnostics.blocked_by_buffer_guard);
}

#[test]
fn downswitch_hysteresis_keeps_previous_quality_for_small_bandwidth_drops() {
    let mut controller = single_quality_controller(balanced_config());

    controller.observe(Observation {
        throughput_sample_bps: Some(10_000_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(0),
        ..Default::default()
    });
    assert_eq!(
        controller.decide_pending().selected_quality_ids,
        vec![QualityId::from_index(1)]
    );

    controller.observe(Observation {
        throughput_sample_bps: Some(8_300_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(1000),
        ..Default::default()
    });

    let decision = controller.decide_pending();
    assert_eq!(
        decision.selected_quality_ids,
        vec![QualityId::from_index(1)]
    );
    assert_eq!(decision.allowed_qualities[0].id.as_index(), 0);
    assert!(decision.diagnostics.held_by_downswitch_hysteresis);
}

#[test]
fn downswitch_happens_when_drop_exceeds_margin() {
    let mut controller = single_quality_controller(balanced_config());

    controller.observe(Observation {
        throughput_sample_bps: Some(10_000_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(0),
        ..Default::default()
    });
    assert_eq!(
        controller.decide_pending().selected_quality_ids,
        vec![QualityId::from_index(1)]
    );

    controller.observe(Observation {
        throughput_sample_bps: Some(7_400_000.0),
        playback_buffer_s: Some(0.2),
        decision_time_ms: Some(1000),
        ..Default::default()
    });

    let decision = controller.decide_pending();
    assert_eq!(
        decision.selected_quality_ids,
        vec![QualityId::from_index(0)]
    );
    assert!(!decision.diagnostics.held_by_downswitch_hysteresis);
}

#[test]
fn advanced_mode_uses_latency_risk_without_depressing_estimator() {
    let mut config = AbrConfig::for_mode(AbrMode::Advanced);
    config.bandwidth_alpha = 1.0;
    config.enable_hysteresis = false;
    config.enable_hold_down = false;
    config.enable_buffer_guard = false;

    let mut abr = AbrFactory::new_default(AbrMode::Advanced)
        .with_config(config)
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
    assert!(decision.diagnostics.latency_risk_fraction > 0.0);
    assert!(
        decision.diagnostics.risk_adjusted_bandwidth_budget_bps
            < decision.diagnostics.bandwidth_budget_bps
    );
    assert_eq!(decision.recommended_quality.unwrap().id.as_index(), 0);
}

#[test]
fn advanced_mode_uses_pacing_and_loss_signals() {
    let mut config = AbrConfig::for_mode(AbrMode::Advanced);
    config.bandwidth_alpha = 1.0;
    config.enable_hysteresis = false;
    config.enable_hold_down = false;
    config.enable_buffer_guard = false;

    let mut abr = AbrFactory::new_default(AbrMode::Advanced)
        .with_config(config)
        .with_quality_ladder(ladder(&[4_000_000, 8_000_000, 12_000_000]))
        .build();

    abr.observe(Observation {
        throughput_sample_bps: Some(10_000_000.0),
        pacing_rate_bps: Some(6_000_000.0),
        lost_packets_delta: Some(4),
        lost_bytes_delta: Some(32_000),
        ..Default::default()
    });

    let decision = abr.decide();

    assert_eq!(decision.estimated_bandwidth_bps, 10_000_000.0);
    assert!(decision.diagnostics.risk_adjusted_bandwidth_budget_bps <= 6_000_000.0);
    assert_eq!(decision.recommended_quality.unwrap().id.as_index(), 0);
}
