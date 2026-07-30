use rayon::ThreadPoolBuilder;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::info;
use tracing_subscriber::FmtSubscriber;

mod graph;
mod handlers;
mod metrics_logger;
mod router;
mod structs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .compact()
        .without_time()
        .with_file(false)
        .with_line_number(false)
        .with_target(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;

    info!("Starting controller");

    // Build a multi-threaded Tokio runtime with custom worker thread names.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        // Use a counter to create stable names like "MAIN_R w-0", "MAIN_R w-1", ...
        .thread_name_fn(|| {
            static ATOMIC_WEBRTC_ID: AtomicUsize = AtomicUsize::new(0);
            let id = ATOMIC_WEBRTC_ID.fetch_add(1, Ordering::SeqCst);
            format!("MAIN_R w-{id}")
        })
        // .worker_threads(10) // optional: set an explicit number, otherwise Tokio picks one
        .build()?;

    rt.block_on(async {
        // Create a common thread pool with a desired number of threads
        let thread_pool = Arc::new(
            ThreadPoolBuilder::new()
                .thread_name(|i| format!("TP w-{}", i + 1))
                .num_threads(10)
                .build()
                .unwrap(),
        );

        // Thread-safe storage for active jobs
        let active_jobs = Arc::new(tokio::sync::RwLock::new(HashMap::<
            String,
            oneshot::Sender<()>,
        >::new()));

        let app = router::create_router(active_jobs.clone(), thread_pool.clone());

        let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
            .await
            .map_err(|e| format!("Failed to bind to port 3000: {e}"))?;
        axum::serve(listener, app).await.unwrap();

        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    Ok(())
}
