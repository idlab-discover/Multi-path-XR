use metrics::{MetricsBuilder, get_all_interfaces, start_server};
use tracing::{info, debug, error};
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use tokio::time::{self, Duration};

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
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(1));
        loop {
            metrics_clone.update();
            debug!("Metrics updated");
            interval.tick().await;
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
