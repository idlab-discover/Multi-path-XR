use std::{
    thread,
    time::{Duration, Instant},
};

use prometheus::IntGauge;

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const MICROS_PER_SECOND: u128 = 1_000_000;

pub fn frame_offset_duration(frame_index: u64, fps: u32) -> Duration {
    let fps = fps.max(1) as u128;
    let nanos = NANOS_PER_SECOND.saturating_mul(frame_index as u128) / fps;
    Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
}

pub fn frame_index_for_elapsed(elapsed: Duration, fps: u32) -> u64 {
    let fps = fps.max(1) as u128;
    let index = elapsed.as_nanos().saturating_mul(fps) / NANOS_PER_SECOND;
    index.min(u64::MAX as u128) as u64
}

pub fn frame_period_duration(fps: u32) -> Duration {
    frame_offset_duration(1, fps)
}

pub fn scheduler_lateness_gauge(loop_id: &str) -> IntGauge {
    metrics::get_metrics()
        .get_or_create_labelled_gauge(
            "server_scheduler_lateness_us",
            "Server producer loop lateness relative to its absolute frame schedule in microseconds",
            &["loop_id"],
            &[loop_id],
        )
        .expect("failed to create server scheduler lateness metric")
}

pub fn sleep_until_and_record_lateness(target: Instant, lateness_metric: &IntGauge) -> Duration {
    let now = Instant::now();
    let entry_lateness = if now < target {
        Duration::ZERO
    } else {
        now.duration_since(target)
    };

    if now < target {
        thread::sleep(target - now);
    }

    let after_wait = Instant::now();
    let observed_lateness = if after_wait < target {
        Duration::ZERO
    } else {
        after_wait.duration_since(target)
    };
    lateness_metric.set(duration_to_i64_us(observed_lateness));

    entry_lateness
}

pub fn duration_to_i64_us(duration: Duration) -> i64 {
    duration.as_micros().min(i64::MAX as u128) as i64
}

pub fn micros_to_timescale_units(timestamp_us: u64, timescale: u32) -> u64 {
    let units = (timestamp_us as u128).saturating_mul(timescale as u128) / MICROS_PER_SECOND;
    units.min(u64::MAX as u128) as u64
}
