use std::sync::Arc;

use metrics::{get_all_interfaces, MetricsBuilder, start_server};
use tokio::runtime::Builder;
use tracing::{debug, error, info};


pub fn create_metrics() -> Result<(), Box<dyn std::error::Error>> {
    // Retrieve all network interfaces
    let interfaces = get_all_interfaces();
    if interfaces.is_empty() {
        error!("No network interfaces found to track.");
        return Err("No network interfaces available.".into());
    }
    info!("Tracking the following interfaces: {:?}", interfaces);

    // Build the metrics instance, tracking all interfaces
    let mut builder = MetricsBuilder::new().add_label("mode", "client");

    for interface in interfaces {
        builder = builder.track_interface(&interface);
    }

    let metrics = builder.build();

    // Start the metrics update loop
    // These are for some default system metrics
    // We are responsible for updating your custom metrics
    let metrics_clone = Arc::new(metrics);
    std::thread::spawn(move || {
        let interval = std::time::Duration::from_secs(1);
        loop {
            metrics_clone.update();
            debug!("Metrics updated");
            std::thread::sleep(interval);
        }
    });
    Ok(())
}

pub fn start_metrics_server(port: u16) {
    // Spawn a new thread
    std::thread::spawn(move || {
        // Inside this thread, create a runtime
        let runtime = 
            Builder::new_multi_thread()
                .thread_name_fn(|| {
                    static ATOMIC_WEBRTC_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                    let id = ATOMIC_WEBRTC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    format!("MTRC_R w-{}", id)
                })
                .enable_all()
                .build()
                .expect("Failed to build runtime");

        // Now, run the server from the runtime
        runtime.block_on(async {
            start_server(port).await;
        });

    });
}