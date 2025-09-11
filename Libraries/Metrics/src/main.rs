use metrics::{MetricsBuilder, get_all_interfaces, start_server};
use tracing::{info, debug, error};
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use tokio::time::{self, Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing subscriber for logging
    let subscriber = FmtSubscriber::new();
    tracing::subscriber::set_global_default(subscriber)?;

    // Retrieve all network interfaces
    let interfaces = get_all_interfaces();
    if interfaces.is_empty() {
        error!("No network interfaces found to track.");
        return Err("No network interfaces available.".into());
    }
    info!("Tracking the following interfaces: {:?}", interfaces);

    // Build the metrics instance, tracking all interfaces
    let mut builder = MetricsBuilder::new().add_label("mode", "standalone"); // Example label
    for interface in interfaces {
        builder = builder.track_interface(&interface);
    }
    builder = builder.add_gauge("custom_metric", "A custom example metric"); // Example custom metric
    let metrics = builder.build();

    // Start the metrics update loop
    // These are for some default system metrics
    // You are responsible for updating your custom metrics
    let metrics_clone = Arc::new(metrics);
    tokio::spawn({
        let metrics = Arc::clone(&metrics_clone);

        const PERIOD: Duration = Duration::from_secs(1);
        const CATCHUP_FRACTION: f64 = 0.75;                 // shave up to 75% to catch up
        const SKIP_THRESHOLD: Duration = Duration::from_millis(950); // only skip if ≥ 0.95s late
        async move {

            // Anchor to a fixed monotonic grid: t = start + n * PERIOD
            let start: Instant = Instant::now();
            let mut tick_idx: u64 = 1;
            loop {
                // 1) Do the work for this tick
                metrics.update();
                debug!("Metrics updated");

                // 2) Drift-resistant timing with bounded catch-up
                let now = Instant::now();
                let target = start + PERIOD.saturating_mul(tick_idx as u32);

                if now < target {
                    // Early: sleep exactly until the grid time (no drift)
                    time::sleep_until(target).await;
                    tick_idx += 1;
                    continue;
                }

                // We're late compared to the grid
                let lateness = now.saturating_duration_since(target);

                if lateness < SKIP_THRESHOLD {
                    // Prefer not to skip: shorten the *next* sleep (bounded by CATCHUP_FRACTION)
                    let catchup_cap = PERIOD.mul_f64(CATCHUP_FRACTION);
                    let shave = if lateness > catchup_cap { catchup_cap } else { lateness };
                    let sleep_dur = PERIOD.saturating_sub(shave);

                    if !sleep_dur.is_zero() {
                        time::sleep(sleep_dur).await;
                    }
                    tick_idx += 1;
                } else {
                    // Very late (~full second): snap to current grid slot (single skip)
                    let elapsed = now - start;
                    let full_ticks = (elapsed.as_nanos() / PERIOD.as_nanos()) as u64;
                    tick_idx = full_ticks + 1;
                    debug!("update loop late by {:?}; snapping to tick {}", lateness, tick_idx);
                    // no sleep; immediately loop again
                }
            }
        }
    });

    // Start the server on port 8080 (optional)
    tokio::spawn(start_server(8080));

    // Main application logic here
    info!("Metrics server running on http://0.0.0.0:8080/metrics");

    // Keep the main thread alive
    loop {
        time::sleep(Duration::from_secs(60)).await;
        debug!("Main thread still alive");
    }
}
