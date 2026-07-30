use abr_core::{AbrConfig, AbrController, AbrMode, Observation, QualityId, QualitySelectionPolicy};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::collections::BTreeMap;
use std::time::Duration;

fn build_quality_ladder(quality_count: usize, base_bitrate_bps: u64) -> BTreeMap<QualityId, u64> {
    (0..quality_count)
        .map(|index| {
            let bitrate_bps = base_bitrate_bps + (index as u64 * base_bitrate_bps / 2);
            (QualityId::from_index(index), bitrate_bps)
        })
        .collect()
}

fn make_controller(
    mode: AbrMode,
    selection_policy: QualitySelectionPolicy,
    quality_count: usize,
) -> AbrController {
    let mut config = AbrConfig::for_mode(mode);
    config.bandwidth_alpha = 1.0;

    let mut controller = AbrController::new(config, selection_policy);
    controller.update_quality_ladder(&build_quality_ladder(quality_count, 4_000_000));
    controller.observe(Observation {
        throughput_sample_bps: Some(42_000_000.0),
        playback_buffer_s: Some(0.15),
        decision_time_ms: Some(0),
        estimated_rtt_s: Some(0.030),
        ..Default::default()
    });
    black_box(controller.decide(true));
    controller
}

fn steady_state_observation(step: usize) -> Observation {
    const THROUGHPUT_PATTERN_BPS: [f64; 8] = [
        24_000_000.0,
        38_000_000.0,
        52_000_000.0,
        68_000_000.0,
        44_000_000.0,
        31_000_000.0,
        57_000_000.0,
        47_000_000.0,
    ];
    const RTT_PATTERN_S: [f64; 8] = [0.020, 0.025, 0.030, 0.060, 0.045, 0.035, 0.028, 0.022];
    const BUFFER_PATTERN_S: [f64; 8] = [0.16, 0.14, 0.12, 0.09, 0.11, 0.15, 0.18, 0.13];

    let index = step % THROUGHPUT_PATTERN_BPS.len();
    Observation {
        throughput_sample_bps: Some(THROUGHPUT_PATTERN_BPS[index]),
        playback_buffer_s: Some(BUFFER_PATTERN_S[index]),
        decision_time_ms: Some((step as u64) * 16),
        estimated_rtt_s: Some(RTT_PATTERN_S[index]),
        pacing_rate_bps: Some(THROUGHPUT_PATTERN_BPS[index] * 1.05),
        ..Default::default()
    }
}

fn benchmark_steady_state_decide(c: &mut Criterion) {
    let mut group = c.benchmark_group("steady_state_observe_and_decide");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));

    for quality_count in [8_usize, 16, 32] {
        let benchmark_id = BenchmarkId::new("subscriber_single", quality_count);
        group.bench_with_input(benchmark_id, &quality_count, |b, &quality_count| {
            let mut controller = make_controller(
                AbrMode::Advanced,
                QualitySelectionPolicy::subscriber_with_composite_qualities(false),
                quality_count,
            );
            let mut step = 0_usize;

            b.iter(|| {
                let observation = steady_state_observation(step);
                controller.observe(observation);
                let decision = controller.decide(true);
                black_box(decision.selected_total_bitrate_bps);
                step = step.wrapping_add(1);
            });
        });

        let benchmark_id = BenchmarkId::new("publisher", quality_count);
        group.bench_with_input(benchmark_id, &quality_count, |b, &quality_count| {
            let mut controller = make_controller(
                AbrMode::Advanced,
                QualitySelectionPolicy::publisher(),
                quality_count,
            );
            let mut step = 0_usize;

            b.iter(|| {
                let observation = steady_state_observation(step);
                controller.observe(observation);
                let decision = controller.decide(true);
                black_box(decision.selected_quality_ids.len());
                step = step.wrapping_add(1);
            });
        });
    }

    group.finish();
}

fn benchmark_composite_selection_pressure(c: &mut Criterion) {
    let mut group = c.benchmark_group("steady_state_composite_subscriber");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(20);

    for quality_count in [6_usize, 10, 12] {
        let benchmark_id = BenchmarkId::new("composite", quality_count);
        group.bench_with_input(benchmark_id, &quality_count, |b, &quality_count| {
            let mut controller = make_controller(
                AbrMode::Advanced,
                QualitySelectionPolicy::subscriber(),
                quality_count,
            );
            let mut step = 0_usize;

            b.iter(|| {
                let observation = steady_state_observation(step);
                controller.observe(observation);
                let decision = controller.decide(true);
                black_box(&decision.selected_quality_ids);
                step = step.wrapping_add(1);
            });
        });
    }

    group.finish();
}

fn benchmark_ladder_update_cost(c: &mut Criterion) {
    let mut group = c.benchmark_group("ladder_update_then_decide");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    group.sample_size(20);

    for quality_count in [8_usize, 16, 32] {
        let ladders = [1_000_u64, 1_050, 1_100, 1_150]
            .into_iter()
            .map(|scale_per_mille| {
                let base_ladder = build_quality_ladder(quality_count, 4_000_000);
                base_ladder
                    .into_iter()
                    .map(|(quality_id, bitrate_bps)| {
                        (
                            quality_id,
                            bitrate_bps.saturating_mul(scale_per_mille) / 1_000,
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .collect::<Vec<_>>();

        let benchmark_id = BenchmarkId::new("subscriber_single", quality_count);
        group.bench_with_input(benchmark_id, &quality_count, |b, &quality_count| {
            let mut controller = make_controller(
                AbrMode::Advanced,
                QualitySelectionPolicy::subscriber_with_composite_qualities(false),
                quality_count,
            );
            let mut step = 0_usize;

            b.iter(|| {
                let ladder = &ladders[step % ladders.len()];
                controller.update_quality_ladder(ladder);
                controller.observe(steady_state_observation(step));
                let decision = controller.decide(true);
                black_box(decision.recommended_quality);
                step = step.wrapping_add(1);
            });
        });
    }

    group.finish();
}

criterion_group!(
    decision_hot_path,
    benchmark_steady_state_decide,
    benchmark_composite_selection_pressure,
    benchmark_ladder_update_cost,
);
criterion_main!(decision_hot_path);
