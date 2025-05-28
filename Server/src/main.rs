// main.rs

use std::{collections::HashMap, sync::Arc, time};
use clap::{Parser, ValueEnum};
use metrics::{get_all_interfaces, Metrics, MetricsBuilder};
use tokio::{runtime, sync::oneshot, time as tokioTime};
use tracing::{debug, error, info, instrument, level_filters::LevelFilter};
use tracing_subscriber::{layer::SubscriberExt, Layer};
use rayon::ThreadPoolBuilder;

mod handlers;
mod services;
mod router;
mod decoders;
mod encoders;
mod processing;
mod ingress;
mod egress;
mod types;
mod generators;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, ValueEnum)]
enum LogLevel {
    Trace = 0, // Designates very fine-grained informational events, extremely verbose.
    Debug = 1, // Designates fine-grained informational events.
    Info = 2, // Designates informational messages.
    Warn = 3, // Designates hazardous situations.
    Error = 4, // Designates very serious errors.
}

#[derive(Parser, Debug)]
#[command(author, version, about = "pc-server")]
struct Args {
    // Set the port number
    #[arg(short, long, default_value = "3001")]
    port: u16,
    // Set the log level (possible values: error, warn, info, debug, trace)
    #[arg(short, long, default_value = "info")]
    log_level: LogLevel,
    /// Number of threads in the thread pool
    #[arg(short, long, default_value_t = 10)]
    threads: usize,
    /// FLUTE endpoint URL
    #[arg(long, default_value = "239.0.2.1")]
    flute_endpoint_url: String,
    /// FLUTE port
    #[arg(long, default_value_t = 40085)]
    flute_port: u16,
}

#[instrument(skip_all)]
fn main() -> Result<(), Box<dyn std::error::Error>> {

    // Parse command-line arguments
    let args = Args::parse();

    // Build the FmtSubscriber layer
    let fmt_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .compact()
        .with_target(false)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_filter(match args.log_level {
            LogLevel::Trace => LevelFilter::TRACE,
            LogLevel::Debug => LevelFilter::DEBUG,
            LogLevel::Info => LevelFilter::INFO,
            LogLevel::Warn => LevelFilter::WARN,
            LogLevel::Error => LevelFilter::ERROR,
        });

    // Initialize console tracing if enabled
    #[cfg(feature = "console-tracing")]
    let subscriber = {
        let console_layer = console_subscriber::ConsoleLayer::builder()
            .retention(std::time::Duration::from_secs(60))
            .server_addr(([127, 0, 0, 1], 5556))
            .spawn();
        let tracy_layer = tracing_tracy::TracyLayer::default();
        tracing_subscriber::registry()
            .with(console_layer)
            .with(tracy_layer)
            .with(fmt_layer)
    };

    #[cfg(not(feature = "console-tracing"))]
    let subscriber = {
        tracing_subscriber::registry()
            .with(fmt_layer)
    };

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global default subscriber");


    info!("{:?}", args);

    let runtime = runtime::Builder::new_multi_thread()
        //.worker_threads(2)
        .thread_name_fn(|| {
            static ATOMIC_WEBRTC_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let id = ATOMIC_WEBRTC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            format!("MAIN_R w-{}", id)
        })
        .enable_all()
        .build().unwrap();


    // Initialize thread pool
    let thread_pool = Arc::new(
        ThreadPoolBuilder::new()
            .thread_name(|i| format!("Tpool w-{}", i+1))
            .num_threads(args.threads)
            .build()
            .expect("Failed to build thread pool"),
    );

    // Thread-safe storage for active jobs
    let active_jobs = Arc::new(tokio::sync::RwLock::new(HashMap::<String, oneshot::Sender<()>>::new()));

    // Retrieve all network interfaces
    let interfaces = get_all_interfaces();
    if interfaces.is_empty() {
        error!("No network interfaces found to track.");
        return Err("No network interfaces available.".into());
    }
    info!("Tracking the following interfaces: {:?}", interfaces);

    // Build the metrics instance, tracking all interfaces
    let mut builder = MetricsBuilder::new().add_label("mode", "server");

    for interface in interfaces {
        builder = builder.track_interface(&interface);
    }

    let metrics = builder.build();

    // Start the metrics update loop
    // These are for some default system metrics
    // We are responsible for updating your custom metrics
    let metrics_clone = Arc::new(metrics);
    runtime.spawn(update_metrics_loop(metrics_clone));

    // Initialize services
    let stream_manager = Arc::new(services::stream_manager::StreamManager::new());
    let mut mpd_manager = services::mpd_manager::MpdManager::new();
    let processing_pipeline = Arc::new(processing::ProcessingPipeline::new(thread_pool.clone()));

    // Add signalling callback to the MPD manager
    let stream_manager_clone = stream_manager.clone();
    let callback = {
        let stream_manager_clone = stream_manager_clone.clone();
        runtime.block_on(async move {
            let local_runtime = tokio::runtime::Handle::current();
            Arc::new(move |group_id: String| {
                let stream_manager_clone = stream_manager_clone.clone();
                local_runtime.spawn(async move {
                    if let Some(io) = stream_manager_clone.get_socket_io() {
                        let _ = io.emit("mpd::group_id", &group_id);
                    } else {
                        error!("Socket IO is not initialized");
                    }
                });
            })
        })
    };

    mpd_manager.set_notify_callback(callback);
    // Wrap the MPD manager in an Arc
    let mpd_manager = Arc::new(mpd_manager);



    // Initialize singleton egress protocols
    egress::initialize_egress_protocols(
        stream_manager.clone(),
        mpd_manager.clone(),
        processing_pipeline.clone(),
        args.flute_endpoint_url.clone(),
        args.flute_port,
    );

    // Initialize singleton ingress protocols
    ingress::initialize_ingress_protocols(
        stream_manager.clone(),
        processing_pipeline.clone(),
    );

    // Create router
    let app = router::create_router(
        stream_manager.clone(),
        processing_pipeline.clone(),
        active_jobs.clone().into(),
    );

    runtime.block_on(async move {
        let addr: std::net::SocketAddr = format!("0.0.0.0:{}", args.port).parse().unwrap();
        let sock = socket2::Socket::new(
            match addr {
                std::net::SocketAddr::V4(_) => socket2::Domain::IPV4,
                std::net::SocketAddr::V6(_) => socket2::Domain::IPV6,
            },
            socket2::Type::STREAM, // Will become SOCK_CLOEXEC internally on Linux
            None,
        ).unwrap();

        sock.set_reuse_address(true).unwrap();
        #[cfg(unix)]
        sock.set_reuse_port(true).unwrap();
        sock.set_nonblocking(true).unwrap();
        sock.bind(&addr.into()).unwrap();
        sock.listen(1024).unwrap();

        let listener = tokio::net::TcpListener::from_std(sock.into()).unwrap();

        // let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", args.port)).await.unwrap();
        axum::serve(listener, app).await.unwrap();
    });

    info!("Server started");


    loop {
        std::thread::sleep(time::Duration::from_secs(1));
    }

    #[allow(unreachable_code)]
    Ok(())
}

#[instrument(skip_all)]
async fn update_metrics_loop(metrics: Arc<Metrics>) {
    let mut interval = tokioTime::interval(tokioTime::Duration::from_secs(1));
    loop {
        metrics.update();
        debug!("Metrics updated");
        interval.tick().await;
    }
}