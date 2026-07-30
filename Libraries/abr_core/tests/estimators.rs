use std::time::Duration;

use abr_core::{Ewma, ThroughputEstimator};

#[test]
fn ewma_initializes_from_first_sample() {
    let mut ewma = Ewma::new(0.25);

    let updated = ewma.update(8_000.0).unwrap();

    assert_eq!(updated, 8_000.0);
    assert_eq!(ewma.value(), Some(8_000.0));
}

#[test]
fn ewma_applies_weighted_update() {
    let mut ewma = Ewma::new_initialized(0.25, 8_000.0);

    let updated = ewma.update(16_000.0).unwrap();

    assert_eq!(updated, 10_000.0);
}

#[test]
fn throughput_estimator_uses_default_before_first_sample() {
    let estimator = ThroughputEstimator::new(0.25, 50_000_000.0);

    assert_eq!(estimator.estimate_bps(), 50_000_000.0);
}

#[test]
fn throughput_estimator_observes_bytes_over_duration() {
    let mut estimator = ThroughputEstimator::new(0.25, 0.0);

    let estimate = estimator
        .observe_bytes(1_000, Duration::from_secs(1))
        .unwrap();

    assert_eq!(estimate, 8_000);
    assert_eq!(estimator.estimate_bps(), 8_000.0);
}

#[test]
fn throughput_estimator_resets_to_default() {
    let mut estimator = ThroughputEstimator::new(0.25, 123.0);
    estimator.observe_sample_bps(8_000.0).unwrap();

    estimator.reset();

    assert_eq!(estimator.estimate_bps(), 123.0);
}
