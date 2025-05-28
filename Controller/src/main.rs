use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::FmtSubscriber;
use tokio::sync::oneshot;
use rayon::ThreadPoolBuilder;


mod graph;
mod handlers;
mod metrics_logger;
mod router;
mod structs;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = FmtSubscriber::builder()
        .compact()
        .without_time()
        .with_file(false)
        .with_line_number(false)
        .with_target(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber)?;

    info!("Starting controller");

    // Create a common thread pool with a desired number of threads
    let thread_pool = Arc::new(ThreadPoolBuilder::new().num_threads(10).build().unwrap());

    // Thread-safe storage for active jobs
    let active_jobs = Arc::new(tokio::sync::RwLock::new(HashMap::<String, oneshot::Sender<()>>::new()));

    let app = router::create_router(active_jobs.clone(), thread_pool.clone());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.map_err(| e| format!("Failed to bind to port 3000: {}", e))?;
    axum::serve(listener, app).await.unwrap();

    Ok(())
}